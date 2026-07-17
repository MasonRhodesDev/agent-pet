//! Pure drag FSM: click-vs-drag threshold + coalesced delta-margins math.
//!
//! Motion events only stash the latest surface-local position; the surface
//! actually moves once per compositor frame callback via [`Drag::take_pending`],
//! which measures the stash against the LAST APPLIED position and rebases.
//! Applying margins per motion event instead double-counts movement the
//! compositor has not committed yet (margin commits land asynchronously),
//! so the pet drifts away from the pointer.

/// Squared click-vs-drag threshold (~4 px).
const THRESHOLD_SQ: f64 = 16.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Drag {
    Idle,
    /// Button down, not yet past the threshold.
    Pressed {
        grab: (f64, f64),
        origin: (i32, i32),
        latest: (f64, f64),
    },
    Dragging {
        grab: (f64, f64),
        /// Last position handed out by `take_pending`.
        applied: (i32, i32),
        latest: (f64, f64),
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
    /// `position` is the mascot's current logical top-left on the output.
    pub fn press(&mut self, pointer: (f64, f64), position: (i32, i32)) {
        *self = Drag::Pressed {
            grab: pointer,
            origin: position,
            latest: pointer,
        };
    }

    /// Stash the latest pointer position. Returns true exactly once, when
    /// the threshold is first crossed — the caller starts the frame-callback
    /// apply loop then.
    pub fn motion(&mut self, pointer: (f64, f64)) -> bool {
        match *self {
            Drag::Idle => false,
            Drag::Pressed { grab, origin, .. } => {
                let (dx, dy) = (pointer.0 - grab.0, pointer.1 - grab.1);
                if dx * dx + dy * dy <= THRESHOLD_SQ {
                    *self = Drag::Pressed {
                        grab,
                        origin,
                        latest: pointer,
                    };
                    return false;
                }
                *self = Drag::Dragging {
                    grab,
                    applied: origin,
                    latest: pointer,
                };
                true
            }
            Drag::Dragging { grab, applied, .. } => {
                *self = Drag::Dragging {
                    grab,
                    applied,
                    latest: pointer,
                };
                false
            }
        }
    }

    /// Consume the coalesced delta: the next position to apply, measured
    /// against the last applied one. Rebases the stash so the same movement
    /// is never applied twice (after the surface moves, a stationary pointer
    /// reads back as the grab point).
    pub fn take_pending(&mut self) -> Option<(i32, i32)> {
        match self {
            Drag::Dragging {
                grab,
                applied,
                latest,
            } => {
                let dx = (latest.0 - grab.0).round() as i32;
                let dy = (latest.1 - grab.1).round() as i32;
                applied.0 += dx;
                applied.1 += dy;
                *latest = *grab;
                Some(*applied)
            }
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
        assert!(!drag.motion((12.0, 12.0))); // sqrt(8) < 4
        assert!(!drag.motion((13.0, 12.5))); // sqrt(15.25) < 4
        assert_eq!(drag.take_pending(), None);
        assert_eq!(drag.release(), Release::Click);
        assert_eq!(drag, Drag::Idle);
    }

    #[test]
    fn exact_threshold_is_still_a_click() {
        let mut drag = Drag::Idle;
        drag.press((0.0, 0.0), (0, 0));
        assert!(!drag.motion((4.0, 0.0))); // 16 == 16, not past
        assert_eq!(drag.release(), Release::Click);
    }

    #[test]
    fn threshold_crossing_reports_drag_start_once() {
        let mut drag = Drag::Idle;
        drag.press((10.0, 10.0), (100, 200));
        assert!(drag.motion((15.0, 10.0)));
        assert!(!drag.motion((20.0, 10.0)));
        assert!(drag.dragging());
    }

    #[test]
    fn motions_between_callbacks_coalesce_to_one_net_delta() {
        let mut drag = Drag::Idle;
        drag.press((10.0, 10.0), (100, 200));
        drag.motion((15.0, 10.0));
        drag.motion((20.0, 14.0));
        drag.motion((25.0, 12.0)); // net delta from grab: (+15, +2)
        assert_eq!(drag.take_pending(), Some((115, 202)));
        // No motion since: the stash was rebased, the delta must be zero —
        // this is the double-count regression guard.
        assert_eq!(drag.take_pending(), Some((115, 202)));
        assert_eq!(drag.take_pending(), Some((115, 202)));
    }

    #[test]
    fn post_apply_motions_measure_from_the_new_position() {
        let mut drag = Drag::Idle;
        drag.press((10.0, 10.0), (100, 200));
        drag.motion((30.0, 10.0));
        assert_eq!(drag.take_pending(), Some((120, 200)));
        // Surface moved +20 under a pointer that kept moving right: the
        // compositor now reports coordinates in the moved surface's space.
        drag.motion((15.0, 10.0)); // 5 right of grab in the new space
        assert_eq!(drag.take_pending(), Some((125, 200)));
        // Stationary pointer reads back exactly the grab point: no drift.
        drag.motion((10.0, 10.0));
        assert_eq!(drag.take_pending(), Some((125, 200)));
    }

    #[test]
    fn fractional_deltas_round_per_apply() {
        let mut drag = Drag::Idle;
        drag.press((0.0, 0.0), (0, 0));
        drag.motion((10.0, 0.0));
        drag.motion((3.6, 0.4)); // stash overwrite: net (+3.6, +0.4)
        assert_eq!(drag.take_pending(), Some((4, 0)));
        // Compositor reports the sub-pixel residue after the move.
        drag.motion((-0.4, 0.4));
        assert_eq!(drag.take_pending(), Some((4, 0)));
    }

    #[test]
    fn release_semantics() {
        let mut drag = Drag::Idle;
        assert!(!drag.motion((50.0, 50.0)));
        assert_eq!(drag.release(), Release::None);
        drag.press((0.0, 0.0), (10, 10));
        drag.motion((9.0, 0.0));
        assert_eq!(drag.release(), Release::Dropped);
        assert_eq!(drag.take_pending(), None);
    }
}
