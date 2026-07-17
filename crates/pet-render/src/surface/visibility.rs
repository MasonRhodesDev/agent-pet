//! Mascot visibility. Hidden = unmapped (null buffer committed); remapping
//! requires a fresh configure round-trip per the layer-shell protocol, so
//! show() passes through `Remapping` until that configure lands.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    #[default]
    Visible,
    Hidden,
    /// Waiting for the post-unmap configure before attaching a buffer.
    Remapping,
}

impl Visibility {
    pub fn shown(self) -> bool {
        matches!(self, Visibility::Visible)
    }
}
