//! The mascot's layer surface. One fixed-size surface holds the sprite plus
//! a transparent bubble zone (no resize round-trips when the bubble toggles);
//! the input region covers only the sprite rect, so the bubble area stays
//! click-through. Anchored TOP|LEFT — margins are the position, and the
//! persisted position is the *mascot's* top-left, with the surface offset
//! derived from the layout quadrant.

use anyhow::{Context as _, Result};
use smithay_client_toolkit::compositor::{CompositorState, Region};
use smithay_client_toolkit::delegate_layer;
use smithay_client_toolkit::reexports::client::protocol::wl_output::WlOutput;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::WaylandSurface;
use tracing::{info, warn};

use crate::app::App;
use crate::input::router::{self, Rect};
use crate::surface::bubble;
use crate::surface::visibility::Visibility;

/// Gap from the screen edges for the default bottom-right placement.
pub const EDGE_MARGIN: i32 = 24;

/// Surface geometry mode. Docked is the resting mascot+bubble surface moved
/// by margins. Drag expands the surface to cover the whole output and holds
/// it STATIONARY (anchored all four edges, margins 0) so `wl_pointer`'s
/// surface-local coords equal output coords — the sprite is then drawn at an
/// internal pixel offset with no coordinate feedback. See input/drag.rs.
pub struct Mascot {
    pub layer: LayerSurface,
    /// Logical sprite size (frame size x sprite scale).
    pub mascot_w: u32,
    pub mascot_h: u32,
    /// Fixed logical surface size (sprite + bubble zone) in Docked mode.
    pub surf_w: u32,
    pub surf_h: u32,
    /// Sprite offset within the surface (layout quadrant in Docked; drag
    /// offset in Drag).
    pub mascot_x: u32,
    pub mascot_y: u32,
    /// Bubble sits above the sprite (else below, when near the top edge).
    pub bubble_above: bool,
    /// Sprite hugs the surface's right edge (bubble grows leftward).
    pub anchor_right: bool,
    /// Integer upscale applied to sprite pixels (independent of HiDPI).
    pub sprite_scale: u32,
    /// wl_output integer buffer scale.
    pub output_scale: i32,
    pub configured: bool,
    pub visibility: Visibility,
    pub entered: Option<WlOutput>,
    /// Bubble box (logical surface coords) while shown: click target and
    /// input-region member.
    pub bubble_rect: Option<Rect>,
    input_region: Option<Region>,
}

impl Mascot {
    pub fn create(
        compositor: &CompositorState,
        layer_shell: &LayerShell,
        qh: &QueueHandle<App>,
        mascot_w: u32,
        mascot_h: u32,
        sprite_scale: u32,
        visible: bool,
    ) -> Result<Self> {
        let surf_w = bubble::MAX_WIDTH.max(mascot_w);
        let surf_h = mascot_h + bubble::zone_height();
        let surface = compositor.create_surface(qh);
        let layer =
            layer_shell.create_layer_surface(qh, surface, Layer::Overlay, Some("agent-pet"), None);
        layer.set_anchor(Anchor::TOP | Anchor::LEFT);
        layer.set_size(surf_w, surf_h);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        // Initial commit with no buffer requests the first configure.
        layer.commit();
        let mut mascot = Self {
            layer,
            mascot_w,
            mascot_h,
            surf_w,
            surf_h,
            mascot_x: surf_w - mascot_w,
            mascot_y: surf_h - mascot_h,
            bubble_above: true,
            anchor_right: true,
            sprite_scale,
            output_scale: 1,
            configured: false,
            visibility: if visible {
                Visibility::Remapping
            } else {
                Visibility::Hidden
            },
            entered: None,
            bubble_rect: None,
            input_region: None,
        };
        mascot.update_input_region(compositor)?;
        Ok(mascot)
    }

    /// Current logical surface size (always the docked size — the pet moves
    /// via margins, it does not resize to drag).
    pub fn surface_size(&self) -> (u32, u32) {
        (self.surf_w, self.surf_h)
    }

