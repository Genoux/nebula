# Set up Nebula

Nebula runs as your normal user. The daemon owns audio and the cloud session; `nebula-ui` only displays status and sends mute/end commands. Headphones are recommended because this version has no built-in acoustic echo cancellation. For room speakers, see [PipeWire echo cancellation](speaker-audio.md).

## Build

On the inspected Arch/Hyprland system, Rust, ALSA development libraries, PipeWire, WirePlumber, and the compiler are already available. The HUD uses Wayland layer-shell and software rendering; it does not use GTK or modify Flow.

```sh
cargo build --workspace --release --locked
```

Cargo may use a configured target directory. `cargo metadata --no-deps --format-version 1` reports `target_directory`; the executables are in its `release` subdirectory. Alternatively use `cargo run --release -p nebula-daemon -- <arguments>` and `cargo run --release -p nebula-ui -- <arguments>`.

## Install files

The installer creates `~/.local/bin/nebula`, `~/.local/bin/nebula-ui`, `~/.config/systemd/user/nebula.service`, and, if absent, `~/.config/nebula/config.toml`. It reloads user units but never starts or enables the service. Review these changes before running it. No sudo is required.

```sh
./scripts/install-user-service.sh
```

To reuse a build, pass `--bin-dir /absolute/path/to/release`. To inspect a staged installation without changing your login configuration, also pass `--destdir /absolute/path/to/staging-directory`.

Existing configuration and credentials are preserved. For consistent service paths the supplied unit fixes `XDG_CONFIG_HOME` to `~/.config`; manual runs otherwise honor your shell's `XDG_CONFIG_HOME`.

## Configure credentials locally

Create `~/.config/nebula/nebula.env` with your editor. Its single assignment is `OPENAI_API_KEY=<your key>`. Enter the real value only in that local file, never in chat or a shell command. Protect the file before putting a key in it:

```sh
install -d -m 700 ~/.config/nebula
(umask 077; touch ~/.config/nebula/nebula.env)
chmod 600 ~/.config/nebula/nebula.env
```

The file must be a regular file owned by you, mode 0600, with no duplicate key assignments. Symlinks are refused. The user service loads it through `EnvironmentFile`; Nebula removes the key from its inherited environment before starting threads or subprocesses. The initial process environment can still be inspected by processes with sufficient permissions; `EnvironmentFile` is not a secret vault.

The example config selects `gpt-live-1` and `marin`, with `gpt-5.6-luna` as the
reasoning backend. Configure `session.backend_model` to use another supported
Responses model. Backend tools are disabled. Voice time and delegated backend
usage are billed separately. See the [official GPT-Live guide](https://developers.openai.com/api/docs/guides/live).

The previous Realtime fields `end_of_turn_silence_ms`, `transcription_model`, and
`language` are removed. GPT-Live manages speech timing and supplies native
transcript fragments. Old configs containing those fields must be migrated;
changing only the model name is not sufficient.

Before a microphone conversation, test the actual API connection with:

```sh
~/.local/bin/nebula --check-live
```

This paid, short test sends synthetic silence and a fixed arithmetic prompt,
checks the configured reasoning backend and generated voice audio, then
finalizes the session. It does not capture or play microphone/speaker audio.
It requires the key and config but no wake model or audio devices.

## Test audio and train the wake phrase

```sh
~/.local/bin/nebula --test-audio --no-loopback
~/.local/bin/nebula --train-wake
~/.local/bin/nebula --test-wake
~/.local/bin/nebula --check
```

Training asks for five takes of the configured phrase, each 0.25–1.6 seconds. Only feature templates are saved in `~/.config/nebula/wake/`; raw recordings are discarded. Train and test in your normal room, at your normal speaking distance. `--test-wake` never opens an OpenAI connection.

To test “Hey Nebula,” set `wake_word.phrase = "hey nebula"`, then train and test again. Each phrase has a separate model. Higher sensitivity values make matching stricter; reduce the value for missed wakes, increase it for false wakes. Repeated false wakes require retraining or replacing the detector, not just accepting a higher score.

`--check` checks local configuration, device availability, wake-model structure, and credential permissions. It does not contact OpenAI or prove credential validity, model access, audible output, or detection accuracy.

## Converse

```sh
~/.local/bin/nebula --manual-live
```

Wait for “connected - speak now.” Test a short response, interrupt it while speaking, then say “Nebula, stop.” This mode bypasses the wake word and has no HUD connection.

For the complete flow, use two terminals:

```sh
~/.local/bin/nebula
~/.local/bin/nebula-ui
```

Say the trained phrase and wait for the Listening indicator. The wake tone confirms detection; Connecting means the cloud session is not yet ready. Live microphone capture starts after the server sends `session.started`. Audio spoken during Connecting is not buffered.

- Left-click the pill to mute/unmute the microphone.
- Right-click to end the live session, including while connecting.
- `nebula-ui --toggle-mute` and `nebula-ui --end-session` provide keyboard-bindable equivalents.
- Say “Nebula, stop” or wait for `session.idle_timeout_seconds` of inactivity to return to local standby.
- Ctrl-C and SIGTERM shut down the daemon.

Speech stopping is recognized through native GPT-Live transcript fragments during a live session. It is not a local emergency stop and can be delayed by transcription. Use the HUD or Ctrl-C for immediate local control.

## Start at login, after approval

The installer does not enable anything. Only run this when you want background listening at login:

```sh
systemctl --user enable --now nebula.service
systemctl --user status nebula.service
journalctl --user -u nebula.service -f
```

Disable and stop:

```sh
systemctl --user disable --now nebula.service
```

The HUD needs the graphical session's Wayland environment. On the inspected machine the systemd user manager did not have `WAYLAND_DISPLAY`, so start the HUD from Hyprland, not the daemon service. Add this line to your Hyprland configuration only when you choose to enable it:

```ini
exec-once = ~/.local/bin/nebula-ui
```

Optional bindings:

```ini
bind = SUPER SHIFT, M, exec, ~/.local/bin/nebula-ui --toggle-mute
bind = SUPER SHIFT, N, exec, ~/.local/bin/nebula-ui --end-session
```

The HUD defaults to bottom-center. Use `ui.position = "top-center"` if it overlaps another overlay. `ui.enabled = false` disables the HUD. On X11 or a compositor without layer-shell, it falls back to desktop notifications using `notify-send`; CLI controls still work.
