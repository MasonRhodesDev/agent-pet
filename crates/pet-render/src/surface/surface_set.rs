//! Per-output mascot surfaces. A wlr layer surface is bound to one output at
//! creation and cannot span or move between outputs, so "any monitor" means
//! one surface per output, all managed here: the pet is drawn on the surface
//! of the *active* output (the one whose rect contains its global position);
//! the rest stay blank and click-through.
//!
//! Each surface is the same fixed-size canvas: the sprite plus a transparent
//! bubble zone (no resize round-trips when the bubble toggles). The input
//! region covers only the sprite rect (+ bubble box while shown), so the
//! rest stays click-through. Anchored TOP|LEFT — margins are the position,
//! and the persisted position is the *mascot's* top-left, with the surface
//! offset derived from the layout quadrant.

use anyhow::{Context as _, Result};
use smithay_client_toolkit::compositor::{CompositorState, Region};
use smithay_client_toolkit::delegate_layer;
use smithay_client_toolkit::reexports::client::protocol::wl_output::WlOutput;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
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

/// One mascot layer surface, bound to `output` for its whole life.
pub struct OutputSurface {
    pub output: WlOutput,
    pub layer: LayerSurface,
    pub configured: bool,
    /// wl_output integer buffer scale for this surface's output.
    pub scale: i32,
    input_region: Option<Region>,
}

/// All mascot surfaces (one per output) plus the shared canvas geometry.
/// Which surface draws the pet is the app's call (`active_output`); the set
/// only manages lifecycles and per-surface protocol state.
pub struct SurfaceSet {
    surfaces: Vec<OutputSurface>,
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
    /// Set-wide desired visibility; per-surface mapping state is `configured`.
    pub visibility: Visibility,
    /// Bubble box (logical surface coords) while shown: click target and
    /// input-region member. Meaningful on the active surface only.
    pub bubble_rect: Option<Rect>,
}

impl SurfaceSet {
    pub fn new(mascot_w: u32, mascot_h: u32, sprite_scale: u32, visible: bool) -> Self {
        let surf_w = bubble::MAX_WIDTH.max(mascot_w);
        let surf_h = mascot_h + bubble::zone_height();
        Self {
            surfaces: Vec::new(),
            mascot_w,
            mascot_h,
            surf_w,
            surf_h,
            mascot_x: surf_w - mascot_w,
            mascot_y: surf_h - mascot_h,
            bubble_above: true,
            anchor_right: true,
            sprite_scale,
            visibility: if visible {
                Visibility::Remapping
            } else {
                Visibility::Hidden
            },
            bubble_rect: None,
        }
    }

