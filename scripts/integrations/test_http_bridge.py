import io
import json
import unittest
from unittest.mock import patch

from http_bridge import execute


class Response(io.BytesIO):
    code = 200


class BridgeTests(unittest.TestCase):
    def setUp(self):
        self.service = {
            "base_url": "http://127.0.0.1:3000",
            "credential_container": "cabine_api",
            "actions": {"lights-off": {"method": "POST", "path": "/api/lights-off"}},
        }

    def test_discovery_and_invalid_actions_never_contact_service(self):
        with patch("http_bridge.subprocess.check_output") as credential, patch("http_bridge.urllib.request.build_opener") as opener:
            self.assertIn("lights-off", execute(self.service, {})["actions"])
            for request in [{"action": "unknown"}, {"action": "lights-off", "url": "http://other"}, {"action": "lights-off", "body": {}}, {"action": []}]:
                with self.assertRaises(ValueError):
                    execute(self.service, request)
            credential.assert_not_called()
            opener.assert_not_called()

    def test_post_uses_server_credential_and_reports_partial_failure(self):
        with patch("http_bridge.subprocess.check_output", return_value=b"test-secret\n"), patch("http_bridge.urllib.request.build_opener") as opener:
            opener.return_value.open.return_value = Response(json.dumps({"success": True, "results": {"lights": {"success": False}}, "message": "test-secret"}).encode())
            result = execute(self.service, {"action": "lights-off"})
            request = opener.return_value.open.call_args.args[0]
            self.assertEqual(request.method, "POST")
            self.assertEqual(request.full_url, "http://127.0.0.1:3000/api/lights-off")
            self.assertEqual(request.get_header("X-webhook-secret"), "test-secret")
            self.assertFalse(result["ok"])
            self.assertNotIn("test-secret", json.dumps(result))

    def test_camera_start_stop_use_post(self):
        with open(__file__.replace("test_http_bridge.py", "integration-actions.example.json")) as file:
            camera = json.load(file)["live-camera"]
        with patch("http_bridge.urllib.request.build_opener") as opener:
            for action in ["start", "stop"]:
                opener.return_value.open.return_value = Response(b'{"ok":true}')
                self.assertTrue(execute(camera, {"action": action})["ok"])
                request = opener.return_value.open.call_args.args[0]
                self.assertEqual(request.method, "POST")
                self.assertTrue(request.full_url.endswith("/" + action))


if __name__ == "__main__":
    unittest.main()
