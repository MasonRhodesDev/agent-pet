//! Table-driven coverage of the phase × input transition table plus expiry
//! with a fake clock.

use pet_core::{reduce, step, Effect, Input, Model, Ttls};
use pet_proto::{AgentState, Event, Meta, SessionKey, Source};

fn ev(state: AgentState) -> Event {
    Event {
        v: 1,
        source: Source::Claude,
        session: "s1".into(),
        state,
        body: None,
        ts: None,
        via: None,
        meta: Meta::default(),
    }
    .validate(0)
    .unwrap()
}

fn ev_at(state: AgentState, ts: i64) -> Event {
    let mut e = ev(state);
    e.ts = Some(ts);
    e
}

fn key() -> SessionKey {
    SessionKey::new(Source::Claude, "s1")
}

fn phase(model: &Model) -> Option<AgentState> {
    model.sessions.get(&key()).map(|s| s.phase)
}

use AgentState::*;

#[test]
fn transition_table() {
    // (start phase or None=new, incoming state, expected phase or None=removed)
    let cases: &[(Option<AgentState>, AgentState, Option<AgentState>)] = &[
        (None, Running, Some(Running)),
        (None, Waiting, Some(Waiting)),
        (None, Ready, Some(Ready)),
        (None, Failed, Some(Failed)),
        (None, Idle, Some(Idle)),
        (None, Gone, None),
        (Some(Running), Running, Some(Running)),
        (Some(Running), Waiting, Some(Waiting)),
        (Some(Running), Ready, Some(Ready)),
        (Some(Running), Failed, Some(Failed)),
        (Some(Running), Idle, Some(Idle)),
        (Some(Running), Gone, None),
        (Some(Waiting), Running, Some(Running)),
        (Some(Waiting), Ready, Some(Ready)),
        (Some(Waiting), Gone, None),
        (Some(Ready), Running, Some(Running)),
        (Some(Ready), Failed, Some(Failed)),
        (Some(Failed), Running, Some(Running)),
        (Some(Failed), Gone, None),
        (Some(Idle), Running, Some(Running)),
        (Some(Idle), Waiting, Some(Waiting)),
        (Some(Idle), Gone, None),
    ];

    for (start, incoming, expected) in cases {
        let mut model = Model::default();
        if let Some(start) = start {
            step(&mut model, Input::Event(ev(*start)), 0);
            assert_eq!(phase(&model), Some(*start), "setup failed for {start:?}");
        }
        step(&mut model, Input::Event(ev(*incoming)), 10);
        assert_eq!(
            phase(&model),
            *expected,
            "from {start:?} on {incoming:?} expected {expected:?}"
        );
    }
}

#[test]
fn every_state_entry_arms_its_ttl_deadline() {
    let ttls = Ttls::default();
    let cases = [
        (Running, ttls.running_ms),
        (Waiting, ttls.waiting_ms),
        (Ready, ttls.ready_ms),
        (Failed, ttls.failed_ms),
        (Idle, ttls.idle_gc_ms),
    ];
    for (state, ttl) in cases {
        let mut model = Model::default();
        let effects = step(&mut model, Input::Event(ev_at(state, 1_000)), 1_000);
        assert_eq!(model.sessions[&key()].deadline, 1_000 + ttl, "{state:?}");
        assert!(
            effects.contains(&Effect::ScheduleTick(1_000 + ttl)),
            "{state:?} should schedule its expiry tick, got {effects:?}"
        );
    }
}

#[test]
fn tick_decays_expired_states_to_idle_then_gcs() {
    let ttls = Ttls::default();
    let mut model = Model::default();
    step(&mut model, Input::Event(ev_at(Running, 0)), 0);

    // Before the deadline: nothing happens.
    assert!(step(&mut model, Input::Tick, ttls.running_ms - 1).is_empty());
    assert_eq!(phase(&model), Some(Running));

    // At the deadline: stale running decays to idle.
    let effects = step(&mut model, Input::Tick, ttls.running_ms);
    assert_eq!(phase(&model), Some(Idle));
    assert!(effects.contains(&Effect::PublishSnapshot));

    // Idle GC removes the session entirely.
    step(&mut model, Input::Tick, ttls.running_ms + ttls.idle_gc_ms);
    assert_eq!(phase(&model), None);
}

#[test]
fn ready_and_failed_enter_unseen_and_seen_marks_them() {
    for state in [Ready, Failed] {
        let mut model = Model::default();
        step(&mut model, Input::Event(ev(state)), 0);
        assert!(!model.sessions[&key()].seen, "{state:?} should enter unseen");
        assert_eq!(reduce(&model, 0).unread, 1);

        let effects = step(&mut model, Input::Seen(key()), 1);
        assert!(model.sessions[&key()].seen);
        assert!(effects.contains(&Effect::PublishSnapshot));
        assert_eq!(reduce(&model, 1).unread, 0);

        // Seen is idempotent.
        assert!(step(&mut model, Input::Seen(key()), 2).is_empty());
    }
}

