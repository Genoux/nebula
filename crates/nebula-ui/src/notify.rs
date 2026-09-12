//! Fallback for sessions without wlr-layer-shell - GNOME, or X11 - where a
//! floating overlay is not available. Only the states worth interrupting for
//! become notifications; a notification per listening/speaking flip would be
//! noise.

use crate::look;
use nebula_core::event::UiStatus;
use std::process::Command;
use std::sync::mpsc::Receiver;

pub fn run(statuses: &Receiver<UiStatus>) {
    let mut last: Option<UiStatus> = None;
    for status in statuses.iter() {
        if last
            .as_ref()
            .is_some_and(|previous| look::same_kind(previous, &status))
        {
            continue;
        }
        if worth_notifying(&status) {
            notify(&look::look(&status).label);
        }
        last = Some(status);
    }
}

fn worth_notifying(status: &UiStatus) -> bool {
    matches!(
        status,
        UiStatus::Connecting | UiStatus::Muted | UiStatus::Error { .. }
    )
}

fn notify(body: &str) {
    let sent = Command::new("notify-send")
        .args(["--app-name=Nebula", "--expire-time=3000", "Nebula", body])
        .status();
    if let Err(e) = sent {
        tracing::warn!("notify-send unavailable: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_interrupting_states_notify() {
        assert!(worth_notifying(&UiStatus::Connecting));
        assert!(worth_notifying(&UiStatus::Muted));
        assert!(worth_notifying(&UiStatus::Error {
            message: "x".into()
        }));
        assert!(!worth_notifying(&UiStatus::Listening { level: 0.5 }));
        assert!(!worth_notifying(&UiStatus::Speaking { level: 0.5 }));
        assert!(!worth_notifying(&UiStatus::Standby));
    }
}
