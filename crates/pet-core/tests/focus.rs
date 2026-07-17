//! Focus-aware suppression: the session whose window the user is looking at
//! is demoted from the mascot/bubble but stays in the tray and unread count.

use pet_core::{reduce, step, Effect, Input, Model};
use pet_proto::{AgentState, Event, Meta, SessionKey, Source};

use AgentState::*;

fn ev(source: Source, session: &str, state: AgentState) -> Event {
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
}

fn key(source: Source, session: &str) -> SessionKey {
    SessionKey::new(source, session)
}

#[test]
fn focus_changed_sets_dedupes_and_clears() {
    let mut model = Model::default();
    step(&mut model, Input::Event(ev(Source::Claude, "a", Waiting)), 0);
    let k = key(Source::Claude, "a");

    let effects = step(&mut model, Input::FocusChanged(Some(k.clone())), 1);
    assert_eq!(effects, vec![Effect::PublishSnapshot], "focus republishes only");
    assert_eq!(model.focused, Some(k.clone()));

    // Same focus again: no-op.
    assert!(step(&mut model, Input::FocusChanged(Some(k.clone())), 2).is_empty());

    // Clearing republishes.
    let effects = step(&mut model, Input::FocusChanged(None), 3);
    assert_eq!(effects, vec![Effect::PublishSnapshot]);
    assert_eq!(model.focused, None);
}

#[test]
fn focus_resolves_alias_to_canonical() {
    let mut model = Model::default();
    // Direct claude session self-identifying as gastown crew work: the
    // gastown key aliases to the claude key.
    let mut e = ev(Source::Claude, "c1", Running);
    e.meta.extra.insert("gastown_ref".into(), "crew/rig/dave".into());
    step(&mut model, Input::Event(e), 0);

    // FocusChanged addressed to the wrapper key resolves to the canonical.
    step(
        &mut model,
        Input::FocusChanged(Some(key(Source::Gastown, "crew/rig/dave"))),
        1,
    );
    assert_eq!(model.focused, Some(key(Source::Claude, "c1")));
}

#[test]
fn focused_session_demoted_from_mascot_but_kept_in_tray() {
    let mut model = Model::default();
    step(&mut model, Input::Event(ev(Source::Claude, "a", Waiting)), 0);
    step(&mut model, Input::Event(ev(Source::Codex, "b", Running)), 0);

    // Nothing focused: waiting drives the mascot.
    assert_eq!(reduce(&model, 0).top, Waiting);

    // Look at the waiting session: mascot falls to the next-best (running),
    // but the waiting session is still listed and flagged focused.
    step(&mut model, Input::FocusChanged(Some(key(Source::Claude, "a"))), 1);
    let snap = reduce(&model, 1);
    assert_eq!(snap.top, Running, "focused waiting no longer drives mascot");
    let focused = snap.sessions.iter().find(|s| s.key == key(Source::Claude, "a")).unwrap();
    assert!(focused.focused);
    assert_eq!(focused.state, Waiting, "still in tray, unchanged");
}

#[test]
fn sole_focused_attention_session_calms_the_mascot() {
    let mut model = Model::default();
    step(&mut model, Input::Event(ev(Source::Claude, "a", Waiting)), 0);
    step(&mut model, Input::FocusChanged(Some(key(Source::Claude, "a"))), 1);
    let snap = reduce(&model, 1);
    assert_eq!(snap.top, Idle, "only session is focused → mascot idles");
    assert_eq!(snap.sessions.len(), 1, "still shown in tray");
}

#[test]
fn focused_unseen_ready_still_counts_as_unread() {
    let mut model = Model::default();
    step(&mut model, Input::Event(ev(Source::Claude, "a", Ready)), 0);
    step(&mut model, Input::FocusChanged(Some(key(Source::Claude, "a"))), 1);
    let snap = reduce(&model, 1);
    assert_eq!(snap.top, Idle, "mascot suppressed");
    assert_eq!(snap.unread, 1, "but the result is still unacknowledged");
}

