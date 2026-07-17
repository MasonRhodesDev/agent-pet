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
    /// The user is currently looking at this session's window: it stays in
    /// the tray but no longer drives the mascot or the bubble.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub focused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// A coarse "what is this session about" tagline (from the local
    /// pane-summarizer). Secondary to `body`; shown as a tray subtitle and
    /// used as a last-resort bubble caption when `body` is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(default)]
    pub meta: Meta,
}

/// A compositor's currently-active toplevel window, reported by the renderer
/// for focus-aware suppression. `pid` is present on Hyprland (socket1
/// activewindow) and absent on the generic wlr foreign-toplevel path, which
/// only exposes app_id/title.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ActiveWindow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
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
    /// The active toplevel changed (already debounced by the renderer).
    /// `None` = focus left every tracked session's window.
    ActiveWindowChanged { window: Option<ActiveWindow> },
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
                focused: true,
                body: Some("Permission".into()),
                subtitle: None,
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
    fn active_window_round_trips() {
        let w = ActiveWindow {
            pid: Some(4242),
            address: Some("0x5f".into()),
            app_id: Some("kitty".into()),
            title: Some("claude".into()),
        };
        let back: ActiveWindow = serde_json::from_str(&serde_json::to_string(&w).unwrap()).unwrap();
        assert_eq!(back, w);
        // Foreign-toplevel: no pid.
        let w = ActiveWindow { app_id: Some("firefox".into()), ..Default::default() };
        let back: ActiveWindow = serde_json::from_str(&serde_json::to_string(&w).unwrap()).unwrap();
        assert_eq!(back, w);
    }

    #[test]
    fn default_snapshot_is_idle() {
        assert_eq!(Snapshot::default().top, AgentState::Idle);
    }
}
