//! Cursor-follow gaze: pick the 16-direction stare frame for a v2 pet.
//!
//! Convention reverse-engineered from the ChatGPT desktop pet
//! (`avatar-overlay-pill-material` module). The angle to the cursor is
//! measured *clockwise from straight up* via `atan2(dx, -dy)`, quantized to
//! 16 sectors of 22.5°. Sectors 0..=7 map to sprite row 9 (columns 0..=7),
//! sectors 8..=15 to row 10 — so on the canonical 8-column sheet the frame
//! index is simply `row9_base + sector` (72..=87). A deadzone around the pet
//! centre yields `None` (look straight ahead / resume idle).
//!
//! Pure and compositor-agnostic: it only needs a delta in screen pixels
//! (y-down). Where that delta comes from — a Hyprland `cursorpos` poll today,
//! some other backend tomorrow — is decided elsewhere.

/// The two spritesheet rows that hold the gaze frames (v2 pets only).
pub const GAZE_ROW_LO: usize = 9;
pub const GAZE_ROW_HI: usize = 10;

const SECTORS: i32 = 16;
const SECTOR_DEG: f64 = 360.0 / SECTORS as f64; // 22.5°
const PER_ROW: i32 = 8; // gaze always uses 8 cells per row, regardless of sheet width

/// A resolved gaze frame: which sprite cell to draw, and whether to mirror it
/// horizontally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GazeFrame {
    pub index: usize,
    pub flip_h: bool,
}

/// Pick the gaze frame for a cursor at `(dx, dy)` relative to the pet centre
/// (screen coordinates, y pointing down), or `None` inside the deadzone.
/// `columns` is the sheet's column count (canonically 8).
///
/// Only the up→right→down half is stored as art (sectors 0..=8, cells
/// 72..=80). The left half (sectors 9..=15) is those same frames **mirrored**:
/// sector `s` reuses source sector `16 - s` flipped, so looking left is
/// looking right flipped. Perfectly symmetric, and half the art to author.
pub fn gaze_frame(dx: f64, dy: f64, deadzone_px: f64, columns: usize) -> Option<GazeFrame> {
    if !dx.is_finite() || !dy.is_finite() || dx.hypot(dy) <= deadzone_px {
        return None;
    }
    let s = sector(dx, dy);
    let (src, flip_h) = if s > SECTORS / 2 {
        (SECTORS - s, true) // left half mirrors the right half
    } else {
        (s, false) // up (0), right side (1..=7), down (8)
    };
    Some(GazeFrame {
        index: sector_to_index(src, columns),
        flip_h,
    })
}

/// Sector 0..=15, clockwise from straight up (0 = up, 4 = right, 8 = down,
/// 12 = left).
fn sector(dx: f64, dy: f64) -> i32 {
    // atan2(dx, -dy): up→0°, right→90°, down→180°, left→270°.
    let deg = (dx.atan2(-dy).to_degrees() + 360.0) % 360.0;
    (((deg / SECTOR_DEG).round() as i32) % SECTORS + SECTORS) % SECTORS
}

fn sector_to_index(sector: i32, columns: usize) -> usize {
    let row = GAZE_ROW_LO + (sector / PER_ROW) as usize;
    let col = (sector % PER_ROW) as usize;
    row * columns + col
}

#[cfg(test)]
mod tests {
    use super::*;

    // Canonical 8-column sheet: source gaze frames are 72..=80; the left half
    // reuses 73..=79 mirrored.
    #[test]
    fn cardinals_and_the_left_mirror() {
        let dz = 8.0;
        let f = |dx, dy| gaze_frame(dx, dy, dz, 8).unwrap();
        // up → cell 72, no flip.
        assert_eq!(f(0.0, -100.0), GazeFrame { index: 72, flip_h: false });
        // right → cell 76, no flip.
        assert_eq!(f(100.0, 0.0), GazeFrame { index: 76, flip_h: false });
        // down → cell 80, no flip.
        assert_eq!(f(0.0, 100.0), GazeFrame { index: 80, flip_h: false });
        // left → the RIGHT cell 76, flipped (not a separate cell 84).
        assert_eq!(f(-100.0, 0.0), GazeFrame { index: 76, flip_h: true });
    }

    #[test]
    fn diagonals_reuse_right_frames_on_the_left() {
        let dz = 8.0;
        let f = |dx, dy| gaze_frame(dx, dy, dz, 8).unwrap();
        assert_eq!(f(100.0, -100.0), GazeFrame { index: 74, flip_h: false }); // up-right, sector 2
        assert_eq!(f(100.0, 100.0), GazeFrame { index: 78, flip_h: false }); // down-right, sector 6
        // down-left (sector 10) mirrors down-right (sector 6) → cell 78 flipped.
        assert_eq!(f(-100.0, 100.0), GazeFrame { index: 78, flip_h: true });
        // up-left (sector 14) mirrors up-right (sector 2) → cell 74 flipped.
        assert_eq!(f(-100.0, -100.0), GazeFrame { index: 74, flip_h: true });
    }

    #[test]
    fn deadzone_and_nonfinite_yield_no_gaze() {
        assert_eq!(gaze_frame(3.0, 4.0, 5.0, 8), None); // hypot 5 == deadzone
        assert_eq!(gaze_frame(0.0, 0.0, 8.0, 8), None);
        assert_eq!(gaze_frame(f64::NAN, 10.0, 1.0, 8), None);
    }

    #[test]
    fn wraparound_near_360_stays_up() {
        // A hair counter-clockwise of straight up (≈ -1°) must round back to
        // sector 0, not 16.
        assert_eq!(
            gaze_frame(-1.0, -1000.0, 1.0, 8),
            Some(GazeFrame { index: 72, flip_h: false })
        );
    }

    #[test]
    fn only_source_cells_72_to_80_are_used_and_cover_the_circle() {
        // Sweep a full circle: every frame is one of the 9 source cells
        // 72..=80, and the left half is served by flips.
        let mut used = std::collections::BTreeSet::new();
        let mut saw_flip = false;
        for deg in 0..360 {
            let (dx, dy) = ((deg as f64).to_radians().sin(), -(deg as f64).to_radians().cos());
            if let Some(g) = gaze_frame(dx * 100.0, dy * 100.0, 1.0, 8) {
                assert!((72..=80).contains(&g.index), "index {} out of source range", g.index);
                let row = g.index / 8;
                assert!(row == GAZE_ROW_LO || row == GAZE_ROW_HI);
                used.insert(g.index);
                saw_flip |= g.flip_h;
            }
        }
        assert_eq!(used.iter().min(), Some(&72));
        assert_eq!(used.iter().max(), Some(&80));
        assert!(saw_flip, "left-half directions must use flipped frames");
    }
}
