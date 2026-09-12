# Troubleshoot Nebula

Start with `nebula --check`. It is local-only and does not print credential values. For audio issues, run `nebula --test-audio --no-loopback`; for wake issues, run `nebula --test-wake` before opening cloud sessions.

| Symptom | Action |
|---|---|
| Missing configuration or model | Install/copy the example config, configure the key locally, and run `--train-wake` |
| Secret file refused | Ensure it is a regular file owned by you, exactly mode 0600; do not use a symlink |
| No default audio device | Check `systemctl --user status pipewire wireplumber` and `pactl info` |
| Wrong input/output | Inspect `pactl list short sources` / `pactl list short sinks`; select a node name in `[audio]` |
| No wake detections | Retrain in your normal room and speaking position; verify the configured phrase matches your enrollment |
| False wakes | Raise sensitivity, retrain, and test with ordinary background speech; this detector is experimental |
| Long response or interruption latency | Compare wired headphones with Bluetooth; the software queue metric excludes hardware/wireless buffers |
| Assistant hears itself | Use headphones or configure [PipeWire echo cancellation](speaker-audio.md) for both input and output |
| Connection failure | Check network, API project access, the configured voice/backend models, credentials and quota; use `--check-live` for an API probe |
| Service exits with status 78 | Correct configuration, missing model/key, credentials, or duplicate-daemon startup; then start the service again |
| No HUD under systemd | Launch `nebula-ui` from the graphical session or Hyprland `exec-once` |
| HUD overlaps Flow | Set `ui.position = "top-center"`; neither app changes the other's configuration |
| X11/GNOME shows no pill | Notification fallback requires `notify-send`; use CLI controls for mute/end |
| “Already running” | Inspect `systemctl --user status nebula`; stop that instance before manual testing |
| Microphone stops delivering audio | Check the device connection and PipeWire; the daemon recovers with backoff |

Useful commands:

```sh
systemctl --user status nebula.service
journalctl --user -u nebula.service -n 80
journalctl --user -u nebula.service -f
~/.local/bin/nebula-ui --end-session
~/.local/bin/nebula-ui --toggle-mute
```

Do not send logs containing unrelated credentials. Nebula's own messages omit raw server bodies, microphone samples and transcripts. Do not enable external packet/body logging around the application.

Stopping the service sends SIGTERM; manual runs also handle Ctrl-C. If a crash leaves a socket file, a subsequent instance removes the stale socket only after obtaining the singleton lock. Do not delete a running daemon's socket or lock file.

The acoustic round-trip test needs speaker-to-microphone coupling and a quiet room. On headphones use `--no-loopback`. Queue-to-device measurements alone cannot establish end-to-end voice latency.
