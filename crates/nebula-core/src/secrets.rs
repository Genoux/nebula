//! Loading `nebula.env` and keeping what is in it out of everything else.
//!
//! The whole point of the wrapper below is that a secret cannot reach a log by
//! accident. `tracing` will happily format any `Debug` value, `anyhow` chains
//! carry `Display`, and a panic prints both - so [`Secret`] implements them to
//! print a placeholder, and the real bytes come out only through
//! [`Secret::expose`], which is greppable.

use anyhow::{Context, Result, bail};
use std::fmt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub const API_KEY_VAR: &str = "OPENAI_API_KEY";

#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// The only way out. Named so that `rg 'expose'` finds every site where a
    /// secret becomes an ordinary string.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Mode 0600. Group and world bits are refused rather than repaired: a file
/// that was readable by somebody else has already been readable by them, and
/// quietly chmod'ing it would hide that from the person who needs to rotate the
/// key.
pub fn check_permissions(path: &Path) -> Result<()> {
    let file = open_secret(path)?;
    validate_file(&file, path)
}

fn open_secret(path: &Path) -> Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("could not open secret file {}", path.display()))
}

fn validate_file(file: &std::fs::File, path: &Path) -> Result<()> {
    let metadata = file.metadata()?;
    let mode = metadata.permissions().mode() & 0o7777;
    // SAFETY: geteuid has no preconditions and does not mutate process state.
    if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } || mode != 0o600 {
        bail!(
            "{} must be a regular file owned by this user with mode 0600",
            path.display()
        );
    }
    Ok(())
}

/// Reads the env file into this process only. Deliberately does *not* call
/// `dotenvy::dotenv`, which mutates the process environment and would put the
/// key into `/proc/self/environ` for anything that can read it, and into any
/// child process Nebula ever spawns.
/// Take the key out of the service environment, so nothing Nebula spawns
/// (it runs `pactl`) inherits it.
///
/// The systemd unit loads `nebula.env` with `EnvironmentFile=`, which puts the
/// key in the environment whether Nebula wants it there or not. Removing it is
/// the best available after the fact: children stop inheriting it, although
/// `/proc/<pid>/environ` still shows the block the process started with.
///
/// # Safety
///
/// Call while the process is single-threaded - as the first statement of
/// `main` - because changing the environment races any thread reading it.
pub unsafe fn take_from_environment() -> Option<Secret> {
    let value = std::env::var(API_KEY_VAR).ok();
    // SAFETY: forwarded to the caller, see above.
    unsafe { std::env::remove_var(API_KEY_VAR) };
    value.filter(|v| !v.trim().is_empty()).map(Secret)
}

/// The service environment when it carries a key, the file otherwise. The
/// file's 0600 rule is enforced either way: a key that arrived through the
/// environment still came out of that file.
pub fn resolve_api_key(from_environment: Option<Secret>, env_path: &Path) -> Result<Secret> {
    match from_environment {
        Some(key) => {
            check_permissions(env_path)?;
            Ok(key)
        }
        None => load_api_key(env_path),
    }
}

