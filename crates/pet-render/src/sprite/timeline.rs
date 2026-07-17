//! Playback engine, ported from Codex's ambient behavior: per-frame
//! durations, loop_start wrap, one-shot -> fallback handoff, and state
//! animations that play 3 passes then settle to idle. Pure (ms clock in),
//! so the calloop side only has to arm one timer at `next_deadline_ms`.

use std::collections::HashMap;

use super::pet_json::{Animation, PetDef};

/// How many passes a state track plays before settling to idle.
pub const STATE_PLAYS: u32 = 3;

const IDLE: &str = "idle";

pub struct Timeline {
    animations: HashMap<String, Animation>,
    current: String,
    frame_idx: usize,
    deadline_ms: u64,
    /// `Some(n)` while playing a state animation (n passes left before the
    /// fallback handoff); `None` in ambient playback.
    passes_left: Option<u32>,
    /// Last requested state track, so a repeated snapshot with the same top
    /// state does not restart the animation.
    requested: String,
}

impl Timeline {
    /// `pet.animations` must contain "idle" (the loader guarantees it).
    pub fn new(pet: &PetDef, now_ms: u64) -> Self {
        let mut tl = Self {
            animations: pet.animations.clone(),
            current: IDLE.to_string(),
            frame_idx: 0,
            deadline_ms: now_ms,
            passes_left: None,
            requested: IDLE.to_string(),
        };
        tl.deadline_ms = now_ms + tl.frame_duration();
        tl
    }

    /// Switch to a state track: plays `STATE_PLAYS` passes, then idles.
    /// Requesting the track already playing is a no-op.
    pub fn request_state(&mut self, track: &str, now_ms: u64) {
        if track == self.requested {
            return;
        }
        self.requested = track.to_string();
        if track == IDLE {
            self.enter(IDLE, None, now_ms);
        } else {
            self.enter(track, Some(STATE_PLAYS), now_ms);
        }
    }

    /// Play a track looping indefinitely (no burst, no settle) until the next
    /// request. Used for the drag-walk override, which persists as long as
    /// the pet is being dragged sideways. No-op if already the requested track.
    pub fn request_loop(&mut self, track: &str, now_ms: u64) {
        if track == self.requested {
            return;
        }
        self.requested = track.to_string();
        self.enter(track, None, now_ms);
    }

    /// Step past any elapsed frame deadlines. Returns true if the visible
    /// sprite may have changed.
    pub fn advance(&mut self, now_ms: u64) -> bool {
        let mut changed = false;
        while now_ms >= self.deadline_ms {
            changed = true;
            let anim = &self.animations[&self.current];
            if self.frame_idx + 1 < anim.frames.len() {
                self.frame_idx += 1;
            } else {
                // End of a pass.
                match self.passes_left {
                    Some(1) => {
                        let fallback = anim.fallback.clone();
                        self.current = self.existing(&fallback);
                        self.frame_idx = 0;
                        self.passes_left = None;
                    }
                    Some(n) => {
                        self.passes_left = Some(n - 1);
                        self.frame_idx = anim.loop_start.unwrap_or(0).min(anim.frames.len() - 1);
                    }
                    None => match anim.loop_start {
                        Some(ls) if ls < anim.frames.len() => self.frame_idx = ls,
                        _ => {
                            // One-shot: hand off to the fallback track.
                            let fallback = anim.fallback.clone();
                            self.current = self.existing(&fallback);
                            self.frame_idx = 0;
                        }
                    },
                }
            }
            self.deadline_ms += self.frame_duration();
        }
        changed
    }

    pub fn sprite_index(&self) -> usize {
        self.animations[&self.current].frames[self.frame_idx].sprite_index
    }

    pub fn next_deadline_ms(&self) -> u64 {
        self.deadline_ms
    }

    pub fn current_track(&self) -> &str {
        &self.current
    }

