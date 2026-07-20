//! pet-render: Wayland layer-shell mascot renderer.
//!
//! The daemon calls [`spawn`] with the snapshot watch channel, a control
//! watch (Show/Hide from D-Bus), and a UiAction sender for interactions the
//! daemon should know about. Everything Wayland lives on the returned
//! thread. Headless-tolerant: connect failures and panics are caught and
//! retried with backoff, so bus activation before the session is up never
//! hurts the daemon.

mod app;
mod canvas;
mod compose;
pub mod compositor;
mod input;
#[doc(hidden)]
pub mod preview;
pub mod sprite;
mod surface;
mod text;
mod wayland;

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pet_proto::{Snapshot, UiAction};
use tokio::sync::{mpsc, watch};
use tracing::{error, info, warn};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PetSelection {
    pub skin: Option<String>,
}

/// Daemon -> renderer commands. Watch-carried with a sequence number so a
/// restarted renderer can tell a fresh command from a stale replay.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Control {
    pub seq: u64,
    pub cmd: Option<ControlCmd>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlCmd {
    Show,
    Hide,
}

pub fn spawn(
    snapshot_rx: watch::Receiver<Arc<Snapshot>>,
    control_rx: watch::Receiver<Control>,
    ui_tx: mpsc::UnboundedSender<UiAction>,
    pet_rx: watch::Receiver<PetSelection>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("pet-render".into())
        .spawn(move || supervise(snapshot_rx, control_rx, ui_tx, pet_rx))
        .expect("spawn pet-render thread")
}

const BACKOFF: [Duration; 3] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(60),
];

fn supervise(
    rx: watch::Receiver<Arc<Snapshot>>,
    control_rx: watch::Receiver<Control>,
    ui_tx: mpsc::UnboundedSender<UiAction>,
    pet_rx: watch::Receiver<PetSelection>,
) {
    let mut failures: usize = 0;
    loop {
        let attempt_started = Instant::now();
        let result = catch_unwind(AssertUnwindSafe(|| {
            app::run(
                rx.clone(),
                control_rx.clone(),
                ui_tx.clone(),
                pet_rx.clone(),
            )
        }));
        match result {
            Ok(Ok(())) => {
                info!("renderer shut down");
                return;
            }
            Ok(Err(e)) => warn!("renderer stopped: {e:#}"),
            Err(payload) => error!("renderer panicked: {}", panic_message(payload.as_ref())),
        }
        if rx.has_changed().is_err() {
            info!("daemon shutting down; renderer exiting");
            return;
        }
        // A run that survived a while was healthy — start backoff over.
        if attempt_started.elapsed() >= BACKOFF[BACKOFF.len() - 1] {
            failures = 0;
        }
        let delay = BACKOFF[failures.min(BACKOFF.len() - 1)];
        failures += 1;
        info!("retrying renderer in {delay:?}");
        std::thread::sleep(delay);
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("<non-string panic payload>")
}
