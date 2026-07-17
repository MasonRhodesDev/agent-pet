use pet_proto::AgentState;
use serde::{Deserialize, Serialize};

/// Per-state lifetimes, in milliseconds. Defaults follow the Codex pet
/// semantics: a stale "running" decays quickly, "needs input" nags for a
/// day, unseen "ready" persists a week.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ttls {
    pub running_ms: i64,
    pub waiting_ms: i64,
    pub ready_ms: i64,
    pub failed_ms: i64,
    /// How long an idle session lingers before it is garbage-collected.
    pub idle_gc_ms: i64,
    /// Grace window after the last direct event during which a wrapper may
    /// still override with a stronger state (waiting/failed).
    pub wrapper_grace_ms: i64,
}

impl Default for Ttls {
    fn default() -> Self {
        Self {
            running_ms: 3 * 60 * 1000,
            waiting_ms: 24 * 60 * 60 * 1000,
            ready_ms: 7 * 24 * 60 * 60 * 1000,
            failed_ms: 60 * 60 * 1000,
            idle_gc_ms: 10 * 60 * 1000,
            wrapper_grace_ms: 5 * 1000,
        }
    }
}

impl Ttls {
    pub fn for_state(&self, state: AgentState) -> i64 {
        match state {
            AgentState::Running => self.running_ms,
            AgentState::Waiting => self.waiting_ms,
            AgentState::Ready => self.ready_ms,
            AgentState::Failed => self.failed_ms,
            AgentState::Idle | AgentState::Gone => self.idle_gc_ms,
        }
    }
}
