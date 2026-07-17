//! Pure focus-settle debounce. Raw active-window facts arrive faster than
//! the daemon needs them (alt-tab flicker, rapid re-focus); this coalesces a
//! stream of `(now, Option<ActiveWindow>)` into a single emit once focus has
//! held on the same value for `settle_ms`. It never emits the same value
//! twice in a row.

use std::time::{Duration, Instant};

use pet_proto::ActiveWindow;

/// Default quiet period before a focus change is reported. The renderer will
/// take this from config once threaded through `spawn`.
pub const DEFAULT_SETTLE_MS: u64 = 300;

#[derive(Debug)]
pub struct Settle {
    quiet: Duration,
    /// Last value actually emitted (dedup guard).
    emitted: Option<ActiveWindow>,
    /// Candidate awaiting the quiet period, plus its deadline.
    pending: Option<(Option<ActiveWindow>, Instant)>,
}

/// What the owner should do after feeding a fact or checking the timer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Nothing to do (fact matched the settled or pending value).
    None,
    /// Arm/keep a timer for this instant, then call [`Settle::on_timer`].
    ArmTimer(Instant),
    /// Emit this active-window fact to the daemon now.
    Emit(Option<ActiveWindow>),
}

impl Settle {
    pub fn new(settle_ms: u64) -> Self {
        Self {
            quiet: Duration::from_millis(settle_ms),
            emitted: None,
            pending: None,
        }
    }

    /// Feed a raw fact observed at `now`.
    pub fn observe(&mut self, value: Option<ActiveWindow>, now: Instant) -> Action {
        // Already settled on this exact value: cancel any stale candidate.
        if self.emitted == value {
            self.pending = None;
            return Action::None;
        }
        // Same candidate still pending: keep its original deadline (a stable
        // target must not have its quiet period refreshed by duplicates).
        if let Some((pending, deadline)) = &self.pending {
            if *pending == value {
                return Action::ArmTimer(*deadline);
            }
        }
        let deadline = now + self.quiet;
        self.pending = Some((value, deadline));
        Action::ArmTimer(deadline)
    }

    /// Called when a timer set for `now` fires (or any later check).
    pub fn on_timer(&mut self, now: Instant) -> Action {
        match &self.pending {
            Some((value, deadline)) if now >= *deadline => {
                let value = value.clone();
                self.emitted = value.clone();
                self.pending = None;
                Action::Emit(value)
            }
            // Candidate changed under us / not due yet: re-arm to the
            // current deadline so the owner keeps a single live timer.
            Some((_, deadline)) => Action::ArmTimer(*deadline),
            None => Action::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(title: &str) -> Option<ActiveWindow> {
        Some(ActiveWindow {
            title: Some(title.into()),
            ..Default::default()
        })
    }

    fn at(ms: u64) -> Instant {
        // A fixed base kept comfortably above the quiet period so
        // subtractions never underflow.
        Instant::now() + Duration::from_millis(ms)
    }

    #[test]
    fn emits_after_the_quiet_period() {
        let mut s = Settle::new(300);
        let t0 = at(1000);
        assert_eq!(s.observe(win("kitty"), t0), Action::ArmTimer(t0 + ms(300)));
        // Too early: re-arm, no emit.
        assert_eq!(s.on_timer(t0 + ms(200)), Action::ArmTimer(t0 + ms(300)));
        // Due: emit.
        assert_eq!(s.on_timer(t0 + ms(300)), Action::Emit(win("kitty")));
    }

    #[test]
    fn alt_tab_flicker_only_emits_the_final_target() {
        let mut s = Settle::new(300);
        let t0 = at(1000);
        s.observe(win("a"), t0);
        s.observe(win("b"), t0 + ms(50));
        // Landing on c resets the quiet period from t=100.
        let a = s.observe(win("c"), t0 + ms(100));
        assert_eq!(a, Action::ArmTimer(t0 + ms(400)));
        // A timer from the earlier candidate fires but c is not due yet.
        assert_eq!(s.on_timer(t0 + ms(350)), Action::ArmTimer(t0 + ms(400)));
        assert_eq!(s.on_timer(t0 + ms(400)), Action::Emit(win("c")));
    }

    #[test]
    fn duplicate_pending_facts_do_not_refresh_the_deadline() {
        let mut s = Settle::new(300);
        let t0 = at(1000);
        assert_eq!(s.observe(win("x"), t0), Action::ArmTimer(t0 + ms(300)));
        // Heartbeat repeat of the same window mid-wait keeps the original
        // deadline (otherwise a steadily-focused window never settles).
        assert_eq!(s.observe(win("x"), t0 + ms(150)), Action::ArmTimer(t0 + ms(300)));
        assert_eq!(s.on_timer(t0 + ms(300)), Action::Emit(win("x")));
    }

    #[test]
    fn never_emits_the_same_value_twice() {
        let mut s = Settle::new(300);
        let t0 = at(1000);
        s.observe(win("k"), t0);
        assert_eq!(s.on_timer(t0 + ms(300)), Action::Emit(win("k")));
        // The same window re-reported after settling is a no-op.
        assert_eq!(s.observe(win("k"), t0 + ms(500)), Action::None);
        assert_eq!(s.on_timer(t0 + ms(800)), Action::None);
    }

    #[test]
    fn refocusing_settled_value_cancels_a_competing_candidate() {
        let mut s = Settle::new(300);
        let t0 = at(1000);
        s.observe(win("k"), t0);
        s.on_timer(t0 + ms(300)); // emitted k
        // Briefly focus j...
        s.observe(win("j"), t0 + ms(400));
        // ...then back to k before j settles: the j candidate is dropped and
        // nothing is emitted (still on k).
        assert_eq!(s.observe(win("k"), t0 + ms(450)), Action::None);
        assert_eq!(s.on_timer(t0 + ms(700)), Action::None);
    }

    #[test]
    fn none_focus_is_a_reportable_value() {
        let mut s = Settle::new(300);
        let t0 = at(1000);
        s.observe(win("k"), t0);
        s.on_timer(t0 + ms(300));
        // Focus leaves all known windows.
        assert_eq!(s.observe(None, t0 + ms(400)), Action::ArmTimer(t0 + ms(700)));
        assert_eq!(s.on_timer(t0 + ms(700)), Action::Emit(None));
        // And a second None settles to nothing new.
        assert_eq!(s.observe(None, t0 + ms(800)), Action::None);
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }
}
