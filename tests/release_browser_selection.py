#!/usr/bin/env python3
"""Verify an actual zuse CLI through MCP stdio and its private headless Chromium."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
from urllib.parse import quote

from transports import Child, check_tool

TITLE = "zuse release selection fixture"
PAYLOAD = "preserve 中文 🙂"
STATE = {"title": TITLE, "text": PAYLOAD, "marker": 37}
STATE_EXPRESSION = "({title:document.title,text:document.querySelector('#entry').value,marker:window.fixtureMarker})"


class Session:
    def __init__(self, binary, env, report):
        self.child = Child([str(binary), "mcp", "--stdio"], env)
        self.ident = 0
        self.report = report

    def call(self, method, params):
        self.ident += 1
        request = {"jsonrpc": "2.0", "id": self.ident, "method": method, "params": params}
        self.child.send(request)
        response = json.loads(self.child.line(timeout=45))
        assert response.get("id") == self.ident, response
        assert response.get("jsonrpc") == "2.0" and "error" not in response, response
        self.report["wire"].append({"request": request, "response": response})
        return response

    def execute(self, code, expected_error=False):
        response = self.call("tools/call", {"name": "zuse.execute", "arguments": {"code": code}})
        result = check_tool(response)
        assert result.get("isError") is expected_error, response
        outcome = result["structuredContent"]
        assert outcome["status"] == ("failed" if expected_error else "completed"), outcome
        return outcome

    def value(self, code):
        outcome = self.execute(code)
        assert outcome["completed"] == 1 and len(outcome["results"]) == 1, outcome
        return outcome["results"][0]["value"]

    def close(self):
        # EOF lets the runtime drop any owned browser before forced process cleanup.
        if self.child.process.poll() is None:
            self.child.process.stdin.close()
            try:
                self.child.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                pass
        self.child.close()


def invocation(name, **arguments):
    values = ",".join(f"{key}={json.dumps(value, ensure_ascii=False)}" for key, value in arguments.items())
    return f"{name}({values})"


def endpoint_closed(port):
    try:
        connection = socket.create_connection(("127.0.0.1", port), timeout=0.5)
    except OSError:
        return True
    else:
        connection.close()
        return False


def run(binary, report):
    session = None
    browser_open = False
    with tempfile.TemporaryDirectory(prefix="zuse-release-browser-") as workspace:
        directory = Path(workspace)
        temporary = directory / "tmp"
        temporary.mkdir()
        env = os.environ.copy()
        for name in ("ZUSE_TOKEN", "DISPLAY", "XAUTHORITY", "WAYLAND_DISPLAY", "HYPRLAND_INSTANCE_SIGNATURE"):
            env.pop(name, None)
        env["TMPDIR"] = env["TMP"] = env["TEMP"] = str(temporary)
        env["ZUSE_STATE_DIR"] = str(directory / "state")
        env["DBUS_SESSION_BUS_ADDRESS"] = "unix:path=/does-not-exist/zuse-release-browser-bus"
        active_check = "mcp_stdio_handshake"
        try:
            session = Session(binary, env, report)
            initialized = session.call("initialize", {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "zuse-release-browser-selection", "version": "1"},
            })
            server = initialized["result"]["serverInfo"]
            assert server["name"] == "zuse", initialized
            assert server["version"] == report["binary_version"].removeprefix("zuse "), server
            session.child.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
            report["checks"].append({"name": active_check, "passed": True, "server": server})

            active_check = "isolated_headless_fixture"
            session.value("browser.launch(headless=true)")
            browser_open = True
            profiles = list(temporary.glob("zuse-browser-*"))
            assert len(profiles) == 1 and profiles[0].is_dir(), profiles
            port = int((profiles[0] / "DevToolsActivePort").read_text().splitlines()[0])
            html = f"<!doctype html><meta charset=utf-8><title>{TITLE}</title><input id=entry>"
            session.execute("; ".join([
                invocation("browser.navigate", url="data:text/html," + quote(html)),
                invocation("browser.fill", selector="#entry", text=PAYLOAD),
                invocation("browser.evaluate", expression="window.fixtureMarker=37"),
            ]))
            tabs = session.value("browser.tabs()")["targetInfos"]
            targets = [tab["targetId"] for tab in tabs if tab.get("title") == TITLE]
            assert len(targets) == 1, tabs
            target = targets[0]
            assert session.value(invocation("browser.evaluate", expression=STATE_EXPRESSION)) == STATE
            report["checks"].append({"name": active_check, "passed": True, "target": target})

            active_check = "reselect_current_page_twice_preserves_dom_and_utf8"
            observations = []
            for _ in range(2):
                selected = session.value(invocation("browser.select", id=target))
                assert selected["targetId"] == target, selected
                observed = session.value(invocation("browser.evaluate", expression=STATE_EXPRESSION))
                assert observed == STATE, observed
                observations.append(observed)
            report["checks"].append({"name": active_check, "passed": True, "observations": observations})

            active_check = "unknown_target_is_rejected"
            failed = session.execute(
                invocation("browser.select", id="zuse-missing-release-fixture-target"),
                expected_error=True,
            )
            assert failed["completed"] == 0 and failed["failed_step"] == 0, failed
            assert failed["operation"] == "browser.select", failed
            report["checks"].append({"name": active_check, "passed": True, "error": failed["error"]})

            active_check = "original_page_usable_after_failed_selection"
            observed = session.value(invocation("browser.evaluate", expression=STATE_EXPRESSION))
            assert observed == STATE, observed
            report["checks"].append({"name": active_check, "passed": True, "observation": observed})

            active_check = "close_removes_owned_profile_and_endpoint"
            closed = session.value("browser.close()")
            assert closed == {"ok": True}, closed
            browser_open = False
            assert all(not profile.exists() for profile in profiles), profiles
            assert endpoint_closed(port), "owned Chromium debugging endpoint remains open"
            report["checks"].append({
                "name": active_check, "passed": True,
                "profile_removed": True, "debugging_endpoint_closed": True,
            })

            active_check = "closed_browser_rejects_further_operations"
            failed = session.execute(
                invocation("browser.evaluate", expression="window.fixtureMarker"),
                expected_error=True,
            )
            assert failed["completed"] == 0 and failed["failed_step"] == 0, failed
            assert not list(temporary.glob("zuse-browser-*"))
            report["checks"].append({"name": active_check, "passed": True, "error": failed["error"]})
        except Exception as error:
            report["checks"].append({"name": active_check, "passed": False, "error": str(error)})
            raise
        finally:
            if session:
                try:
                    if browser_open:
                        session.execute("browser.close()")
                finally:
                    session.close()
                report["server_exit_code"] = session.child.process.returncode
                assert report["server_exit_code"] == 0, report["server_exit_code"]
                report["remaining_profiles"] = [path.name for path in temporary.glob("zuse-browser-*")]
                assert not report["remaining_profiles"], report["remaining_profiles"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path, help="Directory for result.json")
    arguments = parser.parse_args()
    binary = arguments.binary.resolve(strict=True)
    output = arguments.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    began = time.monotonic()
    report = {
        "schema": "zuse.release-browser-selection.v1",
        "started_at": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "binary": str(binary),
        "scope": "Actual CLI, MCP stdio, self-owned headless Chromium, local data URL",
        "checks": [],
        "wire": [],
    }
    try:
        with binary.open("rb") as source:
            report["binary_sha256"] = hashlib.file_digest(source, "sha256").hexdigest()
        report["binary_version"] = subprocess.check_output(
            [str(binary), "--version"], text=True, timeout=10,
        ).strip()
        run(binary, report)
        report["passed"] = all(check["passed"] for check in report["checks"])
    except Exception as error:
        report["passed"] = False
        report["error"] = str(error)
    report["elapsed_seconds"] = round(time.monotonic() - began, 3)
    receipt = output / "result.json"
    receipt.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps({
        "passed": report["passed"],
        "checks_passed": sum(check["passed"] for check in report["checks"]),
        "checks_total": len(report["checks"]),
        "binary_version": report.get("binary_version"),
        "receipt": str(receipt),
        "error": report.get("error"),
    }, ensure_ascii=False))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
