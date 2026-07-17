use serde::{Deserialize, Serialize};

use crate::event::{AgentState, Meta, Source};
use crate::key::SessionKey;

/// Aggregated display state, derived from the daemon's model by a pure
/// reduce. This is the only thing renderers and external consumers see.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The mascot's state: highest-priority state across all sessions.
    pub top: AgentState,
    /// Sessions worth showing, already sorted (priority, then newest first).
    pub sessions: Vec<SessionView>,
    /// Number of sessions with unseen Ready/Failed transitions.
    pub unread: u32,
    /// Epoch ms the snapshot was derived at.
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionView {
    pub key: SessionKey,
    pub state: AgentState,
    /// Epoch ms of the last state entry.
    pub since: i64,
    pub seen: bool,
    /// Wrapper that observes this session, when it is not a direct feed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<Source>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default)]
    pub meta: Meta,
}

/// Actions the renderer (or CLI) can request from the daemon. The renderer
/// never executes effects itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum UiAction {
    FocusSession { key: SessionKey },
    MarkSeen { keys: Vec<SessionKey> },
    MarkAllSeen,
    /// The user changed the mascot's visibility from the UI itself
    /// (right-click hide today; tray close later). Informational — the
    /// renderer owns and persists visibility.
    SetVisible { visible: bool },
    OpenSettings,
    Quit,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_round_trips() {
        let snap = Snapshot {
            top: AgentState::Waiting,
            sessions: vec![SessionView {
                key: SessionKey::new(Source::Claude, "s1"),
                state: AgentState::Waiting,
                since: 123,
                seen: false,
                via: None,
                body: Some("Permission".into()),
                meta: Meta::default(),
            }],
            unread: 1,
            at: 456,
        };
        let json = serde_json::to_string(&snap).unwrap();
        let back: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, snap);
    }

    #[test]
    fn default_snapshot_is_idle() {
        assert_eq!(Snapshot::default().top, AgentState::Idle);
    }
}
