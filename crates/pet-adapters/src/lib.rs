//! Harness-specific payload → `pet_proto::Event` mapping.
//!
//! Everything here is pure (JSON in, events out); the emitting/polling I/O
//! lives in pet-cli and pet-daemon. One module per harness — adding a
//! harness means adding a module, not touching the others.

pub mod claude;
pub mod hygiene;
pub mod codex;
pub mod gastown;