#[test]
fn focus_on_unknown_session_suppresses_nothing() {
    let mut model = Model::default();
    step(&mut model, Input::Event(ev(Source::Claude, "a", Waiting)), 0);
    step(&mut model, Input::FocusChanged(Some(key(Source::Pi, "ghost"))), 1);
    let snap = reduce(&model, 1);
    assert_eq!(snap.top, Waiting, "focus on a non-session doesn't calm the pet");
    assert!(snap.sessions.iter().all(|s| !s.focused));
}

#[test]
fn clearing_focus_resumes_the_nag() {
    let mut model = Model::default();
    step(&mut model, Input::Event(ev(Source::Claude, "a", Waiting)), 0);
    step(&mut model, Input::FocusChanged(Some(key(Source::Claude, "a"))), 1);
    assert_eq!(reduce(&model, 1).top, Idle);
    step(&mut model, Input::FocusChanged(None), 2);
    assert_eq!(reduce(&model, 2).top, Waiting);
}

#[test]
fn removing_focused_session_clears_focus() {
    let mut model = Model::default();
    step(&mut model, Input::Event(ev(Source::Claude, "a", Waiting)), 0);
    step(&mut model, Input::FocusChanged(Some(key(Source::Claude, "a"))), 1);
    assert!(model.focused.is_some());

    // Session ends: focus pointer must not dangle.
    let mut gone = ev(Source::Claude, "a", Gone);
    gone.ts = Some(2);
    step(&mut model, Input::Event(gone), 2);
    assert_eq!(model.focused, None);
    assert_eq!(reduce(&model, 2).top, Idle);
}

#[test]
fn focus_survives_a_wrapper_gone_for_a_different_session() {
    // Sanity: unrelated removals don't clear an active focus.
    let mut model = Model::default();
    step(&mut model, Input::Event(ev(Source::Claude, "a", Waiting)), 0);
    step(&mut model, Input::Event(ev(Source::Codex, "b", Running)), 0);
    step(&mut model, Input::FocusChanged(Some(key(Source::Claude, "a"))), 1);

    let mut gone = ev(Source::Codex, "b", Gone);
    gone.ts = Some(2);
    step(&mut model, Input::Event(gone), 2);
    assert_eq!(model.focused, Some(key(Source::Claude, "a")));
}


// --- ready presentation window (mascot calms after 4s, stays in tray) ---

#[test]
fn ready_drives_mascot_only_within_presentation_window() {
    use pet_core::READY_PRESENT_MS;
    let mut model = Model::default();
    // ready enters at t=0.
    let mut e = ev(Source::Claude, "a", Ready);
    e.ts = Some(0);
    e.body = Some("done".into());
    step(&mut model, Input::Event(e), 0);

    // Within the window: ready drives the mascot.
    assert_eq!(reduce(&model, 1_000).top, Ready);
    assert_eq!(reduce(&model, READY_PRESENT_MS - 1).top, Ready);

    // Past the window: mascot calms to idle, but the session stays listed and
    // still counts as unread.
    let snap = reduce(&model, READY_PRESENT_MS);
    assert_eq!(snap.top, Idle, "ready calms after its presentation window");
    assert_eq!(snap.sessions.len(), 1, "still in the tray");
    assert_eq!(snap.unread, 1, "still unread until seen");
}

#[test]
fn presented_out_ready_yields_to_a_fresh_running() {
    use pet_core::READY_PRESENT_MS;
    let mut model = Model::default();
    let mut r = ev(Source::Claude, "a", Ready);
    r.ts = Some(0);
    step(&mut model, Input::Event(r), 0);
    // Another session starts running later.
    let mut run = ev(Source::Codex, "b", Running);
    run.ts = Some(READY_PRESENT_MS);
    step(&mut model, Input::Event(run), READY_PRESENT_MS);
    // Past the ready window, running drives the mascot (ready has calmed).
    assert_eq!(reduce(&model, READY_PRESENT_MS).top, Running);
}

#[test]
fn next_deadline_includes_ready_presentation_end() {
    use pet_core::{next_deadline, READY_PRESENT_MS};
    let mut model = Model::default();
    let mut e = ev(Source::Claude, "a", Ready);
    e.ts = Some(0);
    step(&mut model, Input::Event(e), 0);
    // The presentation end (0 + 4s) is sooner than the 7-day ready expiry, so
    // it's what the timer should fire on.
    assert_eq!(next_deadline(&model), Some(READY_PRESENT_MS));
}
