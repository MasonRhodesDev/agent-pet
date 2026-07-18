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

/// Pick the gaze sprite index for a cursor at `(dx, dy)` relative to the pet
/// centre (screen coordinates, y pointing down), or `None` inside the
/// deadzone. `columns` is the sheet's column count (canonically 8).
pub fn gaze_index(dx: f64, dy: f64, deadzone_px: f64, columns: usize) -> Option<usize> {
    if !dx.is_finite() || !dy.is_finite() || dx.hypot(dy) <= deadzone_px {
        return None;
    }
    Some(sector_to_index(sector(dx, dy), columns))
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

    // Canonical 8-column sheet: gaze frames are 72..=87.
    #[test]
    fn cardinals_match_the_chatgpt_convention() {
        let dz = 8.0;
        // up (dy negative) → sector 0 → frame 72.
        assert_eq!(gaze_index(0.0, -100.0, dz, 8), Some(72));
        // right → sector 4 → frame 76.
        assert_eq!(gaze_index(100.0, 0.0, dz, 8), Some(76));
        // down → sector 8 → frame 80.
        assert_eq!(gaze_index(0.0, 100.0, dz, 8), Some(80));
        // left → sector 12 → frame 84 (the left-profile cell).
        assert_eq!(gaze_index(-100.0, 0.0, dz, 8), Some(84));
    }

    #[test]
    fn diagonals_land_on_the_between_frames() {
        let dz = 8.0;
        assert_eq!(gaze_index(100.0, -100.0, dz, 8), Some(74)); // up-right, 45° → sector 2
        assert_eq!(gaze_index(100.0, 100.0, dz, 8), Some(78)); // down-right, 135° → sector 6
        assert_eq!(gaze_index(-100.0, 100.0, dz, 8), Some(82)); // down-left, 225° → sector 10
        assert_eq!(gaze_index(-100.0, -100.0, dz, 8), Some(86)); // up-left, 315° → sector 14
    }

    #[test]
    fn deadzone_and_nonfinite_yield_no_gaze() {
        assert_eq!(gaze_index(3.0, 4.0, 5.0, 8), None); // hypot 5 == deadzone
        assert_eq!(gaze_index(0.0, 0.0, 8.0, 8), None);
        assert_eq!(gaze_index(f64::NAN, 10.0, 1.0, 8), None);
    }

    #[test]
    fn wraparound_near_360_stays_up() {
        // A hair counter-clockwise of straight up (≈ -1°) must round back to
        // sector 0, not 16.
        let dz = 1.0;
        assert_eq!(gaze_index(-1.0, -1000.0, dz, 8), Some(72));
    }

    #[test]
    fn every_sector_maps_into_rows_9_and_10() {
        for s in 0..16 {
            let idx = sector_to_index(s, 8);
            let row = idx / 8;
            assert!(row == GAZE_ROW_LO || row == GAZE_ROW_HI, "sector {s} → row {row}");
        }
        // Sweep a full circle: all indices land in 72..=87, no gaps in coverage.
        let mut seen = std::collections::BTreeSet::new();
        for deg in 0..360 {
            let (dx, dy) = ((deg as f64).to_radians().sin(), -(deg as f64).to_radians().cos());
            if let Some(i) = gaze_index(dx * 100.0, dy * 100.0, 1.0, 8) {
                seen.insert(i);
            }
        }
        assert_eq!(seen.iter().min(), Some(&72));
        assert_eq!(seen.iter().max(), Some(&87));
        assert_eq!(seen.len(), 16, "all 16 gaze cells reachable");
    }
}
