# Nebula architecture

Nebula uses GPT-Live-1 for continuous voice conversation. A local 16 kHz template
wake detector opens no cloud connection. A wake pauses detection, plays a tone,
connects to GPT-Live, and starts 24 kHz microphone streaming after `session.started`.
Teardown stops capture, finalizes the session, and returns to local standby.

| Crate | Responsibility |
|---|---|
| nebula-core | Configuration, paths, secrets, state transitions and IPC types |
| nebula-audio | CPAL capture/playback, PipeWire routing, tones and levels |
| nebula-wake | Local MFCC/DTW template detector |
| nebula-live | GPT-Live protocol, transport, session controls and finalization |
| nebula-daemon | Wake/live lifecycle, timeouts, signals, singleton lock and IPC |
| nebula-ui | Wayland HUD, microphone/end controls and notification fallback |

## GPT-Live

The primary WebSocket connects to `wss://api.openai.com/v1/live/sessions` without
query parameters. Its first message is `session.start`, with `model = gpt-live-1`,
a shared PCM16 mono 24 kHz audio format, voice, conversation instructions,
`store = false`, and a managed Responses delegation configuration.

GPT-Live handles listening, speaking, acknowledgments and interruptions. The
backend model defaults to `gpt-5.6-luna` and supplies reasoning, with an empty tool
list and `tool_choice = none`. There are no local execution or desktop tools.
The live prompt describes conversation style and when to delegate; the backend
prompt describes reasoning and its lack of computer or web access.

Microphone chunks use `session.input_audio.append`. Output arrives through
`session.output_audio.delta`. Input transcripts are native
`session.input_transcript.delta` fragments; a bounded in-memory window matches
“Nebula, stop” across fragments and is discarded at session teardown. A gap over
two seconds resets that window. This heuristic needs testing with natural speech
and is not a local emergency stop.

There is no VAD cancellation, audio commit, voice-turn `response.create`, or
conversation-item truncation. Incoming speech does not clear the playback queue:
GPT-Live decides how to respond to interruptions and backchannels. The daemon
plays audio in order and derives the speaking indicator from audible playback,
with a short hold to avoid flicker. Silent output does not keep the session alive.
Backend completion is independent of spoken output and does not mark speech done.

Mute immediately substitutes zeros for microphone samples while preserving the
stream cadence. The daemon also sends `session.input_audio.mute` or `.unmute` and
requires the matching server acknowledgment within five seconds. End-session and
process signals stop capture and clear playback locally.

Teardown sends `session.close`, continues reading up to 15 seconds for
`session.closed`, and then closes the socket. Voice-duration snapshots replace
one another; they are not summed. Missing finalization is reported. The service
allows 20 seconds for shutdown. Timing reports describe queue-to-device delivery,
not acoustic or Bluetooth latency.

## Audio and local lifecycle

CPAL uses the ALSA default device; configured PipeWire nodes are selected by
moving Nebula's own streams. Ownership can be recorded directly on a stream or
on a separate PipeWire client for each capture/playback stream. The lookup
briefly retries while nodes register. Other applications and system defaults
are not moved.

The local room-speaker setup routes both directions through a PipeWire
WebRTC echo-cancellation pair. See [speaker audio](speaker-audio.md). Cancellation
still depends on the speaker, microphone, room and Bluetooth delay.

The daemon checks `Standby → Waking → Connecting → Live → Disconnecting → Standby`
transitions. Failures recover through an error state and backoff capped at 30
seconds. Missing configuration, key or wake model exits with status 78. A runtime
file lock prevents concurrent daemons, including manual-live instances.

Handshake and session startup have deadlines. Audio queues are bounded; playback
backlog over two seconds fails instead of creating a long interruption delay.
Lost microphone input, transport failure, explicit stop or inactivity ends the
session. Backend work does not suppress the idle deadline indefinitely.

## Wake detection and HUD

The wake detector compares rolling MFCC features with five recorded templates
using dynamic time warping. It is experimental and personal to the training
voice. Synthetic tests do not establish real-room false-accept or miss rates.
Only feature templates are stored; enrollment recordings are discarded.

The optional `nebula-ui` process connects to the private runtime socket and can
reconnect after daemon restarts. Start it separately in the graphical session.
The bottom-center Wayland layer takes no keyboard focus. Standby/errors fade;
conversation and mute states remain visible. Unsupported compositors use desktop
notifications. Shared-memory drawing buffers wait for compositor release before
reuse.

No microphone recordings or transcripts are written locally. Cloud session
recording is explicitly disabled. Secrets are read from a private user-owned
file, redacted in diagnostics, and removed from the inherited environment before
threads start. Server error bodies and WebSocket wire payloads are not logged.

Protocol sources: [GPT-Live WebSocket reference](https://developers.openai.com/api/reference/resources/live/primary-websocket),
[migration](https://developers.openai.com/api/docs/guides/live-migration),
[session lifecycle](https://developers.openai.com/api/docs/guides/live-conversations),
and [delegation](https://developers.openai.com/api/docs/guides/live-delegation).
