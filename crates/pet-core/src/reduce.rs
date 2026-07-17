//! Pure derivation of display state from the model.

use pet_proto::{AgentState, SessionView, Snapshot};

use crate::model::Model;

/// How long a non-blocking `ready` result actively drives the mascot/bubble
/// before it calms (staying in the tray, still unread). Blocking states
/// (needs-input / blocked) are NOT time-limited — they nag until acted on.
pub const READY_PRESENT_MS: i64 = 4_000;

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

/// A `ready` session only drives the mascot within its presentation window:
/// after that it has "been said" and calms, without being marked seen.
fn presented_out(view: &SessionView, now_ms: i64) -> bool {
    view.state == AgentState::Ready && now_ms - view.since >= READY_PRESENT_MS
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

    // A session drives the mascot only if it's not focused (you're looking at
    // it) and not a ready result past its presentation window (already said).
    let top = sessions
        .iter()
        .filter(|s| !s.focused && !presented_out(s, now_ms))
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

/// Earliest instant the reduced snapshot could change on its own: a session
/// expiry, or the end of a `ready` session's presentation window (when the
/// mascot calms). The shell arms its timer to this so the mascot calms on
/// time, not only on the next event.
pub fn next_deadline(model: &Model) -> Option<i64> {
    let expiries = model.sessions.values().map(|s| s.deadline);
    let present_ends = model
        .sessions
        .values()
        .filter(|s| s.phase == AgentState::Ready)
        .map(|s| s.since + READY_PRESENT_MS);
    expiries.chain(present_ends).min()
}