    /// Create the layer surface for `output` (no-op if it already has one).
    /// The initial commit with no buffer requests the first configure; a
    /// buffer is only ever attached to the active surface.
    pub fn add_for_output(
        &mut self,
        compositor: &CompositorState,
        layer_shell: &LayerShell,
        qh: &QueueHandle<App>,
        output: &WlOutput,
    ) -> Result<()> {
        if self.by_output(output).is_some() {
            return Ok(());
        }
        let surface = compositor.create_surface(qh);
        let layer = layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Overlay,
            Some("agent-pet"),
            Some(output),
        );
        layer.set_anchor(Anchor::TOP | Anchor::LEFT);
        layer.set_size(self.surf_w, self.surf_h);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.commit();
        let mut os = OutputSurface {
            output: output.clone(),
            layer,
            configured: false,
            scale: 1,
            input_region: None,
        };
        // Surfaces start inactive: fully click-through. The active surface
        // gets its real input region via sync_layout.
        set_input_region(&mut os, compositor, &[])?;
        self.surfaces.push(os);
        Ok(())
    }

    /// Drop the surface bound to `output` (destroys the protocol objects).
    /// Returns whether one existed.
    pub fn remove_output(&mut self, output: &WlOutput) -> bool {
        let before = self.surfaces.len();
        self.surfaces.retain(|os| os.output != *output);
        self.surfaces.len() != before
    }

    pub fn by_output(&self, output: &WlOutput) -> Option<&OutputSurface> {
        self.surfaces.iter().find(|os| os.output == *output)
    }

    pub fn by_surface(&self, surface: &WlSurface) -> Option<&OutputSurface> {
        self.surfaces
            .iter()
            .find(|os| os.layer.wl_surface() == surface)
    }

    pub fn by_surface_mut(&mut self, surface: &WlSurface) -> Option<&mut OutputSurface> {
        self.surfaces
            .iter_mut()
            .find(|os| os.layer.wl_surface() == surface)
    }

    pub fn first_output(&self) -> Option<WlOutput> {
        self.surfaces.first().map(|os| os.output.clone())
    }

    /// Current logical surface size (always the docked size — the pet moves
    /// via margins, it does not resize to drag).
    pub fn surface_size(&self) -> (u32, u32) {
        (self.surf_w, self.surf_h)
    }

    /// Pick the layout quadrant so the surface margins stay non-negative
    /// (the bubble flips below/right of the sprite near the top/left edges).
    /// `(mx, my)` is the mascot's top-left local to the active output.
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

    pub fn sprite_rect(&self) -> Rect {
        Rect {
            x: self.mascot_x as i32,
            y: self.mascot_y as i32,
            w: self.mascot_w,
            h: self.mascot_h,
        }
    }

    /// Derive `output`'s surface margins from the mascot's output-local
    /// top-left (takes effect on the next commit).
    pub fn apply_margins(&self, output: &WlOutput, (mx, my): (i32, i32)) {
        let Some(os) = self.by_output(output) else {
            return;
        };
        let left = mx - self.mascot_x as i32;
        let top = my - self.mascot_y as i32;
        os.layer.set_margin(top, 0, 0, left);
    }

    pub fn commit(&self, output: &WlOutput) {
        if let Some(os) = self.by_output(output) {
            os.layer.commit();
        }
    }

    /// Input region = sprite rect + the bubble box while shown (tail/gap and
    /// the rest of the canvas stay click-through), on `output`'s surface.
    pub fn update_input_region(
        &mut self,
        output: &WlOutput,
        compositor: &CompositorState,
    ) -> Result<()> {
        let rects = router::input_rects(self.sprite_rect(), self.bubble_rect);
        let Some(os) = self.surfaces.iter_mut().find(|os| os.output == *output) else {
            return Ok(());
        };
        set_input_region(os, compositor, &rects)
    }

    /// Make `output`'s surface fully click-through (an inactive surface).
    pub fn clear_input_region(
        &mut self,
        output: &WlOutput,
        compositor: &CompositorState,
    ) -> Result<()> {
        let Some(os) = self.surfaces.iter_mut().find(|os| os.output == *output) else {
            return Ok(());
        };
        set_input_region(os, compositor, &[])
    }

    /// Unmap every surface by committing null buffers. Remapping requires a
    /// fresh initial commit + configure per surface.
    pub fn unmap_all(&mut self) {
        for os in &mut self.surfaces {
            os.layer.attach(None, 0, 0);
            os.layer.commit();
            os.configured = false;
        }
        self.visibility = Visibility::Hidden;
    }

    /// Re-request the mapped state on every unmapped surface. Margins are
    /// applied to the active output's surface only — the others never carry
    /// a buffer until they become active.
    pub fn request_remap_all(&mut self, active: Option<&WlOutput>, margins: (i32, i32)) {
        let (surf_w, surf_h) = (self.surf_w, self.surf_h);
        let (mascot_x, mascot_y) = (self.mascot_x as i32, self.mascot_y as i32);
        for os in &mut self.surfaces {
            if os.configured {
                continue;
            }
            os.layer.set_size(surf_w, surf_h);
            os.layer.set_anchor(Anchor::TOP | Anchor::LEFT);
            os.layer.set_exclusive_zone(-1);
            os.layer
                .set_keyboard_interactivity(KeyboardInteractivity::None);
            let (mx, my) = if Some(&os.output) == active {
                margins
            } else {
                (mascot_x, mascot_y) // zero surface margins
            };
            os.layer.set_margin(my - mascot_y, 0, 0, mx - mascot_x);
            os.layer.commit();
        }
    }
}

fn set_input_region(
    os: &mut OutputSurface,
    compositor: &CompositorState,
    rects: &[Rect],
) -> Result<()> {
    // An empty region (no rects added) means fully click-through.
    let region = Region::new(compositor).context("create input region")?;
    for rect in rects {
        region.add(rect.x, rect.y, rect.w as i32, rect.h as i32);
    }
    os.layer.set_input_region(Some(region.wl_region()));
    os.input_region = Some(region);
    Ok(())
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
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let _ = configure;
        let Some(os) = self.surfaces.by_surface_mut(layer.wl_surface()) else {
            return;
        };
        let first = !os.configured;
        os.configured = true;
        let output = os.output.clone();
        if first {
            info!(
                sprite_w = self.surfaces.mascot_w,
                sprite_h = self.surfaces.mascot_h,
                surf_w = self.surfaces.surf_w,
                surf_h = self.surfaces.surf_h,
                visible = self.surfaces.visibility != Visibility::Hidden,
                "mascot surface configured"
            );
        }
        self.ensure_position();
        self.resolve_active();
        if self.active_output.as_ref() != Some(&output) {
            // An inactive surface: keep it mapped but invisible so the 2b
            // seam hand-off never waits on a configure round-trip.
            self.blank_surface(&output);
        }
        self.sync_layout();
        self.sync_active();
    }
}

delegate_layer!(App);
