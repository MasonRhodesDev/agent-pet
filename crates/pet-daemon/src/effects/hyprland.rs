//! Hyprland enhancement backend for focus: socket1 IPC (dispatches and
//! `j/clients` queries) plus pid-ancestry window matching. Guarded by
//! `HYPRLAND_INSTANCE_SIGNATURE`; on other compositors the ladder reports
//! that only the kitty/tmux paths are available.
//!
//! TODO(foreign-toplevel): compositor-agnostic focus belongs to the renderer
//! via `zwlr_foreign_toplevel_management_v1.activate()`; this module is the
//! Hyprland-only enhancement in the meantime.

use std::sync::OnceLock;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::debug;

const MAX_ANCESTRY_HOPS: usize = 20;

/// Config-manager dialect of the running Hyprland, detected once per daemon
/// lifetime from the first dispatch reply (per the dual-mode policy: never
/// a hyprlang-only path).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialect {
    Classic,
    Lua,
}

static DIALECT: OnceLock<Dialect> = OnceLock::new();

fn socket_path() -> Option<String> {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok()?;
    let signature = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    Some(format!("{runtime_dir}/hypr/{signature}/.socket.sock"))
}

/// One request/response exchange on Hyprland's socket1.
async fn command(cmd: &str) -> anyhow::Result<String> {
    let path = socket_path()
        .ok_or_else(|| anyhow::anyhow!("not under Hyprland (no HYPRLAND_INSTANCE_SIGNATURE)"))?;
    let mut stream = tokio::net::UnixStream::connect(&path).await?;
    stream.write_all(cmd.as_bytes()).await?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply).await?;
    Ok(reply)
}

async fn dispatch(args: &str) -> anyhow::Result<()> {
    let reply = command(&format!("dispatch {args}")).await?;
    anyhow::ensure!(reply.trim() == "ok", "hyprland said {reply:?}");
    Ok(())
}

/// Focus a window by selector (`pid:N`, `address:0x...`), dual-dialect:
/// classic hyprlang `focuswindow <sel>` first; when the reply carries the
/// Lua-config shorthand signature (`hl.dispatch(...)` — classic string
/// dispatchers are unreachable there), retry with the Lua dispatcher form
/// (`hl.dsp.focus({ window = "<sel>" })`, live-verified to move focus) and
/// remember the dialect so later dispatches skip the failing probe.
async fn dispatch_focus_selector(selector: &str) -> anyhow::Result<()> {
    if DIALECT.get() == Some(&Dialect::Lua) {
        return dispatch(&lua_focus(selector)).await;
    }
    let reply = command(&format!("dispatch focuswindow {selector}")).await?;
    if reply.trim() == "ok" {
        let _ = DIALECT.set(Dialect::Classic);
        return Ok(());
    }
    if !is_lua_dialect_reply(&reply) {
        anyhow::bail!("hyprland said {reply:?}");
    }
    let _ = DIALECT.set(Dialect::Lua);
    dispatch(&lua_focus(selector)).await
}

fn lua_focus(selector: &str) -> String {
    format!("hl.dsp.focus({{ window = \"{selector}\" }})")
}

/// Detect a Lua-config Hyprland from a dispatch error reply: those builds
/// evaluate `dispatch <text>` as `return hl.dispatch(<text>)`, so classic
/// string-dispatcher syntax fails with an error naming that shorthand. Pure.
fn is_lua_dialect_reply(reply: &str) -> bool {
    reply.contains("hl.dispatch")
}

/// Best-effort raise by pid (kitty fast path); silently skipped when not
/// under Hyprland.
pub async fn dispatch_focus_pid(pid: u32) {
    if let Err(e) = dispatch_focus_selector(&format!("pid:{pid}")).await {
        debug!("hyprland focus dispatch skipped: {e}");
    }
}

/// Rung c: walk `pid`'s parent chain and focus the Hyprland client owning
/// the nearest ancestor (the terminal hosting a tmux client / harness).
pub async fn focus_by_ancestry(pid: u32) -> Result<(), String> {
    if std::env::var("HYPRLAND_INSTANCE_SIGNATURE").is_err() {
        // TODO(foreign-toplevel): activate() via the renderer's Wayland
        // connection on wlroots compositors.
        return Err(
            "not under Hyprland; only the kitty/tmux focus paths are available".into(),
        );
    }
    let chain = ancestry_chain(pid, read_proc_stat);
    let clients = command("j/clients")
        .await
        .map_err(|e| format!("querying hyprland clients: {e}"))?;
    let addr = find_client_address(&clients, &chain)
        .ok_or_else(|| format!("no Hyprland client in the ancestry of pid {pid} {chain:?}"))?;
    dispatch_focus_selector(&format!("address:{addr}"))
        .await
        .map_err(|e| format!("focuswindow address:{addr}: {e}"))
}

pub(crate) fn read_proc_stat(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()
}

