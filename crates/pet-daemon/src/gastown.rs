//! Gas Town poll adapter: supervised in-daemon task. The mapping logic is
//! pure (pet_adapters::gastown); this module only runs the CLIs and feeds
//! events into the runtime.

use std::process::Stdio;
use std::time::Duration;

use pet_adapters::gastown::{parse_lenient_list, Rig, TownObservation};
use pet_core::Input;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::config::GastownConfig;

const SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn poll_task(
    config: GastownConfig,
    inputs: mpsc::UnboundedSender<Input>,
    persisted_sessions: Vec<String>,
) {
    info!(town = %config.town_dir.display(), every = config.poll_secs, "gastown poller starting");
    // Seeding from the persisted model makes the first gone-diff clear
    // sessions this poller no longer reports (e.g. after a policy change).
    let mut previous: Vec<String> = persisted_sessions;
    let mut escalations = pet_adapters::gastown::EscalationTracker::default();
    let mut backoff_polls = 0u32;

    loop {
        let jitter = (std::process::id() % 5) as u64;
        let wait = config.poll_secs * (1 + backoff_polls as u64 * 9) + jitter;
        tokio::time::sleep(Duration::from_secs(wait)).await;

        match observe(&config).await {
            Ok(obs) => {
                backoff_polls = 0;
                let (events, tracked) = pet_adapters::gastown::poll_step(
                    &previous,
                    &obs,
                    config.include_polecats,
                    &mut escalations,
                );
                previous = tracked;
                for event in events {
                    let stamped = match event.validate(crate::bus::now_ms()) {
                        Ok(ev) => ev,
                        Err(e) => {
                            warn!("gastown produced an invalid event: {e}");
                            continue;
                        }
                    };
                    if inputs.send(Input::Event(stamped)).is_err() {
                        return; // runtime is gone
                    }
                }
            }
            Err(e) => {
                // Missing binaries / stopped town: degrade quietly, retry at
                // 10x the interval.
                if backoff_polls == 0 {
                    warn!("gastown poll failed ({e:#}); backing off");
                }
                backoff_polls = 1;
            }
        }
    }
}

/// One sweep of the town. The rig listing is the only hard requirement;
/// every other probe degrades to "not observed this poll".
async fn observe(config: &GastownConfig) -> anyhow::Result<TownObservation> {
    let rigs_json = run_cli(config, "gt", &["rig", "list", "--json"]).await?;
    let rigs: Vec<Rig> = parse_lenient_list(&rigs_json)?;

    let mut obs = TownObservation::default();

    obs.mayor_running = match run_cli(config, "gt", &["mayor", "status", "--running"]).await {
        Ok(out) => match out.trim() {
            "true" => Some(true),
            "false" => Some(false),
            other => {
                debug!("unexpected mayor probe output {other:?}");
                None
            }
        },
        Err(e) => {
            debug!("mayor probe failed: {e:#}");
            None
        }
    };

    for rig in rigs.iter().filter(|r| r.crew > 0) {
        match run_cli(config, "gt", &["crew", "list", &rig.name, "--json"]).await {
            Ok(json) => match parse_lenient_list(&json) {
                Ok(crew) => obs.crew.extend(crew),
                Err(e) => warn!("crew list unparseable for {}: {e}", rig.name),
            },
            Err(e) => debug!("crew list failed for {}: {e:#}", rig.name),
        }
    }

    // Narrow at the source to genuine escalations; the pure adapter
    // (EscalationTracker) enforces the same label filter and is unit-tested.
    match run_cli(
        config,
        "bd",
        &["list", "--json", "--assignee=overseer", "--label=gt:escalation"],
    )
    .await
    {
        Ok(json) => match parse_lenient_list(&json) {
            Ok(beads) => obs.escalations = beads,
            Err(e) => warn!("escalation list unparseable: {e}"),
        },
        Err(e) => debug!("escalation list failed: {e:#}"),
    }

    if config.include_polecats && rigs.iter().any(|r| r.polecats > 0) {
        match run_cli(config, "gt", &["polecat", "list", "--all", "--json"]).await {
            Ok(json) => match parse_lenient_list(&json) {
                Ok(polecats) => obs.polecats = polecats,
                Err(e) => warn!("polecat list unparseable: {e}"),
            },
            Err(e) => debug!("polecat list failed: {e:#}"),
        }
    }

    Ok(obs)
}

async fn run_cli(config: &GastownConfig, bin: &str, args: &[&str]) -> anyhow::Result<String> {
    let child = tokio::process::Command::new(bin)
        .args(args)
        .current_dir(&config.town_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(SUBPROCESS_TIMEOUT, child)
        .await
        .map_err(|_| anyhow::anyhow!("{bin} timed out"))??;
    anyhow::ensure!(
        out.status.success(),
        "{bin} exited {}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8(out.stdout)?)
}
