//! Claude Code hook payload → Event.
//!
//! One `agent-pet emit claude` command serves every hook; the payload's
//! `hook_event_name` selects the mapping:
//! SessionStart→running, UserPromptSubmit→running(prompt), PostToolUse→
//! running heartbeat(tool), Notification→waiting, Stop→ready, SessionEnd→gone.
//!
//! Alert text comes from real agent content, not canned strings: on Stop and
//! on the generic idle Notification, the emitter passes in `tail` — the last
//! assistant message read from the transcript — which is usually the actual
//! answer or the literal question Claude just asked. Specific permission
//! prompts are already meaningful and kept verbatim.

use pet_proto::{AgentState, Event, Meta, Source};
use serde::Deserialize;

use crate::hygiene;

#[derive(Debug, Deserialize)]
pub struct ClaudeHookPayload {
    pub hook_event_name: String,
    pub session_id: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub transcript_path: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    /// Codex `stop` hooks carry this directly; Claude does not (its body is
    /// derived from `tail` instead). Shared struct, so it lives here.
    #[serde(default)]
    pub last_assistant_message: Option<String>,
    /// Codex `permission_request` carries the tool's arguments.
    #[serde(default)]
    pub tool_input: Option<serde_json::Value>,
}

/// Does a Notification message name a specific pending action (a permission
/// prompt) rather than the generic idle "waiting for your input"?
pub fn is_permission_message(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("permission") || m.contains("approve") || m.contains("wants to")
}

