//! Compositor-specific behavior seam. v0 renders through portable protocols
//! only; later milestones plug fullscreen auto-hide and click-to-focus in
//! behind this trait (wlr_generic via zwlr_foreign_toplevel_management_v1,
//! hyprland via socket2 events + socket1 queries).

pub trait CompositorBackend {
    fn name(&self) -> &'static str;
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
