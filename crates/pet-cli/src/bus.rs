//! D-Bus client side: fire-and-forget emission and control calls.

use std::time::Duration;

use anyhow::Context;
use pet_proto::{Event, BUS_NAME, INTERFACE, OBJECT_PATH};

/// Send one event to an already-running daemon; never auto-start it.
/// Emits fire from agent hooks that may run before the graphical session
/// exists — activation there just enqueues doomed start jobs (the unit is
/// Requisite=graphical-session.target). The session starts the daemon.
/// The caller (emit path) treats any error as non-fatal.
pub async fn send_event(event: &Event) -> anyhow::Result<()> {
    let json = serde_json::to_string(event)?;
    // Hard cap the whole exchange: a wedged bus must never stall a hook.
    tokio::time::timeout(Duration::from_millis(500), async {
        let conn = zbus::Connection::session().await?;
        let msg = zbus::message::Message::method_call(OBJECT_PATH, "Emit")?
            .destination(BUS_NAME)?
            .interface(INTERFACE)?
            .with_flags(zbus::message::Flags::NoReplyExpected)?
            .with_flags(zbus::message::Flags::NoAutoStart)?
            .build(&(json.as_str(),))?;
        conn.send(&msg).await?;
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("timed out talking to the session bus")??;
    Ok(())
}

async fn proxy(conn: &zbus::Connection) -> anyhow::Result<zbus::Proxy<'static>> {
    Ok(zbus::Proxy::new(conn, BUS_NAME, OBJECT_PATH, INTERFACE).await?)
}

pub async fn status() -> anyhow::Result<()> {
    let conn = zbus::Connection::session().await?;
    let reply: String = proxy(&conn).await?.call("Status", &()).await?;
    // Re-render compactly for humans; fall back to the raw payload.
    match serde_json::from_str::<pet_proto::Snapshot>(&reply) {
        Ok(snap) => {
            println!("mascot: {}  (unread {})", snap.top.label(), snap.unread);
            for s in &snap.sessions {
                println!(
                    "  [{}{}] {}  {}{}",
                    s.state.label(),
                    if s.seen { "" } else { " *" },
                    s.key,
                    s.via.map(|v| format!("via {v} ")).unwrap_or_default(),
                    s.body.as_deref().unwrap_or(""),
                );
            }
        }
        Err(_) => println!("{reply}"),
    }
    Ok(())
}

pub async fn seen(key: &str) -> anyhow::Result<()> {
    let conn = zbus::Connection::session().await?;
    proxy(&conn).await?.call::<_, _, ()>("MarkSeen", &(key,)).await?;
    Ok(())
}

pub async fn seen_all() -> anyhow::Result<()> {
    let conn = zbus::Connection::session().await?;
    proxy(&conn).await?.call::<_, _, ()>("MarkAllSeen", &()).await?;
    Ok(())
}

pub async fn focus(key: &str) -> anyhow::Result<()> {
    let conn = zbus::Connection::session().await?;
    proxy(&conn).await?.call::<_, _, ()>("Focus", &(key,)).await?;
    Ok(())
}

pub async fn set_visible(visible: bool) -> anyhow::Result<()> {
    let conn = zbus::Connection::session().await?;
    let method = if visible { "Show" } else { "Hide" };
    proxy(&conn).await?.call::<_, _, ()>(method, &()).await?;
    Ok(())
}
