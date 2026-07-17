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

/// Surface geometry mode. Docked is the resting mascot+bubble surface moved
/// by margins. Drag expands the surface to cover the whole output and holds
/// it STATIONARY (anchored all four edges, margins 0) so `wl_pointer`'s
/// surface-local coords equal output coords — the sprite is then drawn at an
/// internal pixel offset with no coordinate feedback. See input/drag.rs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceMode {
    Docked,
    Drag,
}

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
    pub mode: SurfaceMode,
    /// Awaiting the configure for a pending size change (Docked<->Drag);
    /// rendering is suppressed until the surface size is acked so the buffer
    /// always matches the committed surface size.
    pub resizing: bool,
    /// Full-output logical size while in Drag mode (from the drag configure).
    pub drag_dims: (u32, u32),
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
            mode: SurfaceMode::Docked,
            resizing: false,
            drag_dims: (surf_w, surf_h),
            entered: None,
            bubble_rect: None,
            input_region: None,
        };
        mascot.update_input_region(compositor)?;
        Ok(mascot)
    }

    /// Current logical surface size, mode-dependent.
    pub fn surface_size(&self) -> (u32, u32) {
        match self.mode {
            SurfaceMode::Docked => (self.surf_w, self.surf_h),
            SurfaceMode::Drag => self.drag_dims,
        }
    }

    /// Expand to a stationary full-output surface for dragging: anchor all
    /// four edges with size 0 (the compositor fills the output and reports
    /// the size in the configure), margins 0, whole-surface input region.
    /// The pointer can never escape and surface-local == output coords.
    pub fn enter_drag(&mut self, compositor: &CompositorState) -> Result<()> {
        self.mode = SurfaceMode::Drag;
        self.resizing = true;
        self.layer
            .set_anchor(Anchor::TOP | Anchor::LEFT | Anchor::BOTTOM | Anchor::RIGHT);
        self.layer.set_size(0, 0);
        self.layer.set_margin(0, 0, 0, 0);
        self.layer.set_exclusive_zone(-1);
        self.update_input_region(compositor)?;
        self.layer.commit();
        Ok(())
    }

    /// Shrink back to the docked mascot surface at `pos` (mascot top-left on
    /// the output). Rendering resumes on the ensuing configure.
    pub fn exit_drag(&mut self, compositor: &CompositorState, pos: &Position) -> Result<()> {
        self.mode = SurfaceMode::Docked;
        self.resizing = true;
        self.relayout(pos);
        self.layer.set_anchor(Anchor::TOP | Anchor::LEFT);
        self.layer.set_size(self.surf_w, self.surf_h);
        self.layer.set_exclusive_zone(-1);
        self.apply_margins(pos);
        self.update_input_region(compositor)?;
        self.layer.commit();
        Ok(())
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

    /// Docked: input region = sprite rect + the bubble box while shown
    /// (tail/gap and the rest of the canvas stay click-through). Drag: the
    /// whole surface, so the pointer can never leave the input region.
    pub fn update_input_region(&mut self, compositor: &CompositorState) -> Result<()> {
        let region = Region::new(compositor).context("create input region")?;
        match self.mode {
            SurfaceMode::Docked => {
                for rect in router::input_rects(self.sprite_rect(), self.bubble_rect) {
                    region.add(rect.x, rect.y, rect.w as i32, rect.h as i32);
                }
            }
            SurfaceMode::Drag => {
                let (w, h) = self.drag_dims;
                region.add(0, 0, w as i32, h as i32);
            }
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
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let first = !self.mascot.configured;
        self.mascot.configured = true;

        // Drag mode: the surface is the full output (size from the
        // compositor). Ack it and render the drag frame; never run the
        // docked layout while expanded.
        if self.mascot.mode == SurfaceMode::Drag {
            let (w, h) = configure.new_size;
            if w > 0 && h > 0 {
                self.mascot.drag_dims = (w, h);
            }
            self.mascot.resizing = false;
            self.render_frame();
            return;
        }

        self.mascot.resizing = false;
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
