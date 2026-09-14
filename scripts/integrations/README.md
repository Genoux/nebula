# Server API integrations

`http_bridge.py` runs on the media server. Install it as
`~/.local/bin/nebula-http-bridge` and copy `integration-actions.example.json` to
`~/.config/nebula/integration-actions.json`. Python 3 and access to the existing
`cabine_api` Docker container are required. The bridge reads the webhook credential
inside that server at call time; no credential is stored on the laptop.

Register one laptop integration per service using `/usr/bin/ssh` as the program:

```toml
[[tools.integrations]]
name = "cabine"
description = 'Office lights and PC arrival/departure. Input is JSON: {"action":"list"} discovers actions; then pass {"action":"lights-off"}, etc. Leave sleeps the office PC.'
program = "/usr/bin/ssh"
args = ["-T", "-oBatchMode=yes", "-oStrictHostKeyChecking=yes", "-oConnectTimeout=5", "-oForwardAgent=no", "-oClearAllForwardings=yes", "-oPermitLocalCommand=no", "media-server", "/home/john/.local/bin/nebula-http-bridge", "cabine"]

[[tools.integrations]]
name = "live-camera"
description = 'Live camera stream control. Input is JSON: {"action":"list"} discovers actions; status, start and stop are supported. Controls streaming; does not inspect video.'
program = "/usr/bin/ssh"
args = ["-T", "-oBatchMode=yes", "-oStrictHostKeyChecking=yes", "-oConnectTimeout=5", "-oForwardAgent=no", "-oClearAllForwardings=yes", "-oPermitLocalCommand=no", "media-server", "/home/john/.local/bin/nebula-http-bridge", "live-camera"]
```

The SSH process forwards tool input on stdin. Empty input or `{"action":"list"}`
returns the current action catalog without calling the API. The server catalog
is loaded each call. To expose a new API feature, add an action with `method`,
`path`, and `description` to that file. Set `accepts_body` to true for endpoints
accepting a JSON `body` field, and describe the expected fields. No Rust rebuild
is needed for catalog changes. Endpoint discovery is configured, not automatic.

Cabine supports status, arrive, leave, wake-pc, sleep-pc, lights-on and lights-off.
Its status endpoint reports PC availability and whether LIFX is configured, not
whether the lights are on. Arrive/leave component failures remain failures even
when the upstream API returns HTTP 200. Camera start/stop use POST against the
existing camctl bridge, which forwards POST `/stream/start` or `/stream/stop` to
the camera publisher. Only JSON is returned; no video frames are sent to Nebula.

Calls use fixed configured origins and paths, refuse redirects, bound response
size, and do not retry actions. A timeout may leave an action completed; check
status before retrying. The current Nebula approval policy applies as usual.

Validate without changing devices:

```sh
printf '%s' '{"action":"status"}' | ssh media-server /home/john/.local/bin/nebula-http-bridge cabine
printf '%s' '{"action":"status"}' | ssh media-server /home/john/.local/bin/nebula-http-bridge live-camera
python3 -m unittest discover -s scripts/integrations -p 'test_*.py'
```
