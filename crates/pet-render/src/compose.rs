//! Frame composition: sprite blit + bubble, shared verbatim by the live
//! renderer (app.rs) and the offline preview harness so what we test is
//! what we ship.

use crate::canvas::Canvas;
use crate::sprite::sheet::Sheet;
use crate::sprite::timeline::Timeline;
use crate::surface::bubble::{self, Bubble, BubbleArea};
use crate::text::TextRenderer;

/// Logical target height the sprite scale aims for (integer scaling only,
/// so 64px pets render 2x, Codex 208px pets 1x).
const TARGET_LOGICAL_PX: u32 = 128;

/// Pure geometry of the surface: everything compose needs, nothing Wayland.
pub(crate) struct Geometry {
    pub surf_w: u32,
    pub surf_h: u32,
    pub mascot_x: u32,
    pub mascot_y: u32,
    pub mascot_w: u32,
    pub bubble_above: bool,
    pub anchor_right: bool,
    pub sprite_scale: u32,
    pub oscale: u32,
}

impl Geometry {
    pub fn buf_size(&self) -> (u32, u32) {
        (self.surf_w * self.oscale, self.surf_h * self.oscale)
    }
}

/// Compose one frame. Returns the bubble's box rect in physical px when one
/// was drawn (the click target; feeds the input region).
pub(crate) fn scene(
    buf: &mut [u8],
    geo: &Geometry,
    sheet: &mut Sheet,
    timeline: &Timeline,
    bubble: Option<(&Bubble, &mut TextRenderer)>,
    now_ms: u64,
) -> Option<(i32, i32, u32, u32)> {
    let oscale = geo.oscale.max(1);
    let factor = geo.sprite_scale * oscale;
    let (buf_w, buf_h) = geo.buf_size();
    let index = timeline.sprite_index();
    // Anchor the bubble to the sprite's visible ink, not the frame rect:
    // sprites sit low inside padded frames, and the bubble must hug what
    // the eye sees. Track-wide extent so it does not jitter per frame.
    let (ink_top, ink_bottom) = sheet.content_vspan(timeline.current_sprites());
    let sprite_w = sheet.frame_width * factor;
    let sprite_h = sheet.frame_height * factor;
    let frames = sheet.frames_at(factor);
    let frame = frames.get(index).or_else(|| frames.first())?;

    let mut canvas = Canvas::new(buf, buf_w, buf_h);
    canvas.clear();
    canvas.blit(
        frame,
        sprite_w,
        sprite_h,
        geo.mascot_x * oscale,
        geo.mascot_y * oscale,
    );
    let (bubble, text) = bubble?;
    let sprite_y = (geo.mascot_y * oscale) as i32;
    let area = BubbleArea {
        canvas_w: buf_w,
        canvas_h: buf_h,
        sprite_x: (geo.mascot_x * oscale) as i32,
        sprite_w: geo.mascot_w * oscale,
        content_top: sprite_y + (ink_top * factor) as i32,
        content_bottom: sprite_y + (ink_bottom * factor) as i32,
        above: geo.bubble_above,
        anchor_right: geo.anchor_right,
        scale: oscale,
    };
    Some(bubble::draw(bubble, text, &mut canvas, &area, now_ms))
}

pub(crate) fn sprite_scale_for(frame_height: u32) -> u32 {
    // TODO(render-v1): drive from config `[pet] scale`; env stub for v0.
    if let Some(scale) = std::env::var("AGENT_PET_SCALE")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
    {
        return scale.clamp(1, 8);
    }
    ((TARGET_LOGICAL_PX + frame_height / 2) / frame_height).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sprite_scale_targets_logical_size() {
        assert_eq!(sprite_scale_for(64), 2);
        assert_eq!(sprite_scale_for(208), 1);
        assert_eq!(sprite_scale_for(32), 4);
        assert_eq!(sprite_scale_for(1000), 1);
    }
}
