# Nebula

A Rust voice assistant for Linux: local wake detection, interruptible OpenAI GPT-Live-1 conversations, and an optional Wayland HUD. No local command execution or desktop tools.

Build and validate:

```sh
cargo build --workspace --release --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --locked
```

Start with [setup](docs/setup.md). See [architecture](docs/architecture.md), [troubleshooting](docs/troubleshooting.md), and [validation status](docs/validation.md).

GPT-Live-1 handles voice and interruptions; a configurable Responses backend handles reasoning, web search, and optional local file, terminal, SSH, desktop, browser and integration tools. See [tool setup](docs/setup.md#local-tools) for configuration and current limits. Setup requires an API key and a wake model trained on your voice. The template wake detector is experimental; synthetic tests do not establish real-world detection accuracy.
