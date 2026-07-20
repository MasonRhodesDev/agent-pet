//! Hook emitters. Contract: fast, silent-on-failure, exit 0 no matter what.

use std::io::Read;
use std::process::ExitCode;

use pet_proto::{Event, Meta};

use crate::bus;

pub fn run(rest: &[&str]) -> ExitCode {
    if let Err(e) = try_run(rest) {
        eprintln!("agent-pet emit: {e:#}");
    }
    ExitCode::SUCCESS
}

fn try_run(rest: &[&str]) -> anyhow::Result<()> {
    let event = match rest {
        ["claude"] | ["pi"] => {
            let json = read_stdin()?;
            // Claude's Stop/idle-Notification bodies come from the transcript
            // (the real answer / question), not the canned hook string.
            let tail = claude_tail(&json);
            pet_adapters::claude::map_hook(&json, parent_pid(), tail)?
        }
        // Headless `codex exec` runs (automation, pane-summarizer title jobs)
        // are non-interactive, unfocusable, and flood the pet with one-shot
        // sessions — drop them. Interactive codex (the TUI) is kept.
        ["codex"] if codex_is_headless() => {
            drain_stdin();
            return Ok(());
        }
        ["codex-notify", _] if codex_is_headless() => return Ok(()),
        ["codex"] => pet_adapters::codex::map_hook(&read_stdin()?, parent_pid())?,
        ["codex-notify", json] => pet_adapters::codex::map_notify(json)?,
        _ => anyhow::bail!("unknown emit target {rest:?}"),
    };

    let Some(mut event) = event else {
        return Ok(()); // hook event the pet doesn't care about
    };
    enrich(&mut event);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(bus::send_event(&event))
}

/// For Stop / generic Notification, read the last assistant message from the
/// transcript so the alert says something specific. Bounded and time-guarded
/// so a hook is never slowed.
fn claude_tail(json: &str) -> pet_adapters::claude::Tail {
    use pet_adapters::claude::{ClaudeHookPayload, Tail};
    let Ok(payload) = serde_json::from_str::<ClaudeHookPayload>(json) else {
        return Tail::default();
    };
    match payload.hook_event_name.as_str() {
        "Stop" | "Notification" => {}
        _ => return Tail::default(),
    }
    payload
        .transcript_path
        .as_deref()
        .map(transcript_tail)
        .unwrap_or_default()
}

/// Scan the tail of a Claude transcript JSONL for the last assistant text.
/// Reads at most the final 64 KiB and reverse-scans lines, so cost is a few
/// `serde_json` parses regardless of transcript size. The returned `Tail`
/// also flags `isApiErrorMessage` entries (a turn that died on an API error).
fn transcript_tail(path: &str) -> pet_adapters::claude::Tail {
    transcript_tail_inner(path).unwrap_or_default()
}

fn transcript_tail_inner(path: &str) -> Option<pet_adapters::claude::Tail> {
    use pet_adapters::claude::Tail;
    use std::io::{Read, Seek, SeekFrom};

    const WINDOW: u64 = 64 * 1024;
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(WINDOW);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::with_capacity(WINDOW as usize);
    file.take(WINDOW).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);

    let mut lines: Vec<&str> = text.split('\n').collect();
    // A non-zero start almost certainly began mid-line: drop that torn head.
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    for line in lines.iter().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue; // torn trailing line from a live append
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        let content = v.get("message").and_then(|m| m.get("content"));
        let Some(blocks) = content.and_then(|c| c.as_array()) else {
            continue;
        };
        // A pending AskUserQuestion is the actual thing needing input — show
        // the question, not "needs permission to use AskUserQuestion".
        if let Some(q) = blocks.iter().find_map(ask_user_question) {
            return Some(Tail::text(Some(q)));
        }
        let text: Vec<&str> = blocks
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect();
        if !text.is_empty() {
            // Claude tags a turn that died on an API error (overload, dropped
            // stream, 5xx) with isApiErrorMessage — that turn is Blocked.
            let is_error = v
                .get("isApiErrorMessage")
                .and_then(|e| e.as_bool())
                .unwrap_or(false);
            return Some(Tail {
                text: Some(text.join("\n")),
                is_error,
            });
        }
    }
    None
}

