//! Session identity: alias resolution and wrapper precedence.
//!
//! Wrappers (Happy, Gas Town) observe work that a harness may also report
//! directly. The rules, applied before any transition:
//!
//! 1. Direct events win: while the direct channel is fresh, wrapper events
//!    only enrich metadata — except strictly stronger states (waiting or
//!    failed) newer than the last direct event plus a grace window.
//! 2. Join when possible: `via.links` (or a matching `agent_pid`) aliases
//!    the wrapper's key to the canonical direct key so both feeds drive one
//!    session.
//! 3. Unjoinable wrapper sessions stand alone, tagged by their wrapper.
//! 4. A wrapper's `gone` is always honored — wrappers reliably know when
//!    their child died.

use pet_proto::{AgentState, Event, SessionKey, Source};

use crate::model::{Model, Origin};

/// Follow the alias table (single hop — values are canonical by
/// construction).
pub fn resolve(model: &Model, key: &SessionKey) -> SessionKey {
    model.aliases.get(key).cloned().unwrap_or_else(|| key.clone())
}

/// Decide which session an event addresses, recording any alias it proves.
///
/// Returns the canonical key the event should be applied to.
pub fn canonicalize(model: &mut Model, event: &Event) -> SessionKey {
    let event_key = event.key();

    let Some(via) = &event.via else {
        // A direct harness session that self-identifies as Gas Town work
        // (the daemon's intake tags town cwds with `gastown_ref`, e.g.
        // "crew/<rig>/<name>" or "mayor") collapses with the poller's row
        // for the same work: the gastown key aliases to the direct key.
        if event.source != Source::Gastown {
            if let Some(gref) = event.meta.extra.get("gastown_ref").and_then(|v| v.as_str()) {
                let wrapper_key = SessionKey::new(Source::Gastown, gref);
                record_alias(model, wrapper_key, event_key.clone());
            }
        }
        return resolve(model, &event_key);
    };

    // The wrapper told us exactly which direct session this is.
    if let Some(links) = &via.links {
        record_alias(model, event_key.clone(), links.clone());
        return resolve(model, links);
    }

    // Join hint: a direct session from the same source with the same
    // harness pid is the same work (Happy hostPid == emitter getppid).
    if let Some(pid) = event.meta.agent_pid {
        let found = model.sessions.iter().find(|(k, s)| {
            **k != event_key
                && k.source == event.source
                && s.origin == Origin::Direct
                && s.meta.agent_pid == Some(pid)
        });
        if let Some((canonical, _)) = found {
            let canonical = canonical.clone();
            record_alias(model, event_key, canonical.clone());
            return canonical;
        }
    }

    resolve(model, &event_key)
}

/// Point `from` at `to`, migrating any session accumulated under `from`.
pub fn record_alias(model: &mut Model, from: SessionKey, to: SessionKey) {
    if from == to {
        return;
    }
    if let Some(orphan) = model.sessions.remove(&from) {
        // The canonical session keeps its own state; the orphan only
        // contributes metadata it learned first.
        match model.sessions.get_mut(&to) {
            Some(canonical) => {
                canonical.meta.fill_from(&orphan.meta);
                if canonical.body.is_none() {
                    canonical.body = orphan.body;
                }
            }
            None => {
                model.sessions.insert(to.clone(), orphan);
            }
        }
    }
    // Re-point any aliases that resolved to `from`.
    for target in model.aliases.values_mut() {
        if *target == from {
            *target = to.clone();
        }
    }
    model.aliases.insert(from, to);
}

/// What a wrapper event is allowed to do to a session, per the precedence
/// rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrapperVerdict {
    /// Apply fully (state + metadata).
    Apply,
    /// Only fill in missing metadata/body.
    EnrichOnly,
}

/// Applies to wrapper events (`via` set) AND to via-less sibling feeds that
/// reach a session through an alias (e.g. Gas Town poller rows aliased to a
/// direct harness session via `gastown_ref`).
pub fn wrapper_verdict(
    model: &Model,
    key: &SessionKey,
    event: &Event,
    now_ms: i64,
) -> WrapperVerdict {
    // Rule 4: gone is always authoritative.
    if event.state == AgentState::Gone {
        return WrapperVerdict::Apply;
    }

    let Some(session) = model.sessions.get(key) else {
        return WrapperVerdict::Apply; // brand-new, wrapper-only session
    };
    let Some(last_direct) = session.last_direct else {
        return WrapperVerdict::Apply; // never had a direct feed
    };

    let direct_fresh = now_ms - last_direct < model.ttls.for_state(session.phase);
    if !direct_fresh {
        return WrapperVerdict::Apply;
    }

    // Rule 1 exception: strictly stronger states newer than the direct
    // channel (plus grace) still get through.
    let stronger = matches!(event.state, AgentState::Waiting | AgentState::Failed);
    let ts = event.ts.unwrap_or(now_ms);
    if stronger && ts > last_direct + model.ttls.wrapper_grace_ms {
        return WrapperVerdict::Apply;
    }

    WrapperVerdict::EnrichOnly
}

/// Remove a session and every alias pointing at it.
pub fn remove_session(model: &mut Model, key: &SessionKey) {
    model.sessions.remove(key);
    model.aliases.retain(|_, target| target != key);
    if model.focused.as_ref() == Some(key) {
        model.focused = None;
    }
}
