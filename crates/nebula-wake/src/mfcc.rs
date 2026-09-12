//! Mel-frequency cepstral coefficients: turning a window of audio into the
//! short vector-per-frame that speech comparison actually works on.
//!
//! Raw samples are useless for matching a spoken word - the same word said
//! twice differs in every sample while being obviously the same sound. MFCCs
//! throw away phase and fine pitch detail and keep the shape of the spectrum,
//! which is roughly what makes a word sound like itself.

use rustfft::{FftPlanner, num_complex::Complex32};
use std::sync::Arc;

pub const SAMPLE_RATE: u32 = 16_000;
/// 25ms at 16kHz, the standard analysis window for speech.
pub const FRAME: usize = 400;
/// 10ms, so frames overlap and a word cannot fall between two of them.
pub const HOP: usize = 160;
const FFT_SIZE: usize = 512;
const MEL_FILTERS: usize = 26;
/// The first coefficient is overall loudness, which says how close the speaker
/// was rather than what they said, so it is dropped.
pub const COEFFS: usize = 12;
const LOW_HZ: f32 = 100.0;
/// RMS above which a frame counts as sound rather than room tone.
pub const SPEECH_FLOOR: f32 = 0.01;
const HIGH_HZ: f32 = 7_600.0;

pub struct Extractor {
    fft: Arc<dyn rustfft::Fft<f32>>,
    window: Vec<f32>,
    filters: Vec<Vec<f32>>,
    scratch: Vec<Complex32>,
}

impl Extractor {
    pub fn new() -> Self {
        let fft = FftPlanner::new().plan_fft_forward(FFT_SIZE);
        let window = (0..FRAME)
            .map(|i| 0.54 - 0.46 * (std::f32::consts::TAU * i as f32 / (FRAME - 1) as f32).cos())
            .collect();
        Self {
            fft,
            window,
            filters: mel_filterbank(),
            scratch: vec![Complex32::default(); FFT_SIZE],
        }
    }

    /// One MFCC vector per frame, oldest first.
    pub fn frames(&mut self, samples: &[f32]) -> Vec<Vec<f32>> {
        if samples.len() < FRAME {
            return Vec::new();
        }
        let mut out: Vec<Vec<f32>> = samples
            .windows(FRAME)
            .step_by(HOP)
            .map(|frame| self.one_frame(frame))
            .collect();
        let loud: Vec<bool> = samples
            .windows(FRAME)
            .step_by(HOP)
            .map(|frame| energy(frame) > SPEECH_FLOOR)
            .collect();
        cepstral_mean_normalise(&mut out, &loud);
        out
    }

    fn one_frame(&mut self, frame: &[f32]) -> Vec<f32> {
        // Pre-emphasis lifts the high frequencies, which carry most of what
        // distinguishes consonants and are the quietest part of the signal.
        let mut previous = 0.0;
        for (i, slot) in self.scratch.iter_mut().enumerate() {
            let sample = if i < FRAME {
                let emphasised = frame[i] - 0.97 * previous;
                previous = frame[i];
                emphasised * self.window[i]
            } else {
                0.0
            };
            *slot = Complex32::new(sample, 0.0);
        }
        self.fft.process(&mut self.scratch);

        let power: Vec<f32> = self.scratch[..FFT_SIZE / 2 + 1]
            .iter()
            .map(|c| c.norm_sqr() / FFT_SIZE as f32)
            .collect();

        let bands: Vec<f32> = self
            .filters
            .iter()
            .map(|filter| filter.iter().zip(power.iter()).map(|(w, p)| w * p).sum())
            .collect();

        // Floored 50dB below the loudest band before the log. Bands far from
        // anything being said hold only window leakage, whose power varies by
        // orders of magnitude with frame phase; `ln` turns that into swings of
        // twenty or more in the coefficients, measured on a steady 440Hz tone,
        // and every comparison downstream inherits it as noise.
        let floor = bands.iter().fold(0.0f32, |m, &b| m.max(b)) * 1e-5 + 1e-10;
        let energies: Vec<f32> = bands.iter().map(|&b| b.max(floor).ln()).collect();

        dct(&energies)
    }
}

impl Default for Extractor {
    fn default() -> Self {
        Self::new()
    }
}

fn hz_to_mel(hz: f32) -> f32 {
    2595.0 * (1.0 + hz / 700.0).log10()
}

fn mel_to_hz(mel: f32) -> f32 {
    700.0 * (10f32.powf(mel / 2595.0) - 1.0)
}

fn mel_filterbank() -> Vec<Vec<f32>> {
    let bins = FFT_SIZE / 2 + 1;
    let low = hz_to_mel(LOW_HZ);
    let high = hz_to_mel(HIGH_HZ);
    let points: Vec<f32> = (0..MEL_FILTERS + 2)
        .map(|i| {
            let mel = low + (high - low) * i as f32 / (MEL_FILTERS + 1) as f32;
            mel_to_hz(mel) * FFT_SIZE as f32 / SAMPLE_RATE as f32
        })
        .collect();

    (0..MEL_FILTERS)
        .map(|f| {
            let (left, centre, right) = (points[f], points[f + 1], points[f + 2]);
            (0..bins)
                .map(|bin| {
                    let x = bin as f32;
                    if x < left || x > right {
                        0.0
                    } else if x <= centre {
                        (x - left) / (centre - left).max(1e-6)
                    } else {
                        (right - x) / (right - centre).max(1e-6)
                    }
                })
                .collect()
        })
        .collect()
}

