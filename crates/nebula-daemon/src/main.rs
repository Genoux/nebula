//! One voice call with a HUD; flags expose individual diagnostics.

mod daemon;
mod hud;
mod ipc;
mod live;
mod speech_display;
mod test_audio;
mod tools;
mod wake;

use anyhow::{Context, Result};
use clap::Parser;
use nebula_audio::{LIVE_RATE, Playback};
use nebula_core::event::UiStatus;
use nebula_core::{config::Config, paths, secrets};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;
use tokio::sync::{mpsc, watch};

#[derive(Parser, Debug)]
#[command(name = "nebula", version, about = "Local-first voice assistant")]
#[command(group(clap::ArgGroup::new("mode").args(["check", "check_live", "check_web", "check_tools", "check_tools_live", "test_audio", "train_wake", "test_wake", "manual_live"]).multiple(false)))]
struct Args {
    /// Validate configuration, secrets and their permissions, then exit.
    /// Prints what is wrong, never what is secret.
    #[arg(long)]
    check: bool,

    /// Test GPT-Live startup, generated audio and finalization using synthetic silence.
    #[arg(long)]
    check_live: bool,

    /// Verify a real web search through the GPT-Live reasoning backend.
    #[arg(long)]
    check_web: bool,

    /// Show local tool availability without loading credentials or connecting to servers.
    #[arg(long)]
    check_tools: bool,

    /// Test a harmless tools_list function call through GPT-Live using synthetic silence.
    #[arg(long)]
    check_tools_live: bool,

    /// Open the microphone and speakers, play the local tones, and measure
    /// the acoustic round trip.
    #[arg(long)]
    test_audio: bool,

    /// Skip the acoustic round-trip measurement, which cannot work on
    /// headphones.
    #[arg(long, requires = "test_audio")]
    no_loopback: bool,

    /// Record the configured wake phrase a few times and save its model.
    #[arg(long)]
    train_wake: bool,

    /// Listen for the wake phrase and report detections. Never contacts
    /// OpenAI.
    #[arg(long)]
    test_wake: bool,

    /// Skip the wake word and start a live conversation immediately.
    #[arg(long)]
    manual_live: bool,

    /// Override the config file location.
    #[arg(long, value_name = "PATH")]
    config: Option<std::path::PathBuf>,
}

fn main() -> Result<()> {
    // SAFETY: first statement of main - no thread exists yet.
    let key_from_service = unsafe { secrets::take_from_environment() };
    let args = Args::parse();
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        anyhow::bail!("run Nebula as a normal user, without sudo");
    }
    let config_path = match args.config {
        Some(path) => path,
        None => paths::config_file()?,
    };
    let config = match Config::load(&config_path) {
        Ok(config) => config,
        Err(_) => {
            eprintln!("invalid configuration at {}", config_path.display());
            std::process::exit(daemon::EX_CONFIG);
        }
    };
    init_logging(&config.logging.level);

    if args.check_tools {
        return tools::check(&config);
    }
    if args.check {
        return check(&config, &config_path, key_from_service);
    }
    if args.check_live || args.check_web || args.check_tools_live {
        let problems = config.problems();
        if !problems.is_empty() {
            anyhow::bail!("{}", problems.join("; "));
        }
        let key = secrets::resolve_api_key(key_from_service, &paths::env_file()?)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        return runtime.block_on(live::check_connection(
            &config,
            key.expose(),
            args.check_web,
            args.check_tools_live,
        ));
    }
    if args.test_audio {
        return test_audio::run(&config, args.no_loopback);
    }
    if args.train_wake {
        return wake::train(&config);
    }
    if args.test_wake {
        return wake::test(&config);
    }
    if args.manual_live {
        return manual_live(&config, key_from_service);
    }

    match daemon::run(config, key_from_service, config_path) {
        Err(e) if e.downcast_ref::<daemon::StartupError>().is_some() => {
            tracing::error!("{e:#}");
            std::process::exit(daemon::EX_CONFIG);
        }
        other => other,
    }
}

