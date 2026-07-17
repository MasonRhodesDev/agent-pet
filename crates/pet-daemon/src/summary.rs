//! Optional enrichment from the local pane-summarizer: a coarse "what is this
//! session about" tagline. Best-effort — absence is normal (the summarizer is
//! a separate, optional daemon on some machines).
//!
//! Summaries live at `$XDG_RUNTIME_DIR/wezterm-pane-summary/pane-<key>.json`,
//! keyed by pane (`kitty-<pid>-<wid>`) with a `paneTty` fallback join.

use std::path::PathBuf;

use pet_proto::Snapshot;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct PaneSummary {
    #[serde(default)]
    summary: Option<String>,
    #[serde(default, rename = "paneTty")]
    pane_tty: Option<String>,
    #[serde(default)]
    active: bool,
}

fn summary_dir() -> PathBuf {
    let base = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(base).join("wezterm-pane-summary")
}

/// Fill each session's `subtitle` from its pane summary, and use it as a
/// last-resort `body` when the session has no specific caption yet. Cheap:
/// one small file read per session that has a pane/tty.
pub fn decorate(snapshot: &mut Snapshot) {
    let dir = summary_dir();
    if !dir.is_dir() {
        return;
    }
    for view in &mut snapshot.sessions {
        let Some(summary) = lookup(&dir, view.meta.pane.as_deref(), view.meta.tty.as_deref())
        else {
            continue;
        };
        if view.body.is_none() {
            view.body = Some(summary.clone());
        }
        view.subtitle = Some(summary);
    }
}

/// Read the summary for a pane key, falling back to a tty match across the
/// directory. Returns the trimmed summary text if present and non-empty.
fn lookup(dir: &std::path::Path, pane: Option<&str>, tty: Option<&str>) -> Option<String> {
    // Fast path: the pane-keyed file.
    if let Some(pane) = pane {
        if let Some(s) = read_summary(&dir.join(format!("pane-{pane}.json"))) {
            return Some(s);
        }
    }
    // Fallback: scan for a file whose paneTty matches (handles sessions with
    // a tty but no kitty pane, e.g. tmux-hosted).
    let tty = tty?;
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(s) = serde_json::from_str::<PaneSummary>(&text) else {
            continue;
        };
        if s.pane_tty.as_deref() == Some(tty) {
            return clean(s.summary);
        }
    }
    None
}

fn read_summary(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let s: PaneSummary = serde_json::from_str(&text).ok()?;
    // A stale/cleared pane summary marks itself inactive.
    if !s.active {
        return None;
    }
    clean(s.summary)
}

fn clean(summary: Option<String>) -> Option<String> {
    let s = summary?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(dir: &std::path::Path, name: &str, json: &str) {
        let mut f = std::fs::File::create(dir.join(name)).unwrap();
        f.write_all(json.as_bytes()).unwrap();
    }

    #[test]
    fn lookup_by_pane_then_tty() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(
            dir,
            "pane-kitty-100-1.json",
            r#"{"summary":"getting started","paneTty":"/dev/pts/17","active":true}"#,
        );
        write(
            dir,
            "pane-kitty-200-1.json",
            r#"{"summary":"tmux work","paneTty":"/dev/pts/9","active":true}"#,
        );

        // Direct pane hit.
        assert_eq!(
            lookup(dir, Some("kitty-100-1"), None).as_deref(),
            Some("getting started")
        );
        // Fallback by tty when the pane key is unknown.
        assert_eq!(
            lookup(dir, Some("kitty-999-9"), Some("/dev/pts/9")).as_deref(),
            Some("tmux work")
        );
        // Neither matches.
        assert_eq!(lookup(dir, Some("kitty-999-9"), Some("/dev/pts/1")), None);
    }

    #[test]
    fn inactive_and_empty_summaries_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        write(dir, "pane-kitty-1-1.json", r#"{"summary":"stale","active":false}"#);
        write(dir, "pane-kitty-2-1.json", r#"{"summary":"  ","active":true}"#);
        assert_eq!(lookup(dir, Some("kitty-1-1"), None), None);
        assert_eq!(lookup(dir, Some("kitty-2-1"), None), None);
    }
}
