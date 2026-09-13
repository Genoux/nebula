use std::time::{Duration, Instant};

const PAUSE_HOLD: Duration = Duration::from_millis(1800);
const TAKEOVER_HOLD: Duration = Duration::from_millis(300);

#[derive(Default)]
pub struct SpeechDisplay {
    last_audio: Option<Instant>,
    last_output: Option<Instant>,
    last_user: Option<Instant>,
}

impl SpeechDisplay {
    pub fn audio(&mut self, level: f32, now: Instant) {
        if level > 0.005 {
            self.last_audio = Some(now);
            self.last_output = Some(now);
        }
    }

    pub fn output_text(&mut self, now: Instant) {
        if self.speaking(now) {
            self.last_output = Some(now);
        }
    }

    pub fn user_text(&mut self, now: Instant) {
        self.last_user = Some(now);
    }

    pub fn speaking(&self, now: Instant) -> bool {
        let Some(audio) = self.last_audio else {
            return false;
        };
        if self.last_user.is_some_and(|user| user > audio)
            && now.duration_since(audio) >= TAKEOVER_HOLD
        {
            return false;
        }
        // GPT-Live has no speech-done event; bridge phrase pauses without holding indefinitely.
        self.last_output
            .is_some_and(|output| now.duration_since(output) < PAUSE_HOLD)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrase_pauses_keep_speaking_but_finished_speech_releases() {
        let mut display = SpeechDisplay::default();
        let at = Instant::now();
        display.audio(0.1, at);
        display.audio(0.0, at + Duration::from_secs(1));
        assert!(display.speaking(at + Duration::from_secs(1)));
        assert!(!display.speaking(at + Duration::from_secs(2)));
    }

    #[test]
    fn continued_transcript_bridges_a_longer_phrase_gap() {
        let mut display = SpeechDisplay::default();
        let at = Instant::now();
        display.audio(0.1, at);
        display.output_text(at + Duration::from_secs(1));
        assert!(display.speaking(at + Duration::from_secs(2)));
        assert!(!display.speaking(at + Duration::from_secs(3)));
    }

    #[test]
    fn user_takeover_releases_after_audio_stops() {
        let mut display = SpeechDisplay::default();
        let at = Instant::now();
        display.audio(0.1, at);
        display.user_text(at + Duration::from_millis(100));
        assert!(display.speaking(at + Duration::from_millis(150)));
        assert!(!display.speaking(at + Duration::from_millis(350)));
        display.audio(0.1, at + Duration::from_millis(400));
        assert!(display.speaking(at + Duration::from_millis(450)));
    }

    #[test]
    fn silence_and_transcripts_alone_do_not_start_speaking() {
        let mut display = SpeechDisplay::default();
        let at = Instant::now();
        display.audio(0.0, at);
        display.output_text(at);
        assert!(!display.speaking(at));
    }
}
