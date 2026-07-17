//! The D-Bus surface. Methods translate straight into FSM inputs; Status
//! answers from the latest published snapshot without touching the model.

use std::sync::Arc;

use pet_core::Input;
use pet_proto::{Event, SessionKey, Snapshot};
use tokio::sync::{mpsc, watch};
use tracing::debug;
use zbus::fdo;
use zbus::object_server::SignalEmitter;

pub struct PetBus {
    inputs: mpsc::UnboundedSender<Input>,
    snapshot: watch::Receiver<Arc<Snapshot>>,
    renderer: watch::Sender<pet_render::Control>,
    /// Gas Town root, for the town intake policy.
    town_dir: String,
}

impl PetBus {
    pub fn new(
        inputs: mpsc::UnboundedSender<Input>,
        snapshot: watch::Receiver<Arc<Snapshot>>,
        renderer: watch::Sender<pet_render::Control>,
        town_dir: String,
    ) -> Self {
        Self {
            inputs,
            snapshot,
            renderer,
            town_dir,
        }
    }

    fn send(&self, input: Input) -> fdo::Result<()> {
        self.inputs
            .send(input)
            .map_err(|_| fdo::Error::Failed("daemon runtime is gone".into()))
    }

    fn renderer_control(&self, cmd: pet_render::ControlCmd) -> fdo::Result<()> {
        if self.renderer.receiver_count() == 0 {
            return Err(fdo::Error::Failed(
                "renderer is not running (daemon started --headless?)".into(),
            ));
        }
        self.renderer.send_modify(|control| {
            control.seq += 1;
            control.cmd = Some(cmd);
        });
        Ok(())
    }
}

#[zbus::interface(name = "io.github.masonrhodesdev.AgentPet1")]
impl PetBus {
    async fn emit(&self, event_json: &str) -> fdo::Result<()> {
        let event: Event = serde_json::from_str(event_json)
            .map_err(|e| fdo::Error::InvalidArgs(format!("bad event json: {e}")))?;
        let event = event
            .validate(now_ms())
            .map_err(|e| fdo::Error::InvalidArgs(e.to_string()))?;
        debug!(source = %event.source, session = %event.session, state = ?event.state, "event");
        match town_intake(event, &self.town_dir) {
            Some(event) => self.send(Input::Event(event)),
            None => Ok(()), // town infrastructure: silently dropped
        }
    }

    async fn status(&self) -> String {
        serde_json::to_string(&**self.snapshot.borrow()).unwrap_or_else(|_| "{}".into())
    }

    async fn mark_seen(&self, session_key: &str) -> fdo::Result<()> {
        let key: SessionKey = session_key
            .parse()
            .map_err(|e| fdo::Error::InvalidArgs(format!("{e}")))?;
        self.send(Input::Seen(key))
    }

    async fn mark_all_seen(&self) -> fdo::Result<()> {
        self.send(Input::SeenAll)
    }

    async fn focus(&self, session_key: &str) -> fdo::Result<()> {
        let key: SessionKey = session_key
            .parse()
            .map_err(|e| fdo::Error::InvalidArgs(format!("{e}")))?;
        self.send(Input::FocusRequested(key))
    }

    /// Remap the mascot (undoes a right-click or `Hide()`); persists.
    async fn show(&self) -> fdo::Result<()> {
        debug!("show requested over dbus");
        self.renderer_control(pet_render::ControlCmd::Show)
    }

    /// Unmap the mascot; persists, so it stays hidden across restarts.
    async fn hide(&self) -> fdo::Result<()> {
        debug!("hide requested over dbus");
        self.renderer_control(pet_render::ControlCmd::Hide)
    }

    #[zbus(signal)]
    pub async fn snapshot_changed(
        emitter: &SignalEmitter<'_>,
        snapshot_json: &str,
    ) -> zbus::Result<()>;
}

/// Town session policy: harness sessions working inside the Gas Town dir
/// are classified by role — infrastructure agents (witness/refinery/deacon/
/// dogs/polecats) must NEVER surface as pet sessions; only the mayor and
/// crew matter. Crew/mayor sessions are tagged with `gastown_ref` so the
/// FSM collapses them with the poller's rows and the focus effect can route
/// by role. Returns `None` to drop the event.
fn town_intake(mut event: Event, town_dir: &str) -> Option<Event> {
    use pet_adapters::gastown::{classify_town_path, TownRole};
    use pet_proto::Source;

    if !matches!(event.source, Source::Claude | Source::Codex) {
        return Some(event);
    }
    let Some(cwd) = event.meta.cwd.as_deref() else {
        return Some(event);
    };
    match classify_town_path(town_dir, cwd) {
        TownRole::Infra => {
            debug!(session = %event.session, cwd, "dropping town-infra session");
            None
        }
        TownRole::Crew { rig, name } => {
            event
                .meta
                .extra
                .insert("gastown_ref".into(), format!("crew/{rig}/{name}").into());
            event.meta.title = Some(format!("crew {name}"));
            Some(event)
        }
        TownRole::Mayor => {
            event.meta.extra.insert("gastown_ref".into(), "mayor".into());
            event.meta.title = Some("Mayor".into());
            Some(event)
        }
        TownRole::NotTown => Some(event),
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pet_proto::{AgentState, Meta, Source};

    fn ev(source: Source, cwd: &str) -> Event {
        Event {
            v: 1,
            source,
            session: "s1".into(),
            state: AgentState::Running,
            body: None,
            ts: None,
            via: None,
            meta: Meta {
                cwd: Some(cwd.into()),
                ..Default::default()
            },
        }
    }

    const TOWN: &str = "/home/mason/agent-town/town";

    #[test]
    fn infra_dropped_crew_and_mayor_tagged_rest_passed() {
        assert!(town_intake(ev(Source::Claude, &format!("{TOWN}/odin/witness")), TOWN).is_none());
        assert!(town_intake(ev(Source::Codex, &format!("{TOWN}/deacon/dogs/boot")), TOWN).is_none());

        let crew = town_intake(
            ev(Source::Claude, &format!("{TOWN}/lifemd/crew/user_merge")),
            TOWN,
        )
        .unwrap();
        assert_eq!(
            crew.meta.extra.get("gastown_ref").unwrap(),
            "crew/lifemd/user_merge"
        );
        assert_eq!(crew.meta.title.as_deref(), Some("crew user_merge"));

        let mayor = town_intake(ev(Source::Claude, &format!("{TOWN}/mayor")), TOWN).unwrap();
        assert_eq!(mayor.meta.extra.get("gastown_ref").unwrap(), "mayor");
        assert_eq!(mayor.meta.title.as_deref(), Some("Mayor"));

        // Outside the town / non-harness sources: untouched.
        let out = town_intake(ev(Source::Claude, "/home/mason/repos/x"), TOWN).unwrap();
        assert!(out.meta.extra.is_empty());
        let gt = town_intake(ev(Source::Gastown, &format!("{TOWN}/odin/witness")), TOWN).unwrap();
        assert!(gt.meta.extra.is_empty());
    }
}

