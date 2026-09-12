//! The Nebula HUD: a small floating pill showing what the daemon is doing.
//!
//! Its own process, so a crash here costs a picture and nothing else - the
//! daemon keeps listening and talking whether or not this is running.

mod hud;
mod link;
mod look;
mod notify;
mod paint;

use anyhow::Result;
use clap::Parser;
use nebula_core::event::UiCommand;
use nebula_core::{config::Config, paths};
use std::path::PathBuf;
use std::sync::mpsc;

#[derive(Parser, Debug)]
#[command(name = "nebula-ui", version, about = "Minimal status HUD for Nebula")]
struct Args {
    /// Toggle microphone mute and exit. Bind this to a key in the compositor:
    /// the HUD itself never takes keyboard focus.
    #[arg(long, conflicts_with = "end_session")]
    toggle_mute: bool,

    /// End the live session, if there is one, and exit.
    #[arg(long)]
    end_session: bool,

    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let socket = paths::socket_path()?;
    if args.toggle_mute {
        return link::send_once(&socket, UiCommand::ToggleMute);
    }
    if args.end_session {
        return link::send_once(&socket, UiCommand::EndSession);
    }

    let config = Config::load(&args.config.map_or_else(paths::config_file, Ok)?)?;
    init_logging(&config.logging.level);
    if !config.ui.enabled {
        tracing::info!("ui.enabled is false - nothing to show");
        return Ok(());
    }

    let (tx, statuses) = mpsc::channel();
    let link = link::Link::spawn(socket, tx);
    match hud::Hud::connect() {
        Ok(hud) => hud.run(&config.ui, &statuses, &link),
        Err(e) => {
            tracing::warn!("no overlay ({e:#}) - falling back to desktop notifications");
            notify::run(&statuses);
            Ok(())
        }
    }
}

fn init_logging(level: &str) {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("nebula_ui={level}")));
    fmt().with_env_filter(filter).with_target(false).init();
}
