//! Gas Town town-state → Events (pure mapping; the daemon owns the
//! subprocess polling).
//!
//! What the pet tracks, per Mason's priorities:
//! - **Escalations** — beads assigned to the overseer are decisions waiting
//!   on the human: `waiting`, one session per bead (`esc/<id>`).
//! - **Crew** — persistent human-managed workers: `crew/<rig>/<name>`,
//!   running while their session exists.
//! - **The Mayor** — quiet baseline while up (idle, hidden from the tray);
//!   `failed` the moment it is down, because a headless town is the one
//!   thing worth an alert.
//! - **Polecats** (optional, off by default) — ephemeral witness-managed
//!   workers; noisy, and their outcomes reach the human as escalations
//!   anyway.
//!
//! Gas Town is a *direct* source — nothing else on the host can see this
//! work (container hooks can't reach the host bus).

use pet_proto::{AgentState, Event, Meta, Source};
use serde::Deserialize;

/// One worker from `gt polecat list --all --json`. Lenient: the gt CLI is
/// the contract but its fields drift.
#[derive(Debug, Clone, Deserialize)]
pub struct Polecat {
    #[serde(default)]
    pub rig: String,
    pub name: String,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub verdict: Option<String>,
    #[serde(default)]
    pub session_running: bool,
    #[serde(default)]
    pub blockers: Vec<serde_json::Value>,
}

/// One issue from `bd list --json`. NOTE: beads carry both `owner` (the
/// human) and `assignee` (the actor holding it) — never alias these.
#[derive(Debug, Clone, Deserialize)]
pub struct Bead {
    pub id: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub assignee: Option<String>,
    /// RFC3339 — lexicographic order is arrival order.
    #[serde(default)]
    pub created_at: Option<String>,
}

/// Escalation nag policy: only the *latest* open escalation nags. Any open
/// escalation that is ever observed alongside a newer one is superseded and
/// never nags again, even if the newer one gets resolved first. In-memory:
/// a daemon restart re-nags the newest open escalation, nothing else.
#[derive(Debug, Default)]
pub struct EscalationTracker {
    superseded: std::collections::BTreeSet<String>,
}

impl EscalationTracker {
    /// Pick the bead to nag about (if any) and record supersessions.
    pub fn select<'b>(&mut self, beads: &'b [Bead]) -> Option<&'b Bead> {
        let open: Vec<&Bead> = beads
            .iter()
            .filter(|b| b.status.as_deref() != Some("closed"))
            .collect();
        let newest = open.iter().max_by_key(|b| (&b.created_at, &b.id)).copied()?;
        for stale in open.iter().filter(|b| b.id != newest.id) {
            self.superseded.insert(stale.id.clone());
        }
        (!self.superseded.contains(&newest.id)).then_some(newest)
    }
}

/// One crew workspace from `gt crew list <rig> --json`.
#[derive(Debug, Clone, Deserialize)]
pub struct CrewMember {
    pub name: String,
    #[serde(default)]
    pub rig: String,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub has_session: bool,
    #[serde(default)]
    pub git_clean: bool,
}

/// One rig from `gt rig list --json`; used by the poller to skip rigs with
/// nothing to ask about.
#[derive(Debug, Clone, Deserialize)]
pub struct Rig {
    pub name: String,
    #[serde(default)]
    pub crew: u32,
    #[serde(default)]
    pub polecats: u32,
}

/// Everything one poll of the town observed. `mayor_running: None` means
/// the probe itself failed (no mayor event is emitted rather than a false
/// "down" alert).
#[derive(Debug, Clone, Default)]
pub struct TownObservation {
    pub mayor_running: Option<bool>,
    pub crew: Vec<CrewMember>,
    pub escalations: Vec<Bead>,
    pub polecats: Vec<Polecat>,
}

