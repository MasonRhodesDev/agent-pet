//! Shared vocabulary for agent-pet: the wire event contract, the aggregated
//! snapshot handed to renderers/consumers, and session identity.
//!
//! This crate is a leaf: serde + std only. Every other crate depends on it;
//! it depends on nothing in the workspace.

pub mod event;
pub mod key;
pub mod snapshot;

pub use event::{AgentState, Event, Meta, Source, Via, PROTOCOL_VERSION};
pub use key::SessionKey;
pub use snapshot::{ActiveWindow, SessionView, Snapshot, UiAction};

/// Well-known D-Bus identity for the daemon.
pub const BUS_NAME: &str = "io.github.masonrhodesdev.AgentPet";
pub const OBJECT_PATH: &str = "/io/github/masonrhodesdev/AgentPet";
pub const INTERFACE: &str = "io.github.masonrhodesdev.AgentPet1";
