//! Global binding + the protocol handler impls that are not surface or
//! input logic.

use anyhow::{Context as _, Result};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputInfo, OutputState};
use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::protocol::{wl_output, wl_surface};
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::pointer::cursor_shape::CursorShapeManager;
use smithay_client_toolkit::seat::SeatState;
use smithay_client_toolkit::shell::wlr_layer::LayerShell;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_output, delegate_registry, delegate_shm, registry_handlers,
};
use tracing::debug;

use crate::app::App;

pub struct Bound {
    pub compositor: CompositorState,
    pub layer_shell: LayerShell,
    pub shm: Shm,
    pub registry_state: RegistryState,
    pub output_state: OutputState,
    pub seat_state: SeatState,
    /// None when the compositor lacks cursor-shape-v1 (cursor stays default;
    /// TODO(render-v1): wayland-cursor theme fallback).
    pub cursor_shapes: Option<CursorShapeManager>,
}

pub fn bind(globals: &GlobalList, qh: &QueueHandle<App>) -> Result<Bound> {
    Ok(Bound {
        compositor: CompositorState::bind(globals, qh).context("wl_compositor not available")?,
        layer_shell: LayerShell::bind(globals, qh)
            .context("zwlr_layer_shell_v1 not available (compositor lacks layer-shell)")?,
        shm: Shm::bind(globals, qh).context("wl_shm not available")?,
        registry_state: RegistryState::new(globals),
        output_state: OutputState::new(globals, qh),
        seat_state: SeatState::new(globals, qh),
        cursor_shapes: CursorShapeManager::bind(globals, qh).ok(),
    })
}

/// Logical size of an output, falling back to mode/scale when the compositor
/// does not send xdg-output logical geometry.
pub fn logical_size(info: &OutputInfo) -> Option<(i32, i32)> {
    info.logical_size.or_else(|| {
        let scale = info.scale_factor.max(1);
        info.modes
            .iter()
            .find(|m| m.current)
            .map(|m| (m.dimensions.0 / scale, m.dimensions.1 / scale))
    })
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        // Integer buffer scale only in v0. TODO(render-v1): bind
        // wp_fractional_scale_v1 + wp_viewporter for fractional outputs
        // (SCTK 0.20 has no helper; ~80 lines of manual protocol).
        debug!(new_factor, "buffer scale changed");
        self.set_output_scale(new_factor);
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        // Animation is timer-driven at sprite cadence; frame callbacks are
        // only requested by the drag render loop.
        self.on_frame_callback();
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        output: &wl_output::WlOutput,
    ) {
        self.mascot.entered = Some(output.clone());
        self.ensure_position();
        self.sync_layout();
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
        self.ensure_position();
        self.sync_layout();
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
        self.ensure_position();
        self.sync_layout();
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
        // The compositor sends `closed` on the layer surface if its output
        // goes away; recreation happens through the supervisor.
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_registry!(App);
