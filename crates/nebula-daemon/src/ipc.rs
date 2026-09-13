//! The local socket the HUD talks to.
//!
//! Status goes out through a `watch` channel, which holds only the latest value
//! and never makes the sender wait. That is the whole guarantee that the UI
//! cannot slow audio down: a HUD that stops reading just misses frames, and a
//! HUD that crashes is a closed socket nobody is waiting on.

use anyhow::{Context, Result};
use nebula_core::event::{UiCommand, UiStatus};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, watch};

/// A live socket that answers means another daemon owns this session. Checked
/// before binding, because binding first would unlink its socket out from
/// under it.
pub fn another_daemon_running(path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

pub struct Instance {
    _file: std::fs::File,
}

impl Instance {
    pub fn acquire(path: &Path) -> Result<Self> {
        let lock = path.with_extension("lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&lock)?;
        // SAFETY: file owns a valid descriptor for the duration of flock.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            anyhow::bail!("another nebula daemon is already running");
        }
        Ok(Self { _file: file })
    }
}

pub fn bind(path: &Path) -> Result<UnixListener> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if !metadata.file_type().is_socket() {
            anyhow::bail!("HUD socket path is not a socket");
        }
        if another_daemon_running(path) {
            anyhow::bail!("another nebula daemon is already running");
        }
        std::fs::remove_file(path)?;
    }
    let listener =
        UnixListener::bind(path).with_context(|| format!("could not bind {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("could not restrict {}", path.display()))?;
    Ok(listener)
}

pub async fn serve(
    listener: UnixListener,
    status: watch::Sender<UiStatus>,
    muted: Arc<AtomicBool>,
    commands: mpsc::Sender<UiCommand>,
) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(client(
                    stream,
                    status.clone(),
                    Arc::clone(&muted),
                    commands.clone(),
                ));
            }
            Err(e) => tracing::warn!("hud connection failed: {e}"),
        }
    }
}

async fn client(
    stream: UnixStream,
    status: watch::Sender<UiStatus>,
    muted: Arc<AtomicBool>,
    commands: mpsc::Sender<UiCommand>,
) {
    let mut updates = status.subscribe();
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();

    let first = visible_status(&updates.borrow_and_update(), &muted).to_line();
    if write.write_all(first.as_bytes()).await.is_err() {
        return;
    }

    loop {
        tokio::select! {
            changed = updates.changed() => {
                if changed.is_err() {
                    return;
                }
                let line = visible_status(&updates.borrow_and_update(), &muted).to_line();
                if write.write_all(line.as_bytes()).await.is_err() {
                    return;
                }
            }
            line = lines.next_line() => {
                let Ok(Some(text)) = line else { return };
                match serde_json::from_str::<UiCommand>(&text) {
                    Ok(UiCommand::ToggleMute) => {
                        let now_muted = !muted.fetch_xor(true, Ordering::Relaxed);
                        tracing::info!(muted = now_muted, "microphone toggled from the hud");
                        status.send_modify(|_| {});
                    }
                    Ok(UiCommand::EndSession) => {
                        let _ = commands.try_send(UiCommand::EndSession);
                    }
                    Err(_) => tracing::warn!("ignoring a malformed hud command"),
                }
            }
        }
    }
}

fn visible_status(status: &UiStatus, muted: &AtomicBool) -> UiStatus {
    if muted.load(Ordering::Relaxed) {
        UiStatus::Muted.with_activity(status.activity())
    } else {
        status.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncBufReadExt;

    fn socket(name: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("nebula-ipc-{name}-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn only_one_instance_can_hold_the_lock() {
        let path = socket("lock");
        let first = Instance::acquire(&path).unwrap();
        assert!(Instance::acquire(&path).is_err());
        drop(first);
        assert!(Instance::acquire(&path).is_ok());
    }

    #[tokio::test]
    async fn binding_does_not_unlink_a_live_daemon() {
        let path = socket("keep");
        let _listener = bind(&path).unwrap();
        assert!(bind(&path).is_err());
        assert!(another_daemon_running(&path));
    }

    #[tokio::test]
    async fn the_socket_is_owner_only() {
        let path = socket("perms");
        let _listener = bind(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[tokio::test]
    async fn a_running_daemon_is_detected_and_a_stale_socket_is_not() {
        let path = socket("running");
        assert!(!another_daemon_running(&path));
        let listener = bind(&path).unwrap();
        assert!(another_daemon_running(&path));
        drop(listener);
        assert!(
            !another_daemon_running(&path),
            "a dead socket file is not a daemon"
        );
    }

    #[tokio::test]
    async fn a_client_gets_the_current_status_and_can_mute() {
        let path = socket("client");
        let listener = bind(&path).unwrap();
        let (status, _keep) = watch::channel(UiStatus::Thinking);
        let muted = Arc::new(AtomicBool::new(false));
        let (commands, mut received) = mpsc::channel(32);
        tokio::spawn(serve(
            listener,
            status.clone(),
            Arc::clone(&muted),
            commands,
        ));

        let stream = UnixStream::connect(&path).await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();

        let first: UiStatus =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(first, UiStatus::Thinking);

        write
            .write_all(b"{\"command\":\"toggle_mute\"}\n")
            .await
            .unwrap();
        let next: UiStatus =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(next, UiStatus::Muted);
        assert!(muted.load(Ordering::Relaxed));
        write
            .write_all(b"{\"command\":\"toggle_mute\"}\n")
            .await
            .unwrap();
        let restored: UiStatus =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(restored, UiStatus::Thinking);

        write
            .write_all(b"{\"command\":\"end_session\"}\n")
            .await
            .unwrap();
        assert_eq!(received.recv().await, Some(UiCommand::EndSession));
    }

    #[tokio::test]
    async fn garbage_from_a_client_does_not_disconnect_it() {
        let path = socket("garbage");
        let listener = bind(&path).unwrap();
        let (status, _keep) = watch::channel(UiStatus::Standby);
        let (commands, _received) = mpsc::channel(32);
        tokio::spawn(serve(
            listener,
            status.clone(),
            Arc::new(AtomicBool::new(false)),
            commands,
        ));

        let stream = UnixStream::connect(&path).await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        lines.next_line().await.unwrap();

        write.write_all(b"not json\n").await.unwrap();
        status.send_replace(UiStatus::Connecting);
        let next: UiStatus =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(next, UiStatus::Connecting);
    }
}
