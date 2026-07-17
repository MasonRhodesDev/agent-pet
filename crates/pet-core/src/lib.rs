//! The agent-pet aggregator: a pure state machine.
//!
//! No tokio, no zbus, no clocks, no I/O. The daemon shell feeds `step()`
//! inputs with an injected timestamp and executes the returned effects;
//! everything here is deterministic and unit-testable.

pub mod expiry;
pub mod fsm;
pub mod identity;
pub mod model;
pub mod reduce;

pub use expiry::Ttls;
pub use fsm::step;
pub use model::{Effect, Input, Model, Origin, SessionFsm};
pub use reduce::{next_deadline, reduce, READY_PRESENT_MS};