/// `--manual-live`: the conversation path with the wake word taken out of it,
/// so it can be debugged before the wake word exists.
fn manual_live(config: &Config, key_from_service: Option<secrets::Secret>) -> Result<()> {
    let problems = config.problems();
    if !problems.is_empty() {
        anyhow::bail!("{} - run --check for the full list", problems.join("; "));
    }
    let api_key = secrets::resolve_api_key(key_from_service, &paths::env_file()?)?;
    let _instance = ipc::Instance::acquire(&paths::socket_path()?)?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("could not start the async runtime")?;

    println!(
        "connecting to {} - wait for connected, ctrl-c to stop",
        config.session.model
    );
    let outcome = runtime.block_on(async {
        let started = Instant::now();
        let speakers = Playback::open(
            LIVE_RATE,
            &config.audio.output_device,
            config.audio.buffer_frames,
        )?;
        speakers
            .queue()
            .push(&nebula_audio::Tone::Wake.render(LIVE_RATE, 0.3));
        let session = tokio::select! {
            result = live::connect(config, api_key.expose(), started) => result?,
            _ = shutdown_signal() => return Ok(None),
        };
        println!("connected - speak now");
        let (status, _status_keep) = watch::channel(UiStatus::Connecting);
        // Held so the command channel stays open; this mode has no HUD.
        let (_command_keep, mut commands) = mpsc::channel(32);
        let controls = live::Controls {
            muted: Arc::new(AtomicBool::new(false)),
            commands: &mut commands,
            status,
        };
        session.run(config, speakers, controls).await.map(Some)
    })?;

    if let Some(outcome) = outcome {
        println!("\nsession ended: {}", outcome.reason.describe());
        println!("{}", outcome.metrics.report());
    }
    Ok(())
}

/// Written to stdout rather than through `tracing`: this is a report the user
/// asked for, not a log line, and it should not be reordered or filtered by the
/// level they happen to have set.
fn check(
    config: &Config,
    config_path: &std::path::Path,
    key_from_service: Option<secrets::Secret>,
) -> Result<()> {
    let mut failures = 0usize;

    println!("config:  {}", config_path.display());
    println!(
        "voice:   {} | backend: {}",
        config.session.model, config.session.backend_model
    );
    if !config_path.exists() {
        println!("  note   file does not exist; every value below is a default");
    }
    for problem in config.problems() {
        println!("  FAIL   {problem}");
        failures += 1;
    }

    for device in
        nebula_audio::route::check_devices(&config.audio.input_device, &config.audio.output_device)
    {
        match device {
            Ok(name) => println!("  ok     {name}"),
            Err(error) => {
                println!("  FAIL   {error}");
                failures += 1;
            }
        }
    }
    let env_path = paths::env_file()?;
    println!("secrets: {}", env_path.display());
    match secrets::resolve_api_key(key_from_service, &env_path) {
        Ok(_) => println!(
            "  ok     {} is present and the file is 0600",
            secrets::API_KEY_VAR
        ),
        Err(e) => {
            println!("  FAIL   {e:#}");
            failures += 1;
        }
    }

    println!(
        "privacy: store_audio={} store_transcripts={}",
        config.privacy.store_audio, config.privacy.store_transcripts
    );

    if failures == 0 {
        println!("\nall checks passed");
        Ok(())
    } else {
        anyhow::bail!("{failures} check(s) failed")
    }
}

/// `RUST_LOG` wins when set, so a debugging session does not require editing
/// config.toml and does not permanently raise the level the service runs at.
fn init_logging(level: &str) {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("nebula={level},nebula_daemon={level}")));
    let filter = filter
        .add_directive("tungstenite=off".parse().unwrap())
        .add_directive("tokio_tungstenite=off".parse().unwrap());
    fmt().with_env_filter(filter).with_target(false).init();
}

pub async fn shutdown_signal() {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("could not register SIGTERM");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = terminate.recv() => {},
    }
}
