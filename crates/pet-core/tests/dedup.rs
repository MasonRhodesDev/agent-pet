//! Wrapper precedence and identity cases from the wrapping matrix:
//! Happy wraps claude/codex/openclaw; Gas Town beads/polecats; direct wins.

use pet_core::{reduce, step, Input, Model, Origin, Ttls};
use pet_proto::{AgentState, Event, Meta, SessionKey, Source, Via};

use AgentState::*;

fn direct(source: Source, session: &str, state: AgentState, ts: i64) -> Event {
    Event {
        v: 1,
        source,
        session: session.into(),
        state,
        body: None,
        ts: Some(ts),
        via: None,
        meta: Meta::default(),
    }
}

fn wrapped(
    source: Source,
    session: &str,
    state: AgentState,
    ts: i64,
    wrapper: Source,
    links: Option<SessionKey>,
) -> Event {
    let mut e = direct(source, session, state, ts);
    e.via = Some(Via {
        wrapper,
        wrapper_session: format!("{wrapper}-sess"),
        links,
    });
    e
}

#[test]
fn wrapper_only_session_is_fully_driven_by_wrapper() {
    let mut model = Model::default();
    step(
        &mut model,
        Input::Event(wrapped(Source::Openclaw, "oc1", Running, 0, Source::Happy, None)),
        0,
    );
    let key = SessionKey::new(Source::Openclaw, "oc1");
    assert_eq!(model.sessions[&key].phase, Running);
    assert_eq!(model.sessions[&key].origin, Origin::WrapperOnly);

    step(
        &mut model,
        Input::Event(wrapped(Source::Openclaw, "oc1", Ready, 10, Source::Happy, None)),
        10,
    );
    assert_eq!(model.sessions[&key].phase, Ready);

    // Tray badge names the wrapper.
    let snap = reduce(&model, 10);
    assert_eq!(snap.sessions[0].via, Some(Source::Happy));
}

#[test]
fn fresh_direct_feed_downgrades_wrapper_to_enrichment() {
    let mut model = Model::default();
    let key = SessionKey::new(Source::Claude, "c1");

    step(&mut model, Input::Event(direct(Source::Claude, "c1", Running, 1_000)), 1_000);

    // Wrapper echo of the same work (same key via links): state ignored,
    // metadata absorbed.
    let mut echo = wrapped(
        Source::Claude,
        "c1",
        Ready,
        1_500,
        Source::Happy,
        Some(key.clone()),
    );
    echo.meta.title = Some("refactor pass".into());
    step(&mut model, Input::Event(echo), 1_500);

    assert_eq!(model.sessions[&key].phase, Running, "direct state wins");
    assert_eq!(
        model.sessions[&key].meta.title.as_deref(),
        Some("refactor pass"),
        "wrapper still enriches"
    );
}

#[test]
fn stronger_wrapper_state_pierces_after_grace() {
    let ttls = Ttls::default();
    let mut model = Model::default();
    let key = SessionKey::new(Source::Claude, "c1");

    step(&mut model, Input::Event(direct(Source::Claude, "c1", Running, 1_000)), 1_000);

    // Within grace: even waiting is suppressed.
    let early = wrapped(Source::Claude, "c1", Waiting, 1_000 + ttls.wrapper_grace_ms,
        Source::Happy, Some(key.clone()));
    step(&mut model, Input::Event(early), 1_000 + ttls.wrapper_grace_ms);
    assert_eq!(model.sessions[&key].phase, Running);

    // After grace: waiting (stronger) gets through; ready (weaker) would not.
    let late_ts = 1_000 + ttls.wrapper_grace_ms + 1;
    let late = wrapped(Source::Claude, "c1", Waiting, late_ts, Source::Happy, Some(key.clone()));
    step(&mut model, Input::Event(late), late_ts);
    assert_eq!(model.sessions[&key].phase, Waiting);
}

#[test]
fn stale_direct_channel_hands_control_to_wrapper() {
    let ttls = Ttls::default();
    let mut model = Model::default();
    let key = SessionKey::new(Source::Claude, "c1");

    step(&mut model, Input::Event(direct(Source::Claude, "c1", Running, 0)), 0);

    // Direct channel stale (running TTL elapsed): wrapper drives fully.
    let ts = ttls.running_ms + 1;
    let ev = wrapped(Source::Claude, "c1", Ready, ts, Source::Gastown, Some(key.clone()));
    step(&mut model, Input::Event(ev), ts);
    assert_eq!(model.sessions[&key].phase, Ready);
}