pub fn load_api_key(env_path: &Path) -> Result<Secret> {
    if !env_path.exists() {
        bail!(
            "{} does not exist - create it with a line {}=... and chmod 600",
            env_path.display(),
            API_KEY_VAR
        );
    }
    let file = open_secret(env_path)?;
    validate_file(&file, env_path)?;
    let mut found = None;
    for entry in dotenvy::from_read_iter(file) {
        let (key, value) =
            entry.map_err(|_| anyhow::anyhow!("nebula.env contains an invalid assignment"))?;
        if key == API_KEY_VAR {
            if found.is_some() {
                bail!("nebula.env contains duplicate API key assignments");
            }
            found = Some(value);
        }
    }

    match found {
        Some(value) if !value.trim().is_empty() => Ok(Secret(value)),
        Some(_) => bail!("{} is set but empty in {}", API_KEY_VAR, env_path.display()),
        None => bail!("{} is not set in {}", API_KEY_VAR, env_path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_env(dir: &Path, body: &str, mode: u32) -> std::path::PathBuf {
        let path = dir.join("nebula.env");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("nebula-secret-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_service_environment_wins_over_the_file() {
        let dir = scratch("env-wins");
        let path = write_env(&dir, "OPENAI_API_KEY=sk-from-file\n", 0o600);
        let key = resolve_api_key(Some(Secret("sk-from-env".into())), &path).unwrap();
        assert_eq!(key.expose(), "sk-from-env");
    }

    #[test]
    fn a_loose_file_is_refused_even_when_the_environment_has_the_key() {
        let dir = scratch("env-perms");
        let path = write_env(&dir, "OPENAI_API_KEY=sk-x\n", 0o644);
        assert!(resolve_api_key(Some(Secret("sk-from-env".into())), &path).is_err());
    }

    #[test]
    fn without_the_environment_the_file_is_read() {
        let dir = scratch("env-none");
        let path = write_env(&dir, "OPENAI_API_KEY=sk-from-file\n", 0o600);
        assert_eq!(
            resolve_api_key(None, &path).unwrap().expose(),
            "sk-from-file"
        );
    }

    #[test]
    fn a_symlink_to_a_private_file_is_refused() {
        let dir = scratch("symlink");
        let file = write_env(&dir, "OPENAI_API_KEY=test-placeholder\n", 0o600);
        let link = dir.join("link.env");
        std::os::unix::fs::symlink(file, &link).unwrap();
        assert!(load_api_key(&link).is_err());
    }

    #[test]
    fn duplicate_and_malformed_assignments_do_not_echo_values() {
        for (name, body) in [
            (
                "duplicate",
                "OPENAI_API_KEY=private-first\nOPENAI_API_KEY=private-second\n",
            ),
            (
                "malformed",
                "OPENAI_API_KEY=private-first\ninvalid private-second\n",
            ),
        ] {
            let dir = scratch(name);
            let file = write_env(&dir, body, 0o600);
            let error = format!("{:#}", load_api_key(&file).unwrap_err());
            assert!(!error.contains("private-first"));
            assert!(!error.contains("private-second"));
        }
    }

    #[test]
    fn a_secret_never_prints_itself() {
        let s = Secret("sk-should-never-appear".into());
        assert!(!format!("{s:?}").contains("should-never-appear"));
        assert!(!format!("{s}").contains("should-never-appear"));
        assert!(!format!("{s:#?}").contains("should-never-appear"));
        assert_eq!(s.expose(), "sk-should-never-appear");
    }

    #[test]
    fn loads_a_key_from_a_0600_file() {
        let dir = scratch("ok");
        let path = write_env(&dir, "OPENAI_API_KEY=sk-test-value\n", 0o600);
        assert_eq!(load_api_key(&path).unwrap().expose(), "sk-test-value");
    }

    #[test]
    fn a_group_readable_file_is_refused() {
        let dir = scratch("perms");
        let path = write_env(&dir, "OPENAI_API_KEY=sk-test\n", 0o640);
        let err = format!("{:#}", load_api_key(&path).unwrap_err());
        assert!(err.contains("0600"), "{err}");
        assert!(!err.contains("sk-test"), "the error leaked the key: {err}");
    }

    #[test]
    fn a_missing_key_is_named_without_being_printed() {
        let dir = scratch("missing");
        let path = write_env(&dir, "SOMETHING_ELSE=1\n", 0o600);
        let err = format!("{:#}", load_api_key(&path).unwrap_err());
        assert!(err.contains("OPENAI_API_KEY"), "{err}");
    }

    #[test]
    fn an_empty_key_is_not_a_key() {
        let dir = scratch("empty");
        let path = write_env(&dir, "OPENAI_API_KEY=\n", 0o600);
        assert!(load_api_key(&path).is_err());
    }

    #[test]
    fn loading_does_not_touch_the_process_environment() {
        let dir = scratch("noleak");
        let path = write_env(&dir, "OPENAI_API_KEY=sk-not-in-environ\n", 0o600);
        load_api_key(&path).unwrap();
        assert!(std::env::var_os("OPENAI_API_KEY").is_none());
    }
}
