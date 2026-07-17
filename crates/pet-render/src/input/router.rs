//! Pure pointer routing: hit-testing against the sprite/bubble rects,
//! left-click routing (bubble = immediate click target, sprite = drag FSM),
//! input-region composition, and the cursor-shape decision.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn contains(&self, (px, py): (f64, f64)) -> bool {
        px >= self.x as f64
            && py >= self.y as f64
            && px < (self.x + self.w as i32) as f64
            && py < (self.y + self.h as i32) as f64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Bubble,
    Sprite,
    Outside,
}

/// The bubble is drawn over the sprite, so it wins overlaps.
pub fn hit_test(pos: (f64, f64), sprite: Rect, bubble: Option<Rect>) -> Hit {
    if bubble.is_some_and(|b| b.contains(pos)) {
        Hit::Bubble
    } else if sprite.contains(pos) {
        Hit::Sprite
    } else {
        Hit::Outside
    }
}

/// The rects the surface accepts input on: always the sprite; the bubble box
/// only while shown (tail/gap stay click-through).
pub fn input_rects(sprite: Rect, bubble: Option<Rect>) -> Vec<Rect> {
    [Some(sprite), bubble].into_iter().flatten().collect()
}

/// Left-button click routing. Sprite presses go through the drag FSM with
/// its click-vs-drag threshold; bubble presses arm an immediate
/// click-on-release (no threshold).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Clicks {
    bubble_armed: bool,
}

impl Clicks {
    /// Route a left press. Returns true when the press starts a drag grab
    /// (sprite hit).
    pub fn press(&mut self, hit: Hit) -> bool {
        self.bubble_armed = hit == Hit::Bubble;
        hit == Hit::Sprite
    }

    /// Route a left release. Returns true when a bubble click completed
    /// (armed press and released inside the bubble).
    pub fn release(&mut self, hit: Hit) -> bool {
        let fired = self.bubble_armed && hit == Hit::Bubble;
        self.bubble_armed = false;
        fired
    }

    pub fn cancel(&mut self) {
        self.bubble_armed = false;
    }
}

/// Cursor shape for the current hover/drag state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Cursor {
    #[default]
    Default,
    /// Hand over the clickable bubble.
    Pointer,
    /// Open hand over the draggable sprite.
    Grab,
    /// Closed hand during an active drag.
    Grabbing,
}

pub fn cursor_for(dragging: bool, hit: Hit) -> Cursor {
    if dragging {
        return Cursor::Grabbing;
    }
    match hit {
        Hit::Bubble => Cursor::Pointer,
        Hit::Sprite => Cursor::Grab,
        Hit::Outside => Cursor::Default,
    }
}

/// Hover-jump edge action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoverChange {
    /// Started hovering the sprite: play the jumping gesture.
    Jump,
    /// Stopped hovering: return to the base state.
    ReturnToBase,
}

