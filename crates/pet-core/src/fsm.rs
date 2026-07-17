//! The transition function. Pure: `now_ms` is injected, effects are data.

use pet_proto::{AgentState, Event, Meta, SessionKey, Source};

use crate::identity::{self, WrapperVerdict};
use crate::model::{Effect, Input, Model, Origin, SessionFsm};
use crate::reduce::next_deadline;

/// The Gas Town poller's session id for the mayor (gastown adapter
/// contract). Escalation focus routes at this session's terminal.
const MAYOR_SESSION: &str = "mayor";

pub fn step(model: &mut Model, input: Input, now_ms: i64) -> Vec<Effect> {
    let changed = match input {
        Input::Event(event) => apply_event(model, event, now_ms),
        Input::Tick => apply_tick(model, now_ms),
        Input::Seen(key) => mark_seen(model, &key),
        Input::SeenAll => {
            let mut any = false;
            let keys: Vec<SessionKey> = model.sessions.keys().cloned().collect();
            for key in keys {
                any |= mark_seen(model, &key);
            }
            any
        }
        Input::FocusRequested(key) => {
            let canonical = identity::resolve(model, &key);
            let Some(session) = model.sessions.get(&canonical) else {
                return Vec::new();
            };
            // Escalations are decisions the human answers at the mayor's
            // console: carry the MAYOR's terminal correlation, keeping the
            // escalation's caption for the draft.
            let meta = if canonical.source == Source::Gastown
                && canonical.session.starts_with("esc/")
            {
                let mayor =
                    identity::resolve(model, &SessionKey::new(Source::Gastown, MAYOR_SESSION));
                model
                    .sessions
                    .get(&mayor)
                    .map(|m| m.meta.clone())
                    .unwrap_or_else(|| session.meta.clone())
            } else {
                session.meta.clone()
            };
            let mut effects = vec![Effect::Focus {
                key: canonical.clone(),
                meta,
                body: session.body.clone(),
            }];
            if mark_seen(model, &canonical) {
                effects.push(Effect::PublishSnapshot);
                effects.push(Effect::Persist);
            }
            return effects;
        }
    };

    if !changed {
        return Vec::new();
    }
    let mut effects = vec![Effect::PublishSnapshot, Effect::Persist];
    if let Some(deadline) = next_deadline(model) {
        effects.push(Effect::ScheduleTick(deadline));
    }
    effects
}

fn apply_event(model: &mut Model, event: Event, now_ms: i64) -> bool {
    let key = identity::canonicalize(model, &event);
    // A via-less event applied to a different canonical key is an aliased
    // sibling feed (e.g. the Gas Town poller's row for work a harness also
    // reports directly through `gastown_ref`): it follows the same
    // precedence rules as wrapper events — the true direct channel wins.
    let is_direct = event.via.is_none() && key == event.key();

    if !is_direct {
        match identity::wrapper_verdict(model, &key, &event, now_ms) {
            WrapperVerdict::Apply => {}
            WrapperVerdict::EnrichOnly => return enrich(model, &key, &event),
        }
    }

    if event.state == AgentState::Gone {
        let existed = model.sessions.contains_key(&key);
        identity::remove_session(model, &key);
        return existed;
    }

    let ts = event.ts.unwrap_or(now_ms);
    let deadline = ts + model.ttls.for_state(event.state);

    match model.sessions.get_mut(&key) {
        Some(session) => {
            let same_phase = session.phase == event.state;
            // Re-entering Ready with the same caption is a heartbeat, not a
            // new result: don't resurrect the unread dot.
            let keep_seen = same_phase
                && event.state == AgentState::Ready
                && session.body == event.body;

            if !same_phase {
                session.since = ts;
            }
            session.phase = event.state;
            session.deadline = deadline;
            if matches!(event.state, AgentState::Ready | AgentState::Failed) && !keep_seen {
                session.seen = false;
            }
            if event.body.is_some() {
                session.body = event.body;
            }
            session.meta.fill_from(&event.meta);
            if is_direct {
                session.last_direct = Some(ts);
                session.origin = Origin::Direct;
                session.via = None;
                // Direct events carry fresher metadata than accumulated
                // wrapper enrichment for the fields they actually set.
                overwrite_meta(&mut session.meta, &event.meta);
            } else if session.origin == Origin::WrapperOnly {
                session.via = event.via.as_ref().map(|v| v.wrapper);
            }
        }
        None => {
            model.sessions.insert(
                key,
                SessionFsm {
                    phase: event.state,
                    since: ts,
                    deadline,
                    seen: !matches!(event.state, AgentState::Ready | AgentState::Failed),
                    origin: if is_direct {
                        Origin::Direct
                    } else {
                        Origin::WrapperOnly
                    },
                    via: event.via.as_ref().map(|v| v.wrapper),
                    last_direct: is_direct.then_some(ts),
                    body: event.body,
                    meta: event.meta,
                },
            );
        }
    }
    true
}

fn overwrite_meta(dst: &mut Meta, src: &Meta) {
    let mut fresh = src.clone();
    fresh.fill_from(dst);
    *dst = fresh;
}

fn enrich(model: &mut Model, key: &SessionKey, event: &Event) -> bool {
    let Some(session) = model.sessions.get_mut(key) else {
        return false;
    };
    let before_meta = session.meta.clone();
    let before_body = session.body.clone();
    session.meta.fill_from(&event.meta);
    if session.body.is_none() {
        session.body = event.body.clone();
    }
    session.meta != before_meta || session.body != before_body
}

fn apply_tick(model: &mut Model, now_ms: i64) -> bool {
    let expired: Vec<SessionKey> = model
        .sessions
        .iter()
        .filter(|(_, s)| s.deadline <= now_ms)
        .map(|(k, _)| k.clone())
        .collect();

    let mut changed = false;
    for key in expired {
        let session = model.sessions.get_mut(&key).expect("collected above");
        if session.phase == AgentState::Idle {
            identity::remove_session(model, &key);
        } else {
            session.phase = AgentState::Idle;
            session.since = now_ms;
            session.deadline = now_ms + model.ttls.idle_gc_ms;
            session.seen = true;
        }
        changed = true;
    }
    changed
}

fn mark_seen(model: &mut Model, key: &SessionKey) -> bool {
    let canonical = identity::resolve(model, key);
    let Some(session) = model.sessions.get_mut(&canonical) else {
        return false;
    };
    // Waiting persists until the state itself changes; seen only applies to
    // terminal attention states.
    if matches!(session.phase, AgentState::Ready | AgentState::Failed) && !session.seen {
        session.seen = true;
        return true;
    }
    false
}
