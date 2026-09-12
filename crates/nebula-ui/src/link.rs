//! The connection to the daemon, on its own thread and re-established whenever
//! it drops - the daemon starting after the HUD, or restarting under systemd,
//! is normal rather than an error.

use anyhow::{Context, Result};
use nebula_core::event::{UiCommand, UiStatus};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const RETRY: Duration = Duration::from_secs(2);
pub const OFFLINE: &str = "Nebula offline";

pub struct Link {
    writer: Arc<Mutex<Option<UnixStream>>>,
}

impl Link {
    pub fn spawn(path: PathBuf, statuses: Sender<UiStatus>) -> Self {
        let writer = Arc::new(Mutex::new(None));
        let shared = Arc::clone(&writer);
        std::thread::Builder::new()
            .name("daemon-link".into())
            .spawn(move || follow(&path, &statuses, &shared))
            .expect("could not spawn the daemon link thread");
        Self { writer }
    }

    /// Dropped silently while the daemon is offline: there is nothing to mute
    /// or end, and the HUD is already showing that it is offline.
    pub fn send(&self, command: UiCommand) {
        let mut guard = self.writer.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(stream) = guard.as_mut()
            && write_command(stream, command).is_err()
        {
            *guard = None;
        }
    }
}

fn follow(path: &Path, statuses: &Sender<UiStatus>, writer: &Mutex<Option<UnixStream>>) {
    let mut offline_announced = false;
    loop {
        if let Ok(stream) = UnixStream::connect(path) {
            let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
            *writer.lock().unwrap_or_else(|p| p.into_inner()) = stream.try_clone().ok();
            offline_announced = false;
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                match serde_json::from_str::<UiStatus>(&line) {
                    Ok(status) => {
                        if statuses.send(status).is_err() {
                            return;
                        }
                    }
                    Err(_) => tracing::warn!("ignoring a malformed status line"),
                }
            }
            *writer.lock().unwrap_or_else(|p| p.into_inner()) = None;
        }
        // Once per outage, not once per retry.
        if !offline_announced {
            offline_announced = true;
            if statuses
                .send(UiStatus::Error {
                    message: OFFLINE.into(),
                })
                .is_err()
            {
                return;
            }
        }
        std::thread::sleep(RETRY);
    }
}

fn write_command(stream: &mut UnixStream, command: UiCommand) -> std::io::Result<()> {
    let mut line = serde_json::to_string(&command).expect("UiCommand always serialises");
    line.push('\n');
    stream.write_all(line.as_bytes())
}

pub fn send_once(path: &Path, command: UiCommand) -> Result<()> {
    let mut stream = UnixStream::connect(path)
        .with_context(|| format!("the nebula daemon is not running ({})", path.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut first = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut first)?;
    write_command(&mut stream, command)?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut replies = Vec::new();
    stream.take(4096).read_to_end(&mut replies)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;

    fn socket(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("nebula-ui-{name}-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn statuses_arrive_and_a_dropped_daemon_reads_as_offline() {
        let path = socket("follow");
        let listener = UnixListener::bind(&path).unwrap();
        let (tx, rx) = mpsc::channel();
        let _link = Link::spawn(path.clone(), tx);

        let (mut daemon, _) = listener.accept().unwrap();
        daemon.write_all(b"{\"status\":\"thinking\"}\n").unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            UiStatus::Thinking
        );

        drop(daemon);
        drop(listener);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            UiStatus::Error {
                message: OFFLINE.into()
            }
        );
    }

    #[test]
    fn commands_reach_the_daemon_as_one_json_line() {
        let path = socket("send");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(b"{\"status\":\"standby\"}\n").unwrap();
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).unwrap();
            line
        });
        send_once(&path, UiCommand::ToggleMute).unwrap();
        assert_eq!(server.join().unwrap(), "{\"command\":\"toggle_mute\"}\n");
    }

    #[test]
    fn sending_with_no_daemon_says_so() {
        let err = send_once(&socket("absent"), UiCommand::EndSession).unwrap_err();
        assert!(format!("{err:#}").contains("not running"));
    }
}
