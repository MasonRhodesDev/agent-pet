//! Compositor-specific behavior seam. Rendering/input use portable Wayland
//! protocols; compositor-specific *facts* (which toplevel is active, and
//! later fullscreen state) come from a `CompositorBackend`.
//!
//! Two backends produce the same `Option<ActiveWindow>` fact stream:
//!   - `hyprland`: socket2 event stream + socket1 `j/activewindow` queries,
//!     on a bridge thread (the same socket2 reader will feed fullscreen
//!     auto-hide later — see `hyprland::events`).
//!   - `wlr_generic`: `zwlr_foreign_toplevel_management_v1`, tracking the
//!     `activated` toplevel on the renderer's own Wayland connection.
//! Raw facts land in a `calloop::channel`; the renderer debounces them
//! (`settle`) before emitting `UiAction::ActiveWindowChanged`.

pub mod hyprland;
pub mod settle;
pub mod wlr_generic;

use pet_proto::ActiveWindow;
use smithay_client_toolkit::reexports::calloop::channel::Sender;
use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::QueueHandle;

use crate::app::App;

/// Undebounced active-window facts flow to the renderer through this sink.
pub type FactSink = Sender<Option<ActiveWindow>>;

/// Everything a backend might need to start its source: the Wayland globals
/// and queue handle (wlr binds a protocol on the live connection) and the
/// fact sink (Hyprland's socket thread and wlr's Dispatch both push here).
pub struct SourceCtx<'a> {
    pub globals: &'a GlobalList,
    pub qh: &'a QueueHandle<App>,
    pub sink: FactSink,
}

/// Keeps a started source alive; dropping it tears the source down (signals
/// the Hyprland thread to stop / releases the wlr manager).
pub enum ActiveWindowSource {
    Hyprland(hyprland::Source),
    Wlr(wlr_generic::Source),
    /// Backend present but its source is unavailable (e.g. wlr global
    /// absent). Degrades silently — no facts ever emitted.
    None,
}

pub trait CompositorBackend {
    fn name(&self) -> &'static str;

    /// Begin reporting the active toplevel into `ctx.sink`. Never blocks the
    /// caller; failures degrade to `ActiveWindowSource::None`.
    fn start_active_window_source(&self, ctx: SourceCtx) -> ActiveWindowSource;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Hyprland,
    WlrGeneric,
}

pub fn detect() -> Kind {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        Kind::Hyprland
    } else {
        Kind::WlrGeneric
    }
}

/// The backend for the running compositor.
pub fn backend() -> Box<dyn CompositorBackend> {
    match detect() {
        Kind::Hyprland => Box::new(hyprland::Hyprland),
        Kind::WlrGeneric => Box::new(wlr_generic::WlrGeneric),
    }
}
