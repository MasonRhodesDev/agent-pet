//! Codex/ChatGPT-desktop-compatible `pet.json` loader. Community pets are
//! drop-in skins.
//!
//! The sheet is a grid of 192x208 cells, always 8 columns. `spriteVersionNumber`
//! (default 1) picks the row count when the manifest omits an explicit `frame`
//! block: v1 = 9 rows (1536x1872), v2 = 11 rows (1536x2288). Row meanings
//! (shared 0-8; v2 adds 9-10):
//!   0 idle | 1 running-right | 2 running-left | 3 waving | 4 jumping |
//!   5 failed | 6 waiting | 7 running | 8 review | 9,10 gaze frames (v2).
//! When `animations` is also omitted the whole animation map is synthesized
//! from that row table with the app's exact frame counts and timings.
//! Custom pets that DO specify `frame`/`animations` keep the explicit path
//! (any grid that exactly tiles the sheet).

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

pub const MAX_FRAMES: usize = 256;
pub const MAX_FPS: f64 = 60.0;
/// Canonical sheet geometry (ChatGPT desktop app).
pub const CELL_W: u32 = 192;
pub const CELL_H: u32 = 208;
pub const CANONICAL_COLS: u32 = 8;
/// Rows per sprite version.
pub const V1_ROWS: u32 = 9;
pub const V2_ROWS: u32 = 11;
/// The app's bespoke idle loop (row 0): a ~6.6s breathing cycle.
pub const IDLE_TIMINGS_MS: [u64; 6] = [1680, 660, 660, 840, 840, 1920];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub sprite_index: usize,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Animation {
    pub frames: Vec<Frame>,
    /// `Some(i)` = wrap to frame `i` after the last frame; `None` = one-shot
    /// that hands off to `fallback`.
    pub loop_start: Option<usize>,
    pub fallback: String,
}

#[derive(Debug, Clone)]
pub struct PetDef {
    pub id: String,
    pub spritesheet_path: PathBuf,
    pub frame_width: u32,
    pub frame_height: u32,
    pub columns: u32,
    pub rows: u32,
    pub animations: HashMap<String, Animation>,
}

impl PetDef {
    pub fn frame_count(&self) -> usize {
        (self.columns * self.rows) as usize
    }

    /// Load from a pet directory (or a direct path to its pet.json).
    pub fn load(path: &Path) -> Result<Self> {
        let dir = if path.is_dir() {
            path.to_path_buf()
        } else {
            path.parent()
                .context("pet.json path has no containing directory")?
                .to_path_buf()
        };
        let manifest = dir.join("pet.json");
        let raw = std::fs::read_to_string(&manifest)
            .with_context(|| format!("read {}", manifest.display()))?;
        let fallback_id = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("pet")
            .to_string();
        let pet = Self::parse(&raw, &dir, &fallback_id)?;
        let (w, h) = image::image_dimensions(&pet.spritesheet_path)
            .with_context(|| format!("read {}", pet.spritesheet_path.display()))?;
        pet.validate_sheet(w, h)?;
        Ok(pet)
    }

    /// Parse and validate everything except the sheet's real dimensions
    /// (`validate_sheet` covers those once the image is known).
    pub fn parse(raw: &str, dir: &Path, fallback_id: &str) -> Result<Self> {
        let file: PetFile = serde_json::from_str(raw).context("parse pet.json")?;
        let version = file.sprite_version.unwrap_or(1);
        // Explicit `frame` wins (arbitrary custom grids); otherwise derive the
        // canonical grid from the sprite version.
        let frame = match file.frame {
            Some(f) => f,
            None => FrameSpec {
                width: CELL_W,
                height: CELL_H,
                columns: CANONICAL_COLS,
                rows: if version == 2 { V2_ROWS } else { V1_ROWS },
            },
        };
        if frame.width == 0 || frame.height == 0 || frame.columns == 0 || frame.rows == 0 {
            bail!("pet frame dimensions and grid counts must be non-zero");
        }
        let frame_count = frame
            .columns
            .checked_mul(frame.rows)
            .map(|n| n as usize)
            .context("pet frame count overflow")?;
        if frame_count > MAX_FRAMES {
            bail!("pet frame count {frame_count} exceeds maximum {MAX_FRAMES}");
        }

        let sheet_name = file
            .spritesheet_path
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .unwrap_or("spritesheet.webp");
        let spritesheet_path = resolve_spritesheet_path(dir, sheet_name)?;

        let id = file
            .id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .unwrap_or(fallback_id)
            .to_string();

        let animations = build_animations(file.animations, &frame, frame_count)?;

        Ok(Self {
            id,
            spritesheet_path,
            frame_width: frame.width,
            frame_height: frame.height,
            columns: frame.columns,
            rows: frame.rows,
            animations,
        })
    }