#[test]
fn wrapper_gone_always_applies() {
    let mut model = Model::default();
    let key = SessionKey::new(Source::Claude, "c1");

    step(&mut model, Input::Event(direct(Source::Claude, "c1", Running, 1_000)), 1_000);
    let gone = wrapped(Source::Claude, "c1", Gone, 1_001, Source::Happy, Some(key.clone()));
    step(&mut model, Input::Event(gone), 1_001);
    assert!(!model.sessions.contains_key(&key), "wrapper gone removes even a fresh direct session");
}

#[test]
fn links_alias_routes_both_feeds_to_one_session() {
    let mut model = Model::default();
    let canonical = SessionKey::new(Source::Claude, "c1");

    // Wrapper arrives first under its own identity, learning the canonical
    // key: alias is recorded up front.
    let ev = wrapped(Source::Claude, "happy-view", Running, 0, Source::Happy, Some(canonical.clone()));
    step(&mut model, Input::Event(ev), 0);
    assert!(model.sessions.contains_key(&canonical));
    assert!(!model.sessions.contains_key(&SessionKey::new(Source::Claude, "happy-view")));

    // Direct feed for the same work joins the same session.
    step(&mut model, Input::Event(direct(Source::Claude, "c1", Waiting, 10)), 10);
    assert_eq!(model.sessions.len(), 1);
    assert_eq!(model.sessions[&canonical].phase, Waiting);
    assert_eq!(model.sessions[&canonical].origin, Origin::Direct);

    // Later wrapper events addressed to the wrapper key follow the alias.
    let echo = wrapped(Source::Claude, "happy-view", Ready, 20, Source::Happy, Some(canonical.clone()));
    step(&mut model, Input::Event(echo), 20);
    assert_eq!(model.sessions.len(), 1, "no duplicate session appears");
}

#[test]
fn agent_pid_join_hint_aliases_without_links() {
    let mut model = Model::default();
    let canonical = SessionKey::new(Source::Claude, "c1");

    let mut d = direct(Source::Claude, "c1", Running, 0);
    d.meta.agent_pid = Some(777);
    step(&mut model, Input::Event(d), 0);

    // Happy observes the same harness process but only knows its own CUID.
    let mut w = wrapped(Source::Claude, "cuid-1", Running, 5, Source::Happy, None);
    w.meta.agent_pid = Some(777);
    step(&mut model, Input::Event(w), 5);

    assert_eq!(model.sessions.len(), 1, "pid join hint deduplicates");
    assert!(model.sessions.contains_key(&canonical));

    // And the model round-trips with its alias table.
    let json = serde_json::to_string(&model).unwrap();
    let back: Model = serde_json::from_str(&json).unwrap();
    assert_eq!(back.aliases.len(), model.aliases.len());
}

/// A direct harness event tagged by the daemon's town intake policy.
fn town_direct(session: &str, state: AgentState, ts: i64, gref: &str) -> Event {
    let mut e = direct(Source::Claude, session, state, ts);
    e.meta.extra.insert("gastown_ref".into(), gref.into());
    e
}

/// A Gas Town poller row (via-less, source gastown).
fn poller(session: &str, state: AgentState, ts: i64, body: &str) -> Event {
    let mut e = direct(Source::Gastown, session, state, ts);
    e.body = Some(body.into());
    e
}

#[test]
fn gastown_ref_aliases_poller_row_to_direct_session() {
    // Poller row exists first; direct claude session self-identifies later.
    let mut model = Model::default();
    let crew_key = SessionKey::new(Source::Gastown, "crew/lifemd/user_merge");
    let claude_key = SessionKey::new(Source::Claude, "c1");

    step(&mut model, Input::Event(poller("crew/lifemd/user_merge", Running, 0, "develop (dirty)")), 0);
    assert!(model.sessions.contains_key(&crew_key));

    step(&mut model, Input::Event(town_direct("c1", Waiting, 10, "crew/lifemd/user_merge")), 10);
    assert_eq!(model.sessions.len(), 1, "poller row collapsed into the direct session");
    assert!(model.sessions.contains_key(&claude_key));
    assert_eq!(model.sessions[&claude_key].phase, Waiting);
    assert_eq!(
        model.sessions[&claude_key].body.as_deref(),
        Some("develop (dirty)"),
        "poller metadata migrated as enrichment"
    );

    // Clicking either key resolves to the same session.
    let via_alias = step(&mut model, Input::FocusRequested(crew_key.clone()), 11);
    assert!(matches!(&via_alias[0], pet_core::Effect::Focus { key, .. } if *key == claude_key));
}

