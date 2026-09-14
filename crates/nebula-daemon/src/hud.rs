use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub fn start(config: PathBuf, shutdown: Arc<AtomicBool>) {
    if !["WAYLAND_DISPLAY", "DISPLAY"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
    {
        tracing::warn!(
            "no graphical session environment; start nebula-ui from your desktop session"
        );
        return;
    }
    let binary = match std::env::current_exe() {
        Ok(path) => path.with_file_name("nebula-ui"),
        Err(error) => {
            tracing::warn!("could not locate the HUD executable: {error}");
            return;
        }
    };
    tokio::spawn(async move {
        let mut child = match tokio::process::Command::new(&binary)
            .arg("--config")
            .arg(&config)
            .env_remove("OPENAI_API_KEY")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                tracing::warn!("could not start nebula-ui: {error}");
                return;
            }
        };
        match child.wait().await {
            Ok(status) => tracing::info!("HUD closed ({status}); shutting down"),
            Err(error) => tracing::warn!("HUD wait failed: {error}; shutting down"),
        }
        shutdown.store(true, Ordering::Relaxed);
    });
}