    pub fn validate_sheet(&self, sheet_w: u32, sheet_h: u32) -> Result<()> {
        let total_w = self
            .frame_width
            .checked_mul(self.columns)
            .context("pet frame grid width overflow")?;
        let total_h = self
            .frame_height
            .checked_mul(self.rows)
            .context("pet frame grid height overflow")?;
        if total_w != sheet_w || total_h != sheet_h {
            bail!(
                "pet frame grid must cover spritesheet exactly: sheet is {sheet_w}x{sheet_h}, \
                 grid covers {total_w}x{total_h}"
            );
        }
        Ok(())
    }
}

/// Resolve the pet directory for this run.
///
/// TODO(v1): unify with pet-daemon's config.rs so `[pet] skin` drives this;
/// the env vars are the v0 stub.
pub fn resolve_pet_dir() -> Result<PathBuf> {
    if let Ok(skin) = std::env::var("AGENT_PET_SKIN") {
        let dir = if skin.contains('/') {
            PathBuf::from(&skin)
        } else {
            config_home().join("agent-pet/pets").join(&skin)
        };
        if dir.join("pet.json").is_file() || dir.is_file() {
            return Ok(dir);
        }
        bail!("AGENT_PET_SKIN={skin} has no pet.json at {}", dir.display());
    }
    for data_dir in data_dirs() {
        let dir = data_dir.join("agent-pet/default-pet");
        if dir.join("pet.json").is_file() {
            return Ok(dir);
        }
    }
    if let Ok(dev) = std::env::var("AGENT_PET_DEFAULT_ASSET") {
        let dir = PathBuf::from(dev);
        if dir.join("pet.json").is_file() {
            return Ok(dir);
        }
    }
    bail!("no pet asset found (install the default pet or set AGENT_PET_DEFAULT_ASSET)")
}

fn config_home() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join(".config"))
}

fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join(".local/share"))];
    let system = std::env::var("XDG_DATA_DIRS")
        .unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(system.split(':').filter(|p| !p.is_empty()).map(PathBuf::from));
    dirs
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()))
}

#[derive(Debug, Deserialize)]
struct PetFile {
    #[serde(default)]
    id: Option<String>,
    #[serde(default, rename = "spriteVersionNumber")]
    sprite_version: Option<u32>,
    #[serde(default, rename = "spritesheetPath")]
    spritesheet_path: Option<String>,
    frame: Option<FrameSpec>,
    #[serde(default)]
    animations: HashMap<String, AnimationSpec>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
struct FrameSpec {
    width: u32,
    height: u32,
    columns: u32,
    rows: u32,
}

#[derive(Debug, Deserialize)]
struct AnimationSpec {
    #[serde(default)]
    frames: Vec<FrameEntry>,
    fps: Option<f64>,
    #[serde(rename = "loop")]
    loop_animation: Option<bool>,
    #[serde(default)]
    fallback: Option<String>,
}

/// Codex accepts bare sprite indices; we additionally accept per-frame
/// timings so hand-authored pets can pace frames individually.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum FrameEntry {
    Index(usize),
    Timed { sprite_index: usize, duration_ms: u64 },
}

