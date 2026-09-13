use anyhow::{Context, Result};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub fn acquire(socket: &Path) -> Result<Option<File>> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(socket.with_extension("ui.lock"))?;
    // SAFETY: file owns a valid descriptor for the duration of flock.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(Some(file));
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        Ok(None)
    } else {
        Err(error).context("could not lock the HUD instance")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_hud_owns_a_socket_until_it_exits() {
        let socket = std::env::temp_dir().join(format!("nebula-ui-{}.sock", std::process::id()));
        let first = acquire(&socket).unwrap().unwrap();
        assert!(acquire(&socket).unwrap().is_none());
        drop(first);
        assert!(acquire(&socket).unwrap().is_some());
        std::fs::remove_file(socket.with_extension("ui.lock")).unwrap();
    }
}
