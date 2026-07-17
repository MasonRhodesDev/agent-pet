//! Pure drag FSM: click-vs-drag threshold + incremental margin moves.
//!
//! The mascot stays a small docked surface throughout the drag — it is NOT
//! expanded. Wayland's implicit pointer grab keeps motion events flowing to
//! that surface even after the cursor moves off it (coordinates simply run
//! negative / past the surface bounds), so a small surface can follow the
//! cursor across the whole output. Because the surface never resizes, every
//! coordinate stays in ONE space (the docked surface-local frame) for the
//! whole drag — there is no docked→full-output transition to seed the grab
//! from the wrong space.
//!
//! Math: `grab` is the surface-local point where the pet was picked up
//! (fixed for the drag). Each motion applies the incremental move needed to
//! keep that grab point under the cursor:
//!
//! ```text
//! margin += pointer_local - grab
//! ```
//!
//! This self-corrects: after a move the surface slides by that delta, so a
//! stationary cursor's next surface-local reading is `grab` again (delta 0),
//! and real cursor motion since the last move reads as exactly that motion.

/// Squared click-vs-drag threshold (~4 px).
const THRESHOLD_SQ: f64 = 16.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Drag {
    Idle,
    /// Button down, not yet past the threshold.
    Pressed {
        /// Surface-local point where the pet was grabbed.
        grab: (f64, f64),
        /// Pet's margins (top-left on the output) at press.
        margin: (i32, i32),
    },
    /// Past the threshold: following the cursor by moving the margins.
    Dragging {
        /// Surface-local grab point — fixed for the whole drag.
        grab: (f64, f64),
        /// Current margins (updated each motion).
        margin: (i32, i32),
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
    /// Button press. `pointer` is surface-local; `margin` is the pet's
    /// current top-left on the output.
    pub fn press(&mut self, pointer: (f64, f64), margin: (i32, i32)) {
        *self = Drag::Pressed {
            grab: pointer,
            margin,
        };
    }

    /// Pre-drag motion (surface-local). Returns true exactly once, when the
    /// click-vs-drag threshold is first crossed → transitions to `Dragging`,
    /// keeping the same grab point and coordinate space.
    pub fn threshold_crossed(&mut self, pointer: (f64, f64)) -> bool {
        if let Drag::Pressed { grab, margin } = *self {
            let (dx, dy) = (pointer.0 - grab.0, pointer.1 - grab.1);
            if dx * dx + dy * dy > THRESHOLD_SQ {
                *self = Drag::Dragging { grab, margin };
                return true;
            }
        }
        false
    }

    /// Drag motion (surface-local). Applies the incremental move to keep the
    /// grab point under the cursor and returns the new margins, or `None`
    /// when not dragging.
    pub fn drag_to(&mut self, pointer: (f64, f64)) -> Option<(i32, i32)> {
        if let Drag::Dragging { grab, margin } = self {
            margin.0 += (pointer.0 - grab.0).round() as i32;
            margin.1 += (pointer.1 - grab.1).round() as i32;
            Some(*margin)
        } else {
            None
        }
    }

    /// Current margins while dragging.
    pub fn margin(&self) -> Option<(i32, i32)> {
        match self {
            Drag::Dragging { margin, .. } => Some(*margin),
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

/// Decides the walk direction from the drag's *smoothed* horizontal velocity
/// so hand jitter doesn't flip the sprite back and forth. Everything is
/// TIME-based, not sample-based, so it behaves identically whether the mouse
/// polls at 125 Hz or 1000 Hz: the velocity is a time-constant EMA of px/sec,
/// and a candidate reversal must hold for a wall-clock duration before the
/// sprite flips. A fast twitch (dozens of samples in a few ms) can't flip it.
#[derive(Debug, Clone, Copy)]
pub struct Walk {
    last_x: i32,
    last_ms: u64,
    /// Smoothed horizontal velocity, px/sec, signed.
    vel: f64,
    dir: Option<WalkDir>,
    /// Candidate reversal awaiting confirmation, and when it first appeared.
    pending: Option<WalkDir>,
    pending_since_ms: u64,
}

/// Velocity-smoothing time constant (ms). Larger = smoother / more lag.
const TAU_MS: f64 = 140.0;
/// Smoothed |velocity| (px/sec) needed to favor a direction; below this the
/// current direction holds (dead zone around a near-stationary pointer).
const FLIP_PX_S: f64 = 50.0;
/// Wall-clock time a reversal must hold before the sprite flips.
const CONFIRM_MS: u64 = 90;

impl Walk {
    pub fn new(start_x: i32) -> Self {
        Self {
            last_x: start_x,
            last_ms: 0,
            vel: 0.0,
            dir: None,
            pending: None,
            pending_since_ms: 0,
        }
    }

    /// Feed the current mascot x at time `now_ms`. Returns `Some(dir)` only
    /// when the confirmed direction actually changes.
    pub fn update(&mut self, x: i32, now_ms: u64) -> Option<WalkDir> {
        let dt = now_ms.saturating_sub(self.last_ms);
        if dt == 0 {
            return None; // same-ms sample: nothing to integrate
        }
        let inst = (x - self.last_x) as f64 / dt as f64 * 1000.0; // px/sec
        self.last_x = x;
        self.last_ms = now_ms;
        // Time-constant EMA: weight of the new sample depends on elapsed time,
        // so smoothing is poll-rate independent.
        let alpha = 1.0 - (-(dt as f64) / TAU_MS).exp();
        self.vel = alpha * inst + (1.0 - alpha) * self.vel;

        let candidate = if self.vel >= FLIP_PX_S {
            Some(WalkDir::Right)
        } else if self.vel <= -FLIP_PX_S {
            Some(WalkDir::Left)
        } else {
            None // dead zone
        };

        // No candidate, or already walking that way: cancel any pending flip.
        if candidate.is_none() || candidate == self.dir {
            self.pending = None;
            return None;
        }
        // A reversal must hold CONFIRM_MS of wall-clock before it flips.
        if self.pending != candidate {
            self.pending = candidate;
            self.pending_since_ms = now_ms;
            return None;
        }
        if now_ms.saturating_sub(self.pending_since_ms) >= CONFIRM_MS {
            self.dir = candidate;
            self.pending = None;
            candidate
        } else {
            None
        }
    }

    pub fn dir(&self) -> Option<WalkDir> {
        self.dir
    }

    /// Smoothed horizontal velocity (px/sec, signed) — for drag telemetry.
    pub fn vel(&self) -> f64 {
        self.vel
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
        assert!(!drag.threshold_crossed((13.0, 12.5)));
        assert_eq!(drag.margin(), None);
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
    fn threshold_crossing_reports_once() {
        let mut drag = Drag::Idle;
        drag.press((10.0, 10.0), (500, 500));
        assert!(drag.threshold_crossed((20.0, 10.0)));
        assert!(drag.dragging());
        // Already dragging: no second "crossed" event.
        assert!(!drag.threshold_crossed((30.0, 10.0)));
    }

    #[test]
    fn drag_keeps_the_grab_point_under_the_cursor() {
        // Grabbed at surface-local (40,50); pet at margins (300,200).
        let mut drag = Drag::Idle;
        drag.press((40.0, 50.0), (300, 200));
        assert!(drag.threshold_crossed((40.0, 60.0))); // 10px down → drag

        // Cursor moves 100px right within the current frame: pet moves +100.
        assert_eq!(drag.drag_to((140.0, 60.0)), Some((400, 210)));
        // After that move the surface slid +100/+10; a stationary cursor now
        // reads the grab point again → no further move.
        assert_eq!(drag.drag_to((40.0, 50.0)), Some((400, 210)));
        // Further real motion tracks incrementally.
        assert_eq!(drag.drag_to((60.0, 50.0)), Some((420, 210)));
    }

    #[test]
    fn incremental_moves_have_no_scale_or_pin() {
        // Regression for the pin bug: one consistent coordinate space, so the
        // grab offset can never desync the pet from the cursor.
        let mut drag = Drag::Idle;
        drag.press((100.0, 100.0), (1000, 1000));
        drag.threshold_crossed((100.0, 110.0));
        // Drag the cursor far to the left of the pet (surface-local goes very
        // negative under the implicit grab): the pet follows, staying under
        // the cursor, never pinned.
        let m = drag.drag_to((-500.0, 100.0)).unwrap();
        assert_eq!(m.0, 1000 + (-500 - 100)); // 400 — moved left with the cursor
    }

    #[test]
    fn margin_is_none_unless_dragging() {
        let mut drag = Drag::Idle;
        assert_eq!(drag.margin(), None);
        drag.press((0.0, 0.0), (0, 0));
        assert_eq!(drag.margin(), None); // pressed, not dragging
        assert_eq!(drag.drag_to((5.0, 5.0)), None);
    }

    #[test]
    fn release_semantics() {
        let mut drag = Drag::Idle;
        assert_eq!(drag.release(), Release::None);
        drag.press((0.0, 0.0), (0, 0));
        assert_eq!(drag.release(), Release::Click);
        drag.press((0.0, 0.0), (0, 0));
        drag.threshold_crossed((10.0, 10.0));
        assert_eq!(drag.release(), Release::Dropped);
        assert_eq!(drag, Drag::Idle);
    }

    /// Drive `walk` with steady travel of `step` px every `dt_ms` for
    /// `duration_ms`, returning the last direction it flipped to (if any).
    fn drive(walk: &mut Walk, x: &mut i32, step: i32, dt_ms: u64, duration_ms: u64, t: &mut u64) -> Option<WalkDir> {
        let mut fired = None;
        let end = *t + duration_ms;
        while *t < end {
            *t += dt_ms;
            *x += step;
            if let Some(d) = walk.update(*x, *t) {
                fired = Some(d);
            }
        }
        fired
    }

    #[test]
    fn walk_commits_a_direction_and_holds_it() {
        let mut walk = Walk::new(0);
        let (mut x, mut t) = (0, 0);
        // Steady rightward travel (~2000 px/s) for 300ms commits Right.
        assert_eq!(drive(&mut walk, &mut x, 2, 1, 300, &mut t), Some(WalkDir::Right));
        assert_eq!(walk.dir(), Some(WalkDir::Right));
    }

    #[test]
    fn walk_ignores_a_fast_twitch_at_high_poll_rate() {
        // The exact bug from the logs: 1000Hz mouse, a ~24ms rightward twitch
        // during an overall-left drag must NOT flip the sprite.
        let mut walk = Walk::new(0);
        let (mut x, mut t) = (0, 0);
        drive(&mut walk, &mut x, -3, 1, 300, &mut t); // establish Left @1000Hz
        assert_eq!(walk.dir(), Some(WalkDir::Left));
        // 24ms of fast rightward samples (well under CONFIRM_MS).
        let flipped = drive(&mut walk, &mut x, 4, 1, 24, &mut t);
        assert_ne!(flipped, Some(WalkDir::Right), "a 24ms twitch flipped it");
        assert_eq!(walk.dir(), Some(WalkDir::Left));
    }

    #[test]
    fn walk_flips_on_a_sustained_reversal() {
        let mut walk = Walk::new(0);
        let (mut x, mut t) = (0, 0);
        drive(&mut walk, &mut x, -3, 1, 300, &mut t);
        assert_eq!(walk.dir(), Some(WalkDir::Left));
        // A real, sustained (>CONFIRM_MS) rightward reversal flips to Right.
        assert_eq!(drive(&mut walk, &mut x, 3, 1, 300, &mut t), Some(WalkDir::Right));
    }

    #[test]
    fn walk_is_poll_rate_independent() {
        // Same real motion (2 px/ms rightward for 300ms) at 125Hz and 1000Hz
        // both commit Right in comparable wall-clock — smoothing is time-based.
        for dt in [1u64, 8] {
            let mut walk = Walk::new(0);
            let (mut x, mut t) = (0, 0);
            let step = (2 * dt) as i32; // keep px/ms constant across rates
            assert_eq!(
                drive(&mut walk, &mut x, step, dt, 300, &mut t),
                Some(WalkDir::Right),
                "dt={dt}"
            );
        }
    }
}
