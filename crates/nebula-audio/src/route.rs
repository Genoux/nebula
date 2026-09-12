//! Choosing a device, by moving our own PipeWire stream rather than by opening
//! a named one.
//!
//! cpal talks to ALSA here, so the devices it enumerates are ALSA plugin names
//! ("Rate Converter Plugin Using Speex Resampler", "Default ALSA Output") and
//! not the PipeWire node names `pactl` prints and a user would recognise.
//! Opening a raw `hw:` PCM through cpal also loses the free rate conversion
//! that makes [`crate::capture`] able to ask for 24kHz mono from 48kHz stereo
//! hardware. So Nebula always opens cpal's default device, and device selection
//! happens one layer up, on the graph.

use anyhow::{Result, anyhow, bail};
use std::process::{Command, Stdio};

/// What kind of endpoint a stream is being moved between. PipeWire spells the
/// two directions differently in every command, so they are paired here once.
#[derive(Clone, Copy)]
pub enum Direction {
    Capture,
    Playback,
}

impl Direction {
    fn list(self) -> &'static str {
        match self {
            Direction::Capture => "source-outputs",
            Direction::Playback => "sink-inputs",
        }
    }

    fn move_cmd(self) -> &'static str {
        match self {
            Direction::Capture => "move-source-output",
            Direction::Playback => "move-sink-input",
        }
    }
}

