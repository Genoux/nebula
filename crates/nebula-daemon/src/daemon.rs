use anyhow::{Context, Result, anyhow};
use nebula_audio::{LIVE_RATE, Playback, Tone};
use nebula_core::config::Config;
use nebula_core::event::{UiCommand, UiStatus};
use nebula_core::secrets::{self, Secret};
use nebula_core::{Event, State, paths};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};

use crate::ipc;
use crate::live::{self, Controls, EndReason};

/// sysexits `EX_CONFIG`. The unit names this in `RestartPreventExitStatus=`,
/// so a configuration problem stops the service instead of restarting it into
/// the same failure every few seconds.
pub const EX_CONFIG: i32 = 78;

/// A failure no restart can fix: bad config, a missing key, or a
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
    let api_key =
        secrets::resolve_api_key(key_from_service, &paths::env_file()?).map_err(startup)?;
    let muted = Arc::new(AtomicBool::new(false));
    let shutdown = Arc::new(AtomicBool::new(false));
    let (status, _status_keep) = watch::channel(UiStatus::Standby);
    let (command_tx, mut commands) = mpsc::channel::<UiCommand>(32);

    let _instance = ipc::Instance::acquire(&paths::socket_path()?).map_err(startup)?;
    start_ipc(&status, &muted, command_tx)?;
    watch_for_ctrl_c(Arc::clone(&shutdown));
    if config.ui.enabled {
        crate::hud::start(config_path, Arc::clone(&shutdown));
    }

    let mut state = advance(State::Standby, Event::WakeDetected)?;
    tracing::info!("starting one call; run nebula again after it ends");
    let result = tokio::select! {
        result = session(
            &config,
            api_key.expose(),
            Instant::now(),
            &mut state,
            &status,
            &muted,
            &mut commands,
        ) => result.map(|_| ()),
        _ = wait_shutdown(&shutdown) => Ok(()),
    };

    tracing::info!("shutting down");
    if let Ok(path) = paths::socket_path() {
        let _ = std::fs::remove_file(path);
    }
    result
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
        | EndReason::AssistantEnded
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