#[test]
fn seen_noops_on_waiting() {
    let mut model = Model::default();
    step(&mut model, Input::Event(ev(Waiting)), 0);
    assert!(step(&mut model, Input::Seen(key()), 1).is_empty());
    assert_eq!(reduce(&model, 1).top, Waiting);
}

#[test]
fn ready_heartbeat_with_same_body_keeps_seen() {
    let mut model = Model::default();
    let mut e = ev(Ready);
    e.body = Some("done: task A".into());
    step(&mut model, Input::Event(e.clone()), 0);
    step(&mut model, Input::Seen(key()), 1);

    // Same body re-emitted (poller heartbeat): stays seen.
    step(&mut model, Input::Event(e.clone()), 2);
    assert!(model.sessions[&key()].seen);

    // A new result resurrects the unread dot.
    e.body = Some("done: task B".into());
    step(&mut model, Input::Event(e), 3);
    assert!(!model.sessions[&key()].seen);
}

#[test]
fn reduce_priority_and_top() {
    let mut model = Model::default();
    let mk = |source: Source, session: &str, state: AgentState| {
        Event {
            v: 1,
            source,
            session: session.into(),
            state,
            body: None,
            ts: Some(0),
            via: None,
            meta: Meta::default(),
        }
    };
    step(&mut model, Input::Event(mk(Source::Claude, "a", Running)), 0);
    assert_eq!(reduce(&model, 0).top, Running);

    step(&mut model, Input::Event(mk(Source::Codex, "b", Ready)), 0);
    assert_eq!(reduce(&model, 0).top, Ready);

    step(&mut model, Input::Event(mk(Source::Gastown, "c", Failed)), 0);
    assert_eq!(reduce(&model, 0).top, Failed);

    step(&mut model, Input::Event(mk(Source::Claude, "d", Waiting)), 0);
    let snap = reduce(&model, 0);
    assert_eq!(snap.top, Waiting);
    assert_eq!(snap.sessions[0].state, Waiting, "tray sorted by priority");
    assert_eq!(snap.unread, 2); // ready + failed unseen

    // Seeing the failed and ready sessions demotes them below running.
    step(&mut model, Input::SeenAll, 1);
    let snap = reduce(&model, 1);
    assert_eq!(snap.unread, 0);
    assert_eq!(snap.top, Waiting, "waiting unaffected by seen");
}

#[test]
fn seen_ready_no_longer_drives_mascot() {
    let mut model = Model::default();
    step(&mut model, Input::Event(ev(Ready)), 0);
    assert_eq!(reduce(&model, 0).top, Ready);
    step(&mut model, Input::Seen(key()), 1);
    // Seen ready stays in the tray but the mascot returns to idle.
    let snap = reduce(&model, 1);
    assert_eq!(snap.top, Idle);
    assert_eq!(snap.sessions.len(), 1);
}

#[test]
fn idle_sessions_hidden_from_tray() {
    let mut model = Model::default();
    step(&mut model, Input::Event(ev(Idle)), 0);
    let snap = reduce(&model, 0);
    assert!(snap.sessions.is_empty());
    assert_eq!(snap.top, Idle);
}

#[test]
fn focus_requested_emits_focus_effect_and_marks_seen() {
    let mut model = Model::default();
    let mut e = ev(Ready);
    e.meta.cwd = Some("/home/mason/repos/x".into());
    step(&mut model, Input::Event(e), 0);

    let effects = step(&mut model, Input::FocusRequested(key()), 1);
    assert!(matches!(
        &effects[0],
        Effect::Focus { key: k, meta, .. } if *k == key() && meta.cwd.as_deref() == Some("/home/mason/repos/x")
    ));
    assert!(model.sessions[&key()].seen);

    // Unknown session: no effects.
    let none = step(
        &mut model,
        Input::FocusRequested(SessionKey::new(Source::Pi, "nope")),
        2,
    );
    assert!(none.is_empty());
}

#[test]
fn model_persists_and_expires_after_reload() {
    let ttls = Ttls::default();
    let mut model = Model::default();
    step(&mut model, Input::Event(ev_at(Running, 0)), 0);
    step(&mut model, Input::Event(ev_at(Waiting, 5)), 5);

    let json = serde_json::to_string(&model).unwrap();
    let mut reloaded: Model = serde_json::from_str(&json).unwrap();
    assert_eq!(reloaded.sessions.len(), 1);
    assert_eq!(phase(&reloaded), Some(Waiting));

    // Boot-time tick far in the future expires what lapsed while down.
    step(&mut reloaded, Input::Tick, 5 + ttls.waiting_ms + 1);
    assert_eq!(phase(&reloaded), Some(Idle));
}
