//! Sliding-window wake detection over live microphone audio.

use anyhow::{Result, bail};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::mfcc::{self, Extractor, HOP, SAMPLE_RATE};
use crate::model::Model;
use crate::{WakeEvent, WakeWordDetector, dtw};

/// How much audio is searched at once. Long enough for "hey nebula" with room
/// around it, short enough that the DTW stays cheap.
pub const WINDOW_SECONDS: f32 = 1.6;
/// How often the window is scored. 100ms is well under the length of the
/// phrase, so the word cannot fall between two evaluations.
const EVALUATE_EVERY: usize = SAMPLE_RATE as usize / 10;

/// Fewer than this and there is no spread to calibrate against - one recording
/// says what the phrase sounds like, not how much it varies.
pub const MIN_RECORDINGS: usize = 2;

pub struct Dtw {
    model: Model,
    extractor: Extractor,
    window: VecDeque<f32>,
    window_capacity: usize,
    since_evaluation: usize,
    sensitivity: f32,
    cooldown: Duration,
    last_fired: Option<Instant>,
    paused: bool,
    last_score: Option<f32>,
}

impl Dtw {
    pub fn new(model: Model, sensitivity: f32, cooldown: Duration) -> Self {
        let window_capacity = (WINDOW_SECONDS * SAMPLE_RATE as f32) as usize;
        Self {
            model,
            extractor: Extractor::new(),
            window: VecDeque::with_capacity(window_capacity),
            window_capacity,
            since_evaluation: 0,
            sensitivity,
            cooldown,
            last_fired: None,
            paused: false,
            last_score: None,
        }
    }

    pub fn phrase(&self) -> &str {
        &self.model.phrase
    }

    /// The most recent non-silent score, fired or not. `--test-wake` prints it
    /// so sensitivity can be tuned against numbers rather than by feel.
    pub fn last_score(&self) -> Option<f32> {
        self.last_score
    }

    /// Best score across templates for the current window: 1.0 when the audio
    /// is as close to a template as the user's own recordings are to each
    /// other, falling as it gets further away.
    fn score(&mut self) -> Option<f32> {
        if self.window.len() < self.window_capacity {
            return None;
        }
        let samples: Vec<f32> = self.window.iter().copied().collect();
        // A quiet room costs one RMS per evaluation and nothing else, which is
        // what keeps standby near idle.
        if mfcc::energy(&samples) < mfcc::SPEECH_FLOOR {
            return None;
        }
        let heard = self.extractor.frames(&samples);
        let reference = self.model.reference;
        self.model
            .templates
            .iter()
            .filter_map(|template| dtw::best_match(template, &heard))
            .map(|distance| (reference / distance.max(1e-6)).min(1.0))
            .reduce(f32::max)
    }
}

impl WakeWordDetector for Dtw {
    fn process(&mut self, samples: &[f32]) -> Option<WakeEvent> {
        if self.paused {
            return None;
        }
        for &sample in samples {
            if self.window.len() == self.window_capacity {
                self.window.pop_front();
            }
            self.window.push_back(sample);
        }

        self.since_evaluation += samples.len();
        if self.since_evaluation < EVALUATE_EVERY {
            return None;
        }
        self.since_evaluation = 0;

        if self
            .last_fired
            .is_some_and(|fired| fired.elapsed() < self.cooldown)
        {
            return None;
        }

        let score = self.score()?;
        self.last_score = Some(score);
        if score < self.sensitivity {
            return None;
        }

        self.last_fired = Some(Instant::now());
        // Cleared so the phrase that just fired cannot also fire the next
        // evaluation once the cooldown lapses.
        self.window.clear();
        Some(WakeEvent {
            score,
            phrase: self.model.phrase.clone(),
        })
    }

    fn pause(&mut self) -> Result<()> {
        self.paused = true;
        self.window.clear();
        Ok(())
    }

    fn resume(&mut self) -> Result<()> {
        self.paused = false;
        self.window.clear();
        // The cooldown restarts on resume: the first thing a resumed detector
        // hears is often the tail of whatever ended the session.
        self.last_fired = Some(Instant::now());
        Ok(())
    }

    fn is_paused(&self) -> bool {
        self.paused
    }
}

/// Turn raw recordings of the phrase into a model, calibrated on how much the
/// recordings differ from one another.
pub fn train(phrase: &str, recordings: &[Vec<f32>]) -> Result<Model> {
    let mut extractor = Extractor::new();
    let templates: Vec<Vec<Vec<f32>>> = recordings
        .iter()
        .map(|r| extractor.frames(r))
        .filter(|frames| !frames.is_empty())
        .collect();
    if templates.len() < MIN_RECORDINGS {
        bail!(
            "need at least {MIN_RECORDINGS} usable recordings of \"{phrase}\", got {}",
            templates.len()
        );
    }

    let spreads: Vec<f32> = templates
        .iter()
        .enumerate()
        .flat_map(|(i, a)| {
            templates
                .iter()
                .enumerate()
                .filter(move |(j, _)| *j != i)
                .filter_map(move |(_, b)| dtw::best_match(a, b))
        })
        .collect();
    let reference = spreads.iter().sum::<f32>() / spreads.len() as f32;

    Ok(Model {
        phrase: phrase.to_string(),
        templates,
        reference: reference.max(1e-3),
    })
}

