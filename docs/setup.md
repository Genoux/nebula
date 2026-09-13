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
Responses model. Hosted web search is enabled for current information and explicit lookups. Voice time and delegated backend
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
It requires the key and config but no wake model or audio devices. To verify a completed internet search, run:

```sh
~/.local/bin/nebula --check-web
```

This sends a fixed public search request through the reasoning backend and
checks for a completed web-search tool call, backend completion, generated audio,
and session finalization. It does not send microphone audio. Search and backend
usage are billed by the API.


## Local tools

Local tools are disabled by default. Add this to your local config to enable
file access within selected absolute folders, terminal commands, Hyprland window
listing/focus, and opening files and browser URLs:

```toml
[tools]
enabled = true
roots = ["/home/john"]
timeout_seconds = 60
```

File writes and commands require a Zenity dialog showing the requested action.
Command output stays local unless you separately approve sharing the displayed
result with the model. Closing a dialog denies the request. Launch Nebula from
your graphical session so these dialogs can appear. Tools require `zenity`,
`sh`, `ssh`, `hyprctl`, `xdg-open`, `curl`, and `python3` for their respective
capabilities. Commands run as your user; configured folders constrain file tools
and terminal starting directories, not the commands themselves.

Known credential paths are blocked by file tools and recognizable credential
content is redacted. These checks cannot identify every secret in arbitrary
files or command output; review output before sharing it. Authentication stays
in local SSH configuration and integration executables. Do not put passwords,
private keys or tokens in tool arguments or this config.

For a server, first configure and verify an SSH host alias locally, then add:

```toml
[[tools.servers]]
name = "jb-media"
ssh_host = "jb-media"
```

This example does not establish JB Media's address. SSH uses batch mode and
requires an already trusted host key. A timed-out remote command may still have
run; inspect its state before retrying.

Integrations are explicitly registered local executables; input arrives on stdin:

```toml
[[tools.integrations]]
name = "project-status"
program = "/home/john/.local/bin/project-status"
args = []
```

Browser tools open HTTP(S) URLs and fetch page text without browser cookies or
JavaScript. They do not control logged-in tabs. Desktop tools list/focus windows
and open files; they do not provide mouse or keyboard automation.

```sh
~/.local/bin/nebula --check-tools
~/.local/bin/nebula --check-tools-live
```

The first command checks local configuration and executable availability without
network access. The second is a paid API diagnostic: it executes only
`tools_list`, shares configured folder paths and server/integration names with
the model, and verifies result submission and spoken-response generation using
synthetic silence. It does not capture microphone audio or execute system actions.

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

For the complete flow, run Nebula from a terminal in your graphical session:

```sh
~/.local/bin/nebula
```

The daemon starts `nebula-ui` automatically when `ui.enabled = true` and restarts it if it crashes. A separately started HUD is reused. The ready indicator fades after eight seconds; it reappears during conversation. `--manual-live` remains an audio-only diagnostic.

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
