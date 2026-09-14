#!/usr/bin/env python3
import json
import subprocess
import sys
import urllib.error
import urllib.request
from pathlib import Path

LIMIT = 16000


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def execute(service, request):
    if not isinstance(request, dict) or set(request) - {"action", "body"}:
        raise ValueError("expected an object with action and optional body")
    action = request.get("action", "list")
    if not isinstance(action, str):
        raise ValueError("action must be a string")
    if action == "list":
        return {"ok": True, "actions": service["actions"], "input": {"action": "action name", "body": "optional JSON body when accepted"}}
    route = service["actions"].get(action)
    if route is None:
        raise ValueError("unknown action; call list to discover supported actions")
    if "body" in request and not route.get("accepts_body", False):
        raise ValueError("this action does not accept a body")
    path = route["path"]
    if not path.startswith("/") or path.startswith("//") or ".." in path or "#" in path:
        raise ValueError("invalid configured path")
    headers = {"Content-Type": "application/json", "User-Agent": "nebula-integration/1"}
    secret = None
    if service.get("credential_container"):
        secret = subprocess.check_output(
            ["/usr/bin/docker", "exec", service["credential_container"], "printenv", "WEBHOOK_SECRET"],
            timeout=5, stderr=subprocess.DEVNULL,
        ).decode().strip()
        if not secret:
            raise ValueError("server API credential is unavailable")
        headers["x-webhook-secret"] = secret
    body = json.dumps(request["body"]).encode() if "body" in request else None
    if route["method"] == "POST" and body is None:
        body = b"{}"
    req = urllib.request.Request(service["base_url"] + path, data=body, headers=headers, method=route["method"])
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
    try:
        response = opener.open(req, timeout=45)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        raw = response.read(LIMIT + 1)
        if len(raw) > LIMIT:
            raise ValueError("API response exceeds the output limit")
        text = raw.decode("utf-8")
        if secret:
            text = text.replace(secret, "[redacted]")
        try:
            result = json.loads(text)
        except json.JSONDecodeError:
            result = {"message": "API returned a non-JSON response"}
        ok = 200 <= response.code < 300 and not failed(result)
        return {"ok": ok, "http_status": response.code, "result": result}


def failed(value):
    if isinstance(value, dict):
        return value.get("success") is False or value.get("ok") is False or any(failed(v) for v in value.values())
    if isinstance(value, list):
        return any(failed(v) for v in value)
    return False


def main():
    try:
        catalog = json.loads((Path.home() / ".config/nebula/integration-actions.json").read_text())
        service = catalog[sys.argv[1]]
        raw = sys.stdin.buffer.read(LIMIT + 1)
        if len(raw) > LIMIT:
            raise ValueError("input exceeds the size limit")
        result = execute(service, json.loads(raw) if raw.strip() else {})
    except (ValueError, KeyError, IndexError) as error:
        result = {"ok": False, "error": str(error)}
    except Exception:
        result = {"ok": False, "error": "API connection or credential lookup failed; action outcome may be uncertain, check status before retrying"}
    print(json.dumps(result))
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    sys.exit(main())