/// Walk the ppid chain starting at (and including) `pid`, capped at
/// `MAX_ANCESTRY_HOPS`. `read_stat` is injected so the walk is testable.
pub(crate) fn ancestry_chain(pid: u32, read_stat: impl Fn(u32) -> Option<String>) -> Vec<u32> {
    let mut chain = vec![pid];
    let mut cur = pid;
    for _ in 0..MAX_ANCESTRY_HOPS {
        let Some(ppid) = read_stat(cur).as_deref().and_then(parse_stat_ppid) else {
            break;
        };
        if ppid <= 1 {
            break; // init/kthreads never own a window
        }
        chain.push(ppid);
        cur = ppid;
    }
    chain
}

/// Extract the ppid (field 4) from `/proc/<pid>/stat` content. The comm
/// field is parenthesized and may itself contain spaces or parens, so real
/// fields resume after the LAST `)`. Pure.
fn parse_stat_ppid(stat: &str) -> Option<u32> {
    let rest = stat.rsplit_once(')')?.1;
    let mut fields = rest.split_whitespace();
    fields.next()?; // state
    fields.next()?.parse().ok()
}

/// Match Hyprland `j/clients` JSON against an ancestry chain; the ancestor
/// closest to the process wins. Returns the client's window address. Pure.
fn find_client_address(clients_json: &str, ancestry: &[u32]) -> Option<String> {
    let clients: Vec<serde_json::Value> = serde_json::from_str(clients_json).ok()?;
    ancestry.iter().find_map(|pid| {
        clients.iter().find_map(|c| {
            (c.get("pid")?.as_u64()? == u64::from(*pid))
                .then(|| c.get("address")?.as_str().map(str::to_owned))
                .flatten()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn stat_ppid_survives_hostile_comm() {
        assert_eq!(parse_stat_ppid("1234 (bash) S 42 1234 1234 0 -1"), Some(42));
        // comm containing spaces and a nested paren, e.g. "(tmux: server)".
        assert_eq!(
            parse_stat_ppid("999 (tmux: server (v3.5a)) S 7 999 999 0 -1"),
            Some(7)
        );
        assert_eq!(parse_stat_ppid("truncated (comm"), None);
        assert_eq!(parse_stat_ppid("1 (x) Z"), None);
        assert_eq!(parse_stat_ppid(""), None);
    }

    fn fake_tree(edges: &[(u32, u32)]) -> impl Fn(u32) -> Option<String> + '_ {
        let map: HashMap<u32, u32> = edges.iter().copied().collect();
        move |pid| {
            map.get(&pid)
                .map(|ppid| format!("{pid} (proc {pid}) S {ppid} 1 1 0 -1"))
        }
    }

    #[test]
    fn ancestry_walks_to_init_and_stops() {
        // claude(500) -> tmux server(400) -> ... except tmux daemonized to 1.
        let read = fake_tree(&[(500, 400), (400, 1)]);
        assert_eq!(ancestry_chain(500, read), vec![500, 400]);
    }

    #[test]
    fn ancestry_includes_terminal_chain() {
        // claude(500) <- shell(300) <- kitty(200) <- systemd(1)
        let read = fake_tree(&[(500, 300), (300, 200), (200, 1)]);
        assert_eq!(ancestry_chain(500, read), vec![500, 300, 200]);
    }

    #[test]
    fn ancestry_caps_hops_on_cycles() {
        // Degenerate self-parent loop must terminate.
        let read = fake_tree(&[(5, 5)]);
        let chain = ancestry_chain(5, read);
        assert!(chain.len() <= MAX_ANCESTRY_HOPS + 1);
    }

    #[test]
    fn ancestry_survives_missing_stat() {
        let read = fake_tree(&[]);
        assert_eq!(ancestry_chain(42, read), vec![42]);
    }

    #[test]
    fn lua_dialect_detected_from_real_replies() {
        // Verbatim (truncated) replies observed live on the Lua-config build.
        assert!(is_lua_dialect_reply(
            "error: [string \"return hl.dispatch(focuswindow address:0x5b81...\"]:1: \
             ')' expected near 'address'\n\n → Note: dispatch in lua is a shorthand \
             for hl.dispatch(...), your syntax might need to be updated."
        ));
        assert!(!is_lua_dialect_reply("ok"));
        assert!(!is_lua_dialect_reply("Invalid dispatcher"));
        assert!(!is_lua_dialect_reply("error: window not found"));
    }

    #[test]
    fn closest_ancestor_wins_client_match() {
        let clients = r#"[
            {"address": "0xaaa", "pid": 200, "class": "kitty"},
            {"address": "0xbbb", "pid": 300, "class": "kitty"}
        ]"#;
        // 300 appears earlier in the chain (closer to the process) than 200.
        assert_eq!(
            find_client_address(clients, &[500, 300, 200]).as_deref(),
            Some("0xbbb")
        );
        assert_eq!(
            find_client_address(clients, &[500, 200]).as_deref(),
            Some("0xaaa")
        );
        assert_eq!(find_client_address(clients, &[500, 501]), None);
        assert_eq!(find_client_address("not json", &[200]), None);
        assert_eq!(find_client_address("[]", &[200]), None);
    }
}
