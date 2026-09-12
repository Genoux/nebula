use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn isolated() -> (Command, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "nebula-cli-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(root.join("nebula")).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_nebula"));
    command
        .env_remove("OPENAI_API_KEY")
        .env("XDG_CONFIG_HOME", &root);
    (command, root)
}

#[test]
fn missing_configuration_stops_with_the_service_exit_code() {
    let (mut command, root) = isolated();
    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(78));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn malformed_configuration_never_echoes_its_contents() {
    let (mut command, root) = isolated();
    std::fs::write(
        root.join("nebula/config.toml"),
        "sk-private-value = [broken",
    )
    .unwrap();
    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(78));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("sk-private-value"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_test_modes_are_mutually_exclusive() {
    let (mut command, root) = isolated();
    let output = command
        .args(["--test-wake", "--manual-live"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    std::fs::remove_dir_all(root).unwrap();
}