/// PipeWire suspends idle devices, and a suspended source wakes with a burst of
/// zeros - which looks exactly like a muted microphone. Unsuspend before
/// opening a stream so the first callback carries real audio.
pub fn unsuspend_source(name: &str) {
    let _ = Command::new("pactl")
        .args(["suspend-source", name, "0"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Find the numeric id of our own stream in `pactl list` output.
///
/// Split out and pure so the parser is tested against real `pactl` text rather
/// than only against a live graph, which no test can rely on having.
pub fn find_own_stream(listing: &str, pid: u32) -> Option<String> {
    let mut current = None;
    for line in listing.lines() {
        let trimmed = line.trim();
        if let Some(id) = trimmed
            .strip_prefix("Source Output #")
            .or_else(|| trimmed.strip_prefix("Sink Input #"))
            .or_else(|| trimmed.strip_prefix("Client #"))
        {
            current = Some(id.trim().to_string());
        }
        if trimmed
            .strip_prefix("application.process.id = ")
            .and_then(|value| value.trim_matches('"').parse::<u32>().ok())
            == Some(pid)
        {
            return current;
        }
    }
    None
}

fn find_client_stream(listing: &str, client: &str) -> Option<String> {
    let mut current = None;
    for line in listing.lines().map(str::trim) {
        if let Some(id) = line
            .strip_prefix("Source Output #")
            .or_else(|| line.strip_prefix("Sink Input #"))
        {
            current = Some(id.trim().to_string());
        }
        if line.strip_prefix("Client:").map(str::trim) == Some(client) {
            return current;
        }
    }
    None
}

fn find_owned_client_stream(listing: &str, clients: &str, pid: u32) -> Option<String> {
    clients.split("Client #").skip(1).find_map(|block| {
        find_own_stream(&format!("Client #{block}"), pid)
            .and_then(|client| find_client_stream(listing, &client))
    })
}

fn own_stream(direction: Direction, pid: u32) -> Result<Option<String>> {
    let listing = Command::new("pactl")
        .args(["list", direction.list()])
        .output()?;
    if !listing.status.success() {
        bail!("could not list audio streams");
    }
    let listing = String::from_utf8_lossy(&listing.stdout);
    if let Some(id) = find_own_stream(&listing, pid) {
        return Ok(Some(id));
    }
    // PipeWire 1.6 ALSA streams carry the process id on their owning client.
    let clients = Command::new("pactl").args(["list", "clients"]).output()?;
    if !clients.status.success() {
        bail!("could not list audio clients");
    }
    Ok(find_owned_client_stream(
        &listing,
        &String::from_utf8_lossy(&clients.stdout),
        pid,
    ))
}

/// Point our own stream at `target`, leaving every other application and the
/// system default exactly as they were.
pub fn move_own_stream(direction: Direction, target: &str) -> Result<()> {
    let pid = std::process::id();
    let mut id = None;
    for _ in 0..20 {
        id = own_stream(direction, pid)?;
        if id.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let id = id.ok_or_else(|| {
        anyhow!(
            "no {} stream for process {pid} in the graph",
            direction.list()
        )
    })?;

    let moved = Command::new("pactl")
        .args([direction.move_cmd(), &id, target])
        .output()?;
    if !moved.status.success() {
        bail!(
            "pactl {}: {}",
            direction.move_cmd(),
            String::from_utf8_lossy(&moved.stderr).trim()
        );
    }
    Ok(())
}

/// `None` for the configured value `default`, which means "follow whatever
/// WirePlumber has chosen" and needs no routing at all.
pub fn wanted(configured: &str) -> Option<&str> {
    match configured.trim() {
        "" | "default" => None,
        name => Some(name),
    }
}

pub fn check_devices(input: &str, output: &str) -> Vec<Result<String>> {
    [
        ("sources", "get-default-source", input),
        ("sinks", "get-default-sink", output),
    ]
    .into_iter()
    .map(|(kind, default_command, selected)| {
        let listing = Command::new("pactl")
            .args(["list", "short", kind])
            .output()?;
        if !listing.status.success() {
            bail!("cannot list {kind}; is PipeWire running?");
        }
        let name = if selected == "default" {
            let default = Command::new("pactl").arg(default_command).output()?;
            if !default.status.success() {
                bail!("no default device for {kind}");
            }
            String::from_utf8_lossy(&default.stdout).trim().to_string()
        } else {
            selected.to_string()
        };
        if !String::from_utf8_lossy(&listing.stdout)
            .lines()
            .any(|line| line.split_whitespace().nth(1) == Some(name.as_str()))
        {
            bail!("configured {kind} device is unavailable");
        }
        Ok(format!("{kind}: {name}"))
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed from real `pactl list source-outputs` output on this machine.
    const LISTING: &str = r#"
Source Output #128
	Driver: PipeWire
	Owner Module: n/a
	Client: 127
	Properties:
		application.name = "Firefox"
		application.process.binary = "firefox"
		application.process.id = "100"

Source Output #391
	Driver: PipeWire
	Client: 388
	Properties:
		application.name = "nebula"
		application.process.binary = "nebula"
		application.process.id = "200"
"#;

    #[test]
    fn finds_our_own_stream_and_not_someone_elses() {
        assert_eq!(find_own_stream(LISTING, 200), Some("391".into()));
    }

    #[test]
    fn finds_another_clients_stream_by_its_own_name() {
        assert_eq!(find_own_stream(LISTING, 100), Some("128".into()));
    }

    #[test]
    fn absent_client_is_none_rather_than_the_wrong_stream() {
        assert_eq!(find_own_stream(LISTING, 300), None);
    }

    #[test]
    fn sink_inputs_are_parsed_too() {
        let listing = "Sink Input #7\n\tProperties:\n\t\tapplication.process.id = \"200\"\n";
        assert_eq!(find_own_stream(listing, 200), Some("7".into()));
    }

    #[test]
    fn default_means_no_routing() {
        assert_eq!(wanted("default"), None);
        assert_eq!(wanted("  "), None);
        assert_eq!(wanted("alsa_input.usb-Foo"), Some("alsa_input.usb-Foo"));
    }

    #[test]
    fn alsa_stream_is_resolved_through_its_owning_client() {
        let clients = "Client #90\n application.process.id = \"123\"\nClient #91\n application.process.id = \"456\"\n";
        let streams = "Source Output #20\n Client: 91\nSource Output #21\n Client: 90\n";
        let client = find_own_stream(clients, 123).unwrap();
        assert_eq!(find_client_stream(streams, &client).as_deref(), Some("21"));
        assert_eq!(find_client_stream(streams, "92"), None);
        assert_eq!(find_own_stream(clients, 789), None);
        assert_eq!(find_own_stream(streams, 123), None);
    }

    #[test]
    fn capture_and_playback_can_have_separate_clients_for_one_process() {
        let clients = "Client #90\n application.process.id = \"123\"\nClient #91\n application.process.id = \"123\"\nClient #92\n application.process.id = \"456\"\n";
        let streams = "Sink Input #20\n Client: 92\nSink Input #21\n Client: 91\n";
        assert_eq!(
            find_owned_client_stream(streams, clients, 123).as_deref(),
            Some("21")
        );
        assert_eq!(find_owned_client_stream(streams, clients, 789), None);
    }
}