/// Empty crew/rig listings print prose ("No crew workspaces found.")
/// instead of JSON — prose maps to an empty list. Text that *looks* like
/// JSON but fails to parse is a real error and must surface, not vanish.
pub fn parse_lenient_list<T: serde::de::DeserializeOwned>(
    text: &str,
) -> Result<Vec<T>, serde_json::Error> {
    let trimmed = text.trim();
    if trimmed.starts_with('[') || trimmed.starts_with('{') {
        serde_json::from_str(trimmed)
    } else {
        Ok(Vec::new())
    }
}

const MAYOR_SESSION: &str = "mayor";

/// What a working directory under the town means. Only the mayor and crew
/// are human-facing; everything else under the town dir is infrastructure
/// (witness/refinery/deacon/dogs/polecat worktrees) that must never surface
/// as a pet session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TownRole {
    Mayor,
    Crew { rig: String, name: String },
    Infra,
    NotTown,
}

/// Classify a process cwd against the Gas Town layout. Pure.
///
/// Live-verified layout (2026-07-17, tmux server `town-*`):
/// - mayor's claude runs at `<town>/mayor` (session `hq-mayor`)
/// - crew at `<town>/<rig>/crew/<name>` (sessions `<prefix>-crew-<name>`)
/// - infra everywhere else under the town: `<town>/deacon`,
///   `<town>/deacon/dogs/<dog>`, `<town>/<rig>/witness`,
///   `<town>/<rig>/refinery/rig`, `<town>/<rig>/polecats/<name>/<checkout>`.
pub fn classify_town_path(town_dir: &str, cwd: &str) -> TownRole {
    let town = town_dir.trim_end_matches('/');
    let cwd = cwd.trim_end_matches('/');
    let rel = if cwd == town {
        ""
    } else if let Some(rel) = cwd.strip_prefix(town).and_then(|r| r.strip_prefix('/')) {
        rel
    } else {
        return TownRole::NotTown;
    };
    let comps: Vec<&str> = rel.split('/').filter(|c| !c.is_empty()).collect();
    match comps.as_slice() {
        ["mayor", ..] => TownRole::Mayor,
        [rig, "crew", name, ..] => TownRole::Crew {
            rig: (*rig).to_owned(),
            name: (*name).to_owned(),
        },
        _ => TownRole::Infra,
    }
}

fn crew_session(c: &CrewMember) -> String {
    format!("crew/{}/{}", c.rig, c.name)
}

fn escalation_session(b: &Bead) -> String {
    format!("esc/{}", b.id)
}

fn polecat_session(p: &Polecat) -> String {
    if p.rig.is_empty() {
        p.name.clone()
    } else {
        format!("{}/{}", p.rig, p.name)
    }
}

fn held_bead<'b>(p: &Polecat, beads: &'b [Bead]) -> Option<&'b Bead> {
    beads.iter().find(|b| {
        b.assignee
            .as_deref()
            .is_some_and(|a| a.contains(&p.name) && (p.rig.is_empty() || a.contains(&p.rig)))
    })
}

fn polecat_state(p: &Polecat, bead: Option<&Bead>) -> AgentState {
    // Idle workers awaiting cleanup/recovery are town housekeeping, not
    // agent activity — even when flagged NEEDS_RECOVERY.
    if !p.session_running && p.state.as_deref() == Some("idle") {
        return AgentState::Idle;
    }
    if p.verdict.as_deref() == Some("NEEDS_RECOVERY") {
        return AgentState::Failed;
    }
    if !p.blockers.is_empty() || bead.and_then(|b| b.status.as_deref()) == Some("blocked") {
        return AgentState::Waiting;
    }
    if p.session_running {
        return AgentState::Running;
    }
    match p.state.as_deref() {
        Some("done") => AgentState::Ready,
        _ => AgentState::Idle,
    }
}