    /// Sprite indices of the whole current track (e.g. for content-extent
    /// queries that must stay stable across the track's frames).
    pub fn current_sprites(&self) -> impl Iterator<Item = usize> + '_ {
        self.animations[&self.current]
            .frames
            .iter()
            .map(|f| f.sprite_index)
    }

    fn enter(&mut self, track: &str, passes: Option<u32>, now_ms: u64) {
        self.current = self.existing(track);
        self.frame_idx = 0;
        self.passes_left = passes;
        self.deadline_ms = now_ms + self.frame_duration();
    }

    fn existing(&self, track: &str) -> String {
        if self.animations.contains_key(track) {
            track.to_string()
        } else {
            IDLE.to_string()
        }
    }

    fn frame_duration(&self) -> u64 {
        // Clamped so a zero-duration frame can never spin `advance`.
        self.animations[&self.current].frames[self.frame_idx]
            .duration_ms
            .max(1)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::pet_json::Frame;
    use super::*;

    fn pet(tracks: &[(&str, &[(usize, u64)], Option<usize>, &str)]) -> PetDef {
        PetDef {
            id: "t".into(),
            spritesheet_path: PathBuf::new(),
            frame_width: 1,
            frame_height: 1,
            columns: 8,
            rows: 9,
            animations: tracks
                .iter()
                .map(|(name, frames, loop_start, fallback)| {
                    (
                        name.to_string(),
                        Animation {
                            frames: frames
                                .iter()
                                .map(|&(sprite_index, duration_ms)| Frame {
                                    sprite_index,
                                    duration_ms,
                                })
                                .collect(),
                            loop_start: *loop_start,
                            fallback: fallback.to_string(),
                        },
                    )
                })
                .collect(),
        }
    }

    fn idle_only() -> PetDef {
        pet(&[("idle", &[(0, 100), (1, 200)], Some(0), "idle")])
    }

    #[test]
    fn idle_loops_with_wrap_to_loop_start() {
        let mut tl = Timeline::new(&idle_only(), 0);
        assert_eq!(tl.sprite_index(), 0);
        assert_eq!(tl.next_deadline_ms(), 100);
        tl.advance(100);
        assert_eq!(tl.sprite_index(), 1);
        assert_eq!(tl.next_deadline_ms(), 300);
        tl.advance(300);
        assert_eq!(tl.sprite_index(), 0); // wrapped
        assert_eq!(tl.next_deadline_ms(), 400);
    }

    #[test]
    fn advance_catches_up_over_multiple_frames() {
        let mut tl = Timeline::new(&idle_only(), 0);
        assert!(tl.advance(350)); // 0..100 f0, 100..300 f1, 300.. f0
        assert_eq!(tl.sprite_index(), 0);
        assert_eq!(tl.next_deadline_ms(), 400);
    }

    #[test]
    fn one_shot_hands_off_to_fallback() {
        let p = pet(&[
            ("idle", &[(0, 100)], Some(0), "idle"),
            ("wave", &[(8, 50), (9, 50)], None, "idle"),
        ]);
        let mut tl = Timeline::new(&p, 0);
        // Enter ambient one-shot by requesting then exhausting passes is the
        // state path; drive the ambient path directly.
        tl.enter("wave", None, 0);
        tl.advance(50);
        assert_eq!(tl.sprite_index(), 9);
        tl.advance(100);
        assert_eq!(tl.current_track(), "idle");
        assert_eq!(tl.sprite_index(), 0);
    }

    #[test]
    fn state_track_plays_three_times_then_settles_to_idle() {
        let p = pet(&[
            ("idle", &[(0, 100)], Some(0), "idle"),
            ("running", &[(56, 100), (57, 100)], Some(0), "idle"),
        ]);
        let mut tl = Timeline::new(&p, 0);
        tl.request_state("running", 0);
        assert_eq!(tl.sprite_index(), 56);
        for (t, expect) in [
            (100, 57), // pass 1
            (200, 56),
            (300, 57), // pass 2
            (400, 56),
            (500, 57), // pass 3
        ] {
            tl.advance(t);
            assert_eq!(tl.sprite_index(), expect, "at t={t}");
            assert_eq!(tl.current_track(), "running");
        }
        tl.advance(600);
        assert_eq!(tl.current_track(), "idle");
        assert_eq!(tl.sprite_index(), 0);
        // ... and idle keeps looping.
        tl.advance(700);
        assert_eq!(tl.current_track(), "idle");
        assert_eq!(tl.next_deadline_ms(), 800);
    }

    #[test]
    fn one_shot_state_track_respects_pass_count_then_falls_back() {
        let p = pet(&[
            ("idle", &[(0, 100)], Some(0), "idle"),
            ("review", &[(64, 100)], None, "idle"),
        ]);
        let mut tl = Timeline::new(&p, 0);
        tl.request_state("review", 0);
        tl.advance(100);
        assert_eq!(tl.current_track(), "review"); // pass 2
        tl.advance(200);
        assert_eq!(tl.current_track(), "review"); // pass 3
        tl.advance(300);
        assert_eq!(tl.current_track(), "idle");
    }

    #[test]
    fn repeated_state_request_does_not_restart() {
        let p = pet(&[
            ("idle", &[(0, 100)], Some(0), "idle"),
            ("running", &[(56, 100), (57, 100)], Some(0), "idle"),
        ]);
        let mut tl = Timeline::new(&p, 0);
        tl.request_state("running", 0);
        tl.advance(100);
        assert_eq!(tl.sprite_index(), 57);
        tl.request_state("running", 150);
        assert_eq!(tl.sprite_index(), 57); // unchanged, not reset to frame 0
        assert_eq!(tl.next_deadline_ms(), 200);
    }

    #[test]
    fn state_change_switches_immediately() {
        let p = pet(&[
            ("idle", &[(0, 100)], Some(0), "idle"),
            ("running", &[(56, 100)], Some(0), "idle"),
            ("waiting", &[(48, 100)], Some(0), "idle"),
        ]);
        let mut tl = Timeline::new(&p, 0);
        tl.request_state("running", 0);
        tl.request_state("waiting", 30);
        assert_eq!(tl.sprite_index(), 48);
        assert_eq!(tl.next_deadline_ms(), 130);
        // Back to idle on request, too.
        tl.request_state("idle", 50);
        assert_eq!(tl.current_track(), "idle");
    }

    #[test]
    fn unknown_track_falls_back_to_idle() {
        let mut tl = Timeline::new(&idle_only(), 0);
        tl.request_state("running", 0);
        assert_eq!(tl.current_track(), "idle");
    }

    #[test]
    fn request_loop_plays_a_track_indefinitely_without_settling() {
        let p = pet(&[
            ("idle", &[(0, 100)], Some(0), "idle"),
            ("running-right", &[(8, 100), (9, 100)], Some(0), "idle"),
            ("running-left", &[(16, 100), (17, 100)], Some(0), "idle"),
        ]);
        let mut tl = Timeline::new(&p, 0);
        tl.request_loop("running-right", 0);
        assert_eq!(tl.current_track(), "running-right");
        // Loops well past 3 passes (a burst would have settled to idle).
        for t in (100..=1200).step_by(100) {
            tl.advance(t as u64);
            assert_eq!(tl.current_track(), "running-right", "at t={t}");
        }
        // Repeated same-direction request does not restart.
        tl.advance(1250);
        let before = tl.sprite_index();
        tl.request_loop("running-right", 1250);
        assert_eq!(tl.sprite_index(), before);
        // Switching direction switches immediately.
        tl.request_loop("running-left", 1300);
        assert_eq!(tl.current_track(), "running-left");
    }
}