/// Extract the question text from an `AskUserQuestion` tool_use block, if that
/// is what this content block is. Prefers the first question's `question`,
/// falling back to its `header`.
fn ask_user_question(block: &serde_json::Value) -> Option<String> {
    if block.get("type").and_then(|t| t.as_str()) != Some("tool_use")
        || block.get("name").and_then(|n| n.as_str()) != Some("AskUserQuestion")
    {
        return None;
    }
    let questions = block
        .get("input")
        .and_then(|i| i.get("questions"))
        .and_then(|q| q.as_array())?;
    let first = questions.first()?;
    let q = first
        .get("question")
        .or_else(|| first.get("header"))
        .and_then(|t| t.as_str())?;
    Some(if questions.len() > 1 {
        format!("{q} (+{} more)", questions.len() - 1)
    } else {
        q.to_string()
    })
}

fn read_stdin() -> anyhow::Result<String> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}

/// Consume stdin without buffering it. Even a dropped hook must drain the
/// pipe: the harness blocks writing payloads past the pipe buffer and gets
/// EPIPE ("failed to write hook stdin: Broken pipe") if we exit first.
fn drain_stdin() {
    let _ = std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink());
}

/// True when this codex hook/notify was fired by a headless `codex exec`
/// (as opposed to the interactive TUI). The emitter's ancestry includes the
/// codex process; walk up a few levels (past any node/shell shim) and look
/// for a `codex … exec` invocation.
fn codex_is_headless() -> bool {
    let mut pid = parent_pid();
    for _ in 0..6 {
        let Some(p) = pid else { break };
        if let Some(cmdline) = std::fs::read_to_string(format!("/proc/{p}/cmdline")).ok() {
            let args: Vec<&str> = cmdline.split('\0').filter(|a| !a.is_empty()).collect();
            if cmdline_is_codex_exec(&args) {
                return true;
            }
        }
        pid = std::fs::read_to_string(format!("/proc/{p}/stat"))
            .ok()
            .and_then(|s| parse_stat_ppid(&s));
    }
    false
}

/// Args form a `codex … exec …` invocation: a `codex`-like binary followed by
/// the `exec` subcommand (before any non-subcommand flag). Pure.
fn cmdline_is_codex_exec(args: &[&str]) -> bool {
    let Some(bin_idx) = args
        .iter()
        .position(|a| a.rsplit('/').next().is_some_and(|b| b.starts_with("codex")))
    else {
        return false;
    };
    args.get(bin_idx + 1..)
        .unwrap_or_default()
        .iter()
        .take_while(|a| !a.starts_with('-'))
        .take(2)
        .any(|a| *a == "exec")
}

fn parent_pid() -> Option<u32> {
    // The hook subprocess's parent is the harness process itself.
    std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|s| parse_stat_ppid(&s))
}

/// Extract the ppid (field 4) from `/proc/<pid>/stat` content. The comm
/// field (2) is parenthesized and may itself contain spaces or parens, so
/// real fields resume after the LAST `)`.
fn parse_stat_ppid(stat: &str) -> Option<u32> {
    let rest = stat.rsplit_once(')')?.1;
    let mut fields = rest.split_whitespace();
    fields.next()?; // state
    fields.next()?.parse().ok()
}

