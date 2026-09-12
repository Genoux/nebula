//! `--test-audio`: prove the microphone and speakers work, and measure what
//! they cost in latency.
//!
//! The round-trip figure is acoustic - a click is played and listened for - so
//! it includes the speaker, the air, and the microphone, not just our buffers.
//! That is the number barge-in actually feels, which is why it is measured this
//! way rather than read off a buffer size.

use anyhow::{Context, Result};
use nebula_audio::{Capture, LIVE_RATE, Playback, Tone, capture};
use nebula_core::config::Config;
use std::time::{Duration, Instant};

/// Loud enough to hear over a room, short enough to time precisely.
const CLICK_SECONDS: f32 = 0.01;
const CLICK_GAIN: f32 = 0.6;

/// A click has to stand out from whatever the room is already doing, so the
/// threshold is derived from the measured noise floor - with an absolute floor
/// under it, because a very quiet room makes `noise * n` small enough that a
/// chair creak clears it.
const CLICK_OVER_NOISE: f32 = 8.0;
const CLICK_MINIMUM: f32 = 0.08;
const NOISE_FLOOR_SECONDS: f32 = 0.5;
const LISTEN_TIMEOUT: Duration = Duration::from_millis(800);

/// One measurement cannot tell a click from a door closing. Several can: a real
/// round trip is the same every time, and a room noise is not.
const TRIALS: usize = 5;

/// How far apart the fastest and slowest trial may be before the run is
/// reported as unreliable rather than as a number.
const SPREAD_LIMIT_MS: f64 = 30.0;

