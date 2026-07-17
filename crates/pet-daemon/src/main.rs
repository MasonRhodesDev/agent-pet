//! agent-petd: aggregates harness events into a pet snapshot.
//!
//! Wiring only — all state logic lives in pet-core, all payload knowledge in
//! pet-adapters, all pixels in pet-render. `--headless` skips the renderer;
//! the daemon is fully functional without it.

mod bus;
mod config;
mod effects;
mod focus_join;
mod gastown;
mod persist;
mod runtime;

use anyhow::Context;
use tracing::info;

/// Take an exclusive, non-blocking flock on a runtime-dir lock file and
/// hold it for the process lifetime.
fn acquire_singleton_lock() -> anyhow::Result<()> {
    use std::os::fd::AsRawFd;

    let dir = std::path::PathBuf::from(
        std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into()),
    )
    .join("agent-pet");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("daemon.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        anyhow::bail!(
            "another agent-petd is already running (lock held on {}); exiting",
            path.display()
        );
    }
    std::mem::forget(file); // hold the lock until the process dies
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    // HARD singleton, layer 1: an exclusive flock independent of D-Bus
    // semantics. One pet runtime multiplexes all sessions; a second
    // instance must die here, never render.
    acquire_singleton_lock()?;

    let headless = std::env::args().skip(1).any(|arg| arg == "--headless");
    let config = config::Config::load();
    info!(?config.state_path, headless, "agent-petd starting");

    let model = persist::load(&config.state_path).unwrap_or_default();
    // Poll adapters own their sessions' lifecycle: seed each poller's
    // tracked set from the persisted model so its first gone-diff clears
    // sessions the source no longer reports (policy changes, work resolved
    // while we were down).
    let persisted_gastown: Vec<String> = model
        .sessions
        .keys()
        .filter(|k| k.source == pet_proto::Source::Gastown)
        .map(|k| k.session.clone())
        .collect();

    let (input_tx, input_rx) = tokio::sync::mpsc::unbounded_channel();
    let (snapshot_tx, snapshot_rx) =
        tokio::sync::watch::channel(std::sync::Arc::new(pet_proto::Snapshot::default()));
    let (control_tx, control_rx) = tokio::sync::watch::channel(pet_render::Control::default());
    let (ui_tx, mut ui_rx) = tokio::sync::mpsc::unbounded_channel::<pet_proto::UiAction>();
    // Active-window facts need the Model (session metas) to join, so they go
    // to the runtime rather than being mapped inline like other UiActions.
    let (focus_tx, focus_rx) =
        tokio::sync::mpsc::unbounded_channel::<Option<pet_proto::ActiveWindow>>();

    let connection = zbus::connection::Builder::session()?
        .serve_at(
            pet_proto::OBJECT_PATH,
            bus::PetBus::new(
                input_tx.clone(),
                snapshot_rx.clone(),
                control_tx,
                config.gastown.town_dir.to_string_lossy().into_owned(),
            ),
        )?
        .build()
        .await
        .context("connecting to the session bus")?;

    // HARD singleton, layer 2: claim the name non-replaceably and refuse to
    // queue — a racing instance fails instead of stealing the name (which,
    // under Type=dbus, would get THIS instance killed by systemd).
    let reply = connection
        .request_name_with_flags(
            pet_proto::BUS_NAME,
            zbus::fdo::RequestNameFlags::DoNotQueue.into(),
        )
        .await
        .context("requesting the bus name")?;
    if !matches!(
        reply,
        zbus::fdo::RequestNameReply::PrimaryOwner | zbus::fdo::RequestNameReply::AlreadyOwner
    ) {
        anyhow::bail!("another agent-petd owns {}; exiting", pet_proto::BUS_NAME);
    }

    let iface = connection
        .object_server()
        .interface::<_, bus::PetBus>(pet_proto::OBJECT_PATH)
        .await?;

    if config.gastown.enabled() {
        tokio::spawn(gastown::poll_task(
            config.gastown.clone(),
            input_tx.clone(),
            persisted_gastown,
        ));
    }

    // The renderer supervises itself (backoff + panic capture); it can never
    // take the daemon down.
    if !headless {
        let _renderer = pet_render::spawn(snapshot_rx.clone(), control_rx, ui_tx);
    } else {
        drop(control_rx); // Show()/Hide() answer "renderer not running"
        drop(ui_tx);
    }

    // Renderer interactions -> FSM inputs. The FSM turns FocusRequested into
    // Effect::Focus (kitty remote + Hyprland IPC in effects.rs) and marks
    // the session seen.
    let ui_inputs = input_tx.clone();
    tokio::spawn(async move {
        while let Some(action) = ui_rx.recv().await {
            info!(?action, "renderer ui action");
            let input = match action {
                pet_proto::UiAction::FocusSession { key } => {
                    Some(pet_core::Input::FocusRequested(key))
                }
                pet_proto::UiAction::MarkSeen { keys } => {
                    for key in keys {
                        let _ = ui_inputs.send(pet_core::Input::Seen(key));
                    }
                    None
                }
                pet_proto::UiAction::MarkAllSeen => Some(pet_core::Input::SeenAll),
                pet_proto::UiAction::ActiveWindowChanged { window } => {
                    // Joined against the Model in the runtime.
                    let _ = focus_tx.send(window);
                    None
                }
                // Visibility is renderer-owned; settings/quit are later
                // milestones.
                pet_proto::UiAction::SetVisible { .. }
                | pet_proto::UiAction::OpenSettings
                | pet_proto::UiAction::Quit => None,
            };
            if let Some(input) = input {
                let _ = ui_inputs.send(input);
            }
        }
    });

    let runtime = tokio::spawn(runtime::run(
        config,
        model,
        input_rx,
        focus_rx,
        snapshot_tx,
        iface,
    ));

    tokio::signal::ctrl_c().await?;
    info!("shutting down");
    runtime.abort();
    Ok(())
}
