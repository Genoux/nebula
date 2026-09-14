use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub const MAX_NOTES: usize = 24;
pub const MAX_BYTES: usize = 8192;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Note {
    pub value: String,
    pub source: String,
}

type Notes = BTreeMap<String, Note>;

pub fn path() -> Result<PathBuf> {
    Ok(nebula_core::paths::config_dir()?.join("memory.json"))
}

fn validate(notes: &Notes) -> Result<Vec<u8>> {
    if notes.len() > MAX_NOTES {
        bail!("memory is full; update, consolidate or forget an existing note first");
    }
    for (key, note) in notes {
        if key.is_empty()
            || key.len() > 48
            || !key
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"._-".contains(&c))
        {
            bail!(
                "memory keys must be 1-48 lowercase letters, digits, dots, underscores or hyphens"
            );
        }
        if note.value.trim().is_empty()
            || note.value.len() > 320
            || note.source.trim().is_empty()
            || note.source.len() > 120
        {
            bail!("memory needs a short value (1-320 bytes) and source (1-120 bytes)");
        }
        super::reject_credentials(&note.value)?;
        super::reject_credentials(&note.source)?;
    }
    let bytes = serde_json::to_vec(notes)?;
    if bytes.len() > MAX_BYTES {
        bail!("memory exceeds 8192 bytes; shorten or consolidate notes first");
    }
    Ok(bytes)
}

fn load(path: &Path) -> Result<Notes> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Notes::new()),
        Err(_) => bail!("memory file cannot be opened safely"),
    };
    if !file.metadata()?.is_file() {
        bail!("memory must be a regular file");
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_BYTES {
        bail!("memory file exceeds 8192 bytes");
    }
    let notes: Notes = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("memory file is invalid; repair it before updating"))?;
    validate(&notes)?;
    Ok(notes)
}

pub fn read(path: &Path) -> Result<serde_json::Value> {
    let notes = load(path)?;
    let bytes = validate(&notes)?.len();
    Ok(
        serde_json::json!({"ok":true,"notes":notes,"used_bytes":bytes,"max_bytes":MAX_BYTES,"max_notes":MAX_NOTES}),
    )
}

struct MemoryLock(File);

impl Drop for MemoryLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn lock(path: &Path) -> Result<MemoryLock> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path.with_extension("lock"))?;
    if !file.metadata()?.is_file() {
        bail!("memory lock must be a regular file");
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        bail!("memory is being updated; read it again before retrying");
    }
    Ok(MemoryLock(file))
}

pub fn update(
    path: &Path,
    key: &str,
    expected_value: Option<&str>,
    note: Option<Note>,
) -> Result<serde_json::Value> {
    let _lock = lock(path)?;
    let mut notes = load(path)?;
    if notes.get(key).map(|note| note.value.as_str()) != expected_value {
        bail!("memory note changed; call memory_read and use its current value");
    }
    if let Some(note) = note {
        notes.insert(key.into(), note);
    } else if notes.remove(key).is_none() {
        bail!("memory note does not exist");
    }
    let bytes = validate(&notes)?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .context("cannot create memory update file")?;
    let result = (|| -> Result<()> {
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result?;
    Ok(
        serde_json::json!({"ok":true,"key":key,"note":notes.get(key),"used_bytes":bytes.len(),"max_bytes":MAX_BYTES,"note_count":notes.len(),"max_notes":MAX_NOTES}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Folder(PathBuf);
    impl Folder {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "nebula-memory-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> PathBuf {
            self.0.join("memory.json")
        }
    }
    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn note(value: &str) -> Note {
        Note {
            value: value.into(),
            source: "user preference".into(),
        }
    }

    #[test]
    fn notes_survive_reload_and_stale_edits_do_not_overwrite() {
        let folder = Folder::new();
        let path = folder.path();
        assert_eq!(read(&path).unwrap()["notes"], serde_json::json!({}));
        update(
            &path,
            "desktop.wallpaper",
            None,
            Some(note("Flow manages wallpaper")),
        )
        .unwrap();
        assert_eq!(
            read(&path).unwrap()["notes"]["desktop.wallpaper"]["value"],
            "Flow manages wallpaper"
        );
        assert!(update(&path, "desktop.wallpaper", None, Some(note("stale"))).is_err());
        update(
            &path,
            "desktop.wallpaper",
            Some("Flow manages wallpaper"),
            Some(note("New setup")),
        )
        .unwrap();
        assert!(
            update(
                &path,
                "desktop.wallpaper",
                Some("Flow manages wallpaper"),
                None
            )
            .is_err()
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        update(&path, "desktop.wallpaper", Some("New setup"), None).unwrap();
        assert_eq!(read(&path).unwrap()["notes"], serde_json::json!({}));
    }

    #[test]
    fn capacity_rejects_growth_without_losing_existing_notes() {
        let folder = Folder::new();
        let path = folder.path();
        for i in 0..MAX_NOTES {
            update(&path, &format!("note.{i}"), None, Some(note("fact"))).unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        assert!(update(&path, "overflow", None, Some(note("fact"))).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        update(&path, "note.0", Some("fact"), Some(note("corrected fact"))).unwrap();
        let notes: Notes = (0..MAX_NOTES)
            .map(|i| (format!("note.{i}"), note(&"x".repeat(320))))
            .collect();
        assert!(validate(&notes).is_err());
    }

    #[test]
    fn unsafe_or_invalid_notes_are_not_saved() {
        let folder = Folder::new();
        let path = folder.path();
        for (key, value) in [
            ("bad key", "fact"),
            ("note", "API_KEY=private"),
            ("note", ""),
        ] {
            assert!(update(&path, key, None, Some(note(value))).is_err());
        }
        assert!(update(&path, "note", None, Some(note(&"x".repeat(321)))).is_err());
        assert!(!path.exists());
        std::fs::write(&path, "invalid json").unwrap();
        assert!(update(&path, "note", None, Some(note("fact"))).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "invalid json");
        std::fs::remove_file(&path).unwrap();
        symlink(folder.0.join("target"), &path).unwrap();
        assert!(update(&path, "note", None, Some(note("fact"))).is_err());
        assert!(!folder.0.join("target").exists());
    }

    #[test]
    fn concurrent_writer_is_rejected() {
        let folder = Folder::new();
        let path = folder.path();
        let _held = lock(&path).unwrap();
        assert!(update(&path, "note", None, Some(note("fact"))).is_err());
    }
}