/// Terminal correlation for click-to-focus. Hooks inherit the harness's
/// environment (kitty ids, tmux socket/pane) but NOT its stdio — hook
/// stderr is usually a pipe — so the controlling tty is read from the
/// PARENT process's fds instead of our own.
fn enrich(event: &mut Event) {
    if event.meta.pane.is_none() {
        if let (Ok(pid), Ok(win)) = (std::env::var("KITTY_PID"), std::env::var("KITTY_WINDOW_ID"))
        {
            event.meta.pane = Some(format!("kitty-{pid}-{win}"));
        }
    }
    if event.meta.tty.is_none() {
        event.meta.tty = parent_tty(parent_pid());
    }
    enrich_tmux(&mut event.meta, std::env::var("TMUX").ok(), std::env::var("TMUX_PANE").ok());
}

/// The harness process still holds the pty on stdin (fd 0); fall back to
/// stdout/stderr. Only real terminal devices count.
fn parent_tty(ppid: Option<u32>) -> Option<String> {
    let ppid = ppid?;
    for fd in [0u32, 1, 2] {
        if let Ok(target) = std::fs::read_link(format!("/proc/{ppid}/fd/{fd}")) {
            let target = target.to_string_lossy();
            if is_tty_path(&target) {
                return Some(target.into_owned());
            }
        }
    }
    None
}

fn is_tty_path(path: &str) -> bool {
    path.starts_with("/dev/pts/") || path.starts_with("/dev/tty")
}

/// tmux correlation (gt-managed town agents run under a private tmux server
/// with no kitty env): store the server socket and pane id so the daemon's
/// focus ladder can switch an attached client to this pane.
fn enrich_tmux(meta: &mut Meta, tmux_env: Option<String>, tmux_pane: Option<String>) {
    if meta.extra.contains_key("tmux_socket") || meta.extra.contains_key("tmux_pane") {
        return;
    }
    let (Some(tmux_env), Some(pane)) = (tmux_env, tmux_pane) else {
        return;
    };
    let Some(socket) = tmux_socket_path(&tmux_env) else {
        return;
    };
    if pane.is_empty() {
        return;
    }
    meta.extra.insert("tmux_socket".into(), socket.into());
    meta.extra.insert("tmux_pane".into(), pane.into());
}

