//! socket1 `j/activewindow` request + response parsing.
//!
//! Queries are NOT dispatchers, so they behave identically under both the
//! classic and Lua config dialects (only `dispatch`/`keyword` differ). The
//! renderer keeps its own copy of this rather than calling the daemon — it
//! is a separate process and may only share `pet-proto` types.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use anyhow::{Context, Result};
use pet_proto::ActiveWindow;

const IO_TIMEOUT: Duration = Duration::from_millis(500);

pub fn socket1_path() -> Option<String> {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok()?;
    let signature = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    Some(format!("{runtime_dir}/hypr/{signature}/.socket.sock"))
}

/// One `j/activewindow` exchange. Returns the parsed active window, or `None`
/// when nothing is focused (Hyprland replies with an empty JSON object).
pub fn active_window() -> Result<Option<ActiveWindow>> {
    let path = socket1_path().context("not under Hyprland (no signature)")?;
    let reply = request(&path, "j/activewindow")?;
    Ok(parse_active_window(&reply))
}

fn request(path: &str, cmd: &str) -> Result<String> {
    let mut stream = UnixStream::connect(path).with_context(|| format!("connect {path}"))?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    stream.write_all(cmd.as_bytes()).context("write socket1 request")?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply).context("read socket1 reply")?;
    Ok(reply)
}

/// Parse a `j/activewindow` JSON reply into an `ActiveWindow`. Hyprland
/// returns `{}` (or `null`) when no window is focused → `None`. Pure.
pub fn parse_active_window(json: &str) -> Option<ActiveWindow> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let obj = v.as_object()?;
    // Empty object = nothing focused.
    if obj.is_empty() {
        return None;
    }
    let string = |key: &str| -> Option<String> {
        obj.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .filter(|s| !s.is_empty())
    };
    let pid = obj
        .get("pid")
        .and_then(serde_json::Value::as_i64)
        .filter(|p| *p > 0)
        .map(|p| p as u32);
    let window = ActiveWindow {
        pid,
        address: string("address"),
        app_id: string("class"),
        title: string("title"),
    };
    // A reply with no identifying fields at all is as good as no focus.
    if window == ActiveWindow::default() {
        None
    } else {
        Some(window)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_focused_window() {
        let json = r#"{
            "address": "0x5f3a1c0",
            "pid": 4242,
            "class": "kitty",
            "title": "claude — agent-pet",
            "workspace": {"id": 3}
        }"#;
        let w = parse_active_window(json).unwrap();
        assert_eq!(w.pid, Some(4242));
        assert_eq!(w.address.as_deref(), Some("0x5f3a1c0"));
        assert_eq!(w.app_id.as_deref(), Some("kitty"));
        assert_eq!(w.title.as_deref(), Some("claude — agent-pet"));
    }

    #[test]
    fn empty_reply_is_no_focus() {
        assert_eq!(parse_active_window("{}"), None);
        assert_eq!(parse_active_window("null"), None);
        assert_eq!(parse_active_window("not json"), None);
    }

    #[test]
    fn missing_pid_and_blanks_are_dropped() {
        let json = r#"{"address": "0xabc", "class": "", "title": "Firefox", "pid": 0}"#;
        let w = parse_active_window(json).unwrap();
        assert_eq!(w.pid, None); // pid 0 dropped
        assert_eq!(w.app_id, None); // empty class dropped
        assert_eq!(w.address.as_deref(), Some("0xabc"));
        assert_eq!(w.title.as_deref(), Some("Firefox"));
    }
}