pub fn run(config: &Config, skip_loopback: bool) -> Result<()> {
    let rate = LIVE_RATE;

    println!(
        "opening microphone at {rate} Hz mono, {} frames/callback...",
        config.audio.buffer_frames
    );
    let mic = Capture::open(rate, &config.audio.input_device, config.audio.buffer_frames)
        .context("could not open the microphone")?;
    println!(
        "  input  {} ch @ {} Hz, routed to {}",
        mic.config().channels,
        mic.config().sample_rate,
        mic.routed_to().unwrap_or("system default")
    );

    println!("opening speakers at {rate} Hz mono...");
    let speakers = Playback::open(
        rate,
        &config.audio.output_device,
        config.audio.buffer_frames,
    )
    .context("could not open the speakers")?;
    println!(
        "  output {} ch @ {} Hz, routed to {}",
        speakers.config().channels,
        speakers.config().sample_rate,
        speakers.routed_to().unwrap_or("system default")
    );

    let noise = measure_noise_floor(&mic, rate)?;
    println!("\nnoise floor: rms {noise:.5}");
    if noise > 0.05 {
        println!("  note: that is loud for a resting room - is something playing?");
    }

    println!("\nsay something for 3 seconds...");
    let heard = listen(&mic, rate, Duration::from_secs(3));
    let loudest = capture::peak(&heard);
    println!("  captured {} samples, peak {loudest:.3}", heard.len());
    if loudest < noise * 3.0 {
        println!("  FAIL   nothing above the noise floor - is the microphone muted?");
    } else {
        println!("  ok     microphone is delivering audio");
    }

    println!("\nplaying the three local tones...");
    let queue = speakers.queue();
    for tone in [Tone::Wake, Tone::Disconnect, Tone::Error] {
        println!("  {tone:?}");
        queue.push(&tone.render(rate, 0.35));
        while !queue.is_idle() {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    println!("\noutput path latency (queue to device, excludes the sink itself):");
    for _ in 0..3 {
        match queue_to_device(&speakers, rate) {
            Some(ms) => println!("  {ms:.1} ms"),
            None => println!("  the device never consumed the buffer"),
        }
    }

    if skip_loopback {
        println!("\nround trip: skipped");
        return Ok(());
    }

    println!("\nmeasuring acoustic round trip - keep the room quiet...");
    let threshold = (noise * CLICK_OVER_NOISE).max(CLICK_MINIMUM);

    // Listen for a click that was never played. Anything heard here is the
    // room, and every trial below would have "detected" it just as happily.
    if listen_for(&mic, threshold, LISTEN_TIMEOUT).is_some() {
        println!(
            "  the room is too loud to measure - something crossed the click\n  \
             threshold while nothing was playing. Try again in silence."
        );
        return Ok(());
    }

    let mut times: Vec<f64> = (0..TRIALS)
        .filter_map(|_| {
            let ms = one_click(&mic, &speakers, rate, threshold);
            std::thread::sleep(Duration::from_millis(150));
            ms
        })
        .collect();

    // A majority, not all: one trial swallowed by a cough should not discard a
    // measurement the other four agree on.
    if times.len() * 2 <= TRIALS {
        println!(
            "  could not hear the click ({}/{TRIALS} trials). Expected on\n  \
             headphones - pass --no-loopback to skip, or measure on speakers.",
            times.len()
        );
        return Ok(());
    }

    times.sort_by(|a, b| a.total_cmp(b));
    let spread = times[times.len() - 1] - times[0];
    if spread > SPREAD_LIMIT_MS {
        println!(
            "  heard {}/{TRIALS} clicks but they disagree by {spread:.0} ms, so the\n  \
             room is answering rather than the speakers. Not reporting a number.",
            times.len()
        );
        return Ok(());
    }

    println!(
        "  {}/{TRIALS} clicks heard, spread {spread:.0} ms",
        times.len()
    );
    report_round_trip(times[times.len() / 2]);
    Ok(())
}

fn report_round_trip(ms: f64) {
    println!("  round trip: {ms:.0} ms (speaker + air + microphone)");
    // Barge-in is the requirement this number decides: everything below is
    // still ahead of Nebula's own reply, and everything above is a reply that
    // keeps talking after the user has started.
    if ms > 150.0 {
        println!(
            "  That is high. A Bluetooth sink costs 120-200ms of it; a USB or\n  \
             analogue output would make interruption feel immediate."
        );
    } else {
        println!("  Comfortable for barge-in.");
    }
}

fn measure_noise_floor(mic: &Capture, rate: u32) -> Result<f32> {
    let samples = listen(mic, rate, Duration::from_secs_f32(NOISE_FLOOR_SECONDS));
    Ok(capture::rms(&samples).max(1e-6))
}

fn listen(mic: &Capture, rate: u32, how_long: Duration) -> Vec<f32> {
    let wanted = (how_long.as_secs_f32() * rate as f32) as usize;
    let deadline = Instant::now() + how_long + Duration::from_millis(500);
    let mut got = Vec::with_capacity(wanted);
    while got.len() < wanted && Instant::now() < deadline {
        match mic.try_recv() {
            Some(chunk) => got.extend(chunk),
            None => std::thread::sleep(Duration::from_millis(2)),
        }
    }
    got
}

/// Play one click and time how long until it comes back.
fn one_click(mic: &Capture, speakers: &Playback, rate: u32, threshold: f32) -> Option<f64> {
    while mic.try_recv().is_some() {}

    let click: Vec<f32> = (0..(CLICK_SECONDS * rate as f32) as usize)
        .map(|i| {
            let t = i as f32 / rate as f32;
            (t * 1000.0 * std::f32::consts::TAU).sin() * CLICK_GAIN
        })
        .collect();

    let sent = Instant::now();
    speakers.queue().push(&click);
    let (heard_at, into_chunk) = listen_for(mic, threshold, LISTEN_TIMEOUT)?;
    Some((heard_at.duration_since(sent).as_secs_f64() - into_chunk) * 1000.0)
}

/// Wait for anything louder than `threshold`. Returns when the buffer carrying
/// it arrived, and how far into that buffer the crossing was - counting from
/// arrival alone would round every measurement up by a whole buffer.
fn listen_for(mic: &Capture, threshold: f32, timeout: Duration) -> Option<(Instant, f64)> {
    let rate = mic.config().sample_rate as f64;
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let Some(chunk) = mic.try_recv() else {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        };
        let arrived = Instant::now();
        if let Some(offset) = chunk.iter().position(|s| s.abs() > threshold) {
            return Some((arrived, offset as f64 / rate));
        }
    }
    None
}

/// How long a buffer waits between being queued and the device starting to
/// consume it.
///
/// This is the part of output latency Nebula owns. It deliberately excludes
/// what happens after the samples leave us - a Bluetooth sink adds its own
/// 120-200ms downstream and no amount of care here removes it - so a good
/// number below does not mean interruption will feel immediate. That is what
/// the acoustic round trip measures, and why both are reported.
fn queue_to_device(speakers: &Playback, rate: u32) -> Option<f64> {
    let queue = speakers.queue();
    while !queue.is_idle() {
        std::thread::sleep(Duration::from_millis(5));
    }

    // Silence, not a tone: this runs three times and nobody needs to hear it.
    let filled = rate as usize / 2;
    let sent = Instant::now();
    queue.push(&vec![0.0f32; filled]);

    let deadline = sent + Duration::from_secs(1);
    while Instant::now() < deadline {
        if queue.queued() < filled as u64 {
            return Some(sent.elapsed().as_secs_f64() * 1000.0);
        }
        std::hint::spin_loop();
    }
    None
}
