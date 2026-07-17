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
//!     coords (== output coords).
//!
//! CRITICAL — the coordinate-space transition is an EXPLICIT FSM state, not a
//! timing check. The docked -> full-output surface swap is asynchronous, so a
//! motion event still carrying docked-local coords (~0..280) can arrive after
//! the swap is requested. If such a motion established `grab` while `start_pos`
//! is output-absolute (~3160), every subsequent output-absolute pointer would
//! be offset by ~3000px and the sprite would pin to a screen edge until the
//! cursor moved back across it.
//!
//! `Dragging` therefore carries `armed`: false until the caller confirms the
//! full-output surface is live (`arm()`, wired to the resize configure ack),
//! true after. `drag_to` DROPS every pre-arm motion (no grab, no position
//! change); `grab` is established from the FIRST post-arm motion, which is
//! guaranteed to be in output-absolute space. It is structurally impossible
//! for a docked-local coordinate to seed `grab`.

/// Squared click-vs-drag threshold (~4 px).
const THRESHOLD_SQ: f64 = 16.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Drag {
    Idle,
    /// Button down, not yet past the threshold (docked surface-local coords).
    Pressed { grab: (f64, f64), origin: (i32, i32) },
    /// Past the threshold; the surface is (being) expanded to full output.
    Dragging {
        /// Full-output surface confirmed live (coords are output-absolute).
        /// Set by `arm()` on the resize configure ack. Pre-arm motions are
        /// dropped so `grab` can only come from output-absolute space.
        armed: bool,
        /// Established from the FIRST post-arm motion, then fixed. `None`
        /// until then.
        grab: Option<(f64, f64)>,
        /// Mascot top-left on the output at grab time.
        start_pos: (i32, i32),
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
    /// once, when the click-vs-drag threshold is first crossed: transitions to
    /// `Dragging` in the NOT-yet-armed state. The caller then expands the
    /// surface to full output and calls `arm()` when its configure is acked.
    pub fn threshold_crossed(&mut self, pointer: (f64, f64)) -> bool {
        if let Drag::Pressed { grab, origin } = *self {
            let (dx, dy) = (pointer.0 - grab.0, pointer.1 - grab.1);
            if dx * dx + dy * dy > THRESHOLD_SQ {
                *self = Drag::Dragging {
                    armed: false,
                    grab: None,
                    start_pos: origin,
                    pos: origin,
                };
                return true;
            }
        }
        false
    }

    /// Confirm the full-output surface is live (its resize configure was
    /// acked): subsequent pointer coords are output-absolute, so motions may
    /// now establish `grab`. No-op unless dragging.
    pub fn arm(&mut self) {
        if let Drag::Dragging { armed, .. } = self {
            *armed = true;
        }
    }

    pub fn armed(&self) -> bool {
        matches!(self, Drag::Dragging { armed: true, .. })
    }

    /// Drag motion in the full-output surface. While NOT armed, the motion is
    /// DROPPED (returns `None`, no grab, no position change) — this is what
    /// makes a docked-local coordinate unable to seed `grab`. Once armed, the
    /// first motion establishes `grab` (from that output-absolute pointer) and
    /// thereafter `pos = start_pos + (pointer - grab)`, exact 1:1.
    pub fn drag_to(&mut self, pointer: (f64, f64)) -> Option<(i32, i32)> {
        if let Drag::Dragging {
            armed: true,
            grab,
            start_pos,
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

    /// (grab, start_pos) for diagnostics; both `None` unless dragging (and
    /// grab is `None` until the first post-arm motion).
    pub fn debug_grab_start(&self) -> (Option<(f64, f64)>, Option<(i32, i32)>) {
        match self {
            Drag::Dragging {
                grab, start_pos, ..
            } => (*grab, Some(*start_pos)),
            _ => (None, None),
        }
    }
}

/// Which way the pet "walks" while being dragged, matching the app's
/// per-move deltaX sign with a 4px hysteresis so it doesn't flicker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkDir {
    Left,
    Right,
}

impl WalkDir {
    /// The looping animation track for this walk direction.
    pub fn track(self) -> &'static str {
        match self {
            WalkDir::Left => "running-left",
            WalkDir::Right => "running-right",
        }
    }
}

/// Tracks the drag's horizontal travel and decides the walk direction.
#[derive(Debug, Clone, Copy)]
pub struct Walk {
    ref_x: i32,
    dir: Option<WalkDir>,
}

/// Net horizontal move (px) before the walk direction flips.
const WALK_THRESHOLD: i32 = 4;

impl Walk {
    pub fn new(start_x: i32) -> Self {
        Self {
            ref_x: start_x,
            dir: None,
        }
    }

    /// Feed the current mascot x. Returns `Some(dir)` only when the walk
    /// direction changes (>= 4px net move since the last change), so the
    /// caller can switch the looping walk track without restarting it.
    pub fn update(&mut self, x: i32) -> Option<WalkDir> {
        let dx = x - self.ref_x;
        let new = if dx >= WALK_THRESHOLD {
            WalkDir::Right
        } else if dx <= -WALK_THRESHOLD {
            WalkDir::Left
        } else {
            return None; // within hysteresis: keep the current direction
        };
        self.ref_x = x;
        if Some(new) != self.dir {
            self.dir = Some(new);
            Some(new)
        } else {
            None
        }
    }

    pub fn dir(&self) -> Option<WalkDir> {
        self.dir
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

    /// Enter `Dragging` (not yet armed) via a press + threshold cross.
    fn dragging_unarmed(start_pos: (i32, i32)) -> Drag {
        let mut drag = Drag::Idle;
        drag.press((10.0, 10.0), start_pos);
        assert!(drag.threshold_crossed((20.0, 10.0)));
        assert!(drag.dragging());
        assert!(!drag.armed());
        drag
    }

    #[test]
    fn motion_before_arm_is_dropped_and_does_not_establish_grab() {
        let mut drag = dragging_unarmed((3160, 700));
        // Pre-arm motions are ignored entirely: None, no grab, no pos change.
        assert_eq!(drag.drag_to((150.0, 40.0)), None);
        assert_eq!(drag.drag_to((151.0, 41.0)), None);
        assert_eq!(drag.drag_pos(), Some((3160, 700))); // unchanged
        let (grab, _) = drag.debug_grab_start();
        assert_eq!(grab, None); // grab never seeded by a pre-arm motion
    }

    #[test]
    fn first_motion_after_arm_establishes_grab_from_that_pointer() {
        let mut drag = dragging_unarmed((3160, 700));
        // Several pre-arm (docked-local-looking) motions: all ignored.
        drag.drag_to((150.0, 40.0));
        drag.drag_to((160.0, 45.0));
        drag.drag_to((170.0, 50.0));
        assert_eq!(drag.debug_grab_start().0, None);
        // Arm, then the FIRST motion sets grab from THAT pointer (not earlier).
        drag.arm();
        assert_eq!(drag.drag_to((3200.0, 680.0)), Some((3160, 700))); // no jump
        assert_eq!(drag.debug_grab_start().0, Some((3200.0, 680.0)));
    }

    #[test]
    fn post_arm_tracking_is_1_to_1() {
        let mut drag = dragging_unarmed((100, 200));
        drag.arm();
        // First post-arm motion = grab; sprite stays at start_pos.
        assert_eq!(drag.drag_to((500.0, 400.0)), Some((100, 200)));
        // pos = start_pos + (pointer - grab), exact 1:1.
        assert_eq!(drag.drag_to((530.0, 380.0)), Some((130, 180)));
        assert_eq!(drag.drag_to((500.0, 400.0)), Some((100, 200)));
        // Stationary pointer holds exactly — no drift.
        assert_eq!(drag.drag_to((500.0, 400.0)), Some((100, 200)));
    }

    #[test]
    fn the_exact_pin_bug_pre_arm_docked_then_arm_output_tracks_correctly() {
        // start_pos output-absolute (~3160); a docked-local motion (~150)
        // arrives BEFORE arm and must not seed grab. After arm, a large
        // output-absolute motion sets grab and tracking is correct.
        let mut drag = dragging_unarmed((3160, 700));
        assert_eq!(drag.drag_to((150.0, 40.0)), None); // docked-local, dropped
        assert_eq!(drag.drag_pos(), Some((3160, 700))); // not pinned
        drag.arm();
        // First output-absolute motion establishes grab (no jump)...
        assert_eq!(drag.drag_to((3120.0, 680.0)), Some((3160, 700)));
        // ...and subsequent motions track 1:1 (would be pinned ~6000 if the
        // docked-local 150 had seeded grab). grab=(3120,680), start=(3160,700).
        assert_eq!(drag.drag_to((3200.0, 660.0)), Some((3240, 680)));
        assert_eq!(drag.drag_to((1440.0, 700.0)), Some((1480, 720)));
    }

    #[test]
    fn drag_tracks_1_to_1_over_large_distances_no_scale() {
        let mut drag = dragging_unarmed((3160, 700));
        drag.arm();
        drag.drag_to((3160.0, 700.0)); // grab
        // Move the pointer 1720px left (half a 3440 monitor): the pet moves
        // exactly 1720px left. A scale/divide in the path would fail here.
        assert_eq!(drag.drag_to((1440.0, 700.0)), Some((3160 - 1720, 700)));
        assert_eq!(drag.drag_to((3160.0, 100.0)), Some((3160, 100)));
    }

    #[test]
    fn no_feedback_loop_repeated_same_pointer_is_stable() {
        let mut drag = dragging_unarmed((1000, 500));
        drag.arm();
        drag.drag_to((2000.0, 1000.0)); // grab
        for _ in 0..100 {
            assert_eq!(drag.drag_to((2010.0, 1005.0)), Some((1010, 505)));
        }
    }

    #[test]
    fn fractional_pointer_rounds_per_report_without_accumulating() {
        let mut drag = dragging_unarmed((0, 0));
        drag.arm();
        drag.drag_to((100.0, 100.0)); // grab
        assert_eq!(drag.drag_to((103.6, 100.4)), Some((4, 0)));
        assert_eq!(drag.drag_to((100.4, 100.6)), Some((0, 1)));
        assert_eq!(drag.drag_to((100.0, 100.0)), Some((0, 0)));
    }

    #[test]
    fn drag_to_before_dragging_is_none() {
        let mut drag = Drag::Idle;
        assert_eq!(drag.drag_to((50.0, 50.0)), None);
        drag.press((0.0, 0.0), (10, 10));
        assert_eq!(drag.drag_to((50.0, 50.0)), None); // still Pressed
    }

    #[test]
    fn release_from_any_substate_returns_cleanly() {
        // Idle.
        let mut drag = Drag::Idle;
        assert_eq!(drag.release(), Release::None);
        // Pressed (never past threshold) -> Click.
        drag.press((0.0, 0.0), (10, 10));
        assert_eq!(drag.release(), Release::Click);
        assert_eq!(drag, Drag::Idle);
        // Dragging, unarmed -> Dropped.
        let mut drag = dragging_unarmed((10, 10));
        assert_eq!(drag.release(), Release::Dropped);
        assert_eq!(drag, Drag::Idle);
        // Dragging, armed, mid-track -> Dropped.
        let mut drag = dragging_unarmed((10, 10));
        drag.arm();
        drag.drag_to((50.0, 50.0));
        assert_eq!(drag.release(), Release::Dropped);
        assert_eq!(drag.drag_pos(), None);
        assert_eq!(drag, Drag::Idle);
    }

    #[test]
    fn walk_direction_follows_horizontal_travel_with_hysteresis() {
        let mut walk = Walk::new(100);
        assert_eq!(walk.dir(), None);
        // Small moves stay under threshold: no walk yet.
        assert_eq!(walk.update(102), None);
        assert_eq!(walk.update(103), None);
        // Cross +4 from ref -> Right (fires once).
        assert_eq!(walk.update(104), Some(WalkDir::Right));
        // Continuing right is the same direction: no re-fire.
        assert_eq!(walk.update(140), None);
        assert_eq!(walk.dir(), Some(WalkDir::Right));
        // A small back-step within hysteresis doesn't flip.
        assert_eq!(walk.update(138), None);
        // A 4px net reversal flips to Left.
        assert_eq!(walk.update(134), Some(WalkDir::Left));
        assert_eq!(walk.dir(), Some(WalkDir::Left));
    }

    #[test]
    fn walk_tracks_map_to_running_rows() {
        assert_eq!(WalkDir::Right.track(), "running-right");
        assert_eq!(WalkDir::Left.track(), "running-left");
    }
}
