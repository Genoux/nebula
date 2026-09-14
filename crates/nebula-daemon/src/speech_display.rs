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

#[derive(Default)]
pub struct WorkDisplay {
    last_busy: Option<Instant>,
}

impl WorkDisplay {
    pub fn thinking(&mut self, busy: bool, speaking: bool, now: Instant) -> bool {
        if busy {
            self.last_busy = Some(now);
            return true;
        }
        if speaking {
            self.last_busy = None;
        }
        self.last_busy
            .is_some_and(|last| now.duration_since(last) < Duration::from_millis(1200))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_display_bridges_handoffs_and_releases_after_quiet() {
        let mut display = WorkDisplay::default();
        let at = Instant::now();
        assert!(!display.thinking(false, false, at));
        assert!(display.thinking(true, false, at));
        assert!(display.thinking(false, false, at + Duration::from_millis(500)));
        assert!(display.thinking(true, false, at + Duration::from_millis(800)));
        assert!(display.thinking(false, false, at + Duration::from_millis(1500)));
        assert!(!display.thinking(false, false, at + Duration::from_millis(2000)));
    }

    #[test]
    fn spoken_result_clears_hold_but_speech_does_not_clear_active_work() {
        let mut display = WorkDisplay::default();
        let at = Instant::now();
        assert!(display.thinking(true, true, at));
        assert!(display.thinking(false, false, at + Duration::from_millis(100)));
        assert!(!display.thinking(false, true, at + Duration::from_millis(200)));
        assert!(!display.thinking(false, false, at + Duration::from_millis(300)));
    }

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
