//! Shared vocabulary for every Nebula process: where things live on disk, what
//! the daemon is doing right now, and how the UI is told about it.
//!
//! Nothing here talks to a device, a socket, or OpenAI. That is deliberate -
//! the state machine is the part most worth testing, and it stays testable
//! only as long as it cannot perform I/O.

pub mod config;
pub mod event;
pub mod paths;
pub mod secrets;
pub mod state;

pub use event::{Event, UiStatus};
pub use state::{State, Transition};
