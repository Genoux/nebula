//! What each daemon status looks like. Pure, so the mapping is tested without
//! a compositor.

use nebula_core::event::UiStatus;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgb(pub f32, pub f32, pub f32);

pub const INK: Rgb = Rgb(0.95, 0.96, 0.98);
pub const PILL: Rgb = Rgb(0.05, 0.055, 0.07);
const GREY: Rgb = Rgb(0.78, 0.80, 0.84);
const BLUE: Rgb = Rgb(0.36, 0.62, 1.0);
const PURPLE: Rgb = Rgb(0.66, 0.46, 1.0);
const GREEN: Rgb = Rgb(0.36, 0.86, 0.58);
const AMBER: Rgb = Rgb(1.0, 0.66, 0.20);
const RED: Rgb = Rgb(1.0, 0.34, 0.34);

pub const BARS: usize = 5;
/// Taller in the middle, so the bars read as one voice rather than five meters.
const SHAPE: [f32; BARS] = [0.55, 0.8, 1.0, 0.8, 0.55];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Motion {
    Still,
    Pulse { hz: f32 },
    Bars,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    pub color: Rgb,
    pub motion: Motion,
    pub label: String,
    pub orb_alpha: f32,
}

pub fn look(status: &UiStatus) -> Look {
    let (color, motion, label, orb_alpha) = match status.phase() {
        UiStatus::WithActivity { phase, .. } => return look(phase),
        UiStatus::Standby => (GREY, Motion::Still, "Nebula ready".to_string(), 0.5),
        UiStatus::Connecting => (BLUE, Motion::Pulse { hz: 1.6 }, "Connecting".into(), 1.0),
        UiStatus::Listening { .. } => (BLUE, Motion::Bars, "Listening".into(), 1.0),
        UiStatus::Thinking => (PURPLE, Motion::Pulse { hz: 0.8 }, "Thinking".into(), 1.0),
        UiStatus::Speaking { .. } => (GREEN, Motion::Bars, "Nebula speaking".into(), 1.0),
        UiStatus::Muted => (AMBER, Motion::Still, "Muted".into(), 1.0),
        UiStatus::Error { message } => (RED, Motion::Still, message.clone(), 1.0),
    };
    Look {
        color,
        motion,
        label,
        orb_alpha,
    }
}

pub fn level(status: &UiStatus) -> f32 {
    match status.phase() {
        UiStatus::Listening { level } | UiStatus::Speaking { level } => level.clamp(0.0, 1.0),
        _ => 0.0,
    }
}

/// Whether two statuses are the same picture. Level changes are not a change -
/// they arrive twenty times a second and must not restart the hide timer.
pub fn same_kind(a: &UiStatus, b: &UiStatus) -> bool {
    match (a.phase(), b.phase()) {
        (UiStatus::Error { message: x }, UiStatus::Error { message: y }) => x == y,
        _ => std::mem::discriminant(a.phase()) == std::mem::discriminant(b.phase()),
    }
}

/// Standby and errors fade away; anything happening in a session stays up.
pub fn call_finished(previous: &UiStatus, next: &UiStatus) -> bool {
    matches!(next.phase(), UiStatus::Standby)
        && matches!(
            previous.phase(),
            UiStatus::Connecting
                | UiStatus::Listening { .. }
                | UiStatus::Thinking
                | UiStatus::Speaking { .. }
                | UiStatus::Muted
        )
}

pub fn stays_visible(status: &UiStatus, since_change: Duration, hide_after: Duration) -> bool {
    match status.phase() {
        UiStatus::Standby | UiStatus::Error { .. } => since_change < hide_after,
        _ => true,
    }
}

/// 0..=1, starting at 0.
pub fn pulse(hz: f32, seconds: f32) -> f32 {
    0.5 - 0.5 * (seconds * hz * std::f32::consts::TAU).cos()
}

