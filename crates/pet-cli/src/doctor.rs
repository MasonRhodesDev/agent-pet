//! Installation health checks. Read-only, human-oriented output.

use anyhow::Context;
use pet_proto::{AgentState, Event, Meta, Source, BUS_NAME, INTERFACE, OBJECT_PATH};

pub async fn run() -> anyhow::Result<()> {
    let mut failures = 0u32;
    let mut check = |name: &str, ok: bool, detail: String| {
        let detail = if ok || detail.is_empty() {
            String::new()
        } else {
            format!(" — {detail}")
        };
        println!("{} {name}{detail}", if ok { "✓" } else { "✗" });
        if !ok {
            failures += 1;
        }
    };

    // Daemon reachable (this also exercises bus activation).
    let conn = zbus::Connection::session()
        .await
        .context("session bus unavailable")?;
    let proxy = zbus::Proxy::new(&conn, BUS_NAME, OBJECT_PATH, INTERFACE).await?;
    let status: Result<String, _> = proxy.call("Status", &()).await;
    check(
        "daemon reachable (Status)",
        status.is_ok(),
        status.as_ref().err().map(|e| e.to_string()).unwrap_or_default(),
    );

    // Round-trip a synthetic event and see it reflected in Status.
    let session = format!("doctor-{}", std::process::id());
    let ev = Event {
        v: pet_proto::PROTOCOL_VERSION,
        source: Source::Other,
        session: session.clone(),
        state: AgentState::Running,
        body: Some("doctor probe".into()),
        ts: None,
        via: None,
        meta: Meta::default(),
    };
    let round_trip = async {
        proxy
            .call::<_, _, ()>("Emit", &(serde_json::to_string(&ev)?,))
            .await?;
        let snap: String = proxy.call("Status", &()).await?;
        let snap: pet_proto::Snapshot = serde_json::from_str(&snap)?;
        anyhow::ensure!(
            snap.sessions.iter().any(|s| s.key.session == session),
            "probe session not visible in snapshot"
        );
        // Clean up after ourselves.
        let mut gone = ev.clone();
        gone.state = AgentState::Gone;
        proxy
            .call::<_, _, ()>("Emit", &(serde_json::to_string(&gone)?,))
            .await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    check(
        "event round-trip (Emit → Status)",
        round_trip.is_ok(),
        round_trip.err().map(|e| format!("{e:#}")).unwrap_or_default(),
    );

    // Claude hooks present.
    let claude_settings = home().join(".claude/settings.json");
    let claude = std::fs::read_to_string(&claude_settings).unwrap_or_default();
    let events = [
        "SessionStart",
        "UserPromptSubmit",
        "PostToolUse",
        "Notification",
        "Stop",
        "SessionEnd",
    ];
    let wired: Vec<&str> = events
        .iter()
        .filter(|e| hook_wired(&claude, e))
        .copied()
        .collect();
    check(
        "claude hooks",
        wired.len() == events.len(),
        if wired.len() == events.len() {
            String::new()
        } else {
            format!(
                "{}/{} events wired in {} (run: agent-pet print-config claude)",
                wired.len(),
                events.len(),
                claude_settings.display()
            )
        },
    );

    // Codex taps.
    let hooks_json = std::fs::read_to_string(home().join(".codex/hooks.json")).unwrap_or_default();
    let config_toml =
        std::fs::read_to_string(home().join(".codex/config.toml")).unwrap_or_default();
    check(
        "codex hooks.json",
        hooks_json.contains("agent-pet emit codex"),
        "entry missing (run: agent-pet print-config codex)".into(),
    );
    // permission_request is what makes "needs input" show the actual command
    // being approved; without it those alerts stay generic.
    if hooks_json.contains("agent-pet emit codex") {
        check(
            "codex PermissionRequest hook",
            hooks_json.contains("PermissionRequest"),
            "not wired — needs-input alerts will be generic (run: agent-pet print-config codex)".into(),
        );
    }
    let notify_ok = config_toml
        .lines()
        .take_while(|l| !l.trim_start().starts_with('['))
        .any(|l| l.trim_start().starts_with("notify") && l.contains("agent-pet"));
    check(
        "codex top-level notify",
        notify_ok,
        "missing or below the first [table] header".into(),
    );
    if hooks_json.contains("agent-pet emit codex") {
        check(
            "codex trust gate",
            config_toml.contains("hooks.state"),
            "no [hooks.state] hashes yet — run codex once and approve".into(),
        );
    }

    // Gas Town surface (optional adapter).
    let town = home().join("agent-town/town");
    if town.is_dir() {
        let gt = which("gt");
        let bd = which("bd");
        check(
            "gastown CLIs",
            gt && bd,
            format!("gt: {gt}, bd: {bd}"),
        );
    } else {
        println!("- gastown town dir not present, adapter will stay disabled");
    }

    if failures == 0 {
        println!("\nall checks passed");
        Ok(())
    } else {
        anyhow::bail!("{failures} check(s) failed");
    }
}

/// A hook event counts as wired only if an agent-pet command appears inside
/// that event's block. Cheap textual scoping: from the event key to the next
/// top-level event key.
fn hook_wired(settings: &str, event: &str) -> bool {
    let Some(start) = settings.find(&format!("\"{event}\"")) else {
        return false;
    };
    let rest = &settings[start..];
    let end = rest[1..]
        .find("\n    \"")
        .map(|i| i + 1)
        .unwrap_or(rest.len());
    rest[..end].contains("agent-pet emit claude")
}

fn home() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

fn which(bin: &str) -> bool {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .any(|dir| std::path::Path::new(dir).join(bin).is_file())
}