fn build_animations(
    specs: HashMap<String, AnimationSpec>,
    grid: &FrameSpec,
    frame_count: usize,
) -> Result<HashMap<String, Animation>> {
    // Seed the code-driven animation map on canonical-shaped grids so pets
    // that omit `animations` (the ChatGPT desktop pets, most community pets)
    // work out of the box; explicit specs below override. Small custom grids
    // get only what they declare + idle.
    let mut animations = if grid.columns >= CANONICAL_COLS && grid.rows >= V1_ROWS {
        code_driven_animations(grid.columns as usize)
    } else {
        HashMap::new()
    };

    for (name, spec) in specs {
        if spec.frames.is_empty() {
            bail!("animation {name} must include at least one frame");
        }
        let fps = match spec.fps {
            Some(fps) if fps.is_finite() && fps > 0.0 && fps <= MAX_FPS => fps,
            Some(fps) => {
                bail!("animation {name} fps must be finite and between 0 and {MAX_FPS}, got {fps}")
            }
            None => 8.0,
        };
        let default_ms = (1000.0 / fps).round() as u64;
        let frames: Vec<Frame> = spec
            .frames
            .into_iter()
            .map(|entry| match entry {
                FrameEntry::Index(sprite_index) => Frame {
                    sprite_index,
                    duration_ms: default_ms,
                },
                FrameEntry::Timed {
                    sprite_index,
                    duration_ms,
                } => Frame {
                    sprite_index,
                    duration_ms,
                },
            })
            .collect();
        let fallback = spec
            .fallback
            .filter(|f| !f.is_empty())
            .unwrap_or_else(|| "idle".to_string());
        let loop_start = spec.loop_animation.unwrap_or(true).then_some(0);
        animations.insert(
            name,
            Animation {
                frames,
                loop_start,
                fallback,
            },
        );
    }

    if !animations.contains_key("idle") {
        animations.insert(
            "idle".to_string(),
            synthesized_idle(grid.columns as usize, frame_count),
        );
    }

    for (name, animation) in &animations {
        for frame in &animation.frames {
            if frame.sprite_index >= frame_count {
                bail!(
                    "animation {name} references sprite index {}, but pet has {frame_count} frames",
                    frame.sprite_index
                );
            }
        }
        if !animations.contains_key(&animation.fallback) {
            bail!(
                "animation {name} fallback {} does not exist",
                animation.fallback
            );
        }
    }
    Ok(animations)
}

fn synthesized_idle(columns: usize, frame_count: usize) -> Animation {
    let n = IDLE_TIMINGS_MS.len().min(columns).min(frame_count).max(1);
    Animation {
        frames: (0..n)
            .map(|i| Frame {
                sprite_index: i,
                duration_ms: IDLE_TIMINGS_MS[i],
            })
            .collect(),
        loop_start: Some(0),
        fallback: "idle".to_string(),
    }
}

/// The ChatGPT desktop app's code-driven animation map (row table with the
/// app's exact frame counts + timings). Each row is `j(N, normalMs, lastMs)`:
/// N frames of `normalMs`, last frame held `lastMs`. All single-pass with
/// `loop_start = 0`; the timeline supplies the play-3x-then-settle burst for
/// non-idle tracks (equivalent to the app baking `[anim;3, ...idle]`).
///
/// Row map: 0 idle | 1 running-right | 2 running-left | 3 waving |
/// 4 jumping | 5 failed | 6 waiting | 7 running | 8 review.
fn code_driven_animations(columns: usize) -> HashMap<String, Animation> {
    let j = |row: usize, count: usize, ms: u64, last_ms: u64| Animation {
        frames: (0..count)
            .map(|col| Frame {
                sprite_index: row * columns + col,
                duration_ms: if col == count - 1 { last_ms } else { ms },
            })
            .collect(),
        loop_start: Some(0),
        fallback: "idle".to_string(),
    };
    [
        ("idle", idle_animation()),
        ("running-right", j(1, 8, 120, 220)),
        ("running-left", j(2, 8, 120, 220)),
        ("waving", j(3, 4, 140, 280)),
        ("jumping", j(4, 5, 140, 280)),
        ("failed", j(5, 8, 140, 240)),
        ("waiting", j(6, 6, 150, 260)),
        ("running", j(7, 6, 120, 220)),
        ("review", j(8, 6, 150, 280)),
    ]
    .into_iter()
    .map(|(name, anim)| (name.to_string(), anim))
    .collect()
}

/// The bespoke row-0 idle loop with the app's exact breathing timings.
fn idle_animation() -> Animation {
    Animation {
        frames: IDLE_TIMINGS_MS
            .iter()
            .enumerate()
            .map(|(i, &duration_ms)| Frame {
                sprite_index: i,
                duration_ms,
            })
            .collect(),
        loop_start: Some(0),
        fallback: "idle".to_string(),
    }
}