/// Bar heights as 0..=1 fractions of their range.
pub fn bar_heights(level: f32, seconds: f32) -> [f32; BARS] {
    // A power under 1 lifts quiet speech, which otherwise barely moves the bars.
    let loud = level.clamp(0.0, 1.0).powf(0.6);
    std::array::from_fn(|i| {
        let wobble = 0.85 + 0.15 * (seconds * 7.0 + i as f32 * 1.3).sin();
        (SHAPE[i] * loud * wobble).clamp(0.0, 1.0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_calls_dismiss_but_initial_standby_keeps_ready_indicator() {
        assert!(call_finished(&UiStatus::Thinking, &UiStatus::Standby));
        assert!(call_finished(
            &UiStatus::Speaking { level: 0.4 },
            &UiStatus::Standby
        ));
        assert!(!call_finished(&UiStatus::Standby, &UiStatus::Standby));
        assert!(!call_finished(
            &UiStatus::Thinking,
            &UiStatus::Listening { level: 0.0 }
        ));
    }

    #[test]
    fn every_status_has_the_documented_label() {
        assert_eq!(look(&UiStatus::Standby).label, "Nebula ready");
        assert_eq!(look(&UiStatus::Connecting).label, "Connecting");
        assert_eq!(look(&UiStatus::Listening { level: 0.3 }).label, "Listening");
        assert_eq!(look(&UiStatus::Thinking).label, "Thinking");
        assert_eq!(
            look(&UiStatus::Speaking { level: 0.3 }).label,
            "Nebula speaking"
        );
        assert_eq!(look(&UiStatus::Muted).label, "Muted");
        assert_eq!(
            look(&UiStatus::Error {
                message: "offline".into()
            })
            .label,
            "offline"
        );
    }

    #[test]
    fn states_have_distinct_colours() {
        let colours = [
            look(&UiStatus::Standby).color,
            look(&UiStatus::Listening { level: 0.0 }).color,
            look(&UiStatus::Thinking).color,
            look(&UiStatus::Speaking { level: 0.0 }).color,
            look(&UiStatus::Muted).color,
            look(&UiStatus::Error {
                message: String::new(),
            })
            .color,
        ];
        for (i, a) in colours.iter().enumerate() {
            for b in &colours[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn a_level_change_is_not_a_status_change() {
        assert!(same_kind(
            &UiStatus::Listening { level: 0.1 },
            &UiStatus::Listening { level: 0.9 }
        ));
        assert!(!same_kind(
            &UiStatus::Listening { level: 0.1 },
            &UiStatus::Speaking { level: 0.1 }
        ));
        assert!(!same_kind(
            &UiStatus::Error {
                message: "a".into()
            },
            &UiStatus::Error {
                message: "b".into()
            }
        ));
    }

    #[test]
    fn standby_hides_and_a_live_session_does_not() {
        let hide = Duration::from_secs(8);
        assert!(stays_visible(
            &UiStatus::Standby,
            Duration::from_secs(2),
            hide
        ));
        assert!(!stays_visible(
            &UiStatus::Standby,
            Duration::from_secs(9),
            hide
        ));
        assert!(stays_visible(
            &UiStatus::Speaking { level: 0.5 },
            Duration::from_secs(600),
            hide
        ));
        assert!(stays_visible(
            &UiStatus::Muted,
            Duration::from_secs(600),
            hide
        ));
    }

    #[test]
    fn pulse_runs_zero_to_one() {
        assert!(pulse(1.0, 0.0).abs() < 1e-6);
        assert!((pulse(1.0, 0.5) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn silence_flattens_the_bars_and_speech_crests_in_the_middle() {
        assert!(bar_heights(0.0, 1.0).iter().all(|h| *h == 0.0));
        let loud = bar_heights(0.8, 0.0);
        assert!(loud[2] > loud[0] && loud[2] > loud[4]);
        assert!(loud.iter().all(|h| (0.0..=1.0).contains(h)));
    }
}
