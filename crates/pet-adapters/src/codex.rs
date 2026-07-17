//! Codex CLI payloads → Event, from its two complementary taps:
//!
//! - `hooks.json` hooks (`agent-pet emit codex`, stdin): lifecycle, running
//!   heartbeats, `permission_request` → needs-input (with the actual command
//!   / file / question as the body), and `stop` → ready (carrying
//!   `last_assistant_message` directly). Codex hooks share the Claude payload
//!   field names; event-name casing has varied across versions, so matching
//!   is case/underscore-insensitive.
//! - top-level `notify` (`agent-pet emit codex-notify`, argv JSON): delivers
//!   ONLY `agent-turn-complete`. Approvals never arrive here — that path is
//!   the hook `permission_request` above.

use pet_proto::{AgentState, Event, Meta, Source};
use serde::Deserialize;

use crate::hygiene;

/// Hook tap. Reuses the Claude payload shape (shared field names).
pub fn map_hook(json: &str, agent_pid: Option<u32>) -> Result<Option<Event>, serde_json::Error> {
    let payload: crate::claude::ClaudeHookPayload = serde_json::from_str(json)?;

    let (state, body) = match normalize(&payload.hook_event_name).as_str() {
        "sessionstart" => (AgentState::Running, None),
        "userpromptsubmit" => (
            AgentState::Running,
            payload.prompt.as_deref().and_then(hygiene::body),
        ),
        "posttooluse" | "toolexecutionend" => (AgentState::Running, payload.tool_name.clone()),
        "permissionrequest" => {
            let body = payload.tool_input.as_ref().and_then(|input| {
                approval_body(payload.tool_name.as_deref().unwrap_or(""), input)
            });
            (AgentState::Waiting, body)
        }
        "stop" | "sessionshutdown" => (
            AgentState::Ready,
            payload.last_assistant_message.as_deref().and_then(hygiene::body),
        ),
        // A turn that died on an error (overload, rate-limit, non-retryable)
        // is Blocked — the same signal the real Codex TUI shows via on_error.
        // Codex's error hook naming has varied; match liberally.
        "error" | "turnfailed" | "turnaborted" | "turnerror" => (
            AgentState::Failed,
            payload
                .message
                .as_deref()
                .or(payload.last_assistant_message.as_deref())
                .and_then(hygiene::body),
        ),
        "sessionend" => (AgentState::Gone, None),
        _ => return Ok(None),
    };

    Ok(Some(Event {
        v: pet_proto::PROTOCOL_VERSION,
        source: Source::Codex,
        session: payload.session_id,
        state,
        body,
        ts: None,
        via: None,
        meta: Meta {
            cwd: payload.cwd,
            tool: payload.tool_name,
            agent_pid,
            ..Default::default()
        },
    }))
}

/// Per-kind waiting body for a `permission_request`, ported from Codex's
/// notification templates. Unknown tools fall back to the tool name so an
/// unrecognized shape never panics or shows nothing.
fn approval_body(tool_name: &str, tool_input: &serde_json::Value) -> Option<String> {
    let t = tool_name.to_ascii_lowercase();
    let s = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_owned);

    // Exec / shell.
    if t.contains("bash") || t.contains("shell") || t.contains("exec") {
        if let Some(cmd) = s(tool_input, "command") {
            return hygiene::body_capped(&cmd, 30).map(|c| format!("Approval requested: {c}"));
        }
    }
    // Patch / edit.
    if t.contains("patch") || t.contains("edit") || t.contains("write") {
        let files = patch_files(tool_input);
        return Some(match files.as_slice() {
            [] => "Approval requested to edit files".to_string(),
            [one] => format!("wants to edit {}", basename(one)),
            many => format!("wants to edit {} files", many.len()),
        });
    }
    // MCP elicitation.
    if t.contains("mcp") || t.contains("elicit") {
        if let Some(server) = s(tool_input, "server").or_else(|| s(tool_input, "server_name")) {
            return Some(format!("Approval requested by {server}"));
        }
    }
    // Questions / plan.
    if let Some(questions) = tool_input.get("questions").and_then(|q| q.as_array()) {
        return Some(match questions.as_slice() {
            [] => "Input requested".to_string(),
            [one] => {
                let header = one
                    .get("header")
                    .or_else(|| one.get("question"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("Input requested");
                hygiene::body_capped(header, 30).unwrap_or_else(|| "Input requested".into())
            }
            many => format!("{} questions requested", many.len()),
        });
    }
    // Fallback.
    (!tool_name.is_empty()).then(|| format!("Approval requested: {tool_name}"))
}

fn patch_files(input: &serde_json::Value) -> Vec<String> {
    for key in ["file_path", "path"] {
        if let Some(p) = input.get(key).and_then(|x| x.as_str()) {
            return vec![p.to_string()];
        }
    }
    for key in ["files", "paths"] {
        if let Some(arr) = input.get(key).and_then(|x| x.as_array()) {
            return arr
                .iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect();
        }
    }
    Vec::new()
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn normalize(name: &str) -> String {
    name.chars()
        .filter(|c| *c != '_' && *c != '-')
        .collect::<String>()
        .to_ascii_lowercase()
}

#[derive(Debug, Deserialize)]
struct NotifyPayload {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "turn-id", default)]
    turn_id: Option<String>,
    #[serde(rename = "thread-id", default)]
    thread_id: Option<String>,
    #[serde(rename = "last-assistant-message", default)]
    last_assistant_message: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
}

