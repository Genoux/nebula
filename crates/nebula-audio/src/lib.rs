//! Microphone capture, speaker playback, local tones, and PipeWire routing.
//!
//! Nothing here knows what a session is. The daemon opens and drops these as
//! the state machine moves, which is what keeps "no cloud audio in standby"
//! structural: in standby the 24kHz capture stream has not been created.

pub mod capture;
pub mod playback;
pub mod route;
pub mod tone;

pub use capture::{Capture, Level, peak, rms};
pub use playback::{Playback, Queue};
pub use tone::Tone;

pub const LIVE_RATE: u32 = 24_000;
