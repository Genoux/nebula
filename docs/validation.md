# Validation status

Validated September 11, 2026 after replacing the Realtime integration with
GPT-Live-1. The previous Realtime session implementation has been removed.

| Check | Result |
|---|---|
| Release build | `cargo build --workspace --release --locked` passes |
| Formatting | `cargo fmt --all --check` passes |
| Static checks | `cargo clippy --workspace --all-targets --locked -- -D warnings` passes |
| Automated tests | `cargo test --workspace --locked`: 150 passed, one hardware-dependent test ignored by default |
| Hardware lifecycle | Explicitly run with real input/output and a mock GPT-Live transport; idle timeout survives continuous silent output, fragmented stop input ends the session, and transport closes |
| Actual GPT-Live API | `nebula --check-live` confirms `gpt-live-1` startup, configured Responses backend completion, non-silent generated audio and `session.closed` |
| Speaker routing | Input/output open through the PipeWire echo-cancellation pair; microphone samples and tone playback complete |
| HUD | An isolated speaking event maps a 240 × 44 Wayland layer at bottom center; the user HUD process runs separately |

The API diagnostic sends paced synthetic silence and a fixed arithmetic request.
It does not capture microphone audio, play generated speech, or save transcripts
or audio. It checks protocol behavior and account access, not perceptual voice
quality. A voice-only startup probe also passed. An initial attempt to force
backend delegation with a voice instruction did not establish backend completion;
the final diagnostic explicitly submits the fixed backend request and waits for
its lifecycle before checking spoken output.

Regression coverage includes TLS provider initialization, PipeWire client
ownership, refusal of old Realtime startup/configuration, shared Live audio format,
fragmented stop phrases, negation and transcript bounds, mute acknowledgments and
zero substitution, nested backend lifecycle, full-duplex audio without local VAD
cancellation, malformed PCM, safe errors, cumulative usage and graceful close.

Run the hardware-dependent check with working audio devices:

```sh
cargo test -p nebula-daemon --locked hardware_live_lifecycle_with_mock_live -- --ignored
```

## Remaining user acceptance

Restart Nebula and use the actual microphone/speakers to verify:

1. Wake detection with the trained phrase and current echo-cancelled input.
2. A spoken GPT-Live reply, then an intentional interruption and a short
   acknowledgment such as “mm-hmm.”
3. No self-interruption while remaining quiet during speaker playback.
4. The HUD's listening/speaking states, mute/unmute, and right-click to end.
5. “Nebula, stop,” inactivity timeout, and clean shutdown.

GPT-Live manages interruptions; acoustic echo cancellation quality and Bluetooth
latency still need listening tests. The HUD mapping check is not screenshot QA:
computer-use screenshot capture is unavailable in this session. Login startup is
not enabled by the migration.

See [setup](setup.md), [speaker configuration](speaker-audio.md), and
[architecture](architecture.md).

## HUD startup follow-up — September 13, 2026

The daemon now starts the enabled HUD in graphical sessions and restarts failed
HUD processes. A UI instance lock prevents overlapping overlays. The workspace
now has 151 passing tests. Formatting, Clippy and the release build pass.
An isolated daemon smoke test confirms automatic startup, duplicate suppression,
recovery after a killed HUD, and daemon shutdown. Both installed binaries were
replaced; the HUD mapped at bottom center for the existing user daemon.

## Conversation polish and web search — September 13, 2026

The speaking indicator holds through brief phrase pauses and is refreshed by
ongoing assistant transcript activity. Tests cover pause release, longer phrases,
user takeover, and silence without speech. The idle timeout still tracks actual
activity rather than the displayed speaking hold.

The reasoning backend now has hosted web search with automatic tool selection.
`nebula --check-web` passed against the actual GPT-Live API, including a completed
web-search tool call, backend completion, generated audio and graceful close.
No microphone audio was submitted. All 157 automated tests, formatting, Clippy,
and the release build pass. Restart the daemon to apply these changes to a new
conversation session.

## UI polish — September 13, 2026

Added a separate reasoning/web-search activity pill, eased visibility and label
transitions, time-based level and colour smoothing, and a 16 ms frame cadence.
Renderer checks cover both screen edges at 2× scale; the resulting software
render was visually reviewed. Protocol tests cover activity serialization and
parallel search completion. The speaking pause hold remains independent of
backend activity.

## Local tooling — September 13, 2026

The enabled backend exposes file, terminal, SSH, desktop, browser and registered
integration functions. All 173 automated tests pass, including function-call
deduplication and result handling, denied actions, revision conflicts, protected
paths, output sharing, output limits, process timeout and environment inheritance.
Formatting, Clippy with warnings denied, and the release build pass.

Fixed a second environment clear that removed the allowlisted desktop and
SSH-agent settings from child processes; Hyprland instance selection is also
preserved. Both binaries are installed, and local tools are enabled for
`/home/john`. The installed `--check-tools` finds all required executables.
The previous session's live diagnostic verified `tools_list`, result submission
and backend continuation; that paid API check was not repeated in this follow-up.

JB Media remains unconfigured pending confirmation of its SSH destination.
No integrations are registered. Browser support is URL opening and public page
text, and desktop support is window listing/focus and file opening. Logged-in
browser automation and general mouse/keyboard control are not implemented.
The daemon remains stopped, ready for a manual launch.
