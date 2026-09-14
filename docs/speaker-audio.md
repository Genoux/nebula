# Room speakers

With room speakers, microphone pickup of Nebula's own response can trigger
unwanted conversational reactions. PipeWire can provide an echo-cancelled
source and matching playback sink. Both sides must be routed through the pair;
selecting only the filtered microphone does not supply the playback reference.

The local setup uses PipeWire's `module-echo-cancel` with the WebRTC AEC backend,
the HyperX SoloCast microphone, and the Bluetooth speaker. Nebula's config selects:

```toml
[audio]
input_device = "nebula_echo_source"
output_device = "nebula_echo_sink"
buffer_frames = 240
```

The module is loaded for the current session. Its persistent configuration is
`~/.config/pipewire/pipewire-pulse.conf.d/90-nebula-echo.conf`. That file names the
physical microphone and speaker explicitly; update those names when changing
hardware. It loads again when pipewire-pulse starts. The system default input
and output remain the physical devices.

Restart Nebula after changing its audio configuration. Run `nebula --check` and
`nebula --test-audio --no-loopback`, then try a live response while staying quiet.
Next, speak over a response to check intentional interruption. Bluetooth delay,
room acoustics and speaker volume can still affect cancellation; this setup
needs an audible conversation test before treating self-interruption as resolved.

To undo the local setup, restore the audio device values from
`~/.config/nebula/config.before-echo.toml`, remove only the Nebula echo drop-in,
and unload its module using the numeric ID shown by `pactl list short modules`.
Stop Nebula before removing devices it is using.

See the [PipeWire echo-cancel module options](https://docs.pipewire.org/page_pulse_module_echo_cancel.html)
and [module loading documentation](https://docs.pipewire.org/page_pulse_modules.html).

Nebula waits 80 ms before playing incoming audio after an empty queue, including
when the first chunk already contains more than 80 ms of audio. A short final chunk plays
after the bounded wait even if no more audio arrives. Clearing playback discards
the cushion immediately. This absorbs modest network jitter; it does not repair
Bluetooth dropouts or PipeWire scheduling errors.

Playback logs report how often the queue empties. Counts include normal speech
endings; correlate bursts during continuous speech with audible glitches before
attributing them to network delivery.