    /// Pick the layout quadrant so the surface margins stay non-negative
    /// (the bubble flips below/right of the sprite near the top/left edges).
    /// `(mx, my)` is the mascot's top-left local to the surface's output.
    pub fn relayout(&mut self, (mx, my): (i32, i32)) {
        self.bubble_above = my >= (self.surf_h - self.mascot_h) as i32;
        self.anchor_right = mx >= (self.surf_w - self.mascot_w) as i32;
        self.mascot_x = if self.anchor_right {
            self.surf_w - self.mascot_w
        } else {
            0
        };
        self.mascot_y = if self.bubble_above {
            self.surf_h - self.mascot_h
        } else {
            0
        };
    }

    /// Derive surface margins from the mascot's output-local top-left (takes
    /// effect on the next commit).
    pub fn apply_margins(&self, (mx, my): (i32, i32)) {
        let left = mx - self.mascot_x as i32;
        let top = my - self.mascot_y as i32;
        self.layer.set_margin(top, 0, 0, left);
    }

    pub fn sprite_rect(&self) -> Rect {
        Rect {
            x: self.mascot_x as i32,
            y: self.mascot_y as i32,
            w: self.mascot_w,
            h: self.mascot_h,
        }
    }

    /// Docked: input region = sprite rect + the bubble box while shown
    /// (tail/gap and the rest of the canvas stay click-through). Drag: the
    /// whole surface, so the pointer can never leave the input region.
    pub fn update_input_region(&mut self, compositor: &CompositorState) -> Result<()> {
        let region = Region::new(compositor).context("create input region")?;
        for rect in router::input_rects(self.sprite_rect(), self.bubble_rect) {
            region.add(rect.x, rect.y, rect.w as i32, rect.h as i32);
        }
        self.layer.set_input_region(Some(region.wl_region()));
        self.input_region = Some(region);
        Ok(())
    }

    /// Re-request the mapped state after an unmap (the protocol requires a
    /// fresh initial commit + configure before a buffer may attach again).
    pub fn request_remap(&mut self, margins: (i32, i32)) {
        self.configured = false;
        self.visibility = Visibility::Remapping;
        self.layer.set_size(self.surf_w, self.surf_h);
        self.layer.set_anchor(Anchor::TOP | Anchor::LEFT);
        self.layer.set_exclusive_zone(-1);
        self.layer
            .set_keyboard_interactivity(KeyboardInteractivity::None);
        self.apply_margins(margins);
        self.layer.commit();
    }

    /// Unmap by committing a null buffer.
    pub fn unmap(&mut self) {
        self.layer.attach(None, 0, 0);
        self.layer.commit();
        self.configured = false;
        self.visibility = Visibility::Hidden;
        // The next map may land on a different output; re-latch it.
        self.entered = None;
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        // Output gone or compositor shut the surface: bail out of the event
        // loop and let the supervisor rebuild everything.
        warn!("layer surface closed by compositor");
        self.error = Some(anyhow::anyhow!("layer surface closed"));
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let first = !self.mascot.configured;
        self.mascot.configured = true;
        let _ = configure;
        self.ensure_position();
        self.sync_layout();
        if first {
            info!(
                sprite_w = self.mascot.mascot_w,
                sprite_h = self.mascot.mascot_h,
                surf_w = self.mascot.surf_w,
                surf_h = self.mascot.surf_h,
                visible = self.mascot.visibility != Visibility::Hidden,
                "mascot surface configured"
            );
        }
        match self.mascot.visibility {
            Visibility::Hidden => {}
            Visibility::Remapping => {
                self.mascot.visibility = Visibility::Visible;
                self.maybe_greet();
                self.render_frame();
                self.ensure_timer();
            }
            Visibility::Visible => self.render_frame(),
        }
    }
}

delegate_layer!(App);
