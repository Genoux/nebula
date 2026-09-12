use anyhow::{Context, Result};
use nebula_audio::Capture;
use nebula_core::{config::Config, paths};
use nebula_wake::{Dtw, SAMPLE_RATE, WakeWordDetector, mfcc};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const RECORDINGS: usize = 5;
const RECORD_SECONDS: f32 = 2.0;
/// A recording shorter than this after trimming is a click or a breath, not
/// the phrase.
const SHORTEST_PHRASE_SECONDS: f32 = 0.25;

pub fn train(config: &Config) -> Result<()> {
    let phrase = config.wake_word.phrase.trim();
    let path = paths::wake_model(phrase)?;
    let mic = Capture::open(
        SAMPLE_RATE,
        &config.audio.input_device,
        config.audio.buffer_frames,
    )?;

    println!("Recording \"{phrase}\" {RECORDINGS} times. Say it once each time, naturally.");
    println!("Recordings stay in memory; only the extracted features are saved.\n");

    let mut takes = Vec::with_capacity(RECORDINGS);
    while takes.len() < RECORDINGS {
        println!("  {}/{RECORDINGS} - say \"{phrase}\" now", takes.len() + 1);
        let raw = record(&mic, Duration::from_secs_f32(RECORD_SECONDS));
        let spoken = nebula_wake::trim_silence(&raw, mfcc::SPEECH_FLOOR);
        let seconds = spoken.len() as f32 / SAMPLE_RATE as f32;
        if mfcc::energy(spoken) < mfcc::SPEECH_FLOOR
            || !(SHORTEST_PHRASE_SECONDS..=nebula_wake::detector::WINDOW_SECONDS).contains(&seconds)
        {
            println!("        need a clear phrase between 0.25 and 1.6 seconds - again");
            continue;
        }
        println!("        ok ({seconds:.2}s)");
        takes.push(spoken.to_vec());
        std::thread::sleep(Duration::from_millis(400));
    }

    let model = nebula_wake::train(phrase, &takes)?;
    model.save(&path)?;
    println!(
        "\nsaved {} (reference spread {:.2})",
        path.display(),
        model.reference
    );
    println!("next: nebula --test-wake");
    Ok(())
}

pub fn test(config: &Config) -> Result<()> {
    let phrase = config.wake_word.phrase.trim();
    let model = nebula_wake::Model::load(&paths::wake_model(phrase)?)?;
    let mut detector = Dtw::new(model, config.wake_word.sensitivity, config.wake_cooldown());
    let mic = Capture::open(
        SAMPLE_RATE,
        &config.audio.input_device,
        config.audio.buffer_frames,
    )?;

    let stop = Arc::new(AtomicBool::new(false));
    ctrlc_flag(Arc::clone(&stop))?;

    println!(
        "listening for \"{}\" at sensitivity {:.2} - local only, nothing is sent anywhere",
        detector.phrase(),
        config.wake_word.sensitivity
    );
    println!("ctrl-c to stop. Scores print for anything louder than room tone.\n");

    let started = Instant::now();
    let mut detections = 0usize;
    let mut last_printed: Option<f32> = None;
    while !stop.load(Ordering::Relaxed) {
        let Some(chunk) = mic.recv() else { break };
        if let Some(event) = detector.process(&chunk) {
            detections += 1;
            println!("  WAKE  \"{}\" score {:.2}", event.phrase, event.score);
            last_printed = None;
            continue;
        }
        if let Some(score) = detector.last_score().filter(|s| Some(*s) != last_printed) {
            println!("        score {score:.2}");
            last_printed = Some(score);
        }
    }

    let minutes = started.elapsed().as_secs_f32() / 60.0;
    println!("\n{detections} detection(s) in {minutes:.1} min");
    Ok(())
}

fn record(mic: &Capture, how_long: Duration) -> Vec<f32> {
    while mic.try_recv().is_some() {}
    let wanted = (how_long.as_secs_f32() * SAMPLE_RATE as f32) as usize;
    let mut got = Vec::with_capacity(wanted);
    while got.len() < wanted {
        match mic.recv() {
            Some(chunk) => got.extend(chunk),
            None => break,
        }
    }
    got.truncate(wanted);
    got
}

/// The test loop blocks on the microphone, so ctrl-c reaches it through a flag
/// set from a small signal thread; the default handler would kill the process
/// before the detection summary is printed.
fn ctrlc_flag(flag: Arc<AtomicBool>) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("could not start the signal runtime")?;
    std::thread::Builder::new()
        .name("ctrl-c".into())
        .spawn(move || {
            let _ = runtime.block_on(tokio::signal::ctrl_c());
            flag.store(true, Ordering::Relaxed);
        })
        .context("could not install the ctrl-c handler")?;
    Ok(())
}
