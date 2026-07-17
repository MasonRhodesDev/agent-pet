//! Pure derivation of display state from the model.

use pet_proto::{AgentState, SessionView, Snapshot};

use crate::model::Model;

/// Mascot/tray priority. Higher = more attention-worthy. Seen Ready/Failed
/// no longer drive the mascot (the user has acknowledged them).
fn priority(state: AgentState, seen: bool) -> u8 {
    match (state, seen) {
        (AgentState::Waiting, _) => 5,
        (AgentState::Failed, false) => 4,
        (AgentState::Ready, false) => 3,
        (AgentState::Running, _) => 2,
        (AgentState::Failed, true) | (AgentState::Ready, true) => 1,
        (AgentState::Idle | AgentState::Gone, _) => 0,
    }
}

pub fn reduce(model: &Model, now_ms: i64) -> Snapshot {
    let mut sessions: Vec<SessionView> = model
        .sessions
        .iter()
        .filter(|(_, s)| s.phase != AgentState::Idle)
        .map(|(key, s)| SessionView {
            key: key.clone(),
            state: s.phase,
            since: s.since,
            seen: s.seen,
            via: s.via,
            focused: model.focused.as_ref() == Some(key),
            body: s.body.clone(),
            subtitle: None, // decorated by the daemon (needs file I/O)
            meta: s.meta.clone(),
        })
        .collect();

    sessions.sort_by(|a, b| {
        priority(b.state, b.seen)
            .cmp(&priority(a.state, a.seen))
            .then(b.since.cmp(&a.since))
    });

    // The focused session stays listed in the tray but does not drive the
    // mascot: you are already looking at it.
    let top = sessions
        .iter()
        .filter(|s| !s.focused)
        .map(|s| (priority(s.state, s.seen), s.state))
        .max_by_key(|(p, _)| *p)
        .filter(|(p, _)| *p >= 2)
        .map(|(_, state)| state)
        .unwrap_or(AgentState::Idle);

    // Unread still counts the focused session — it is genuinely unacknowledged
    // until seen; only the mascot/bubble nag is suppressed.
    let unread = sessions
        .iter()
        .filter(|s| !s.seen && matches!(s.state, AgentState::Ready | AgentState::Failed))
        .count() as u32;

    Snapshot {
        top,
        sessions,
        unread,
        at: now_ms,
    }
}

/// Earliest pending expiry across all sessions; the shell arms its timer to
/// this instant.
pub fn next_deadline(model: &Model) -> Option<i64> {
    model.sessions.values().map(|s| s.deadline).min()
}