#[test]
fn poller_heartbeat_cannot_stomp_fresh_direct_state() {
    // Direct claude first (the live gap: crew agents flapping back to
    // Running on every 15s poll while actually waiting for input).
    let mut model = Model::default();
    let claude_key = SessionKey::new(Source::Claude, "c1");

    step(&mut model, Input::Event(town_direct("c1", Waiting, 1_000, "crew/lifemd/user_merge")), 1_000);
    step(&mut model, Input::Event(poller("crew/lifemd/user_merge", Running, 1_500, "develop")), 1_500);

    assert_eq!(model.sessions.len(), 1);
    assert_eq!(model.sessions[&claude_key].phase, Waiting, "direct state wins over sibling feed");
    assert_eq!(model.sessions[&claude_key].body.as_deref(), Some("develop"), "sibling still enriches");

    // Poller gone (crew workspace removed) is authoritative.
    let mut gone = poller("crew/lifemd/user_merge", Gone, 2_000, "");
    gone.body = None;
    step(&mut model, Input::Event(gone), 2_000);
    assert!(model.sessions.is_empty());
}

#[test]
fn escalation_focus_carries_the_mayors_terminal() {
    let mut model = Model::default();

    // Mayor's direct claude session, tagged by intake, with tmux hints.
    let mut mayor = town_direct("mayor-uuid", Running, 0, "mayor");
    mayor.meta.extra.insert("tmux_socket".into(), "/tmp/tmux-1000/town-x".into());
    mayor.meta.extra.insert("tmux_pane".into(), "%1".into());
    step(&mut model, Input::Event(mayor), 0);

    // Escalation from the poller.
    step(&mut model, Input::Event(poller("esc/hq-5ta", Waiting, 5, "hq-5ta Confirm Block B state")), 5);

    let esc_key = SessionKey::new(Source::Gastown, "esc/hq-5ta");
    let effects = step(&mut model, Input::FocusRequested(esc_key.clone()), 10);
    match &effects[0] {
        pet_core::Effect::Focus { key, meta, body } => {
            assert_eq!(*key, esc_key, "effect names the escalation");
            assert_eq!(
                meta.extra.get("tmux_pane").and_then(|v| v.as_str()),
                Some("%1"),
                "but carries the mayor's terminal correlation"
            );
            assert_eq!(body.as_deref(), Some("hq-5ta Confirm Block B state"));
        }
        other => panic!("expected Focus, got {other:?}"),
    }

    // Without a mayor session, the escalation's own (hint-less) meta rides.
    let mut model = Model::default();
    step(&mut model, Input::Event(poller("esc/hq-9zz", Waiting, 0, "hq-9zz t")), 0);
    let effects = step(&mut model, Input::FocusRequested(SessionKey::new(Source::Gastown, "esc/hq-9zz")), 1);
    assert!(matches!(
        &effects[0],
        pet_core::Effect::Focus { meta, .. } if !meta.extra.contains_key("tmux_pane")
    ));
}

#[test]
fn unjoinable_wrapper_session_stands_alone_tagged() {
    let mut model = Model::default();

    // Direct claude session with no pid hint.
    step(&mut model, Input::Event(direct(Source::Claude, "c1", Running, 0)), 0);
    // Happy-observed claude session, different id, no join key.
    step(
        &mut model,
        Input::Event(wrapped(Source::Claude, "cuid-9", Running, 5, Source::Happy, None)),
        5,
    );

    let snap = reduce(&model, 5);
    assert_eq!(snap.sessions.len(), 2, "parallel entries accepted");
    let badges: Vec<Option<Source>> = snap.sessions.iter().map(|s| s.via).collect();
    assert!(badges.contains(&Some(Source::Happy)));
    assert!(badges.contains(&None));
}
