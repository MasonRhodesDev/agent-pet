//! Persisted mascot placement: **global** logical top-left of the *mascot*
//! (not the surface — the bubble area around it is derived) plus visibility.
//! Global means the compositor's multi-output logical coordinate space;
//! per-output margins are derived by subtracting the output's origin.
//!
//! Older builds persisted an output-local shape (`{output_name, margin_x,
//! margin_y}`); those files load as [`Loaded::Legacy`] and are migrated to a
//! global point once output geometry is known.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::surface::outputs::OutputRect;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub x: i32,
    pub y: i32,
    #[serde(default = "default_true")]
    pub visible: bool,
}

/// The pre-multi-monitor on-disk shape: margins local to a (named) output.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LegacyPosition {
    #[serde(default)]
    pub output_name: Option<String>,
    pub margin_x: i32,
    pub margin_y: i32,
    #[serde(default = "default_true")]
    pub visible: bool,
}

/// What a state file parsed as. Legacy files cannot be migrated at load time
/// (output rects arrive later, over the wire) — the caller holds the legacy
/// value and retries [`LegacyPosition::migrate`] as geometry comes in.
#[derive(Debug, Clone, PartialEq)]
pub enum Loaded {
    Global(Position),
    Legacy(LegacyPosition),
    None,
}

fn default_true() -> bool {
    true
}

impl LegacyPosition {
    /// Lift output-local margins into a global point: the origin of the
    /// output whose name was recorded (else the first output) plus the
    /// margins. `None` while no output geometry is known yet.
    pub fn migrate(&self, outputs: &[OutputRect]) -> Option<Position> {
        let out = outputs
            .iter()
            .find(|o| Some(o.name.as_str()) == self.output_name.as_deref())
            .or_else(|| outputs.first())?;
        Some(Position {
            x: out.x + self.margin_x,
            y: out.y + self.margin_y,
            visible: self.visible,
        })
    }
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

    pub fn load(path: &Path) -> Loaded {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Loaded::None;
        };
        // The global shape requires `x`/`y`, the legacy one `margin_x`/
        // `margin_y` — the formats are disjoint, so try in order.
        if let Ok(pos) = serde_json::from_str::<Position>(&raw) {
            return Loaded::Global(pos);
        }
        match serde_json::from_str::<LegacyPosition>(&raw) {
            Ok(legacy) => Loaded::Legacy(legacy),
            Err(e) => {
                warn!("ignoring malformed {}: {e}", path.display());
                Loaded::None
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(name: &str, x: i32, y: i32, w: i32, h: i32) -> OutputRect {
        OutputRect {
            name: name.into(),
            x,
            y,
            w,
            h,
        }
    }

    #[test]
    fn round_trips_via_file() {
        let dir = std::env::temp_dir().join(format!("pet-pos-test-{}", std::process::id()));
        let path = dir.join("position.json");
        let pos = Position {
            x: 3100,
            y: 1200,
            visible: false,
        };
        pos.save(&path);
        assert_eq!(Position::load(&path), Loaded::Global(pos.clone()));
        // Overwrite (exercises the rename path onto an existing file).
        let moved = Position { x: 10, ..pos };
        moved.save(&path);
        assert_eq!(Position::load(&path), Loaded::Global(moved));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_missing_or_malformed_is_none() {
        assert_eq!(
            Position::load(Path::new("/nonexistent/position.json")),
            Loaded::None
        );
        let dir = std::env::temp_dir().join(format!("pet-pos-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("position.json");
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(Position::load(&path), Loaded::None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn visible_defaults_true_when_absent() {
        let pos: Position = serde_json::from_str(r#"{"x":1,"y":2}"#).unwrap();
        assert!(pos.visible);
    }

    #[test]
    fn legacy_file_parses_as_legacy() {
        let dir = std::env::temp_dir().join(format!("pet-pos-legacy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("position.json");
        std::fs::write(
            &path,
            r#"{"output_name":"DP-1","margin_x":3100,"margin_y":1200,"visible":false}"#,
        )
        .unwrap();
        assert_eq!(
            Position::load(&path),
            Loaded::Legacy(LegacyPosition {
                output_name: Some("DP-1".into()),
                margin_x: 3100,
                margin_y: 1200,
                visible: false,
            })
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrate_adds_the_named_outputs_origin() {
        let outs = vec![
            rect("DP-1", 0, 0, 3440, 1440),
            rect("DP-2", 3440, 0, 1920, 1080),
        ];
        let legacy = LegacyPosition {
            output_name: Some("DP-2".into()),
            margin_x: 100,
            margin_y: 200,
            visible: false,
        };
        assert_eq!(
            legacy.migrate(&outs),
            Some(Position {
                x: 3540,
                y: 200,
                visible: false
            })
        );
    }

    #[test]
    fn migrate_falls_back_to_the_first_output() {
        let outs = vec![rect("DP-1", 100, 50, 3440, 1440)];
        // Unknown name and no name at all both land on the first output.
        for name in [Some("gone".to_string()), None] {
            let legacy = LegacyPosition {
                output_name: name,
                margin_x: 10,
                margin_y: 20,
                visible: true,
            };
            assert_eq!(
                legacy.migrate(&outs),
                Some(Position {
                    x: 110,
                    y: 70,
                    visible: true
                })
            );
        }
    }

    #[test]
    fn migrate_waits_for_outputs() {
        let legacy = LegacyPosition {
            output_name: None,
            margin_x: 10,
            margin_y: 20,
            visible: true,
        };
        assert_eq!(legacy.migrate(&[]), None);
    }
}