/// Notify tap: codex invokes the program with one JSON argument. The external
/// `notify` program only ever receives `agent-turn-complete`; approvals arrive
/// via the `permission_request` hook, not here.
pub fn map_notify(json: &str) -> Result<Option<Event>, serde_json::Error> {
    let payload: NotifyPayload = serde_json::from_str(json)?;

    let (state, body) = match normalize(&payload.kind).as_str() {
        "agentturncomplete" => (
            AgentState::Ready,
            payload.last_assistant_message.as_deref().and_then(hygiene::body),
        ),
        // A turn that ended on an error → Blocked (Codex's own on_error state).
        "turnfailed" | "error" | "turnaborted" | "turnerror" | "agentturnerror" => (
            AgentState::Failed,
            payload
                .message
                .as_deref()
                .or(payload.last_assistant_message.as_deref())
                .and_then(hygiene::body),
        ),
        _ => return Ok(None),
    };

    let Some(session) = payload
        .thread_id
        .clone()
        .or_else(|| payload.turn_id.clone())
        .or_else(|| std::env::var("CODEX_THREAD_ID").ok())
    else {
        return Ok(None);
    };

    Ok(Some(Event {
        v: pet_proto::PROTOCOL_VERSION,
        source: Source::Codex,
        session,
        state,
        body,
        ts: None,
        via: None,
        meta: Meta {
            cwd: payload.cwd,
            ..Default::default()
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_names_match_across_casings() {
        for name in ["SessionStart", "session_start"] {
            let json = format!(r#"{{"hook_event_name":"{name}","session_id":"t1"}}"#);
            let ev = map_hook(&json, None).unwrap().unwrap();
            assert_eq!(ev.state, AgentState::Running);
            assert_eq!(ev.source, Source::Codex);
        }
        assert!(map_hook(r#"{"hook_event_name":"PreToolUse","session_id":"t"}"#, None)
            .unwrap()
            .is_none());
    }

    #[test]
    fn stop_hook_body_from_last_assistant_message() {
        let json = "{\"hook_event_name\":\"stop\",\"session_id\":\"t1\",\"last_assistant_message\":\"## Fixed\\nthe **race**.\"}";
        let ev = map_hook(json, None).unwrap().unwrap();
        assert_eq!(ev.state, AgentState::Ready);
        assert_eq!(ev.body.as_deref(), Some("Fixed the race."));
    }

    #[test]
    fn permission_request_templates() {
        let ex = |tool: &str, input: &str| {
            let json = format!(
                r#"{{"hook_event_name":"permission_request","session_id":"t","tool_name":"{tool}","tool_input":{input}}}"#
            );
            let ev = map_hook(&json, None).unwrap().unwrap();
            assert_eq!(ev.state, AgentState::Waiting);
            ev.body
        };
        assert_eq!(
            ex("Bash", r#"{"command":"cargo test --workspace"}"#).as_deref(),
            Some("Approval requested: cargo test --workspace")
        );
        assert_eq!(
            ex("Edit", r#"{"file_path":"/home/x/src/main.rs"}"#).as_deref(),
            Some("wants to edit main.rs")
        );
        assert_eq!(
            ex("apply_patch", r#"{"files":["a.rs","b.rs","c.rs"]}"#).as_deref(),
            Some("wants to edit 3 files")
        );
        assert_eq!(
            ex("mcp_call", r#"{"server":"github"}"#).as_deref(),
            Some("Approval requested by github")
        );
        assert_eq!(
            ex("AskUserQuestion", r#"{"questions":[{"header":"Deploy target"}]}"#).as_deref(),
            Some("Deploy target")
        );
        assert_eq!(
            ex("AskUserQuestion", r#"{"questions":[{"header":"a"},{"header":"b"}]}"#).as_deref(),
            Some("2 questions requested")
        );
    }

    #[test]
    fn permission_request_unknown_tool_and_malformed_input_never_panic() {
        assert_eq!(
            map_hook(
                r#"{"hook_event_name":"permission_request","session_id":"t","tool_name":"Mystery","tool_input":{}}"#,
                None
            )
            .unwrap()
            .unwrap()
            .body
            .as_deref(),
            Some("Approval requested: Mystery")
        );
        // Bash approval with no command → falls through to the tool-name fallback.
        assert_eq!(
            map_hook(
                r#"{"hook_event_name":"permission_request","session_id":"t","tool_name":"Bash","tool_input":{}}"#,
                None
            )
            .unwrap()
            .unwrap()
            .body
            .as_deref(),
            Some("Approval requested: Bash")
        );
    }

    #[test]
    fn approval_command_is_grapheme_capped() {
        let long = "echo ".to_string() + &"x".repeat(80);
        let json = format!(
            r#"{{"hook_event_name":"permission_request","session_id":"t","tool_name":"Bash","tool_input":{{"command":"{long}"}}}}"#
        );
        let body = map_hook(&json, None).unwrap().unwrap().body.unwrap();
        // "Approval requested: " + 30-grapheme command.
        assert!(body.starts_with("Approval requested: "));
        use unicode_segmentation::UnicodeSegmentation;
        let cmd = body.strip_prefix("Approval requested: ").unwrap();
        assert!(cmd.graphemes(true).count() <= 30);
    }

    #[test]
    fn notify_maps_only_turn_complete() {
        let done = r#"{"type":"agent-turn-complete","turn-id":"t9","last-assistant-message":"All tests pass."}"#;
        let ev = map_notify(done).unwrap().unwrap();
        assert_eq!(ev.state, AgentState::Ready);
        assert_eq!(ev.session, "t9");
        assert_eq!(ev.body.as_deref(), Some("All tests pass."));

        // Approvals do NOT arrive via notify anymore — ignored here.
        assert!(map_notify(r#"{"type":"exec-approval-requested","turn-id":"t9"}"#)
            .unwrap()
            .is_none());
        assert!(map_notify(r#"{"type":"something-else","turn-id":"t9"}"#)
            .unwrap()
            .is_none());
    }

    #[test]
    fn turn_error_maps_to_blocked_in_both_taps() {
        // Hook tap: an error/aborted turn hook → Blocked with its message.
        for name in ["error", "turn_failed", "turn-aborted", "TurnError"] {
            let json = format!(
                r#"{{"hook_event_name":"{name}","session_id":"t","message":"stream disconnected"}}"#
            );
            let ev = map_hook(&json, None).unwrap().unwrap();
            assert_eq!(ev.state, AgentState::Failed, "{name}");
            assert_eq!(ev.body.as_deref(), Some("stream disconnected"), "{name}");
        }

        // Notify tap: an error kind → Blocked, still resolving the session.
        let n = r#"{"type":"turn-failed","thread-id":"th1","message":"model overloaded"}"#;
        let ev = map_notify(n).unwrap().unwrap();
        assert_eq!(ev.state, AgentState::Failed);
        assert_eq!(ev.session, "th1");
        assert_eq!(ev.body.as_deref(), Some("model overloaded"));
    }

    #[test]
    fn notify_prefers_thread_id_over_turn_id() {
        let json = r#"{"type":"agent-turn-complete","turn-id":"turn-1","thread-id":"thread-1"}"#;
        assert_eq!(map_notify(json).unwrap().unwrap().session, "thread-1");
    }
}
