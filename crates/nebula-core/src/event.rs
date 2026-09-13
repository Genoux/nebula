//! What the daemon tells the UI. One line of JSON per message over the Unix
//! socket, because the HUD is optional and a text protocol means it can be read
//! with `socat` while debugging without a second tool being written to do it.

use serde::{Deserialize, Serialize};

/// Re-exported under the crate root as `Event` for the daemon's own bus; the
/// state machine's input alphabet is [`crate::state::Event`].
pub use crate::state::Event;

/// The HUD's vocabulary. Wider than [`crate::State`] in one direction and
/// narrower in another: a live session is three different pictures, and
/// `Waking`/`Disconnecting` are too brief to be worth drawing on their own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum UiStatus {
    WithActivity {
        phase: Box<UiStatus>,
        activity: UiActivity,
    },
    Standby,
    Connecting,
    /// `level` is 0.0..=1.0 microphone loudness, for the bars.
    Listening {
        level: f32,
    },
    Thinking,
    /// `level` is Nebula's own output loudness.
    Speaking {
        level: f32,
    },
    Muted,
    /// Never carries a cause, a URL, or a response body - only a short line
    /// safe to paint on screen. Anything with detail in it goes to the journal.
    Error {
        message: String,
    },
}

/// UI to daemon. The HUD has exactly two powers, and neither can start a
/// session: waking Nebula is the wake word's job alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum UiCommand {
    ToggleMute,
    EndSession,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiActivity {
    Reasoning,
    WebSearch,
    Files,
    Terminal,
    Server,
    Desktop,
    Browser,
    Integration,
    Approval,
}

impl UiStatus {
    pub fn phase(&self) -> &Self {
        match self {
            Self::WithActivity { phase, .. } => phase.phase(),
            _ => self,
        }
    }
    pub fn activity(&self) -> Option<UiActivity> {
        match self {
            Self::WithActivity { activity, .. } => Some(*activity),
            _ => None,
        }
    }
    pub fn with_activity(self, activity: Option<UiActivity>) -> Self {
        match activity {
            Some(activity) => Self::WithActivity {
                phase: Box::new(self.phase().clone()),
                activity,
            },
            None => self.phase().clone(),
        }
    }

    pub fn to_line(&self) -> String {
        let mut line = serde_json::to_string(self).expect("UiStatus has no unserialisable field");
        line.push('\n');
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_preserves_the_voice_phase_on_the_wire() {
        let phase = UiStatus::Speaking { level: 0.4 };
        let status = phase.clone().with_activity(Some(UiActivity::WebSearch));
        let received: UiStatus = serde_json::from_str(&status.to_line()).unwrap();
        assert_eq!(received.phase(), &phase);
        assert_eq!(received.activity(), Some(UiActivity::WebSearch));
        assert_eq!(received.with_activity(None), phase);
    }

    #[test]
    fn a_status_round_trips_as_one_line() {
        let sent = UiStatus::Listening { level: 0.42 };
        let line = sent.to_line();
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1);
        assert_eq!(serde_json::from_str::<UiStatus>(line.trim()).unwrap(), sent);
    }

    #[test]
    fn commands_round_trip() {
        for cmd in [UiCommand::ToggleMute, UiCommand::EndSession] {
            let json = serde_json::to_string(&cmd).unwrap();
            assert_eq!(serde_json::from_str::<UiCommand>(&json).unwrap(), cmd);
        }
    }
}
