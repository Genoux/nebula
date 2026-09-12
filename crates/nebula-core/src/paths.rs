//! Config and socket locations, resolved from the environment rather than from
//! a crate: the only rules Nebula needs are `$XDG_CONFIG_HOME` with a `$HOME`
//! fallback and `$XDG_RUNTIME_DIR`, both a few lines each.

use anyhow::{Context, Result};
use std::ffi::OsString;
use std::path::PathBuf;

const APP: &str = "nebula";

/// Split from [`config_dir`] so the precedence rule can be tested without
/// mutating the process environment: `set_var` is unsafe in edition 2024 and
/// racy under a threaded test runner, and the rule is the only part worth
/// testing anyway.
fn resolve_config_dir(xdg: Option<OsString>, home: Option<OsString>) -> Result<PathBuf> {
    let base = match xdg {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(home.filter(|h| !h.is_empty()).context(
            "neither XDG_CONFIG_HOME nor HOME is set, so there is nowhere to look for config",
        )?)
        .join(".config"),
    };
    Ok(base.join(APP))
}

pub fn config_dir() -> Result<PathBuf> {
    resolve_config_dir(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

pub fn config_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

pub fn env_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("nebula.env"))
}

/// A trained wake phrase, one file per phrase so "nebula" and "hey nebula" can
/// both be recorded and switched between in config without retraining.
pub fn wake_model(phrase: &str) -> Result<PathBuf> {
    Ok(config_dir()?
        .join("wake")
        .join(format!("{}.bin", model_file_stem(phrase))))
}

fn model_file_stem(phrase: &str) -> String {
    phrase
        .trim()
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// The daemon-to-UI socket. Under `$XDG_RUNTIME_DIR` because the kernel removes
/// it at logout and the directory is already mode 0700 and owned by this user;
/// `/tmp` would be world-listable and would leave a stale path behind a crash.
pub fn socket_path() -> Result<PathBuf> {
    let run = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|dir| !dir.is_empty())
        .context("XDG_RUNTIME_DIR is not set - is this a logind session?")?;
    Ok(PathBuf::from(run).join("nebula.sock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_wins_over_home() {
        let dir = resolve_config_dir(Some("/xdg".into()), Some("/home/u".into())).unwrap();
        assert_eq!(dir, PathBuf::from("/xdg/nebula"));
    }

    #[test]
    fn falls_back_to_home() {
        let dir = resolve_config_dir(None, Some("/home/u".into())).unwrap();
        assert_eq!(dir, PathBuf::from("/home/u/.config/nebula"));
    }

    #[test]
    fn empty_xdg_is_not_a_path() {
        let dir = resolve_config_dir(Some("".into()), Some("/home/u".into())).unwrap();
        assert_eq!(dir, PathBuf::from("/home/u/.config/nebula"));
    }

    #[test]
    fn a_phrase_becomes_a_safe_file_name() {
        assert_eq!(model_file_stem("  Hey   Nebula "), "hey-nebula");
        assert_eq!(model_file_stem("nebula"), "nebula");
        assert_eq!(model_file_stem("../../nebula"), "nebula");
    }

    #[test]
    fn no_home_is_an_error() {
        assert!(resolve_config_dir(None, None).is_err());
    }
}
