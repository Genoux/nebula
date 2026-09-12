//! Local wake-word detection. No audio leaves the machine here, and nothing in
//! this crate can make a network call - that is a property of its dependency
//! list, not a promise in a comment.
//!
//! # Why this is hand-written
//!
//! The intended dependency was `rustpotter`, which does exactly this. Its only
//! usable release (3.0.2) depends on `candle-core` 0.2.2, which does not
//! compile against current `rand`/`half`; versions 1.x and 2.x are yanked. So
//! the reference-template half of what rustpotter does - MFCC features compared
//! with dynamic time warping - is implemented here directly. It is the same
//! approach, it needs no vendor key, and it trains from a handful of recordings
//! of the user's own voice.
//!
//! [`WakeWordDetector`] is the seam. A Porcupine or openWakeWord adapter is a
//! new implementation of this trait and nothing else.

pub mod detector;
pub mod dtw;
pub mod mfcc;
pub mod model;

pub use detector::{Dtw, train, trim_silence};
pub use mfcc::SAMPLE_RATE;
pub use model::Model;

use anyhow::Result;

#[derive(Debug, Clone, PartialEq)]
pub struct WakeEvent {
    /// 0..=1, compared against the configured sensitivity.
    pub score: f32,
    pub phrase: String,
}

/// The replaceable wake-word interface.
///
/// Sketched in the design as `async fn start() -> WakeEvents`, and deliberately
/// not that: a detector that owns its own capture stream cannot be the thing
/// that guarantees standby holds no second microphone stream, and every engine
/// worth swapping in is a synchronous "feed me samples, I will tell you"
/// function underneath. The daemon owns the audio; this owns the decision.
pub trait WakeWordDetector: Send {
    /// Feed the next chunk of 16kHz mono audio.
    fn process(&mut self, samples: &[f32]) -> Option<WakeEvent>;

    /// Stop listening, so Nebula's own confirmation tone and replies cannot
    /// wake it.
    fn pause(&mut self) -> Result<()>;

    fn resume(&mut self) -> Result<()>;

    fn is_paused(&self) -> bool;
}