/// `$TMUX` is `<socket_path>,<server_pid>,<session_idx>`.
fn tmux_socket_path(tmux_env: &str) -> Option<String> {
    let first = tmux_env.split(',').next()?;
    (!first.is_empty()).then(|| first.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_ppid_survives_hostile_comm() {
        assert_eq!(parse_stat_ppid("1234 (bash) S 42 1 1 0 -1"), Some(42));
        assert_eq!(
            parse_stat_ppid("999 (tmux: server (v3)) S 7 9 9 0 -1"),
            Some(7)
        );
        assert_eq!(parse_stat_ppid("garbage"), None);
        assert_eq!(parse_stat_ppid("1 (x) Z"), None);
    }

    #[test]
    fn tmux_socket_is_first_comma_field() {
        assert_eq!(
            tmux_socket_path("/tmp/tmux-1000/default,12345,3").as_deref(),
            Some("/tmp/tmux-1000/default")
        );
        assert_eq!(
            tmux_socket_path("/tmp/gt/town.sock,1,0").as_deref(),
            Some("/tmp/gt/town.sock")
        );
        assert_eq!(tmux_socket_path(",1,0"), None);
        assert_eq!(tmux_socket_path(""), None);
    }

    #[test]
    fn detects_codex_exec_invocations() {
        assert!(cmdline_is_codex_exec(&["codex", "exec", "do a thing"]));
        assert!(cmdline_is_codex_exec(&["/usr/bin/codex", "exec"]));
        // node shim wrapping the real binary.
        assert!(cmdline_is_codex_exec(&["node", "/x/codex-linux-x64/codex", "exec", "prompt"]));
        // Interactive TUI (no exec subcommand) is NOT headless.
        assert!(!cmdline_is_codex_exec(&["codex"]));
        assert!(!cmdline_is_codex_exec(&["codex", "--model", "gpt-5"]));
        assert!(!cmdline_is_codex_exec(&["bash", "-c", "something"]));
    }

    #[test]
    fn tty_paths_must_be_terminal_devices() {
        assert!(is_tty_path("/dev/pts/4"));
        assert!(is_tty_path("/dev/tty2"));
        assert!(!is_tty_path("pipe:[123456]"));
        assert!(!is_tty_path("/dev/null"));
        assert!(!is_tty_path("socket:[99]"));
    }

    fn write_transcript(lines: &[&str]) -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
        f.flush().unwrap();
        f
    }

    #[test]
    fn transcript_tail_finds_last_assistant_text() {
        let f = write_transcript(&[
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"hi"}]}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"first answer"}]}}"#,
            r#"{"type":"attachment","foo":1}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","text":"hmm"},{"type":"text","text":"the real answer"},{"type":"tool_use","name":"Bash"}]}}"#,
            r#"{"type":"system","event":"x"}"#,
        ]);
        assert_eq!(
            transcript_tail(f.path().to_str().unwrap()).text.as_deref(),
            Some("the real answer")
        );
    }

    #[test]
    fn transcript_tail_skips_torn_trailing_line() {
        let f = write_transcript(&[
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"prior good line"}]}}"#,
            // A truncated live-append line (invalid JSON) must be skipped.
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"tex",
        ]);
        assert_eq!(
            transcript_tail(f.path().to_str().unwrap()).text.as_deref(),
            Some("prior good line")
        );
    }

    #[test]
    fn transcript_tail_extracts_askuserquestion() {
        let f = write_transcript(&[
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Let me ask."},{"type":"tool_use","name":"AskUserQuestion","input":{"questions":[{"question":"Which DB?","header":"DB"},{"question":"Which region?"}]}}]}}"#,
        ]);
        assert_eq!(
            transcript_tail(f.path().to_str().unwrap()).text.as_deref(),
            Some("Which DB? (+1 more)")
        );
    }

    #[test]
    fn transcript_tail_flags_api_error_entries() {
        let f = write_transcript(&[
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"working on it"}]}}"#,
            r#"{"type":"assistant","isApiErrorMessage":true,"message":{"role":"assistant","content":[{"type":"text","text":"API Error: Connection closed mid-response."}]}}"#,
        ]);
        let tail = transcript_tail(f.path().to_str().unwrap());
        assert!(tail.is_error);
        assert_eq!(tail.text.as_deref(), Some("API Error: Connection closed mid-response."));

        // A clean final answer is not an error.
        let f = write_transcript(&[
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"all done"}]}}"#,
        ]);
        assert!(!transcript_tail(f.path().to_str().unwrap()).is_error);
    }

    #[test]
    fn transcript_tail_none_when_no_assistant_text() {
        let f = write_transcript(&[
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"hi"}]}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read"}]}}"#,
        ]);
        assert_eq!(transcript_tail(f.path().to_str().unwrap()).text, None);
        assert_eq!(transcript_tail("/no/such/file").text, None);
    }

    #[test]
    fn tmux_enrichment_fills_extra_without_clobbering() {
        let mut meta = Meta::default();
        enrich_tmux(
            &mut meta,
            Some("/tmp/tmux-1000/default,42,0".into()),
            Some("%7".into()),
        );
        assert_eq!(meta.extra.get("tmux_socket").unwrap(), "/tmp/tmux-1000/default");
        assert_eq!(meta.extra.get("tmux_pane").unwrap(), "%7");

        // Pre-existing hints win.
        let mut meta = Meta::default();
        meta.extra
            .insert("tmux_socket".into(), "/already/set".into());
        enrich_tmux(&mut meta, Some("/other,1,0".into()), Some("%9".into()));
        assert_eq!(meta.extra.get("tmux_socket").unwrap(), "/already/set");
        assert!(!meta.extra.contains_key("tmux_pane"));

        // Missing env → no-op.
        let mut meta = Meta::default();
        enrich_tmux(&mut meta, None, Some("%1".into()));
        assert!(meta.extra.is_empty());
    }
}
