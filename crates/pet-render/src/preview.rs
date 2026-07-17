//! Offline scene renderer for dev tooling (`examples/preview_bubble.rs`):
//! runs the real compose path headlessly so bubble placement can be
//! pixel-verified without a compositor.

use std::path::Path;

use anyhow::Result;
use pet_proto::{AgentState, SessionKey, Source};

use crate::compose::{self, Geometry};
use crate::sprite::pet_json::PetDef;
use crate::sprite::semantics;
use crate::sprite::sheet::Sheet;
use crate::sprite::timeline::Timeline;
use crate::surface::bubble::{self, Bubble};
use crate::text::TextRenderer;

/// Render one frame exactly as the live renderer would (default bottom-right
/// quadrant, scale 1). Returns straight-alpha RGBA plus dimensions.
pub fn render_scene(
    pet_dir: &Path,
    state: AgentState,
    body: Option<&str>,
    elapsed_ms: u64,
) -> Result<(Vec<u8>, u32, u32)> {
    let pet = PetDef::load(pet_dir)?;
    let mut sheet = Sheet::load(&pet)?;
    let mut timeline = Timeline::new(&pet, 0);
    timeline.request_state(semantics::track_for(state, &pet), 0);
    timeline.advance(elapsed_ms);

    let sprite_scale = compose::sprite_scale_for(pet.frame_height);
    let (mascot_w, mascot_h) = (pet.frame_width * sprite_scale, pet.frame_height * sprite_scale);
    let surf_w = bubble::MAX_WIDTH.max(mascot_w);
    let surf_h = mascot_h + bubble::zone_height();
    let geo = Geometry {
        surf_w,
        surf_h,
        mascot_x: surf_w - mascot_w,
        mascot_y: surf_h - mascot_h,
        mascot_w,
        bubble_above: true,
        anchor_right: true,
        sprite_scale,
        oscale: 1,
    };

    let bubble = body.map(|body| {
        Bubble::new(
            SessionKey::new(Source::Other, "preview"),
            state.label(),
            body.to_string(),
            0,
        )
    });
    let mut text = TextRenderer::new();

    let (buf_w, buf_h) = geo.buf_size();
    let mut buf = vec![0u8; (buf_w * buf_h * 4) as usize];
    let _bubble_rect = compose::scene(
        &mut buf,
        &geo,
        &mut sheet,
        &timeline,
        bubble.as_ref().map(|b| (b, &mut text)),
        elapsed_ms,
    );

    // Premultiplied BGRA -> straight RGBA for PNG output.
    for px in buf.chunks_exact_mut(4) {
        let (b, g, r, a) = (px[0], px[1], px[2], px[3]);
        let un = |c: u8| -> u8 {
            if a == 0 {
                0
            } else {
                ((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8
            }
        };
        px[0] = un(r);
        px[1] = un(g);
        px[2] = un(b);
        px[3] = a;
    }
    Ok((buf, buf_w, buf_h))
}
