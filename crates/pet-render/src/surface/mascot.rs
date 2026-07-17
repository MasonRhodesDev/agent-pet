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
use crate::surface::position::Position;
use crate::surface::visibility::Visibility;

/// Gap from the screen edges for the default bottom-right placement.
pub const EDGE_MARGIN: i32 = 24;

pub struct Mascot {
    pub layer: LayerSurface,
    /// Logical sprite size (frame size x sprite scale).
    pub mascot_w: u32,
    pub mascot_h: u32,
    /// Fixed logical surface size (sprite + bubble zone).
    pub surf_w: u32,
    pub surf_h: u32,
    /// Sprite offset within the surface (layout quadrant).
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

    /// Pick the layout quadrant so the surface margins stay non-negative
    /// (the bubble flips below/right of the sprite near the top/left edges).
    pub fn relayout(&mut self, pos: &Position) {
        self.bubble_above = pos.margin_y >= (self.surf_h - self.mascot_h) as i32;
        self.anchor_right = pos.margin_x >= (self.surf_w - self.mascot_w) as i32;
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

    /// Derive surface margins from the mascot position (takes effect on the
    /// next commit).
    pub fn apply_margins(&self, pos: &Position) {
        let left = pos.margin_x - self.mascot_x as i32;
        let top = pos.margin_y - self.mascot_y as i32;
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

    /// Input region = sprite rect, plus the bubble box while it is shown
    /// (tail/gap and the rest of the canvas stay click-through).
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
    pub fn request_remap(&mut self, pos: &Position) {
        self.configured = false;
        self.visibility = Visibility::Remapping;
        self.layer.set_size(self.surf_w, self.surf_h);
        self.layer.set_anchor(Anchor::TOP | Anchor::LEFT);
        self.layer.set_exclusive_zone(-1);
        self.layer
            .set_keyboard_interactivity(KeyboardInteractivity::None);
        self.apply_margins(pos);
        self.layer.commit();
    }

    /// Unmap by committing a null buffer.
    pub fn unmap(&mut self) {
        self.layer.attach(None, 0, 0);
        self.layer.commit();
        self.configured = false;
        self.visibility = Visibility::Hidden;
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
        _configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let first = !self.mascot.configured;
        self.mascot.configured = true;
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
                self.render_frame();
                self.ensure_timer();
            }
            Visibility::Visible => self.render_frame(),
        }
    }
}

delegate_layer!(App);
