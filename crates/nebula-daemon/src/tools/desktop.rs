use super::{privacy, process};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::time::Duration;

pub fn validate_address(address: &str) -> Result<()> {
    if !address.starts_with("0x")
        || !(3..=18).contains(&address.len())
        || !address[2..].bytes().all(|c| c.is_ascii_hexdigit())
        || u64::from_str_radix(&address[2..], 16).unwrap_or(0) == 0
    {
        bail!("invalid window address; use desktop_windows");
    }
    Ok(())
}

pub fn validate_workspace(workspace: &str) -> Result<()> {
    let number = workspace
        .parse::<u32>()
        .is_ok_and(|n| n > 0 && n <= i32::MAX as u32)
        && workspace.bytes().all(|c| c.is_ascii_digit());
    let named = workspace.strip_prefix("name:").is_some_and(|name| {
        !name.is_empty()
            && name.len() <= 64
            && name
                .chars()
                .all(|c| c.is_alphanumeric() || " _-.".contains(c))
    });
    if !number && !named && !matches!(workspace, "+1" | "-1" | "previous") {
        bail!("workspace must be a positive number, name:NAME, +1, -1 or previous");
    }
    Ok(())
}

pub async fn query(command: &str, timeout: Duration) -> Result<Value> {
    let result = process::run(
        "/usr/bin/hyprctl",
        &["-j".into(), command.into()],
        None,
        None,
        timeout,
    )
    .await?;
    if result.code != Some(0) {
        bail!("Hyprland is unavailable; run Nebula inside your Hyprland session");
    }
    serde_json::from_str(&result.text).map_err(|_| anyhow::anyhow!("invalid Hyprland response"))
}

pub async fn windows(timeout: Duration) -> Result<Value> {
    let clients = query("clients", timeout).await?;
    let clients = clients
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("invalid window list"))?;
    let active = query("activewindow", timeout).await?;
    Ok(json!({
        "ok":true,
        "active_address":active["address"],
        "windows":clients.iter().take(100).map(|w| json!({
            "address":w["address"],
            "app":w["class"],
            "title":privacy::redact(w["title"].as_str().unwrap_or("")),
            "workspace":w["workspace"],
            "monitor":w["monitor"],
            "floating":w["floating"],
            "position":w["at"],
            "size":w["size"]
        })).collect::<Vec<_>>()
    }))
}

pub async fn workspaces(timeout: Duration) -> Result<Value> {
    let spaces = query("workspaces", timeout).await?;
    let spaces = spaces
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("invalid workspace list"))?;
    let monitors = query("monitors", timeout).await?;
    let monitors = monitors
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("invalid monitor list"))?;
    Ok(json!({
        "ok":true,
        "workspaces":spaces.iter().take(100).map(|w| json!({
            "id":w["id"],"name":w["name"],"monitor":w["monitor"],"windows":w["windows"]
        })).collect::<Vec<_>>(),
        "monitors":monitors.iter().take(16).map(|m| json!({
            "id":m["id"],"name":m["name"],"focused":m["focused"],
            "active_workspace":m["activeWorkspace"],"special_workspace":m["specialWorkspace"]
        })).collect::<Vec<_>>()
    }))
}

pub async fn require_window(address: &str, timeout: Duration) -> Result<()> {
    validate_address(address)?;
    let clients = query("clients", timeout).await?;
    if !clients.as_array().is_some_and(|windows| {
        windows.iter().any(|w| {
            w["address"]
                .as_str()
                .is_some_and(|a| a.eq_ignore_ascii_case(address))
        })
    }) {
        bail!("window no longer exists; refresh desktop_windows");
    }
    Ok(())
}

pub enum Action<'a> {
    Focus(&'a str),
    Workspace(&'a str),
    Move {
        address: &'a str,
        workspace: &'a str,
        follow: bool,
    },
}

impl Action<'_> {
    fn expression(&self) -> Result<String> {
        match self {
            Self::Focus(address) => {
                validate_address(address)?;
                Ok(format!(
                    "hl.dsp.focus({{ window = \"address:{address}\" }})"
                ))
            }
            Self::Workspace(workspace) => {
                validate_workspace(workspace)?;
                Ok(format!("hl.dsp.focus({{ workspace = \"{workspace}\" }})"))
            }
            Self::Move {
                address,
                workspace,
                follow,
            } => {
                validate_address(address)?;
                validate_workspace(workspace)?;
                Ok(format!(
                    "hl.dsp.window.move({{ window = \"address:{address}\", workspace = \"{workspace}\", follow = {follow} }})"
                ))
            }
        }
    }
}

pub async fn dispatch(action: Action<'_>, timeout: Duration) -> Result<Value> {
    // Hyprland 0.55+ Lua dispatch: https://wiki.hypr.land/0.56.0/Configuring/Basics/Dispatchers/
    let expression = action.expression()?;
    let result = process::run(
        "/usr/bin/hyprctl",
        &["dispatch".into(), expression],
        None,
        None,
        timeout,
    )
    .await?;
    dispatch_result(result)
}

fn dispatch_result(result: process::Output) -> Result<Value> {
    if result.code != Some(0) || result.text.trim() != "ok" {
        bail!(
            "Hyprland rejected the action: {}",
            privacy::redact(result.text.trim())
        );
    }
    Ok(json!({"ok":true,"note":"Hyprland accepted the action"}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_reject_empty_addresses_and_dispatch_injection() {
        for address in [
            "",
            "0x",
            "0x0",
            "activewindow",
            "0x123,exec,whoami",
            "0xzz",
            "0xfffffffffffffffff",
        ] {
            assert!(validate_address(address).is_err(), "{address}");
        }
        assert!(validate_address("0x123abc").is_ok());
        for workspace in [
            "",
            "0",
            "-2",
            "2147483648",
            "name:",
            "1,activewindow",
            "1; dispatch exec whoami",
            "name:x\ny",
            "special:special",
        ] {
            assert!(validate_workspace(workspace).is_err(), "{workspace}");
        }
        for workspace in ["1", "10", "+1", "-1", "previous", "name:work stuff"] {
            assert!(validate_workspace(workspace).is_ok(), "{workspace}");
        }
    }

    #[test]
    fn lua_dispatch_rejects_code_in_targets() {
        for workspace in ["name:x\" }); os.exit(); --", "name:x\\y", "name:x\ny"] {
            assert!(Action::Workspace(workspace).expression().is_err());
            assert!(
                Action::Move {
                    address: "0x123",
                    workspace,
                    follow: false
                }
                .expression()
                .is_err()
            );
        }
        assert!(Action::Focus("0x123\" })").expression().is_err());
    }

    #[test]
    fn dispatcher_errors_are_not_reported_as_success() {
        for (code, text) in [(Some(0), "Invalid dispatcher"), (Some(1), "ok"), (None, "")] {
            assert!(
                dispatch_result(process::Output {
                    code,
                    text: text.into()
                })
                .is_err()
            );
        }
        assert!(
            dispatch_result(process::Output {
                code: Some(0),
                text: "ok\n".into()
            })
            .is_ok()
        );
    }
}
