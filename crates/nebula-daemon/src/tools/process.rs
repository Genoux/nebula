use anyhow::{Result, bail};
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

pub const OUTPUT_LIMIT: usize = 24_000;

pub struct Output {
    pub code: Option<i32>,
    pub text: String,
}

struct Group(i32);

impl Drop for Group {
    fn drop(&mut self) {
        // The child owns a process group so cancellation also stops shell descendants.
        unsafe {
            libc::kill(-self.0, libc::SIGKILL);
        }
    }
}

async fn bounded(mut stream: impl AsyncRead + Unpin) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    (&mut stream)
        .take(OUTPUT_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > OUTPUT_LIMIT {
        bail!("tool output exceeds the limit; request a smaller result");
    }
    Ok(bytes)
}

pub async fn run(
    program: &str,
    args: &[String],
    cwd: Option<&std::path::Path>,
    input: Option<&str>,
    timeout: Duration,
) -> Result<Output> {
    let mut command = clean_command(program);
    command
        .args(args)
        .kill_on_drop(true)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.as_std_mut().process_group(0);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = command
        .spawn()
        .map_err(|_| anyhow::anyhow!("tool executable is unavailable"))?;
    let _group = Group(
        child
            .id()
            .ok_or_else(|| anyhow::anyhow!("tool process did not start"))? as i32,
    );
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let mut stdin = child.stdin.take();
    let result = tokio::time::timeout(timeout, async {
        let write = async {
            if let (Some(pipe), Some(text)) = (stdin.as_mut(), input) {
                pipe.write_all(text.as_bytes()).await?;
                pipe.shutdown().await?;
            }
            Ok::<(), anyhow::Error>(())
        };
        let (out, err, status, ()) = tokio::try_join!(
            bounded(stdout),
            bounded(stderr),
            async { Ok::<_, anyhow::Error>(child.wait().await?) },
            write
        )?;
        let mut text = String::from_utf8_lossy(&out).into_owned();
        if !err.is_empty() {
            text.push('\n');
            text.push_str(&String::from_utf8_lossy(&err));
        }
        if text.len() > OUTPUT_LIMIT {
            bail!("combined tool output exceeds the size limit");
        }
        Ok::<_, anyhow::Error>(Output {
            code: status.code(),
            text,
        })
    })
    .await;
    match result {
        Ok(result) => result,
        Err(_) => bail!(
            "tool timed out; local process group stopped; remote actions may already have occurred"
        ),
    }
}

fn clean_command(program: &str) -> Command {
    let mut command = Command::new(program);
    command.env_clear();
    for name in [
        "HOME",
        "USER",
        "LOGNAME",
        "PATH",
        "LANG",
        "LC_ALL",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "HYPRLAND_INSTANCE_SIGNATURE",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "SSH_AUTH_SOCK",
        "XDG_DATA_DIRS",
        "XDG_CURRENT_DESKTOP",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
}

pub async fn open(target: String) -> Result<bool> {
    let mut command = clean_command("/usr/bin/xdg-open");
    command
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.as_std_mut().process_group(0);
    let mut child = command
        .spawn()
        .map_err(|_| anyhow::anyhow!("default application launcher is unavailable"))?;
    // xdg-open may stay attached to the application; launching it must not kill that app.
    match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
        Ok(status) => Ok(status?.success()),
        Err(_) => Ok(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn command_output_is_bounded_and_timeouts_stop_work() {
        assert!(
            run(
                "/bin/sh",
                &["-c".into(), "yes x".into()],
                None,
                None,
                Duration::from_secs(2)
            )
            .await
            .is_err()
        );
        assert!(
            run(
                "/bin/sh",
                &["-c".into(), "sleep 10".into()],
                None,
                None,
                Duration::from_millis(40)
            )
            .await
            .is_err()
        );
    }
    #[tokio::test]
    async fn child_environment_does_not_inherit_service_secrets() {
        let output = run("/usr/bin/env", &[], None, None, Duration::from_secs(2))
            .await
            .unwrap();
        assert!(!output.text.contains("OPENAI_API_KEY="));
        assert!(!output.text.contains("AWS_SECRET_ACCESS_KEY="));
        let environment: std::collections::HashMap<_, _> = output
            .text
            .lines()
            .filter_map(|line| line.split_once('='))
            .collect();
        for name in [
            "HOME",
            "PATH",
            "WAYLAND_DISPLAY",
            "SSH_AUTH_SOCK",
            "HYPRLAND_INSTANCE_SIGNATURE",
        ] {
            if let Ok(value) = std::env::var(name) {
                assert_eq!(
                    environment.get(name).copied(),
                    Some(value.as_str()),
                    "{name}"
                );
            }
        }
    }
}
