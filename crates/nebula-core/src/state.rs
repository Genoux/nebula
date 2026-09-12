//! The session state machine: a pure function from (state, event) to the next
//! state. No timers, no sockets, no audio - the daemon owns all of that and
//! feeds this the events it produces.
//!
//! What the HUD shows inside a live session (listening, thinking, speaking) is
//! deliberately *not* modelled here. Those come from the remote session several
//! times a turn and have no bearing on whether a session exists, so they live
//! in [`crate::event::UiStatus`]; folding them in would quadruple this table to
//! describe something no transition depends on.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    /// Wake word running locally. No microphone audio leaves this machine.
    Standby,
    /// Wake word heard and the detector paused, so Nebula's own confirmation
    /// tone and its replies can never re-trigger it.
    Waking,
    Connecting,
    Live,
    Disconnecting,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Event {
    WakeDetected,
    /// The confirmation tone has finished, so connecting may begin.
    WakeAcknowledged,
    Connected,
    /// "Nebula, stop", or the HUD's end-session action.
    StopRequested,
    IdleTimeout,
    /// The transport is fully closed and the wake detector may resume.
    Closed,
    Failed,
    /// The error tone has played and any backoff has elapsed.
    ErrorAcknowledged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    pub from: State,
    pub to: State,
}

impl State {
    /// `None` means the event does not apply here and was dropped.
    ///
    /// That return is the whole defence against double sessions: a second
    /// `WakeDetected` arriving while connecting - the detector's own tail, or a
    /// reply containing the word "Nebula" - matches no arm outside `Standby`
    /// and is discarded, so a second connection is unrepresentable rather than
    /// merely guarded against.
    #[must_use]
    pub fn on(self, event: Event) -> Option<Transition> {
        use Event::*;
        use State::*;

        let to = match (self, event) {
            (Standby, WakeDetected) => Waking,
            (Waking, WakeAcknowledged) => Connecting,
            (Connecting, Connected) => Live,

            // Honoured while still connecting: the connection can outlive the
            // user's interest in it, and tearing down beats arriving at a live
            // session nobody is waiting for any more.
            (Live | Connecting, StopRequested | IdleTimeout) => Disconnecting,

            (Disconnecting, Closed) => Standby,

            // Already on the way down, or already down: a failure there has
            // nothing left to abandon.
            (Standby | Error | Disconnecting, Failed) => return None,
            (_, Failed) => Error,

            (Error, ErrorAcknowledged) => Standby,

            _ => return None,
        };
        Some(Transition { from: self, to })
    }

    /// True while a remote session exists or is being built, which is exactly
    /// when the wake detector must stay paused.
    pub fn holds_session(self) -> bool {
        matches!(self, State::Connecting | State::Live | State::Disconnecting)
    }

    /// True only where microphone audio is allowed to leave the machine.
    pub fn streams_to_cloud(self) -> bool {
        matches!(self, State::Live)
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            State::Standby => "standby",
            State::Waking => "waking",
            State::Connecting => "connecting",
            State::Live => "live",
            State::Disconnecting => "disconnecting",
            State::Error => "error",
        };
        f.write_str(name)
    }
}

#[cfg(test)]
mod tests {
    use super::Event::*;
    use super::State::*;
    use super::*;

    /// Drive a sequence and return where it ended, ignoring dropped events.
    fn run(start: State, events: &[Event]) -> State {
        events
            .iter()
            .fold(start, |state, &e| state.on(e).map_or(state, |t| t.to))
    }

    #[test]
    fn the_happy_path_returns_to_standby() {
        let end = run(
            Standby,
            &[
                WakeDetected,
                WakeAcknowledged,
                Connected,
                StopRequested,
                Closed,
            ],
        );
        assert_eq!(end, Standby);
    }

    #[test]
    fn idle_timeout_ends_a_live_session() {
        assert_eq!(
            run(
                Standby,
                &[
                    WakeDetected,
                    WakeAcknowledged,
                    Connected,
                    IdleTimeout,
                    Closed
                ]
            ),
            Standby
        );
    }

    #[test]
    fn a_second_wake_cannot_open_a_second_session() {
        for state in [Waking, Connecting, Live, Disconnecting, Error] {
            assert_eq!(
                state.on(WakeDetected),
                None,
                "{state} accepted a second wake"
            );
        }
    }

    #[test]
    fn wake_is_only_accepted_from_standby() {
        assert_eq!(
            Standby.on(WakeDetected),
            Some(Transition {
                from: Standby,
                to: Waking
            })
        );
    }

    #[test]
    fn stop_while_connecting_tears_down() {
        assert_eq!(
            run(Standby, &[WakeDetected, WakeAcknowledged, StopRequested]),
            Disconnecting
        );
    }

    #[test]
    fn failure_anywhere_live_goes_to_error_and_back_to_standby() {
        for state in [Waking, Connecting, Live] {
            let errored = state.on(Failed).expect("{state} should be able to fail").to;
            assert_eq!(errored, Error);
            assert_eq!(errored.on(ErrorAcknowledged).unwrap().to, Standby);
        }
    }

    #[test]
    fn failure_in_standby_or_teardown_is_dropped() {
        for state in [Standby, Error, Disconnecting] {
            assert_eq!(
                state.on(Failed),
                None,
                "{state} should have nothing to abandon"
            );
        }
    }

    #[test]
    fn connecting_is_never_skipped() {
        assert_eq!(Standby.on(Connected), None);
        assert_eq!(Waking.on(Connected), None);
    }

    #[test]
    fn only_live_streams_to_the_cloud() {
        for state in [Standby, Waking, Connecting, Disconnecting, Error] {
            assert!(
                !state.streams_to_cloud(),
                "{state} would stream audio off the machine"
            );
        }
        assert!(Live.streams_to_cloud());
    }

    #[test]
    fn standby_holds_no_session() {
        assert!(!Standby.holds_session());
        assert!(!Waking.holds_session());
        assert!(Live.holds_session());
    }

    #[test]
    fn every_state_can_reach_standby() {
        let escape = [
            (Waking, &[Failed, ErrorAcknowledged][..]),
            (Connecting, &[StopRequested, Closed][..]),
            (Live, &[StopRequested, Closed][..]),
            (Disconnecting, &[Closed][..]),
            (Error, &[ErrorAcknowledged][..]),
        ];
        for (state, events) in escape {
            assert_eq!(run(state, events), Standby, "{state} is a dead end");
        }
    }
}