/// Map one poll to events plus the tracked-session set for the next poll's
/// `gone` diff. Repeated same-state events are heartbeats (deadline
/// refresh) as far as the FSM is concerned.
pub fn poll_step(
    prev_sessions: &[String],
    obs: &TownObservation,
    include_polecats: bool,
    escalations: &mut EscalationTracker,
) -> (Vec<Event>, Vec<String>) {
    let mut events = Vec::new();
    let mut tracked = Vec::new();
    let mut push = |session: String, state: AgentState, body: Option<String>, title: Option<String>| {
        if state != AgentState::Idle {
            tracked.push(session.clone());
        } else if !prev_sessions.contains(&session) {
            return; // never-tracked idle produces no session
        }
        events.push(Event {
            v: pet_proto::PROTOCOL_VERSION,
            source: Source::Gastown,
            session,
            state,
            body,
            ts: None,
            via: None,
            meta: Meta {
                title,
                ..Default::default()
            },
        });
    };

    // Mayor: quiet while up, loud when down.
    match obs.mayor_running {
        Some(true) => push(MAYOR_SESSION.into(), AgentState::Idle, None, Some("Mayor".into())),
        Some(false) => push(
            MAYOR_SESSION.into(),
            AgentState::Failed,
            Some("Mayor is down".into()),
            Some("Mayor".into()),
        ),
        None => {}
    }

    for c in &obs.crew {
        let state = if c.has_session {
            AgentState::Running
        } else {
            AgentState::Idle
        };
        let dirty = if c.git_clean { "" } else { " (dirty)" };
        let body = c
            .branch
            .as_deref()
            .map(|b| format!("{b}{dirty}"))
            .or_else(|| (!dirty.is_empty()).then(|| dirty.trim().to_string()));
        push(crew_session(c), state, body, Some(format!("crew {}", c.name)));
    }

    // Only the latest open escalation nags; superseded ones vanish from the
    // tray via the gone-diff below.
    if let Some(b) = escalations.select(&obs.escalations) {
        push(
            escalation_session(b),
            AgentState::Waiting,
            Some(match &b.title {
                Some(t) => format!("{} {}", b.id, t),
                None => b.id.clone(),
            }),
            Some("escalation".into()),
        );
    }

    if include_polecats {
        for p in &obs.polecats {
            let bead = held_bead(p, &obs.escalations);
            let state = polecat_state(p, bead);
            let body = bead.map(|b| match &b.title {
                Some(t) => format!("{} {}", b.id, t),
                None => b.id.clone(),
            });
            let title = Some(polecat_session(p));
            push(polecat_session(p), state, body, title);
        }
    }

    // Tracked sessions that vanished from this observation are gone:
    // resolved escalations, removed crew, dead polecats, a mayor probe
    // that stopped answering.
    let current: Vec<String> = events.iter().map(|e| e.session.clone()).collect();
    for stale in prev_sessions.iter().filter(|s| !current.contains(s)) {
        events.push(Event {
            v: pet_proto::PROTOCOL_VERSION,
            source: Source::Gastown,
            session: stale.clone(),
            state: AgentState::Gone,
            body: None,
            ts: None,
            via: None,
            meta: Meta::default(),
        });
    }

    (events, tracked)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs() -> TownObservation {
        TownObservation {
            mayor_running: Some(true),
            crew: vec![CrewMember {
                name: "rc_rollout".into(),
                rig: "idp_rc_controller".into(),
                branch: Some("main".into()),
                has_session: true,
                git_clean: false,
            }],
            escalations: vec![Bead {
                id: "hq-5ta".into(),
                status: Some("open".into()),
                title: Some("Confirm Block B state".into()),
                assignee: Some("overseer".into()),
                created_at: Some("2026-07-15T00:00:00Z".into()),
            }],
            polecats: vec![],
        }
    }

    #[test]
    fn crew_and_escalations_tracked_mayor_quiet() {
        let (events, tracked) = poll_step(&[], &obs(), false, &mut EscalationTracker::default());
        assert_eq!(events.len(), 2, "healthy mayor is invisible: {events:?}");

        let crew = events.iter().find(|e| e.session.starts_with("crew/")).unwrap();
        assert_eq!(crew.state, AgentState::Running);
        assert_eq!(crew.body.as_deref(), Some("main (dirty)"));

        let esc = events.iter().find(|e| e.session.starts_with("esc/")).unwrap();
        assert_eq!(esc.state, AgentState::Waiting);
        assert_eq!(esc.body.as_deref(), Some("hq-5ta Confirm Block B state"));

        assert_eq!(tracked.len(), 2);
    }

    #[test]
    fn mayor_down_alerts_and_recovery_clears() {
        let mut o = obs();
        o.mayor_running = Some(false);
        let (events, tracked) = poll_step(&[], &o, false, &mut EscalationTracker::default());
        let mayor = events.iter().find(|e| e.session == "mayor").unwrap();
        assert_eq!(mayor.state, AgentState::Failed);
        assert!(tracked.contains(&"mayor".to_string()));

        // Mayor comes back: tracked session gets the Idle transition.
        let (events, tracked2) = poll_step(&tracked, &obs(), false, &mut EscalationTracker::default());
        let mayor = events.iter().find(|e| e.session == "mayor").unwrap();
        assert_eq!(mayor.state, AgentState::Idle);
        assert!(!tracked2.contains(&"mayor".to_string()));

        // Probe failure: no mayor event at all, never a false alert.
        let mut o = obs();
        o.mayor_running = None;
        let (events, _) = poll_step(&[], &o, false, &mut EscalationTracker::default());
        assert!(events.iter().all(|e| e.session != "mayor"));
    }

    #[test]
    fn only_latest_escalation_nags_and_superseded_stay_ignored() {
        let bead = |id: &str, created: &str| Bead {
            id: id.into(),
            status: Some("open".into()),
            title: Some(format!("t-{id}")),
            assignee: Some("overseer".into()),
            created_at: Some(created.into()),
        };
        let mut tracker = EscalationTracker::default();
        let mut o = obs();

        // Backlog of two: only the newest nags.
        o.escalations = vec![bead("hq-old", "2026-07-10T00:00:00Z"), bead("hq-new", "2026-07-16T00:00:00Z")];
        let (events, tracked) = poll_step(&[], &o, false, &mut tracker);
        let esc: Vec<_> = events.iter().filter(|e| e.session.starts_with("esc/")).collect();
        assert_eq!(esc.len(), 1);
        assert_eq!(esc[0].session, "esc/hq-new");

        // Newest resolved: the superseded one does NOT resurrect.
        o.escalations = vec![bead("hq-old", "2026-07-10T00:00:00Z")];
        let (events, tracked2) = poll_step(&tracked, &o, false, &mut tracker);
        assert!(events.iter().all(|e| !e.session.starts_with("esc/") || e.state == AgentState::Gone));
        assert!(events.iter().any(|e| e.session == "esc/hq-new" && e.state == AgentState::Gone));

        // A genuinely new escalation nags again.
        o.escalations.push(bead("hq-fresh", "2026-07-17T00:00:00Z"));
        let (events, _) = poll_step(&tracked2, &o, false, &mut tracker);
        let esc: Vec<_> = events.iter().filter(|e| e.session.starts_with("esc/") && e.state == AgentState::Waiting).collect();
        assert_eq!(esc.len(), 1);
        assert_eq!(esc[0].session, "esc/hq-fresh");
    }

    #[test]
    fn resolved_escalation_goes_gone() {
        let (_, tracked) = poll_step(&[], &obs(), false, &mut EscalationTracker::default());
        let mut o = obs();
        o.escalations.clear();
        let (events, _) = poll_step(&tracked, &o, false, &mut EscalationTracker::default());
        let gone: Vec<_> = events.iter().filter(|e| e.state == AgentState::Gone).collect();
        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0].session, "esc/hq-5ta");
    }

    #[test]
    fn closed_escalations_and_idle_crew_are_skipped() {
        let mut o = obs();
        o.escalations[0].status = Some("closed".into());
        o.crew[0].has_session = false;
        let (events, tracked) = poll_step(&[], &o, false, &mut EscalationTracker::default());
        assert!(events.is_empty(), "{events:?}");
        assert!(tracked.is_empty());
    }

    #[test]
    fn polecats_only_when_enabled() {
        let mut o = obs();
        o.polecats = vec![Polecat {
            rig: "idp".into(),
            name: "chrome".into(),
            state: None,
            verdict: None,
            session_running: true,
            blockers: vec![],
        }];
        let (without, _) = poll_step(&[], &o, false, &mut EscalationTracker::default());
        assert!(without.iter().all(|e| e.session != "idp/chrome"));

        let (with, _) = poll_step(&[], &o, true, &mut EscalationTracker::default());
        let p = with.iter().find(|e| e.session == "idp/chrome").unwrap();
        assert_eq!(p.state, AgentState::Running);
    }

    #[test]
    fn parked_recovery_polecats_stay_invisible_even_when_enabled() {
        let mut o = obs();
        o.polecats = vec![Polecat {
            rig: "idp".into(),
            name: "fury".into(),
            state: Some("idle".into()),
            verdict: Some("NEEDS_RECOVERY".into()),
            session_running: false,
            blockers: vec![serde_json::json!("cleanup_status=<missing>")],
        }];
        let (events, _) = poll_step(&[], &o, true, &mut EscalationTracker::default());
        assert!(events.iter().all(|e| e.session != "idp/fury"));
    }

    #[test]
    fn lenient_list_tolerates_prose_but_surfaces_bad_json() {
        let empty: Vec<CrewMember> = parse_lenient_list("No crew workspaces found.\n").unwrap();
        assert!(empty.is_empty());
        let one: Vec<CrewMember> =
            parse_lenient_list(r#"[{"name":"dave","rig":"r","has_session":false,"git_clean":true}]"#)
                .unwrap();
        assert_eq!(one.len(), 1);
        assert!(parse_lenient_list::<CrewMember>(r#"[{"name": 42}]"#).is_err());
    }

    #[test]
    fn town_paths_classify_by_role() {
        const TOWN: &str = "/home/mason/agent-town/town";
        let at = |rel: &str| classify_town_path(TOWN, &format!("{TOWN}/{rel}"));

        // Real paths observed live on the running town.
        assert_eq!(at("mayor"), TownRole::Mayor);
        assert_eq!(at("mayor/subdir"), TownRole::Mayor);
        assert_eq!(
            at("lifemd/crew/user_merge"),
            TownRole::Crew {
                rig: "lifemd".into(),
                name: "user_merge".into()
            }
        );
        assert_eq!(
            at("idp_rc_controller/crew/rc_rollout/deep/path"),
            TownRole::Crew {
                rig: "idp_rc_controller".into(),
                name: "rc_rollout".into()
            }
        );
        assert_eq!(at("odin/refinery/rig"), TownRole::Infra);
        assert_eq!(at("deacon"), TownRole::Infra);
        assert_eq!(at("odin/witness"), TownRole::Infra);
        assert_eq!(at("deacon/dogs/boot"), TownRole::Infra);
        assert_eq!(at("odin/polecats/furiosa/odin"), TownRole::Infra);
        assert_eq!(classify_town_path(TOWN, TOWN), TownRole::Infra);

        // Outside the town (incl. the prefix-collision trap).
        assert_eq!(classify_town_path(TOWN, "/home/mason/repos/x"), TownRole::NotTown);
        assert_eq!(
            classify_town_path(TOWN, "/home/mason/agent-town/townhouse"),
            TownRole::NotTown
        );
        // Trailing slashes tolerated on both sides.
        assert_eq!(
            classify_town_path(&format!("{TOWN}/"), &format!("{TOWN}/mayor/")),
            TownRole::Mayor
        );
    }

    #[test]
    fn bead_parses_with_both_owner_and_assignee() {
        // Regression: owner+assignee coexist in bd output; an alias between
        // them made serde reject the whole list.
        let beads: Vec<Bead> = parse_lenient_list(
            r#"[{"id":"hq-5ta","status":"open","title":"t","owner":"mason@lifemd.com","assignee":"overseer","labels":[]}]"#,
        )
        .unwrap();
        assert_eq!(beads[0].assignee.as_deref(), Some("overseer"));
    }
}