/// Edge-detect the hover-jump. A jump-hover is "pointer over the sprite, in
/// the docked surface, not dragging, and the pet has jump art". `hovering`
/// carries the current state and is updated in place; returns the transition
/// action only when it flips.
pub fn hover_transition(
    over_sprite: bool,
    docked: bool,
    dragging: bool,
    has_jump_art: bool,
    hovering: &mut bool,
) -> Option<HoverChange> {
    let want = over_sprite && docked && !dragging && has_jump_art;
    if want == *hovering {
        return None;
    }
    *hovering = want;
    Some(if want {
        HoverChange::Jump
    } else {
        HoverChange::ReturnToBase
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPRITE: Rect = Rect { x: 152, y: 100, w: 128, h: 128 };
    const BUBBLE: Rect = Rect { x: 8, y: 20, w: 264, h: 74 };

    #[test]
    fn input_region_is_sprite_only_without_bubble() {
        assert_eq!(input_rects(SPRITE, None), vec![SPRITE]);
    }

    #[test]
    fn input_region_adds_bubble_box_while_shown() {
        assert_eq!(input_rects(SPRITE, Some(BUBBLE)), vec![SPRITE, BUBBLE]);
    }

    #[test]
    fn hit_test_routes_by_rect_and_bubble_wins_overlap() {
        assert_eq!(hit_test((200.0, 150.0), SPRITE, Some(BUBBLE)), Hit::Sprite);
        assert_eq!(hit_test((50.0, 40.0), SPRITE, Some(BUBBLE)), Hit::Bubble);
        assert_eq!(hit_test((50.0, 40.0), SPRITE, None), Hit::Outside);
        assert_eq!(hit_test((1.0, 99.0), SPRITE, Some(BUBBLE)), Hit::Outside);
        // Bubble overlapping the sprite: bubble is drawn on top and wins.
        let over = Rect { x: 150, y: 90, w: 60, h: 60 };
        assert_eq!(hit_test((160.0, 110.0), SPRITE, Some(over)), Hit::Bubble);
        // Edges: inclusive origin, exclusive far edge.
        assert!(SPRITE.contains((152.0, 100.0)));
        assert!(!SPRITE.contains((280.0, 100.0)));
    }

    #[test]
    fn bubble_click_fires_on_release_inside_only() {
        let mut clicks = Clicks::default();
        assert!(!clicks.press(Hit::Bubble)); // no drag grab
        assert!(clicks.release(Hit::Bubble)); // fires
        assert!(!clicks.release(Hit::Bubble)); // disarmed after firing

        clicks.press(Hit::Bubble);
        assert!(!clicks.release(Hit::Outside)); // slid off: no click

        clicks.press(Hit::Bubble);
        clicks.cancel(); // e.g. pointer left the surface
        assert!(!clicks.release(Hit::Bubble));
    }

    #[test]
    fn sprite_press_grabs_drag_and_never_fires_bubble_click() {
        let mut clicks = Clicks::default();
        assert!(clicks.press(Hit::Sprite));
        assert!(!clicks.release(Hit::Sprite));
        assert!(!clicks.release(Hit::Bubble));
    }

    #[test]
    fn cursor_states() {
        assert_eq!(cursor_for(false, Hit::Bubble), Cursor::Pointer);
        assert_eq!(cursor_for(false, Hit::Sprite), Cursor::Grab);
        assert_eq!(cursor_for(false, Hit::Outside), Cursor::Default);
        // An active drag overrides whatever is hovered.
        assert_eq!(cursor_for(true, Hit::Sprite), Cursor::Grabbing);
        assert_eq!(cursor_for(true, Hit::Outside), Cursor::Grabbing);
    }

    #[test]
    fn hover_jump_edges_only_over_sprite_when_docked_and_idle() {
        let mut hovering = false;
        // Enter the sprite (docked, not dragging, has art): jump once.
        assert_eq!(
            hover_transition(true, true, false, true, &mut hovering),
            Some(HoverChange::Jump)
        );
        assert!(hovering);
        // Staying over the sprite: no re-fire.
        assert_eq!(hover_transition(true, true, false, true, &mut hovering), None);
        // Leave the sprite: return to base.
        assert_eq!(
            hover_transition(false, true, false, true, &mut hovering),
            Some(HoverChange::ReturnToBase)
        );
        assert!(!hovering);
    }

    #[test]
    fn hover_jump_suppressed_by_drag_full_output_and_missing_art() {
        // Over the sprite but dragging: no jump.
        let mut hovering = false;
        assert_eq!(hover_transition(true, true, true, true, &mut hovering), None);
        // Full-output (not docked): no jump.
        assert_eq!(hover_transition(true, false, false, true, &mut hovering), None);
        // No jump art (default pet): no jump.
        assert_eq!(hover_transition(true, true, false, false, &mut hovering), None);
        assert!(!hovering);
        // A drag that begins while hovering forces a return-to-base edge.
        let mut hovering = true;
        assert_eq!(
            hover_transition(true, true, true, true, &mut hovering),
            Some(HoverChange::ReturnToBase)
        );
    }
}
