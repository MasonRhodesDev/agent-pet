//! Persisted mascot placement: logical top-left of the *mascot* (not the
//! surface — the bubble area around it is derived) plus visibility.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::warn;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Position {
    #[serde(default)]
    pub output_name: Option<String>,
    pub margin_x: i32,
    pub margin_y: i32,
    #[serde(default = "default_true")]
    pub visible: bool,
}

fn default_true() -> bool {
    true
}

impl Position {
    pub fn state_path() -> PathBuf {
        let state_home = std::env::var("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
                    .join(".local/state")
            });
        state_home.join("agent-pet/position.json")
    }

    pub fn load(path: &Path) -> Option<Self> {
        let raw = std::fs::read_to_string(path).ok()?;
        match serde_json::from_str(&raw) {
            Ok(pos) => Some(pos),
            Err(e) => {
                warn!("ignoring malformed {}: {e}", path.display());
                None
            }
        }
    }

    /// Atomic write-rename; failures are logged, never fatal.
    pub fn save(&self, path: &Path) {
        let write = || -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec_pretty(self).unwrap_or_default())?;
            std::fs::rename(&tmp, path)
        };
        if let Err(e) = write() {
            warn!("could not persist {}: {e}", path.display());
        }
    }

    /// Keep the mascot rect on-screen.
    pub fn clamp(&mut self, out_w: i32, out_h: i32, mascot_w: i32, mascot_h: i32) {
        self.margin_x = self.margin_x.clamp(0, (out_w - mascot_w).max(0));
        self.margin_y = self.margin_y.clamp(0, (out_h - mascot_h).max(0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_via_file() {
        let dir = std::env::temp_dir().join(format!("pet-pos-test-{}", std::process::id()));
        let path = dir.join("position.json");
        let pos = Position {
            output_name: Some("DP-1".into()),
            margin_x: 3100,
            margin_y: 1200,
            visible: false,
        };
        pos.save(&path);
        assert_eq!(Position::load(&path), Some(pos.clone()));
        // Overwrite (exercises the rename path onto an existing file).
        let moved = Position { margin_x: 10, ..pos };
        moved.save(&path);
        assert_eq!(Position::load(&path), Some(moved));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_or_malformed_is_none() {
        assert_eq!(Position::load(Path::new("/nonexistent/position.json")), None);
        let dir = std::env::temp_dir().join(format!("pet-pos-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("position.json");
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(Position::load(&path), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn visible_defaults_true_when_absent() {
        let pos: Position = serde_json::from_str(r#"{"margin_x":1,"margin_y":2}"#).unwrap();
        assert!(pos.visible);
    }

    #[test]
    fn clamps_to_output_bounds() {
        let mut pos = Position {
            output_name: None,
            margin_x: 5000,
            margin_y: -40,
            visible: true,
        };
        pos.clamp(3440, 1440, 128, 128);
        assert_eq!((pos.margin_x, pos.margin_y), (3312, 0));
        // Output smaller than the mascot: pins to 0.
        pos.clamp(100, 100, 128, 128);
        assert_eq!((pos.margin_x, pos.margin_y), (0, 0));
    }
}
