//! Microphone capture at whatever rate the caller asks for.
//!
//! The rate is a parameter rather than a constant because Nebula wants two
//! different ones and never both at once: the wake detector runs at 16kHz in
//! standby, and the GPT-Live session wants 24kHz. Opening a new stream at the
//! new rate on every state change sounds wasteful and is not - it is what makes
//! "no microphone audio leaves the machine in standby" a property of the code
//! rather than a promise, because in standby the 24kHz stream does not exist.

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

use crate::route;

/// Mono f32, because everything downstream wants one channel and cpal's ALSA
/// path is talking to PipeWire, which downmixes and resamples for free. Asking
/// the hardware for this directly would fail; asking PipeWire for it works.
pub const CHANNELS: u16 = 1;

/// How many chunks may queue before the oldest is dropped.
///
/// Small on purpose. If the consumer stalls, the useful recovery is to lose the
/// audio and stay current, not to deliver a growing backlog of stale speech to
/// a conversation that has moved on.
const BACKLOG: usize = 64;

pub struct Capture {
    _stream: cpal::Stream,
    chunks: Receiver<Vec<f32>>,
    level: Arc<Level>,
    routed_to: Option<String>,
    config: StreamConfig,
}

/// Loudness of the most recent callback, for the HUD. An atomic rather than a
/// lock because the audio callback writes it on every buffer and the UI reads
/// it whenever it happens to draw; neither should ever wait for the other.
#[derive(Default)]
pub struct Level(AtomicU64);

impl Level {
    pub(crate) fn set(&self, peak: f32) {
        self.0.store(peak.to_bits() as u64, Ordering::Relaxed);
    }

    /// 0.0..=1.0 peak of the last buffer.
    pub fn peak(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed) as u32)
    }
}

impl Capture {
    /// `device` is a PipeWire node name, or `default` to follow WirePlumber.
    pub fn open(sample_rate: u32, device: &str, buffer_frames: u32) -> Result<Self> {
        if let Some(target) = route::wanted(device) {
            route::unsuspend_source(target);
        } else {
            route::unsuspend_source("@DEFAULT_SOURCE@");
        }

        let cpal_device = cpal::default_host()
            .default_input_device()
            .ok_or_else(|| anyhow!("no input device - is PipeWire running?"))?;

        let config = StreamConfig {
            channels: CHANNELS,
            sample_rate,
            buffer_size: cpal::BufferSize::Fixed(buffer_frames),
        };

        let (tx, chunks) = sync_channel::<Vec<f32>>(BACKLOG);
        let level = Arc::new(Level::default());
        let stream = build(&cpal_device, &config, tx, Arc::clone(&level))?;
        stream
            .play()
            .context("could not start the capture stream")?;

        // After `play`, not before: the stream has no node in the graph to move
        // until it is running, so a pinned microphone can only be honoured here.
        let routed_to = if let Some(target) = route::wanted(device) {
            route::move_own_stream(route::Direction::Capture, target)
                .context("could not route the configured audio device")?;
            Some(target.to_string())
        } else {
            None
        };

        Ok(Self {
            _stream: stream,
            chunks,
            level,
            routed_to,
            config,
        })
    }

    /// Blocks until the next buffer arrives, or `None` once the stream is gone.
    pub fn recv(&self) -> Option<Vec<f32>> {
        self.chunks.recv().ok()
    }

    pub fn try_recv(&self) -> Option<Vec<f32>> {
        self.chunks.try_recv().ok()
    }

    pub fn level(&self) -> Arc<Level> {
        Arc::clone(&self.level)
    }

    pub fn routed_to(&self) -> Option<&str> {
        self.routed_to.as_deref()
    }

    pub fn config(&self) -> &StreamConfig {
        &self.config
    }
}

/// cpal hands the callback whatever format the device negotiated, so each one
/// is converted to f32 here rather than anywhere later.
fn build(
    device: &cpal::Device,
    config: &StreamConfig,
    tx: SyncSender<Vec<f32>>,
    level: Arc<Level>,
) -> Result<cpal::Stream> {
    let format = device
        .default_input_config()
        .context("device reported no default input config")?
        .sample_format();

    let on_error = |e| tracing::error!("capture stream error: {e}");

    let stream = match format {
        SampleFormat::F32 => device.build_input_stream(
            *config,
            move |data: &[f32], _: &_| deliver(data.to_vec(), &tx, &level),
            on_error,
            None,
        ),
        SampleFormat::I16 => device.build_input_stream(
            *config,
            move |data: &[i16], _: &_| {
                deliver(
                    data.iter().map(|&s| s as f32 / 32768.0).collect(),
                    &tx,
                    &level,
                )
            },
            on_error,
            None,
        ),
        SampleFormat::U16 => device.build_input_stream(
            *config,
            move |data: &[u16], _: &_| {
                deliver(
                    data.iter()
                        .map(|&s| (s as f32 - 32768.0) / 32768.0)
                        .collect(),
                    &tx,
                    &level,
                )
            },
            on_error,
            None,
        ),
        other => return Err(anyhow!("unsupported capture sample format {other:?}")),
    }
    .context("could not open the microphone")?;

    Ok(stream)
}

/// Runs on the audio thread. Allocation-free apart from the buffer being handed
/// over, and never blocks: a full queue drops this buffer rather than stalling
/// the device.
fn deliver(samples: Vec<f32>, tx: &SyncSender<Vec<f32>>, level: &Level) {
    level.set(peak(&samples));
    let _ = tx.try_send(samples);
}

pub fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |hi, s| hi.max(s.abs()))
}

pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_is_the_largest_magnitude() {
        assert_eq!(peak(&[0.1, -0.7, 0.3]), 0.7);
        assert_eq!(peak(&[]), 0.0);
    }

    #[test]
    fn rms_of_silence_is_zero_and_of_empty_is_too() {
        assert_eq!(rms(&[0.0; 16]), 0.0);
        assert_eq!(rms(&[]), 0.0);
    }

    #[test]
    fn rms_of_a_constant_is_that_constant() {
        assert!((rms(&[0.5; 100]) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn level_round_trips_through_its_atomic() {
        let level = Level::default();
        assert_eq!(level.peak(), 0.0);
        level.set(0.375);
        assert_eq!(level.peak(), 0.375);
    }
}
