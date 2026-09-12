//! Local confirmation tones, generated rather than loaded.
//!
//! No WAV files: a tone is a few hundred bytes of arithmetic, and a file is a
//! path that can be missing, a packaging problem, and one more thing the
//! service unit has to be able to read.

/// Rise and fall applied to every tone. Without it the buffer starts at a
/// non-zero sample and the speaker cone steps, which is audible as a click at
/// the very moment Nebula is trying to sound calm.
const FADE: f32 = 0.012;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Wake word heard. Rising, because something is beginning.
    Wake,
    /// Session closed. The same interval falling.
    Disconnect,
    /// Something went wrong. Low and flat, so it is never mistaken for either.
    Error,
}

impl Tone {
    /// (frequency Hz, seconds) per segment.
    fn segments(self) -> &'static [(f32, f32)] {
        match self {
            Tone::Wake => &[(660.0, 0.07), (990.0, 0.09)],
            Tone::Disconnect => &[(990.0, 0.07), (660.0, 0.09)],
            Tone::Error => &[(220.0, 0.18), (185.0, 0.18)],
        }
    }

    pub fn render(self, sample_rate: u32, gain: f32) -> Vec<f32> {
        self.segments()
            .iter()
            .flat_map(|&(freq, secs)| sine(freq, secs, sample_rate, gain))
            .collect()
    }
}

fn sine(freq: f32, seconds: f32, sample_rate: u32, gain: f32) -> Vec<f32> {
    let total = (seconds * sample_rate as f32) as usize;
    let fade = ((FADE * sample_rate as f32) as usize).min(total / 2).max(1);
    (0..total)
        .map(|i| {
            let t = i as f32 / sample_rate as f32;
            // `total - 1 - i` rather than `total - i`: the last sample is the
            // one that has to land on zero, and counting from the length leaves
            // it one step short - a small step, and a click at the end of every
            // tone.
            let envelope = (i as f32 / fade as f32)
                .min((total - 1 - i) as f32 / fade as f32)
                .clamp(0.0, 1.0);
            (t * freq * std::f32::consts::TAU).sin() * envelope * gain
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tone_is_about_as_long_as_its_segments() {
        let samples = Tone::Wake.render(48_000, 0.5);
        let expected = (0.16 * 48_000.0) as usize;
        assert!(
            samples.len().abs_diff(expected) < 64,
            "{} vs {expected}",
            samples.len()
        );
    }

    #[test]
    fn every_tone_starts_and_ends_at_silence() {
        for tone in [Tone::Wake, Tone::Disconnect, Tone::Error] {
            let s = tone.render(48_000, 1.0);
            assert!(
                s[0].abs() < 1e-3,
                "{tone:?} starts at {} - that is a click",
                s[0]
            );
            assert!(
                s[s.len() - 1].abs() < 1e-3,
                "{tone:?} ends at {}",
                s[s.len() - 1]
            );
        }
    }

    #[test]
    fn gain_is_never_exceeded() {
        let s = Tone::Error.render(24_000, 0.3);
        assert!(s.iter().all(|v| v.abs() <= 0.3 + 1e-6));
    }

    #[test]
    fn wake_and_disconnect_are_mirror_images() {
        assert_eq!(
            Tone::Wake
                .segments()
                .iter()
                .map(|s| s.0)
                .collect::<Vec<_>>(),
            Tone::Disconnect
                .segments()
                .iter()
                .rev()
                .map(|s| s.0)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_low_rate_still_renders_without_panicking() {
        assert!(!Tone::Wake.render(8_000, 0.5).is_empty());
    }
}