/// DCT-II, keeping coefficients 1..=COEFFS.
fn dct(energies: &[f32]) -> Vec<f32> {
    (1..=COEFFS)
        .map(|k| {
            energies
                .iter()
                .enumerate()
                .map(|(n, e)| {
                    e * (std::f32::consts::PI * k as f32 * (n as f32 + 0.5) / energies.len() as f32)
                        .cos()
                })
                .sum::<f32>()
        })
        .collect()
}

/// Subtract each coefficient's mean, taken over the loud frames only.
///
/// Normalising removes the constant offset a different microphone, room, or
/// distance puts on every coefficient. The mean must come from sound alone:
/// a trained template is trimmed to the phrase while a live window is mostly
/// silence around it, and a mean taken over everything gave the two different
/// offsets - measured, it cut a genuine match's score to a fifth.
fn cepstral_mean_normalise(frames: &mut [Vec<f32>], loud: &[bool]) {
    let voiced: Vec<&Vec<f32>> = frames
        .iter()
        .zip(loud)
        .filter(|(_, l)| **l)
        .map(|(f, _)| f)
        .collect();
    let basis: Vec<&Vec<f32>> = if voiced.is_empty() {
        frames.iter().collect()
    } else {
        voiced
    };
    if basis.is_empty() {
        return;
    }
    let means: Vec<f32> = (0..COEFFS)
        .map(|c| basis.iter().map(|f| f[c]).sum::<f32>() / basis.len() as f32)
        .collect();
    for frame in frames.iter_mut() {
        for (value, mean) in frame.iter_mut().zip(&means) {
            *value -= mean;
        }
    }
}

/// Root-mean-square loudness, used to skip silence before doing any real work.
pub fn energy(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freq: f32, samples: usize) -> Vec<f32> {
        (0..samples)
            .map(|i| (i as f32 / SAMPLE_RATE as f32 * freq * std::f32::consts::TAU).sin() * 0.5)
            .collect()
    }

    #[test]
    fn mel_scale_round_trips() {
        for hz in [100.0, 1000.0, 4000.0, 8000.0] {
            assert!((mel_to_hz(hz_to_mel(hz)) - hz).abs() < 0.5, "{hz}");
        }
    }

    #[test]
    fn short_input_yields_no_frames() {
        assert!(Extractor::new().frames(&[0.0; 100]).is_empty());
    }

    #[test]
    fn frame_count_follows_the_hop() {
        let frames = Extractor::new().frames(&tone(440.0, SAMPLE_RATE as usize));
        // One second, 10ms hop, minus the frame that does not fit at the end.
        assert!((95..=100).contains(&frames.len()), "{}", frames.len());
        assert!(frames.iter().all(|f| f.len() == COEFFS));
    }

    /// Half one tone, half another, so the spectrum changes inside the window.
    /// A pure steady tone is a poor stand-in for speech: its low bands hold only
    /// window leakage, which interferes with its own mirror image differently
    /// at every frame phase.
    fn two_tones(first: f32, second: f32) -> Vec<f32> {
        let mut s = tone(first, 8000);
        s.extend(tone(second, 8000));
        s
    }

    #[test]
    fn different_spectral_movements_give_different_coefficients() {
        let mut e = Extractor::new();
        let rising = e.frames(&two_tones(300.0, 3000.0));
        let falling = e.frames(&two_tones(3000.0, 300.0));
        let distance: f32 = rising[10]
            .iter()
            .zip(falling[10].iter())
            .map(|(a, b)| (a - b).abs())
            .sum();
        assert!(
            distance > 1.0,
            "rising and falling should not look alike: {distance}"
        );
    }

    #[test]
    fn a_gain_change_barely_moves_the_coefficients() {
        // The whole point of cepstral mean normalisation: the same words at a
        // different volume must still match.
        let mut e = Extractor::new();
        let quiet = e.frames(&two_tones(500.0, 2000.0));
        let loud: Vec<f32> = two_tones(500.0, 2000.0).iter().map(|s| s * 4.0).collect();
        let loud = e.frames(&loud);
        let distance: f32 = quiet[20]
            .iter()
            .zip(loud[20].iter())
            .map(|(a, b)| (a - b).abs())
            .sum();
        assert!(
            distance < 0.5,
            "gain changed the coefficients by {distance}"
        );
    }

    #[test]
    fn energy_of_silence_is_zero() {
        assert_eq!(energy(&[0.0; 100]), 0.0);
        assert!(energy(&tone(440.0, 1000)) > 0.3);
    }
}
