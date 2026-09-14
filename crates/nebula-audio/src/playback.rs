//! Speaker playback fed from a queue the session writes into as audio arrives.
//!
//! The queue is the barge-in mechanism. Interrupting Nebula means two things
//! happening at once - telling the server to stop generating, and stopping the
//! sound coming out of the speakers - and only the second is under our control
//! and instant. [`Playback::clear`] is that second thing.

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::capture::{CHANNELS, Level, peak};
use crate::route;

pub struct Playback {
    _stream: cpal::Stream,
    queue: Arc<Queue>,
    routed_to: Option<String>,
    config: StreamConfig,
}

/// ponytail: a mutex on the audio thread. The lock is held only for the memcpy
/// out of the front of the deque and is never contended for long, which is what
/// Flow ships and what runs clean here. Swap for an SPSC ring (`ringbuf`) if
/// xruns ever show up in the journal - the call sites would not change.
pub struct Queue {
    state: Mutex<Buffer>,
    prefill_samples: usize,
    /// Read by the HUD without taking the lock.
    queued: AtomicU64,
    level: Level,
    /// Set while the callback is producing silence because nothing is queued,
    /// which is how the daemon knows a reply has finished playing rather than
    /// guessing from the last chunk's arrival.
    starved: AtomicBool,
    /// Samples the device has taken, ever. Rising is the only proof that audio
    /// has actually started playing rather than merely been queued.
    consumed: AtomicU64,
    drains: AtomicU64,
}

struct Buffer {
    samples: VecDeque<f32>,
    wait_remaining: usize,
}

impl Queue {
    fn new(prefill_samples: usize) -> Self {
        Self {
            state: Mutex::new(Buffer {
                samples: VecDeque::new(),
                wait_remaining: prefill_samples,
            }),
            prefill_samples,
            queued: AtomicU64::new(0),
            level: Level::default(),
            starved: AtomicBool::new(true),
            consumed: AtomicU64::new(0),
            drains: AtomicU64::new(0),
        }
    }

    pub fn push(&self, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        let mut state = self.state.lock().expect("playback queue poisoned");
        let queued = &mut state.samples;
        queued.extend(samples);
        self.queued.store(queued.len() as u64, Ordering::Relaxed);
        self.starved.store(false, Ordering::Relaxed);
    }

    /// Drop everything not yet played. This is barge-in.
    pub fn clear(&self) -> u64 {
        let mut state = self.state.lock().expect("playback queue poisoned");
        let queued = &mut state.samples;
        let discarded = queued.len() as u64;
        queued.clear();
        state.wait_remaining = self.prefill_samples;
        self.queued.store(0, Ordering::Relaxed);
        self.starved.store(true, Ordering::Relaxed);
        discarded
    }

    /// Samples still waiting. Divided by the sample rate this is how far behind
    /// the speakers are, which is the number barge-in latency is measured in.
    pub fn queued(&self) -> u64 {
        self.queued.load(Ordering::Relaxed)
    }

    pub fn drains(&self) -> u64 {
        self.drains.load(Ordering::Relaxed)
    }

    pub fn consumed(&self) -> u64 {
        self.consumed.load(Ordering::Relaxed)
    }

    pub fn is_idle(&self) -> bool {
        self.starved.load(Ordering::Relaxed) && self.queued() == 0
    }

    pub fn level(&self) -> f32 {
        self.level.peak()
    }

    /// Runs on the audio thread.
    fn fill(&self, out: &mut [f32]) {
        let mut state = self.state.lock().expect("playback queue poisoned");
        if !state.samples.is_empty() && state.wait_remaining > 0 {
            state.wait_remaining = state.wait_remaining.saturating_sub(out.len());
            out.fill(0.0);
            self.level.set(0.0);
            return;
        }
        if !state.samples.is_empty() {
            state.wait_remaining = 0;
        }
        let queued = &mut state.samples;
        let taken = queued.len().min(out.len());
        for slot in out.iter_mut().take(taken) {
            *slot = queued.pop_front().unwrap_or(0.0);
        }
        out[taken..].fill(0.0);
        self.queued.store(queued.len() as u64, Ordering::Relaxed);
        if queued.is_empty() {
            state.wait_remaining = self.prefill_samples;
            if taken > 0 {
                self.drains.fetch_add(1, Ordering::Relaxed);
            }
        }
        drop(state);
        self.consumed.fetch_add(taken as u64, Ordering::Relaxed);

        if taken < out.len() {
            self.starved.store(true, Ordering::Relaxed);
        }
        self.level.set(peak(&out[..taken]));
    }
}

