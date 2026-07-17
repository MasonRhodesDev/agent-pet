//! Pure drag FSM: click-vs-drag threshold + absolute-offset math.
//!
//! Why absolute, not accumulated deltas: `wl_pointer` reports SURFACE-LOCAL
//! coordinates, and a compositor does NOT emit a synthetic motion when a
//! surface slides under a stationary cursor. So any scheme that moves the
//! mascot's own small surface during a drag feeds on corrupted coordinates
//! (the surface origin shifts under the pointer) and the pet trails/snaps.
//!
//! The fix (driven by the renderer, not this module): during a drag the
//! surface is expanded to cover the whole output and held STATIONARY, so
//! surface-local == output coordinates. This FSM then just tracks an
//! absolute offset from a grab point recorded in that stationary space:
//! `pos = start_pos + (pointer - grab)`. No accumulation, no rebasing, no
//! feedback — exact 1:1.
//!
//! Two coordinate spaces are involved and never mixed:
//!   - pre-threshold (`Pressed`): the small docked surface's local coords,
//!     used only for the 4 px threshold test.
//!   - dragging (`Dragging`): the stationary full-output surface's local
//!     coords (== output coords). `grab` is captured lazily from the first
//!     such motion, so the docked→full-output transition never corrupts it.

/// Squared click-vs-drag threshold (~4 px).
const THRESHOLD_SQ: f64 = 16.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Drag {
    Idle,
    /// Button down, not yet past the threshold (docked surface-local coords).
    Pressed { grab: (f64, f64), origin: (i32, i32) },
    /// Past the threshold; the renderer has (or is) switching to the
    /// full-output surface. `grab` is captured from the first full-output
    /// motion so the two coordinate spaces never mix.
    Dragging {
        /// Mascot top-left on the output at grab time.
        start_pos: (i32, i32),
        /// Output-space grab point, established on first drag motion.
        grab: Option<(f64, f64)>,
        /// Current (unclamped) mascot top-left on the output.
        pos: (i32, i32),
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Release {
    Click,
    /// Drag finished — persist the position.
    Dropped,
    None,
}

impl Drag {
    /// Button press. `position` is the mascot's current top-left on the
    /// output; `pointer` is docked surface-local.
    pub fn press(&mut self, pointer: (f64, f64), position: (i32, i32)) {
        *self = Drag::Pressed {
            grab: pointer,
            origin: position,
        };
    }

    /// Pre-drag motion in docked surface-local coords. Returns true exactly
    /// once, when the click-vs-drag threshold is first crossed (transition to
    /// `Dragging`); the caller then switches to the full-output surface.
    pub fn threshold_crossed(&mut self, pointer: (f64, f64)) -> bool {
        if let Drag::Pressed { grab, origin } = *self {
            let (dx, dy) = (pointer.0 - grab.0, pointer.1 - grab.1);
            if dx * dx + dy * dy > THRESHOLD_SQ {
                *self = Drag::Dragging {
                    start_pos: origin,
                    grab: None,
                    pos: origin,
                };
                return true;
            }
        }
        false
    }

    /// Drag motion in the stationary full-output surface's coords (== output
    /// coords). Establishes the grab on first call (sprite does not jump),
    /// then tracks 1:1. Returns the new unclamped mascot top-left, or `None`
    /// when not dragging.
    pub fn drag_to(&mut self, pointer: (f64, f64)) -> Option<(i32, i32)> {
        if let Drag::Dragging {
            start_pos,
            grab,
            pos,
        } = self
        {
            let g = grab.get_or_insert(pointer);
            pos.0 = start_pos.0 + (pointer.0 - g.0).round() as i32;
            pos.1 = start_pos.1 + (pointer.1 - g.1).round() as i32;
            Some(*pos)
        } else {
            None
        }
    }

    /// Current unclamped mascot top-left while dragging.
    pub fn drag_pos(&self) -> Option<(i32, i32)> {
        match self {
            Drag::Dragging { pos, .. } => Some(*pos),
            _ => None,
        }
    }

    pub fn release(&mut self) -> Release {
        let out = match self {
            Drag::Idle => Release::None,
            Drag::Pressed { .. } => Release::Click,
            Drag::Dragging { .. } => Release::Dropped,
        };
        *self = Drag::Idle;
        out
    }

    pub fn dragging(&self) -> bool {
        matches!(self, Drag::Dragging { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_motion_is_a_click() {
        let mut drag = Drag::Idle;
        drag.press((10.0, 10.0), (100, 200));
        assert!(!drag.threshold_crossed((12.0, 12.0))); // sqrt(8) < 4
        assert!(!drag.threshold_crossed((13.0, 12.5))); // sqrt(15.25) < 4
        assert_eq!(drag.drag_pos(), None);
        assert_eq!(drag.release(), Release::Click);
        assert_eq!(drag, Drag::Idle);
    }

    #[test]
    fn exact_threshold_is_still_a_click() {
        let mut drag = Drag::Idle;
        drag.press((0.0, 0.0), (0, 0));
        assert!(!drag.threshold_crossed((4.0, 0.0))); // 16 == 16, not past
        assert_eq!(drag.release(), Release::Click);
    }

    #[test]
    fn threshold_crossing_reports_once_then_switches_to_dragging() {
        let mut drag = Drag::Idle;
        drag.press((10.0, 10.0), (100, 200));
        assert!(drag.threshold_crossed((15.0, 10.0)));
        assert!(drag.dragging());
        // Already dragging: pre-drag motion no longer fires.
        assert!(!drag.threshold_crossed((99.0, 99.0)));
    }

    #[test]
    fn drag_is_absolute_offset_from_a_lazily_captured_grab() {
        let mut drag = Drag::Idle;
        drag.press((10.0, 10.0), (100, 200));
        drag.threshold_crossed((15.0, 10.0)); // start_pos = (100,200)
        // First full-output motion captures the grab; sprite does not jump.
        assert_eq!(drag.drag_to((500.0, 400.0)), Some((100, 200)));
        // Thereafter, pos = start_pos + (pointer - grab), exact 1:1.
        assert_eq!(drag.drag_to((530.0, 380.0)), Some((130, 180)));
        assert_eq!(drag.drag_to((500.0, 400.0)), Some((100, 200)));
        // A stationary pointer holds position exactly — no drift, no snap.
        assert_eq!(drag.drag_to((500.0, 400.0)), Some((100, 200)));
        assert_eq!(drag.drag_to((500.0, 400.0)), Some((100, 200)));
    }

    #[test]
    fn no_feedback_loop_repeated_same_pointer_is_stable() {
        // The old accumulated-delta bug: identical coords must NOT drift.
        let mut drag = Drag::Idle;
        drag.press((0.0, 0.0), (1000, 500));
        drag.threshold_crossed((0.0, 6.0));
        drag.drag_to((2000.0, 1000.0)); // grab here
        for _ in 0..100 {
            assert_eq!(drag.drag_to((2010.0, 1005.0)), Some((1010, 505)));
        }
    }

    #[test]
    fn fractional_pointer_rounds_per_report_without_accumulating() {
        let mut drag = Drag::Idle;
        drag.press((0.0, 0.0), (0, 0));
        drag.threshold_crossed((5.0, 0.0));
        drag.drag_to((100.0, 100.0)); // grab
        assert_eq!(drag.drag_to((103.6, 100.4)), Some((4, 0)));
        assert_eq!(drag.drag_to((100.4, 100.6)), Some((0, 1)));
        assert_eq!(drag.drag_to((100.0, 100.0)), Some((0, 0)));
    }

    #[test]
    fn drag_to_before_threshold_is_none() {
        let mut drag = Drag::Idle;
        assert_eq!(drag.drag_to((50.0, 50.0)), None);
        drag.press((0.0, 0.0), (10, 10));
        assert_eq!(drag.drag_to((50.0, 50.0)), None); // still Pressed
    }

    #[test]
    fn release_semantics() {
        let mut drag = Drag::Idle;
        assert_eq!(drag.release(), Release::None);
        drag.press((0.0, 0.0), (10, 10));
        drag.threshold_crossed((0.0, 9.0));
        assert_eq!(drag.release(), Release::Dropped);
        assert_eq!(drag.drag_pos(), None);
        assert_eq!(drag, Drag::Idle);
    }
}
