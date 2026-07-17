//! The daemon's event loop: feed inputs to the pure FSM, execute effects.

use std::sync::Arc;
use std::time::Duration;

use pet_core::{reduce, step, Effect, Input, Model};
use pet_proto::Snapshot;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tracing::{info, warn};
use zbus::object_server::InterfaceRef;

use crate::bus::{now_ms, PetBus};
use crate::config::Config;
use crate::{effects, persist};

const PERSIST_DEBOUNCE: Duration = Duration::from_secs(2);

pub async fn run(
    config: Config,
    mut model: Model,
    mut inputs: mpsc::UnboundedReceiver<Input>,
    mut focus_facts: mpsc::UnboundedReceiver<Option<pet_proto::ActiveWindow>>,
    snapshot_tx: watch::Sender<Arc<Snapshot>>,
    iface: InterfaceRef<PetBus>,
) {
    model.ttls = config.ttls;

    // Expire anything that lapsed while we were down, then publish.
    let boot_effects = step(&mut model, Input::Tick, now_ms());
    let mut tick_at: Option<Instant> = None;
    let mut persist_at: Option<Instant> = None;
    execute(
        boot_effects
            .into_iter()
            .chain([Effect::PublishSnapshot, Effect::Persist]),
        &config,
        &model,
        &snapshot_tx,
        &iface,
        &mut tick_at,
        &mut persist_at,
    )
    .await;

    loop {
        tokio::select! {
            input = inputs.recv() => {
                let Some(input) = input else {
                    info!("input channel closed; runtime exiting");
                    break;
                };
                let effects = step(&mut model, input, now_ms());
                execute(effects, &config, &model, &snapshot_tx, &iface, &mut tick_at, &mut persist_at).await;
            }
            _ = sleep_until_opt(tick_at), if tick_at.is_some() => {
                tick_at = None;
                let effects = step(&mut model, Input::Tick, now_ms());
                execute(effects, &config, &model, &snapshot_tx, &iface, &mut tick_at, &mut persist_at).await;
            }
            fact = focus_facts.recv() => {
                let Some(window) = fact else { continue };
                // Join the active window to a session (or None), then let the
                // pure FSM decide suppression.
                let key = window.as_ref().and_then(|w| {
                    let metas = model
                        .sessions
                        .iter()
                        .map(|(k, s)| (k.clone(), s.meta.clone()))
                        .collect();
                    crate::focus_join::resolve_live(w, &metas)
                });
                let effects = step(&mut model, Input::FocusChanged(key), now_ms());
                execute(effects, &config, &model, &snapshot_tx, &iface, &mut tick_at, &mut persist_at).await;
            }
            _ = sleep_until_opt(persist_at), if persist_at.is_some() => {
                persist_at = None;
                persist::save(&config.state_path, &model);
            }
        }
    }
    // Final save on orderly shutdown.
    persist::save(&config.state_path, &model);
}

async fn sleep_until_opt(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

async fn execute(
    effects: impl IntoIterator<Item = Effect>,
    config: &Config,
    model: &Model,
    snapshot_tx: &watch::Sender<Arc<Snapshot>>,
    iface: &InterfaceRef<PetBus>,
    tick_at: &mut Option<Instant>,
    persist_at: &mut Option<Instant>,
) {
    for effect in effects {
        match effect {
            Effect::PublishSnapshot => {
                let mut snapshot = reduce(model, now_ms());
                crate::summary::decorate(&mut snapshot);
                let snapshot = Arc::new(snapshot);
                let json = serde_json::to_string(&*snapshot).unwrap_or_default();
                let _ = snapshot_tx.send(snapshot);
                if let Err(e) = PetBus::snapshot_changed(iface.signal_emitter(), &json).await {
                    warn!("snapshot_changed signal failed: {e}");
                }
            }
            Effect::ScheduleTick(deadline_ms) => {
                let in_ms = (deadline_ms - now_ms()).max(0) as u64;
                let at = Instant::now() + Duration::from_millis(in_ms);
                // Keep the earliest requested deadline.
                *tick_at = Some(tick_at.map_or(at, |cur| cur.min(at)));
            }
            Effect::Persist => {
                persist_at.get_or_insert(Instant::now() + PERSIST_DEBOUNCE);
            }
            Effect::Focus { key, meta, body } => {
                effects::focus(
                    &config.focus,
                    &config.gastown.town_dir,
                    &key,
                    &meta,
                    body.as_deref(),
                )
                .await;
            }
        }
    }
}
