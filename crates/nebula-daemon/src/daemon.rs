//! The full flow: standby, wake, converse, back to standby.
//!
//! The loop is sequential on purpose. Wake detection only runs inside the
//! standby step, and nothing else can start a session, so there is no second
//! path by which two sessions could overlap; [`State::on`] then checks every
//! step against the table and treats a transition it rejects as a bug.

use anyhow::{Context, Result, anyhow};
use nebula_audio::{Capture, LIVE_RATE, Playback, Tone};
use nebula_core::config::Config;
use nebula_core::event::{UiCommand, UiStatus};
use nebula_core::secrets::{self, Secret};
use nebula_core::{Event, State, paths};
use nebula_wake::{Dtw, Model, WakeEvent, WakeWordDetector};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};

use crate::ipc;
use crate::live::{self, Controls, EndReason};

/// Longest a user-facing error line may be on the HUD.
const ERROR_LINE: usize = 80;

/// sysexits `EX_CONFIG`. The unit names this in `RestartPreventExitStatus=`,
/// so a configuration problem stops the service instead of restarting it into
/// the same failure every few seconds.
pub const EX_CONFIG: i32 = 78;

/// A failure no restart can fix: bad config, a missing key or wake model, or a
/// daemon already running.
#[derive(Debug)]
pub struct StartupError(String);

impl std::fmt::Display for StartupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for StartupError {}

fn startup(error: anyhow::Error) -> anyhow::Error {
    anyhow::Error::new(StartupError(format!("{error:#}")))
}

pub fn run(
    config: Config,
    key_from_service: Option<Secret>,
    config_path: std::path::PathBuf,
) -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("could not start the async runtime")?
        .block_on(run_async(config, key_from_service, config_path))
}

