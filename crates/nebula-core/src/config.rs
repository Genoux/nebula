//! `~/.config/nebula/config.toml`, with a default for every field so a missing
//! file is a working configuration rather than a startup failure. Only the
//! model id has no sensible default: see [`Session::model`].

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub wake_word: WakeWord,
    pub audio: Audio,
    pub session: Session,
    pub privacy: Privacy,
    pub ui: Ui,
    pub logging: Logging,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct WakeWord {
    pub phrase: String,
    pub sensitivity: f32,
    pub cooldown_seconds: u64,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Audio {
    /// A PipeWire node name, or `default` to follow whatever WirePlumber has
    /// chosen. Kept as a string rather than an index: indices are reassigned
    /// every time a device is replugged.
    pub input_device: String,
    pub output_device: String,
    /// Frames per audio callback. This is the residual: after a barge-in
    /// clears the queue, whatever the device already holds still plays, so it
    /// is also the floor on how fast Nebula can be made to stop talking.
    ///
    /// Measured on PipeWire 1.6: 1024 (its default) is a 40ms period, 240 is
    /// 10ms, 120 is 5ms, and each is honoured exactly. 240 is four times
    /// better than the default without asking the graph for trouble.
    pub buffer_frames: u32,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Session {
    pub model: String,
    pub voice: String,
    pub backend_model: String,
    pub idle_timeout_seconds: u64,
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Privacy {
    pub store_audio: bool,
    pub store_transcripts: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Ui {
    pub enabled: bool,
    pub position: String,
    pub hide_after_seconds: u64,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Logging {
    pub level: String,
}

impl Default for WakeWord {
    fn default() -> Self {
        Self {
            phrase: "nebula".into(),
            sensitivity: 0.55,
            cooldown_seconds: 3,
        }
    }
}

impl Default for Audio {
    fn default() -> Self {
        Self {
            input_device: "default".into(),
            output_device: "default".into(),
            buffer_frames: 240,
        }
    }
}

impl Default for Session {
    fn default() -> Self {
        Self {
            model: String::new(),
            voice: "marin".into(),
            idle_timeout_seconds: 45,
            backend_model: "gpt-5.6-luna".into(),
        }
    }
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            enabled: true,
            position: "bottom-center".into(),
            hide_after_seconds: 8,
        }
    }
}

impl Default for Logging {
    fn default() -> Self {
        Self {
            level: "info".into(),
        }
    }
}

impl Config {
    pub fn parse(toml_text: &str) -> Result<Self> {
        toml::from_str(toml_text).map_err(|_| {
            anyhow::anyhow!("config.toml is invalid; check field names and value types")
        })
    }

    /// A missing file is the default configuration. A present but malformed one
    /// is an error - silently falling back there would hide a typo behind
    /// behaviour the user did not ask for.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                Self::parse(&text).with_context(|| format!("could not read {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("could not read {}", path.display())),
        }
    }

    /// Everything wrong with this configuration, rather than the first thing:
    /// `--check` exists to be run once and fixed once.
    pub fn problems(&self) -> Vec<String> {
        let mut found = Vec::new();
        if self.session.model.trim().is_empty() {
            found.push("session.model is empty - set it to a GPT-Live model id".into());
        }
        if !(0.0..=1.0).contains(&self.wake_word.sensitivity) {
            found.push(format!(
                "wake_word.sensitivity is {}, which is outside 0.0..=1.0",
                self.wake_word.sensitivity
            ));
        }
        if self.wake_word.phrase.trim().is_empty() {
            found.push("wake_word.phrase is empty".into());
        }
        if !matches!(
            self.wake_word.phrase.trim().to_lowercase().as_str(),
            "nebula" | "hey nebula"
        ) {
            found.push("wake_word.phrase must be nebula or hey nebula".into());
        }
        if !self
            .session
            .model
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        {
            found.push("session.model contains invalid characters".into());
        }
        if self.session.voice.trim().is_empty() {
            found.push("session.voice is empty".into());
        }
        if self.audio.input_device.trim().is_empty() || self.audio.output_device.trim().is_empty() {
            found.push("audio device names must not be empty".into());
        }
        if self.session.model.starts_with("gpt-realtime") {
            found.push("session.model must select GPT-Live, not a Realtime model".into());
        }
        if self.session.backend_model.trim().is_empty()
            || !self
                .session
                .backend_model
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        {
            found.push("session.backend_model must be a valid Responses model id".into());
        }
        if !matches!(self.ui.position.as_str(), "top-center" | "bottom-center") {
            found.push("ui.position must be top-center or bottom-center".into());
        }
        if !matches!(
            self.logging.level.as_str(),
            "error" | "warn" | "info" | "debug" | "trace"
        ) {
            found.push("logging.level must be error, warn, info, debug or trace".into());
        }
        if self.privacy.store_audio || self.privacy.store_transcripts {
            found.push(
                "audio and transcript persistence are unsupported; keep privacy flags false".into(),
            );
        }
        if self.audio.buffer_frames == 0 {
            found.push("audio.buffer_frames is 0".into());
        }
        if self.session.idle_timeout_seconds == 0 {
            found
                .push("session.idle_timeout_seconds is 0 - the session would end instantly".into());
        }
        found
    }

    pub fn idle_timeout(&self) -> Duration {
        Duration::from_secs(self.session.idle_timeout_seconds)
    }

    pub fn wake_cooldown(&self) -> Duration {
        Duration::from_secs(self.wake_word.cooldown_seconds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_is_all_defaults() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn defaults_are_the_documented_ones() {
        let c = Config::default();
        assert_eq!(c.wake_word.phrase, "nebula");
        assert_eq!(c.wake_word.sensitivity, 0.55);
        assert_eq!(c.session.idle_timeout_seconds, 45);
        assert_eq!(c.session.backend_model, "gpt-5.6-luna");
        assert!(!c.privacy.store_audio);
        assert!(!c.privacy.store_transcripts);
        assert!(c.ui.enabled);
    }

    #[test]
    fn a_partial_section_keeps_its_other_defaults() {
        let c = Config::parse("[wake_word]\nsensitivity = 0.8\n").unwrap();
        assert_eq!(c.wake_word.sensitivity, 0.8);
        assert_eq!(c.wake_word.phrase, "nebula");
        assert_eq!(c.wake_word.cooldown_seconds, 3);
    }

    #[test]
    fn a_typo_is_rejected_rather_than_ignored() {
        let err = Config::parse("[wake_word]\nsensitivty = 0.8\n").unwrap_err();
        assert!(format!("{err:#}").contains("invalid"), "{err:#}");
    }

    #[test]
    fn privacy_defaults_to_storing_nothing() {
        let c = Config::parse("[privacy]\n").unwrap();
        assert!(!c.privacy.store_audio);
        assert!(!c.privacy.store_transcripts);
    }

    #[test]
    fn an_unset_model_is_a_problem() {
        let problems = Config::default().problems();
        assert!(
            problems.iter().any(|p| p.contains("session.model")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_full_config_has_no_problems() {
        let c = Config::parse(
            r#"
[wake_word]
phrase = "hey nebula"
sensitivity = 0.6
cooldown_seconds = 2

[session]
model = "gpt-live-1"
idle_timeout_seconds = 30
"#,
        )
        .unwrap();
        assert!(c.problems().is_empty(), "{:?}", c.problems());
        assert_eq!(c.wake_word.phrase, "hey nebula");
    }

    #[test]
    fn buffer_frames_defaults_to_the_measured_value() {
        assert_eq!(Config::default().audio.buffer_frames, 240);
        assert_eq!(
            Config::parse("[audio]\nbuffer_frames = 120\n")
                .unwrap()
                .audio
                .buffer_frames,
            120
        );
    }

    #[test]
    fn out_of_range_sensitivity_is_a_problem() {
        let c =
            Config::parse("[wake_word]\nsensitivity = 1.9\n[session]\nmodel = \"m\"\n").unwrap();
        assert_eq!(c.problems().len(), 1);
    }

    #[test]
    fn invalid_session_and_privacy_options_are_reported() {
        let config = Config::parse(
            r#"
[session]
model = "model?api_key=private"
backend_model = ""
[privacy]
store_audio = true
[ui]
position = "middle"
"#,
        )
        .unwrap();
        let problems = config.problems();
        assert_eq!(problems.len(), 4);
        assert!(!problems.join(" ").contains("private"));
    }

    #[test]
    fn old_realtime_configuration_requires_migration() {
        assert!(
            Config::parse("[session]\nmodel = \"gpt-live-1\"\nend_of_turn_silence_ms = 450\n")
                .is_err()
        );
        let config = Config::parse("[session]\nmodel = \"gpt-realtime-2.1\"\n").unwrap();
        assert!(
            config
                .problems()
                .iter()
                .any(|problem| problem.contains("GPT-Live"))
        );
    }

    #[test]
    fn a_missing_file_is_the_default_config() {
        let c = Config::load(Path::new("/nonexistent/nebula/config.toml")).unwrap();
        assert_eq!(c, Config::default());
    }
}
