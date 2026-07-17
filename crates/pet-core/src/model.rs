use std::collections::BTreeMap;

use pet_proto::{AgentState, Event, Meta, SessionKey};
use serde::{Deserialize, Serialize};

use crate::expiry::Ttls;

/// The daemon's entire aggregation state. Serde so it can be persisted and
/// reloaded across restarts (multi-day TTLs must survive reboots).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Model {
    pub sessions: BTreeMap<SessionKey, SessionFsm>,
    /// Wrapper-key -> canonical-key. Values are always canonical (never
    /// chained); lookups follow at most one hop.
    pub aliases: BTreeMap<SessionKey, SessionKey>,
    #[serde(default)]
    pub ttls: Ttls,
    /// The session whose window the user is currently looking at. Transient
    /// UI state — never persisted (a stale focus could wrongly suppress on
    /// boot). Demoted from the mascot/bubble by `reduce`, kept in the tray.
    #[serde(skip)]
    pub focused: Option<SessionKey>,
}

/// One tracked session's state machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionFsm {
    /// Current phase. Never `Gone` (gone removes the session).
    pub phase: AgentState,
    /// Epoch ms of the last phase entry.
    pub since: i64,
    /// Epoch ms at which the current phase expires (decays to Idle, or GC
    /// for Idle itself).
    pub deadline: i64,
    /// Unread flag for Ready/Failed; meaningless in other phases.
    pub seen: bool,
    pub origin: Origin,
    /// Wrapper currently observing this session, shown as a tray badge when
    /// the feed is not direct.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<pet_proto::Source>,
    /// Epoch ms of the last direct (non-wrapper) event, if any.
    pub last_direct: Option<i64>,
    pub body: Option<String>,
    pub meta: Meta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// The harness itself has reported at least once.
    Direct,
    /// Only ever observed through a wrapper (e.g. Gas Town poller).
    WrapperOnly,
}

/// Everything that can drive the state machine.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// A validated harness event.
    Event(Event),
    /// Clock advance; expires deadlines. `step()`'s `now_ms` is the clock.
    Tick,
    Seen(SessionKey),
    SeenAll,
    /// Marks seen and asks the shell to focus the session's window.
    FocusRequested(SessionKey),
    /// The active window now maps to this session (or `None` when focus left
    /// every tracked session). Suppresses that session's mascot/bubble nag.
    FocusChanged(Option<SessionKey>),
}

/// Effects are data; the daemon shell executes them. Nothing in pet-core
/// touches the outside world.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Display state changed: re-derive `reduce()` and push to consumers.
    PublishSnapshot,
    /// Arm (or re-arm) the expiry timer for this epoch-ms instant.
    ScheduleTick(i64),
    /// Focus the window/pane behind this session. `meta` carries the
    /// terminal correlation to act on (for Gas Town escalations it is the
    /// MAYOR's, not the escalation's); `body` is the session caption, used
    /// to draft escalation context into the mayor's pane.
    Focus {
        key: SessionKey,
        meta: Meta,
        body: Option<String>,
    },
    /// Model changed in a way worth writing to disk (debounced by shell).
    Persist,
}
