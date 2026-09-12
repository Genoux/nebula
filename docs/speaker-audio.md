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