/// The manifest may only reference sheets inside its own directory.
fn resolve_spritesheet_path(dir: &Path, name: &str) -> Result<PathBuf> {
    let path = Path::new(name);
    if path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
    {
        bail!("spritesheet path must stay inside {}", dir.display());
    }
    Ok(dir.join(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> Result<PetDef> {
        PetDef::parse(raw, Path::new("/tmp/pet"), "testpet")
    }

    const GRID_8X9_64: &str = r#""frame": {"width":64,"height":64,"columns":8,"rows":9}"#;

    #[test]
    fn accepts_any_exactly_tiling_grid() {
        let pet = parse(&format!(r#"{{ {GRID_8X9_64} }}"#)).unwrap();
        pet.validate_sheet(512, 576).unwrap();
        assert_eq!(pet.frame_count(), 72);
    }

    #[test]
    fn rejects_grid_that_does_not_tile_sheet() {
        let pet = parse(&format!(r#"{{ {GRID_8X9_64} }}"#)).unwrap();
        let err = pet.validate_sheet(512, 640).unwrap_err();
        assert!(err.to_string().contains("cover spritesheet exactly"));
    }

    #[test]
    fn rejects_zero_frame_dimensions() {
        let err = parse(r#"{"frame": {"width":0,"height":64,"columns":8,"rows":9}}"#).unwrap_err();
        assert!(err.to_string().contains("must be non-zero"));
    }

    #[test]
    fn rejects_frame_count_over_cap() {
        let err =
            parse(r#"{"frame": {"width":4,"height":4,"columns":32,"rows":9}}"#).unwrap_err();
        assert!(err.to_string().contains("exceeds maximum 256"));
    }

    #[test]
    fn rejects_sprite_index_outside_grid() {
        let err = parse(&format!(
            r#"{{ {GRID_8X9_64}, "animations": {{"idle": {{"frames": [72]}}}} }}"#
        ))
        .unwrap_err();
        assert!(err.to_string().contains("references sprite index 72"));
    }

    #[test]
    fn rejects_bad_fps() {
        for fps in ["120.0", "0.0", "-2"] {
            let err = parse(&format!(
                r#"{{ {GRID_8X9_64}, "animations": {{"idle": {{"frames": [0], "fps": {fps}}}}} }}"#
            ))
            .unwrap_err();
            assert!(err.to_string().contains("fps must be finite and between"));
        }
    }

    #[test]
    fn rejects_empty_frames() {
        let err = parse(&format!(
            r#"{{ {GRID_8X9_64}, "animations": {{"wave": {{"frames": []}}}} }}"#
        ))
        .unwrap_err();
        assert!(err.to_string().contains("at least one frame"));
    }

    #[test]
    fn rejects_missing_fallback_track() {
        let err = parse(&format!(
            r#"{{ {GRID_8X9_64},
                 "animations": {{"wave": {{"frames": [1], "loop": false, "fallback": "missing"}}}} }}"#
        ))
        .unwrap_err();
        assert!(err.to_string().contains("fallback missing does not exist"));
    }

    #[test]
    fn fps_defaults_to_8() {
        let pet = parse(&format!(
            r#"{{ {GRID_8X9_64}, "animations": {{"idle": {{"frames": [0, 1]}}}} }}"#
        ))
        .unwrap();
        assert_eq!(pet.animations["idle"].frames[0].duration_ms, 125);
    }

    #[test]
    fn loop_defaults_true_and_false_is_one_shot() {
        let pet = parse(&format!(
            r#"{{ {GRID_8X9_64}, "animations": {{
                 "a": {{"frames": [0]}},
                 "b": {{"frames": [1], "loop": false}} }} }}"#
        ))
        .unwrap();
        assert_eq!(pet.animations["a"].loop_start, Some(0));
        assert_eq!(pet.animations["a"].fallback, "idle");
        assert_eq!(pet.animations["b"].loop_start, None);
    }

    #[test]
    fn idle_synthesized_from_row_zero_with_codex_timings() {
        let pet = parse(&format!(r#"{{ {GRID_8X9_64} }}"#)).unwrap();
        let idle = &pet.animations["idle"];
        assert_eq!(
            idle.frames.iter().map(|f| f.sprite_index).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4, 5]
        );
        assert_eq!(
            idle.frames.iter().map(|f| f.duration_ms).collect::<Vec<_>>(),
            IDLE_TIMINGS_MS.to_vec()
        );
        assert_eq!(idle.loop_start, Some(0));
    }

    #[test]
    fn canonical_grid_seeds_semantic_state_tracks() {
        let pet = parse("{}").unwrap();
        // Codex default grid, no animations: rows 5-8 become state tracks.
        assert_eq!(pet.animations["failed"].frames[0].sprite_index, 40);
        assert_eq!(pet.animations["waiting"].frames[0].sprite_index, 48);
        assert_eq!(pet.animations["running"].frames[0].sprite_index, 56);
        assert_eq!(pet.animations["review"].frames[0].sprite_index, 64);
    }

    #[test]
    fn small_grid_gets_only_idle() {
        let pet = parse(r#"{"frame": {"width":16,"height":16,"columns":4,"rows":2}}"#).unwrap();
        assert!(pet.animations.contains_key("idle"));
        assert!(!pet.animations.contains_key("running"));
        // Synthesized idle clamps to the row width.
        assert_eq!(pet.animations["idle"].frames.len(), 4);
    }

    #[test]
    fn frame_entries_accept_bare_and_timed_forms() {
        let pet = parse(&format!(
            r#"{{ {GRID_8X9_64}, "animations": {{"idle": {{"frames":
                 [3, {{"sprite_index": 4, "duration_ms": 900}}], "fps": 10}}}} }}"#
        ))
        .unwrap();
        let idle = &pet.animations["idle"];
        assert_eq!(idle.frames[0], Frame { sprite_index: 3, duration_ms: 100 });
        assert_eq!(idle.frames[1], Frame { sprite_index: 4, duration_ms: 900 });
    }

    #[test]
    fn spritesheet_path_may_not_escape_pet_dir() {
        let err = parse(r#"{"spritesheetPath": "../sheet.png"}"#).unwrap_err();
        assert!(err.to_string().contains("must stay inside"));
    }

    #[test]
    fn v1_grid_derived_when_frame_absent() {
        // No frame, no version -> v1 canonical 8x9.
        let pet = parse(r#"{"id":"x"}"#).unwrap();
        assert_eq!((pet.columns, pet.rows), (8, 9));
        assert_eq!((pet.frame_width, pet.frame_height), (192, 208));
        assert_eq!(pet.frame_count(), 72);
        pet.validate_sheet(1536, 1872).unwrap();
    }

    #[test]
    fn v2_minimal_manifest_derives_8x11_grid_and_gaze_rows() {
        let pet = parse(
            r#"{"id":"fenny-frank","spriteVersionNumber":2,"spritesheetPath":"spritesheet.webp"}"#,
        )
        .unwrap();
        assert_eq!(pet.id, "fenny-frank");
        assert_eq!((pet.columns, pet.rows), (8, 11));
        assert_eq!(pet.frame_count(), 88); // rows 9,10 = gaze frames 72..87
        pet.validate_sheet(1536, 2288).unwrap();
        // The v2 sheet only exact-tiles at 1536x2288.
        assert!(pet.validate_sheet(1536, 1872).is_err());
    }

    #[test]
    fn code_driven_map_has_all_state_and_gesture_tracks_with_app_timings() {
        let pet = parse(r#"{"spriteVersionNumber":2}"#).unwrap();
        for track in [
            "idle",
            "running-right",
            "running-left",
            "waving",
            "jumping",
            "failed",
            "waiting",
            "running",
            "review",
        ] {
            assert!(pet.animations.contains_key(track), "missing {track}");
        }
        // Exact frame counts + timings from the app's row table.
        let waving = &pet.animations["waving"];
        assert_eq!(waving.frames.len(), 4);
        assert_eq!(waving.frames[0].sprite_index, 24); // row 3 * 8
        assert_eq!(
            waving.frames.iter().map(|f| f.duration_ms).collect::<Vec<_>>(),
            vec![140, 140, 140, 280]
        );
        let jumping = &pet.animations["jumping"];
        assert_eq!(jumping.frames.len(), 5);
        assert_eq!(jumping.frames[0].sprite_index, 32); // row 4 * 8
        assert_eq!(jumping.frames[4].duration_ms, 280);
        let walk_r = &pet.animations["running-right"];
        assert_eq!(walk_r.frames.len(), 8);
        assert_eq!(walk_r.frames[0].sprite_index, 8); // row 1 * 8
        assert_eq!(pet.animations["running-left"].frames[0].sprite_index, 16);
        // Idle keeps the bespoke breathing timings.
        assert_eq!(
            pet.animations["idle"]
                .frames
                .iter()
                .map(|f| f.duration_ms)
                .collect::<Vec<_>>(),
            IDLE_TIMINGS_MS.to_vec()
        );
    }

    #[test]
    fn committed_default_asset_loads() {
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/default-pet"));
        let pet = PetDef::load(dir).unwrap();
        assert_eq!(pet.id, "default");
        assert_eq!(pet.frame_count(), 72);
        for track in ["idle", "running", "waiting", "review", "failed"] {
            assert!(pet.animations.contains_key(track), "missing {track}");
        }
        crate::sprite::sheet::Sheet::load(&pet).unwrap();
    }
}