impl Playback {
    pub fn open(sample_rate: u32, device: &str, buffer_frames: u32) -> Result<Self> {
        let cpal_device = cpal::default_host()
            .default_output_device()
            .ok_or_else(|| anyhow!("no output device - is PipeWire running?"))?;

        let config = StreamConfig {
            channels: CHANNELS,
            sample_rate,
            buffer_size: cpal::BufferSize::Fixed(buffer_frames),
        };

        let queue = Arc::new(Queue::new(sample_rate as usize * 80 / 1000));
        let stream = build(&cpal_device, &config, Arc::clone(&queue))?;
        stream
            .play()
            .context("could not start the playback stream")?;

        let routed_to = if let Some(target) = route::wanted(device) {
            route::move_own_stream(route::Direction::Playback, target)
                .context("could not route the configured audio device")?;
            Some(target.to_string())
        } else {
            None
        };

        Ok(Self {
            _stream: stream,
            queue,
            routed_to,
            config,
        })
    }

    pub fn queue(&self) -> Arc<Queue> {
        Arc::clone(&self.queue)
    }

    pub fn routed_to(&self) -> Option<&str> {
        self.routed_to.as_deref()
    }

    pub fn config(&self) -> &StreamConfig {
        &self.config
    }
}

fn build(device: &cpal::Device, config: &StreamConfig, queue: Arc<Queue>) -> Result<cpal::Stream> {
    let format = device
        .default_output_config()
        .context("device reported no default output config")?
        .sample_format();

    let on_error = |e| tracing::error!("playback stream error: {e}");

    let stream = match format {
        SampleFormat::F32 => device.build_output_stream(
            *config,
            move |out: &mut [f32], _: &_| queue.fill(out),
            on_error,
            None,
        ),
        SampleFormat::I16 => {
            let mut scratch = Vec::new();
            device.build_output_stream(
                *config,
                move |out: &mut [i16], _: &_| {
                    scratch.resize(out.len(), 0.0);
                    queue.fill(&mut scratch);
                    for (dst, &src) in out.iter_mut().zip(scratch.iter()) {
                        *dst = (src.clamp(-1.0, 1.0) * 32767.0) as i16;
                    }
                },
                on_error,
                None,
            )
        }
        other => return Err(anyhow!("unsupported playback sample format {other:?}")),
    }
    .context("could not open the speakers")?;

    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The queue is the whole barge-in mechanism, so it is tested without a
    /// device: `fill` is an ordinary function over a deque.
    #[test]
    fn prefill_absorbs_late_chunks_without_a_gap() {
        let q = Queue::new(8);
        let mut out = [0.0; 2];
        q.push(&[0.5; 4]);
        q.fill(&mut out);
        assert_eq!(out, [0.0; 2]);
        assert_eq!(q.consumed(), 0);
        assert!(!q.is_idle());
        q.push(&[0.5; 4]);
        for _ in 0..3 {
            q.fill(&mut out);
            assert_eq!(out, [0.0; 2]);
        }
        for _ in 0..3 {
            q.fill(&mut out);
            assert_eq!(out, [0.5; 2]);
        }
        q.push(&[0.5; 4]);
        for _ in 0..3 {
            q.fill(&mut out);
            assert_eq!(out, [0.5; 2]);
        }
        assert_eq!(q.consumed(), 12);
    }

    #[test]
    fn full_sized_chunks_keep_a_cushion_for_late_delivery() {
        let q = Queue::new(8);
        let mut out = [0.0; 2];
        q.push(&[0.5; 10]);
        for _ in 0..4 {
            q.fill(&mut out);
            assert_eq!(out, [0.0; 2]);
            assert_eq!(q.drains(), 0);
        }
        for _ in 0..3 {
            q.fill(&mut out);
            assert_eq!(out, [0.5; 2]);
        }
        q.push(&[0.5; 10]);
        for _ in 0..7 {
            q.fill(&mut out);
            assert_eq!(out, [0.5; 2]);
        }
        assert_eq!(q.drains(), 1);
        q.fill(&mut out);
        assert_eq!(q.drains(), 1);
    }

    #[test]
    fn short_reply_plays_after_bounded_wait_and_rebuffers_after_starvation() {
        let q = Queue::new(8);
        let mut out = [0.0; 2];
        for _ in 0..2 {
            q.push(&[0.5]);
            for _ in 0..4 {
                q.fill(&mut out);
                assert_eq!(out, [0.0; 2]);
                assert!(!q.is_idle());
            }
            q.fill(&mut out);
            assert_eq!(out, [0.5, 0.0]);
            assert!(q.is_idle());
        }
        assert_eq!(q.consumed(), 2);
    }

    #[test]
    fn clear_discards_prefill_and_resets_delay() {
        let q = Queue::new(8);
        let mut out = [0.0; 2];
        q.push(&[0.7; 4]);
        q.fill(&mut out);
        assert_eq!(q.clear(), 4);
        assert!(q.is_idle());
        q.fill(&mut out);
        assert_eq!(out, [0.0; 2]);
        q.push(&[0.3]);
        for _ in 0..4 {
            q.fill(&mut out);
            assert_eq!(out, [0.0; 2]);
        }
        q.fill(&mut out);
        assert_eq!(out, [0.3, 0.0]);
    }

    #[test]
    fn fill_drains_in_order_and_pads_with_silence() {
        let q = Queue::new(0);
        q.push(&[0.1, 0.2, 0.3]);
        let mut out = [9.0; 5];
        q.fill(&mut out);
        assert_eq!(out, [0.1, 0.2, 0.3, 0.0, 0.0]);
        assert_eq!(q.queued(), 0);
    }

    #[test]
    fn fill_takes_only_what_fits_and_leaves_the_rest() {
        let q = Queue::new(0);
        q.push(&[1.0, 2.0, 3.0, 4.0]);
        let mut out = [0.0; 2];
        q.fill(&mut out);
        assert_eq!(out, [1.0, 2.0]);
        assert_eq!(q.queued(), 2);
        q.fill(&mut out);
        assert_eq!(out, [3.0, 4.0]);
    }

    #[test]
    fn consumed_counts_only_what_the_device_took() {
        let q = Queue::new(0);
        q.push(&[0.1; 10]);
        assert_eq!(q.consumed(), 0, "queued is not played");
        let mut out = [0.0; 4];
        q.fill(&mut out);
        assert_eq!(q.consumed(), 4);
        let mut rest = [0.0; 32];
        q.fill(&mut rest);
        assert_eq!(q.consumed(), 10, "silence padding is not consumed audio");
    }

    #[test]
    fn clear_is_barge_in() {
        let q = Queue::new(0);
        q.push(&[0.5; 48_000]);
        assert_eq!(q.queued(), 48_000);
        q.clear();
        assert_eq!(q.queued(), 0);
        let mut out = [1.0; 8];
        q.fill(&mut out);
        assert_eq!(
            out, [0.0; 8],
            "cleared queue must play silence, not stale audio"
        );
    }

    #[test]
    fn idle_only_once_the_callback_has_run_dry() {
        let q = Queue::new(0);
        assert!(q.is_idle());
        q.push(&[0.1; 4]);
        assert!(!q.is_idle());
        let mut out = [0.0; 2];
        q.fill(&mut out);
        assert!(!q.is_idle(), "two samples still queued is not idle");
        let mut rest = [0.0; 8];
        q.fill(&mut rest);
        assert!(q.is_idle());
    }

    #[test]
    fn level_follows_what_was_actually_played() {
        let q = Queue::new(0);
        q.push(&[0.25, -0.6]);
        let mut out = [0.0; 2];
        q.fill(&mut out);
        assert!((q.level() - 0.6).abs() < 1e-6);
    }
}