async fn run_async(
    config: Config,
    key_from_service: Option<Secret>,
    config_path: std::path::PathBuf,
) -> Result<()> {
    let problems = config.problems();
    if !problems.is_empty() {
        return Err(startup(anyhow!(
            "{} - run nebula --check",
            problems.join("; ")
        )));
    }
    // Loaded once, up front: a missing key should stop the service at start,
    // not surface as an error tone the first time somebody says "Nebula".
    let api_key =
        secrets::resolve_api_key(key_from_service, &paths::env_file()?).map_err(startup)?;
    let phrase = config.wake_word.phrase.trim().to_string();
    let model = Model::load(&paths::wake_model(&phrase)?).map_err(startup)?;
    let mut detector = Dtw::new(model, config.wake_word.sensitivity, config.wake_cooldown());

    let muted = Arc::new(AtomicBool::new(false));
    let shutdown = Arc::new(AtomicBool::new(false));
    let (status, _status_keep) = watch::channel(UiStatus::Standby);
    let (command_tx, mut commands) = mpsc::channel::<UiCommand>(32);

    let _instance = ipc::Instance::acquire(&paths::socket_path()?).map_err(startup)?;
    start_ipc(&status, &muted, command_tx)?;
    watch_for_ctrl_c(Arc::clone(&shutdown));
    if config.ui.enabled {
        crate::hud::start(config_path);
    }

    let mut state = State::Standby;
    let mut backoff = Backoff::default();
    tracing::info!("standby - listening locally for \"{phrase}\"");

    loop {
        status.send_replace(UiStatus::Standby);
        if detector.is_paused() {
            detector.resume()?;
        }

        let (returned, heard) = tokio::task::spawn_blocking({
            let (config, muted, shutdown) =
                (config.clone(), Arc::clone(&muted), Arc::clone(&shutdown));
            move || listen(detector, &config, &muted, &shutdown)
        })
        .await
        .context("the wake listener panicked")?;
        detector = returned;

        let wake = match heard {
            Ok(Some(wake)) => wake,
            Ok(None) => break,
            Err(e) => {
                tracing::error!("wake listener failed: {e:#}");
                recover(&mut state, &status, &e, &mut backoff, &shutdown).await?;
                continue;
            }
        };

        while commands.try_recv().is_ok() {}
        let woke_at = Instant::now();
        tracing::info!(score = wake.score, "wake word");
        state = advance(state, Event::WakeDetected)?;
        detector.pause()?;

        let session_result = tokio::select! {
            result = session(
            &config,
            api_key.expose(),
            woke_at,
            &mut state,
            &status,
            &muted,
            &mut commands,
            ) => result,
            _ = wait_shutdown(&shutdown) => break,
        };
        match session_result {
            Ok(reason) => {
                backoff.reset();
                if reason == EndReason::CtrlC {
                    break;
                }
            }
            Err(e) => {
                if e.downcast_ref::<nebula_live::transport::AuthenticationError>()
                    .is_some()
                {
                    return Err(startup(e));
                }
                tracing::error!("session failed: {e:#}");
                recover(&mut state, &status, &e, &mut backoff, &shutdown).await?;
            }
        }
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
    }

    tracing::info!("shutting down");
    if let Ok(path) = paths::socket_path() {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

/// Waking through Disconnecting. Every transition goes through [`advance`], so
/// the table in nebula-core is what this function is checked against.
async fn session(
    config: &Config,
    api_key: &str,
    woke_at: Instant,
    state: &mut State,
    status: &watch::Sender<UiStatus>,
    muted: &Arc<AtomicBool>,
    commands: &mut mpsc::Receiver<UiCommand>,
) -> Result<EndReason> {
    let speakers = Playback::open(
        LIVE_RATE,
        &config.audio.output_device,
        config.audio.buffer_frames,
    )?;
    // Queued, not awaited: connecting starts while the tone plays, which is
    // the tone's length taken off every wake.
    speakers.queue().push(&Tone::Wake.render(LIVE_RATE, 0.3));
    *state = advance(*state, Event::WakeAcknowledged)?;
    status.send_replace(UiStatus::Connecting);

    let live = tokio::select! {
        result = live::connect(config, api_key, woke_at) => result?,
        _ = crate::shutdown_signal() => return Ok(EndReason::CtrlC),
        Some(UiCommand::EndSession) = commands.recv() => {
            *state = advance(*state, Event::StopRequested)?;
            *state = advance(*state, Event::Closed)?;
            return Ok(EndReason::EndCommand);
        }
    };
    *state = advance(*state, Event::Connected)?;

    let outcome = live
        .run(
            config,
            speakers,
            Controls {
                muted: Arc::clone(muted),
                commands,
                status: status.clone(),
            },
        )
        .await?;

    let event = match outcome.reason {
        EndReason::IdleTimeout => Event::IdleTimeout,
        EndReason::StopPhrase
        | EndReason::EndCommand
        | EndReason::CtrlC
        | EndReason::ServerClosed => Event::StopRequested,
    };
    *state = advance(*state, event)?;
    // `run` consumed the session, so the socket is already closed here.
    *state = advance(*state, Event::Closed)?;

    tracing::info!("session ended: {}", outcome.reason.describe());
    tracing::info!("{}", outcome.metrics.report());
    Ok(outcome.reason)
}

/// Error through Standby: a short line on the HUD, the error tone, and a wait
/// that grows with repeated failures so a dead network is not hammered.
async fn recover(
    state: &mut State,
    status: &watch::Sender<UiStatus>,
    error: &anyhow::Error,
    backoff: &mut Backoff,
    shutdown: &AtomicBool,
) -> Result<()> {
    if let Some(t) = state.on(Event::Failed) {
        *state = t.to;
    }
    status.send_replace(UiStatus::Error {
        message: hud_line(error),
    });

    if let Ok(speakers) = Playback::open(LIVE_RATE, "default", 240) {
        let queue = speakers.queue();
        queue.push(&Tone::Error.render(LIVE_RATE, 0.3));
        let started = Instant::now();
        while !queue.is_idle() && started.elapsed() < Duration::from_secs(1) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    let wait = backoff.next();
    tracing::info!("retrying in {}s", wait.as_secs());
    let until = Instant::now() + wait;
    while Instant::now() < until && !shutdown.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    if *state == State::Error {
        *state = advance(*state, Event::ErrorAcknowledged)?;
    } else {
        *state = State::Standby;
    }
    Ok(())
}

/// Standby. Runs on a blocking thread and owns the only microphone stream that
/// exists in this state - a 16kHz one, which no code path sends anywhere.
///
/// The detector goes in and comes back out, error or not, so its cooldown and
/// pause state survive across sessions.
fn listen(
    mut detector: Dtw,
    config: &Config,
    muted: &AtomicBool,
    shutdown: &AtomicBool,
) -> (Dtw, Result<Option<WakeEvent>>) {
    let mic = match Capture::open(
        nebula_wake::SAMPLE_RATE,
        &config.audio.input_device,
        config.audio.buffer_frames,
    ) {
        Ok(mic) => mic,
        Err(e) => return (detector, Err(e)),
    };
    let mut last_received = Instant::now();
    while !shutdown.load(Ordering::Relaxed) {
        let Some(chunk) = mic.try_recv() else {
            if last_received.elapsed() > Duration::from_secs(3) {
                return (
                    detector,
                    Err(anyhow!("microphone stopped delivering audio")),
                );
            }
            std::thread::sleep(Duration::from_millis(5));
            continue;
        };
        last_received = Instant::now();
        if muted.load(Ordering::Relaxed) {
            let _ = detector.pause();
            continue;
        }
        if detector.is_paused() {
            let _ = detector.resume();
        }
        if let Some(wake) = detector.process(&chunk) {
            return (detector, Ok(Some(wake)));
        }
    }
    (detector, Ok(None))
}

fn advance(state: State, event: Event) -> Result<State> {
    let t = state
        .on(event)
        .ok_or_else(|| anyhow!("bug: {event:?} is not valid in {state}"))?;
    tracing::debug!("{} -> {}", t.from, t.to);
    Ok(t.to)
}

pub(crate) fn start_ipc(
    status: &watch::Sender<UiStatus>,
    muted: &Arc<AtomicBool>,
    commands: mpsc::Sender<UiCommand>,
) -> Result<()> {
    let path = match paths::socket_path() {
        Ok(path) => path,
        Err(e) => {
            tracing::warn!("no hud socket: {e:#}");
            return Ok(());
        }
    };
    // Fatal, unlike every other socket problem: a second daemon would hold a
    // second microphone stream and could open a second session.
    if ipc::another_daemon_running(&path) {
        return Err(startup(anyhow!(
            "another nebula daemon is already running ({})",
            path.display()
        )));
    }
    match ipc::bind(&path) {
        Ok(listener) => {
            tokio::spawn(ipc::serve(
                listener,
                status.clone(),
                Arc::clone(muted),
                commands,
            ));
        }
        // The HUD is optional, so the daemon runs without its socket.
        Err(e) => tracing::warn!("hud unavailable: {e:#}"),
    }
    Ok(())
}

async fn wait_shutdown(shutdown: &AtomicBool) {
    while !shutdown.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn watch_for_ctrl_c(shutdown: Arc<AtomicBool>) {
    tokio::spawn(async move {
        crate::shutdown_signal().await;
        shutdown.store(true, Ordering::Relaxed);
    });
}

/// First line of the error, clipped. The journal gets the full chain; the
/// screen gets something short enough to read and too short to leak through.
fn hud_line(error: &anyhow::Error) -> String {
    let first = error.to_string();
    let line = first.lines().next().unwrap_or("error").trim();
    if line.chars().count() > ERROR_LINE {
        format!("{}…", line.chars().take(ERROR_LINE - 1).collect::<String>())
    } else {
        line.to_string()
    }
}

#[derive(Default)]
struct Backoff {
    failures: u32,
}

impl Backoff {
    const CEILING: Duration = Duration::from_secs(30);

    fn next(&mut self) -> Duration {
        let wait = Duration::from_secs(1u64 << self.failures.min(5)).min(Self::CEILING);
        self.failures = self.failures.saturating_add(1);
        wait
    }

    fn reset(&mut self) {
        self.failures = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_failures_are_recognisable_for_the_exit_code() {
        let e = startup(anyhow!("no wake model"));
        assert!(e.downcast_ref::<StartupError>().is_some());
        assert!(
            anyhow!("network down")
                .downcast_ref::<StartupError>()
                .is_none()
        );
    }

    #[test]
    fn backoff_doubles_to_a_ceiling_and_resets() {
        let mut b = Backoff::default();
        let waits: Vec<u64> = (0..8).map(|_| b.next().as_secs()).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30, 30]);
        b.reset();
        assert_eq!(b.next().as_secs(), 1);
    }

    #[test]
    fn a_hud_line_is_one_short_line() {
        let e = anyhow!("{}\nsecond line with detail", "x".repeat(200));
        let line = hud_line(&e);
        assert!(!line.contains('\n'));
        assert!(line.chars().count() <= ERROR_LINE);
    }

    #[test]
    fn advance_rejects_an_impossible_transition() {
        assert!(advance(State::Standby, Event::Connected).is_err());
        assert_eq!(
            advance(State::Standby, Event::WakeDetected).unwrap(),
            State::Waking
        );
    }

    #[test]
    fn the_full_session_path_is_valid_in_the_table() {
        let mut s = State::Standby;
        for e in [
            Event::WakeDetected,
            Event::WakeAcknowledged,
            Event::Connected,
            Event::StopRequested,
            Event::Closed,
        ] {
            s = advance(s, e).unwrap();
        }
        assert_eq!(s, State::Standby);
    }
}
