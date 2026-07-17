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

/// The transcript tail the emitter read for a Stop / idle-Notification event.
/// `is_error` marks an `isApiErrorMessage` entry — a turn that died on an API
/// error (overload, dropped stream, 5xx). That is Blocked, not a clean Ready.
#[derive(Debug, Default, Clone)]
pub struct Tail {
    pub text: Option<String>,
    pub is_error: bool,
}

impl Tail {
    /// A plain (non-error) tail carrying just assistant text.
    pub fn text(text: Option<String>) -> Self {
        Self { text, is_error: false }
    }
}

/// Does a Notification message name a specific pending action (a permission
/// prompt) rather than the generic idle "waiting for your input"?
pub fn is_permission_message(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("permission") || m.contains("approve") || m.contains("wants to")
}

/// Is this notification actually a multiple-choice question (AskUserQuestion)
/// dressed up as a permission prompt? Those need input, but "needs permission
/// to use AskUserQuestion" is misleading — we surface the real question.
pub fn is_question_prompt(message: &str) -> bool {
    message.to_ascii_lowercase().contains("askuserquestion")
}

/// Map a hook payload to an event. `agent_pid` is the harness process id (the
/// emitter's parent). `tail` is the last assistant message the emitter read
/// from the transcript (kept out of this pure fn). Returns `None` for hook
/// events the pet ignores.
pub fn map_hook(
    json: &str,
    agent_pid: Option<u32>,
    tail: Tail,
) -> Result<Option<Event>, serde_json::Error> {
    let payload: ClaudeHookPayload = serde_json::from_str(json)?;
    let tail_body = tail.text.as_deref().and_then(hygiene::body);
    // A turn that ended on an API error leaves an isApiErrorMessage entry as
    // the transcript tail. A turn *ending* (Stop, or the idle Notification
    // that trails it) is Ready normally, but Blocked when that tail is an
    // error — the only path to `failed` for a first-class harness.
    let ended = |ok| if tail.is_error { AgentState::Failed } else { ok };

    let (state, body) = match payload.hook_event_name.as_str() {
        "SessionStart" => (AgentState::Running, None),
        "UserPromptSubmit" => (
            AgentState::Running,
            payload.prompt.as_deref().and_then(hygiene::body),
        ),
        "PostToolUse" => (AgentState::Running, payload.tool_name.clone()),
        "Notification" => match payload.message.as_deref() {
            // A multiple-choice question needs input, but its permission
            // wording is useless — show the actual question (from the tail).
            Some(m) if is_question_prompt(m) => (
                AgentState::Waiting,
                tail_body.clone().or_else(|| hygiene::body(m)),
            ),
            // A real permission/approval prompt genuinely blocks progress →
            // needs-input (the persistent nag is warranted). Keep its text.
            Some(m) if is_permission_message(m) => (AgentState::Waiting, hygiene::body(m)),
            // The generic idle "Claude is waiting for your input" fires ~60s
            // after any turn ends; nothing is actually blocked, so it is NOT
            // needs-input — it means "done, look whenever" → ready, showing
            // what Claude last said. Unless that turn died on an API error,
            // which is Blocked.
            other => (
                ended(AgentState::Ready),
                tail_body.clone().or_else(|| other.and_then(hygiene::body)),
            ),
        },
        "Stop" => (ended(AgentState::Ready), tail_body.clone()),
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
            let ev = map_hook(json, Some(99), Tail::default()).unwrap().expect(json);
            assert_eq!(ev.state, want, "{json}");
            assert_eq!(ev.session, "s1");
            assert_eq!(ev.meta.agent_pid, Some(99));
        }
    }

    #[test]
    fn stop_body_comes_from_transcript_tail() {
        let json = r#"{"hook_event_name":"Stop","session_id":"s"}"#;
        let ev = map_hook(json, None, Tail::text(Some("## Done\n\nAll **green**.".into())))
            .unwrap()
            .unwrap();
        assert_eq!(ev.state, AgentState::Ready);
        assert_eq!(ev.body.as_deref(), Some("Done All green."));
        // No tail → no invented body.
        let ev = map_hook(json, None, Tail::default()).unwrap().unwrap();
        assert_eq!(ev.body, None);
    }

    #[test]
    fn generic_idle_notification_is_ready_not_needs_input() {
        // The idle "waiting for your input" fires after every turn ends;
        // nothing is blocked, so it must NOT nag as needs-input.
        let generic = r#"{"hook_event_name":"Notification","session_id":"s","message":"Claude is waiting for your input"}"#;
        let ev = map_hook(generic, None, Tail::text(Some("Should I deploy to staging?".into())))
            .unwrap()
            .unwrap();
        assert_eq!(ev.state, AgentState::Ready);
        assert_eq!(ev.body.as_deref(), Some("Should I deploy to staging?"));

        // Without a tail, the generic message is the fallback caption.
        let ev = map_hook(generic, None, Tail::default()).unwrap().unwrap();
        assert_eq!(ev.state, AgentState::Ready);
        assert_eq!(ev.body.as_deref(), Some("Claude is waiting for your input"));
    }

    #[test]
    fn real_permission_prompt_is_needs_input() {
        // A genuine approval blocks progress → waiting, keeping its text over
        // any transcript tail.
        let perm = r#"{"hook_event_name":"Notification","session_id":"s","message":"Claude needs your permission to use Bash"}"#;
        let ev = map_hook(perm, None, Tail::text(Some("some unrelated tail".into())))
            .unwrap()
            .unwrap();
        assert_eq!(ev.state, AgentState::Waiting);
        assert_eq!(ev.body.as_deref(), Some("Claude needs your permission to use Bash"));
    }

    #[test]
    fn api_error_tail_makes_a_turn_end_blocked() {
        // A Stop (or the idle Notification trailing it) whose transcript tail
        // is an isApiErrorMessage entry is Blocked, not Ready — carrying the
        // error text so the bubble says what broke.
        let err = Tail {
            text: Some("API Error: Connection closed mid-response.".into()),
            is_error: true,
        };
        for json in [
            r#"{"hook_event_name":"Stop","session_id":"s"}"#,
            r#"{"hook_event_name":"Notification","session_id":"s","message":"Claude is waiting for your input"}"#,
        ] {
            let ev = map_hook(json, None, err.clone()).unwrap().unwrap();
            assert_eq!(ev.state, AgentState::Failed, "{json}");
            assert_eq!(ev.body.as_deref(), Some("API Error: Connection closed mid-response."));
        }

        // A permission prompt is still needs-input even if an earlier turn
        // errored — the pending approval is what matters now.
        let perm = r#"{"hook_event_name":"Notification","session_id":"s","message":"Claude needs your permission to use Bash"}"#;
        assert_eq!(map_hook(perm, None, err).unwrap().unwrap().state, AgentState::Waiting);
    }

    #[test]
    fn carries_meta() {
        let ev = map_hook(
            r#"{"hook_event_name":"Notification","session_id":"s","message":"Permission to edit","cwd":"/repo","transcript_path":"/t.jsonl"}"#,
            None,
            Tail::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(ev.meta.cwd.as_deref(), Some("/repo"));
        assert_eq!(ev.meta.transcript_path.as_deref(), Some("/t.jsonl"));
    }

    #[test]
    fn ignores_unknown_hooks_and_caps_long_prompts() {
        assert!(map_hook(r#"{"hook_event_name":"PreCompact","session_id":"s"}"#, None, Tail::default())
            .unwrap()
            .is_none());
        let long = "x".repeat(500);
        let json = format!(
            r#"{{"hook_event_name":"UserPromptSubmit","session_id":"s","prompt":"{long}"}}"#
        );
        let ev = map_hook(&json, None, Tail::default()).unwrap().unwrap();
        use unicode_segmentation::UnicodeSegmentation;
        assert!(ev.body.unwrap().graphemes(true).count() <= hygiene::BODY_MAX);
    }

    #[test]
    fn askuserquestion_shows_the_question_not_permission() {
        // The notification's "permission to use AskUserQuestion" wording is
        // replaced by the actual question from the transcript tail, but it
        // still counts as needs-input.
        let n = r#"{"hook_event_name":"Notification","session_id":"s","message":"Claude needs your permission to use AskUserQuestion"}"#;
        let ev = map_hook(n, None, Tail::text(Some("Which database should we use?".into())))
            .unwrap()
            .unwrap();
        assert_eq!(ev.state, AgentState::Waiting);
        assert_eq!(ev.body.as_deref(), Some("Which database should we use?"));
    }
}
