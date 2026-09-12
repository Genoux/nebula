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