/// Trim leading and trailing silence so a template is the phrase and not the
/// pause around it.
pub fn trim_silence(samples: &[f32], floor: f32) -> &[f32] {
    let loud = |chunk: &[f32]| mfcc::energy(chunk) > floor;
    let first = samples.chunks(HOP).position(loud).unwrap_or(0) * HOP;
    let last = samples
        .chunks(HOP)
        .rposition(loud)
        .map_or(samples.len(), |i| ((i + 1) * HOP).min(samples.len()));
    if first >= last {
        samples
    } else {
        &samples[first..last]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repeatable stand-in for a spoken phrase: a sweep whose start frequency
    /// depends on `seed`, so different seeds are different "words".
    fn phrase(seed: f32, samples: usize) -> Vec<f32> {
        (0..samples)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                let freq = 300.0 + seed * 250.0 + 900.0 * t;
                (t * freq * std::f32::consts::TAU).sin() * 0.4
            })
            .collect()
    }

    /// The phrase at a slightly different speed, the way a second recording of
    /// the same word differs from the first.
    fn retake(seed: f32, samples: usize) -> Vec<f32> {
        phrase(seed, samples * 11 / 10)
    }

    /// A live window: silence, the phrase, silence.
    fn in_window(words: Vec<f32>) -> Vec<f32> {
        let total = (WINDOW_SECONDS * SAMPLE_RATE as f32) as usize;
        let pad = total.saturating_sub(words.len()) / 2;
        let mut w = vec![0.0; pad];
        w.extend(words);
        w.resize(total, 0.0);
        w
    }

    fn model() -> Model {
        train("nebula", &[phrase(1.0, 8000), retake(1.0, 8000)]).unwrap()
    }

    #[test]
    fn training_needs_more_than_one_recording() {
        let err = train("nebula", &[phrase(1.0, 8000)]).unwrap_err();
        assert!(format!("{err}").contains("at least 2"), "{err}");
    }

    #[test]
    fn training_skips_recordings_too_short_to_frame() {
        let m = train(
            "nebula",
            &[vec![0.0; 10], phrase(1.0, 8000), retake(1.0, 8000)],
        )
        .unwrap();
        assert_eq!(m.templates.len(), 2);
        assert!(m.reference > 0.0);
    }

    #[test]
    fn silence_never_fires() {
        let mut d = Dtw::new(model(), 0.0, Duration::ZERO);
        for _ in 0..40 {
            assert_eq!(d.process(&[0.0; 1600]), None);
        }
    }

    #[test]
    fn a_paused_detector_is_deaf() {
        let mut d = Dtw::new(model(), 0.0, Duration::ZERO);
        d.pause().unwrap();
        assert!(d.is_paused());
        assert_eq!(d.process(&in_window(phrase(1.0, 8000))), None);
    }

    #[test]
    fn resuming_clears_what_was_heard_while_paused() {
        let mut d = Dtw::new(model(), 0.0, Duration::ZERO);
        d.pause().unwrap();
        d.process(&phrase(1.0, 16_000));
        d.resume().unwrap();
        assert!(!d.is_paused());
        assert!(d.window.is_empty());
    }

    #[test]
    fn the_phrase_scores_above_a_different_one() {
        let mut same = Dtw::new(model(), 1.01, Duration::ZERO);
        same.process(&in_window(phrase(1.0, 8000)));
        let mut other = Dtw::new(model(), 1.01, Duration::ZERO);
        other.process(&in_window(phrase(6.0, 8000)));

        let (same, other) = (same.last_score().unwrap(), other.last_score().unwrap());
        assert!(same > other, "{same} should beat {other}");
    }

    #[test]
    fn the_phrase_fires_at_the_default_sensitivity() {
        let mut d = Dtw::new(model(), 0.55, Duration::ZERO);
        let fired = d.process(&in_window(phrase(1.0, 8000)));
        assert!(fired.is_some(), "scored {:?}", d.last_score());
    }

    #[test]
    fn a_different_phrase_does_not_fire_at_the_default_sensitivity() {
        let mut d = Dtw::new(model(), 0.55, Duration::ZERO);
        let fired = d.process(&in_window(phrase(6.0, 8000)));
        assert!(fired.is_none(), "fired at {:?}", d.last_score());
    }

    #[test]
    fn the_cooldown_suppresses_a_second_fire() {
        let mut d = Dtw::new(model(), 0.55, Duration::from_secs(30));
        let window = in_window(phrase(1.0, 8000));
        let fires = (0..5).filter(|_| d.process(&window).is_some()).count();
        assert_eq!(fires, 1);
    }

    #[test]
    fn trim_removes_the_silence_around_a_phrase() {
        let padded = in_window(phrase(1.0, 8000));
        let trimmed = trim_silence(&padded, 0.01);
        assert!(trimmed.len() < padded.len());
        assert!(trimmed.len() >= 7000, "trimmed too much: {}", trimmed.len());
    }

    #[test]
    fn trim_leaves_all_silence_alone_rather_than_returning_nothing() {
        let quiet = vec![0.0f32; 4000];
        assert_eq!(trim_silence(&quiet, 0.01).len(), quiet.len());
    }
}