/// Map a hook payload to an event. `agent_pid` is the harness process id (the
/// emitter's parent). `tail` is the last assistant message the emitter read
/// from the transcript (kept out of this pure fn). Returns `None` for hook
/// events the pet ignores.
pub fn map_hook(
    json: &str,
    agent_pid: Option<u32>,
    tail: Option<String>,
) -> Result<Option<Event>, serde_json::Error> {
    let payload: ClaudeHookPayload = serde_json::from_str(json)?;
    let tail_body = tail.as_deref().and_then(hygiene::body);

    let (state, body) = match payload.hook_event_name.as_str() {
        "SessionStart" => (AgentState::Running, None),
        "UserPromptSubmit" => (
            AgentState::Running,
            payload.prompt.as_deref().and_then(hygiene::body),
        ),
        "PostToolUse" => (AgentState::Running, payload.tool_name.clone()),
        "Notification" => match payload.message.as_deref() {
            // A real permission/approval prompt genuinely blocks progress →
            // needs-input (the persistent nag is warranted). Keep its text.
            Some(m) if is_permission_message(m) => (AgentState::Waiting, hygiene::body(m)),
            // The generic idle "Claude is waiting for your input" fires ~60s
            // after any turn ends; nothing is actually blocked, so it is NOT
            // needs-input — it means "done, look whenever" → ready, showing
            // what Claude last said.
            other => (
                AgentState::Ready,
                tail_body.clone().or_else(|| other.and_then(hygiene::body)),
            ),
        },
        "Stop" => (AgentState::Ready, tail_body.clone()),
        "SessionEnd" => (AgentState::Gone, payload.reason.clone()),
        _ => return Ok(None),
    };

    Ok(Some(Event {
        v: pet_proto::PROTOCOL_VERSION,
        source: Source::Claude,
        session: payload.session_id,
        state,
        body,
        ts: None, // daemon stamps receive time
        via: None,
        meta: Meta {
            cwd: payload.cwd,
            transcript_path: payload.transcript_path,
            tool: payload.tool_name,
            agent_pid,
            ..Default::default()
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_the_full_lifecycle() {
        let cases = [
            (r#"{"hook_event_name":"SessionStart","session_id":"s1","cwd":"/w"}"#, AgentState::Running),
            (r#"{"hook_event_name":"UserPromptSubmit","session_id":"s1","prompt":"fix the bug"}"#, AgentState::Running),
            (r#"{"hook_event_name":"PostToolUse","session_id":"s1","tool_name":"Bash"}"#, AgentState::Running),
            (r#"{"hook_event_name":"Notification","session_id":"s1","message":"Claude needs your permission to use Bash"}"#, AgentState::Waiting),
            (r#"{"hook_event_name":"Stop","session_id":"s1"}"#, AgentState::Ready),
            (r#"{"hook_event_name":"SessionEnd","session_id":"s1","reason":"exit"}"#, AgentState::Gone),
        ];
        for (json, want) in cases {
            let ev = map_hook(json, Some(99), None).unwrap().expect(json);
            assert_eq!(ev.state, want, "{json}");
            assert_eq!(ev.session, "s1");
            assert_eq!(ev.meta.agent_pid, Some(99));
        }
    }

    #[test]
    fn stop_body_comes_from_transcript_tail() {
        let json = r#"{"hook_event_name":"Stop","session_id":"s"}"#;
        let ev = map_hook(json, None, Some("## Done\n\nAll **green**.".into()))
            .unwrap()
            .unwrap();
        assert_eq!(ev.state, AgentState::Ready);
        assert_eq!(ev.body.as_deref(), Some("Done All green."));
        // No tail → no invented body.
        let ev = map_hook(json, None, None).unwrap().unwrap();
        assert_eq!(ev.body, None);
    }

    #[test]
    fn generic_idle_notification_is_ready_not_needs_input() {
        // The idle "waiting for your input" fires after every turn ends;
        // nothing is blocked, so it must NOT nag as needs-input.
        let generic = r#"{"hook_event_name":"Notification","session_id":"s","message":"Claude is waiting for your input"}"#;
        let ev = map_hook(generic, None, Some("Should I deploy to staging?".into()))
            .unwrap()
            .unwrap();
        assert_eq!(ev.state, AgentState::Ready);
        assert_eq!(ev.body.as_deref(), Some("Should I deploy to staging?"));

        // Without a tail, the generic message is the fallback caption.
        let ev = map_hook(generic, None, None).unwrap().unwrap();
        assert_eq!(ev.state, AgentState::Ready);
        assert_eq!(ev.body.as_deref(), Some("Claude is waiting for your input"));
    }

    #[test]
    fn real_permission_prompt_is_needs_input() {
        // A genuine approval blocks progress → waiting, keeping its text over
        // any transcript tail.
        let perm = r#"{"hook_event_name":"Notification","session_id":"s","message":"Claude needs your permission to use Bash"}"#;
        let ev = map_hook(perm, None, Some("some unrelated tail".into()))
            .unwrap()
            .unwrap();
        assert_eq!(ev.state, AgentState::Waiting);
        assert_eq!(ev.body.as_deref(), Some("Claude needs your permission to use Bash"));
    }

    #[test]
    fn carries_meta() {
        let ev = map_hook(
            r#"{"hook_event_name":"Notification","session_id":"s","message":"Permission to edit","cwd":"/repo","transcript_path":"/t.jsonl"}"#,
            None,
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(ev.meta.cwd.as_deref(), Some("/repo"));
        assert_eq!(ev.meta.transcript_path.as_deref(), Some("/t.jsonl"));
    }

    #[test]
    fn ignores_unknown_hooks_and_caps_long_prompts() {
        assert!(map_hook(r#"{"hook_event_name":"PreCompact","session_id":"s"}"#, None, None)
            .unwrap()
            .is_none());
        let long = "x".repeat(500);
        let json = format!(
            r#"{{"hook_event_name":"UserPromptSubmit","session_id":"s","prompt":"{long}"}}"#
        );
        let ev = map_hook(&json, None, None).unwrap().unwrap();
        use unicode_segmentation::UnicodeSegmentation;
        assert!(ev.body.unwrap().graphemes(true).count() <= hygiene::BODY_MAX);
    }
}
