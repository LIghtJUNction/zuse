#!/usr/bin/env python3
"""Opt-in physical Android acceptance test. Only touches an owned zero-permission fixture.

Requires an explicitly selected authorized serial, a built fixture APK, adb, and
Pillow. Never run this as an unattended CI job or against an unspecified device.
All actions under test go through zuse; adb installs, observes, and removes the
fixture. No device serial or other application's UI is written to the report.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time

from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
PACKAGE = "best.lmm.zuse.fixture"
COMPONENT = PACKAGE + "/.MainActivity"


class Acceptance:
    def __init__(self, args):
        self.args = args
        self.binary = Path(args.binary).resolve()
        self.output = Path(args.output).resolve()
        self.output.mkdir(parents=True, exist_ok=True)
        os.chmod(self.output, 0o700)
        self.results = []
        self.owned_display = None
        self.owner_instance = None
        self.installed = False
        self.original_activity = None
        self.report = {
            "schema": 1,
            "label": args.label,
            "binary_sha256": hashlib.sha256(self.binary.read_bytes()).hexdigest(),
            "tests": self.results,
        }

    def command(self, argv, timeout=30, check=True):
        result = subprocess.run(argv, capture_output=True, text=True, timeout=timeout)
        if check and result.returncode:
            raise RuntimeError((result.stderr or result.stdout).strip()[:1500])
        return result

    def adb(self, *args, **kwargs):
        return self.command(["adb", "-s", self.args.serial, *map(str, args)], **kwargs)

    def state(self, display=0):
        raw = self.adb("shell", "run-as", PACKAGE, "cat", f"files/display-{display}.json")
        value = json.loads(raw.stdout)
        if value.get("package") != PACKAGE or value.get("displayId") != display:
            raise RuntimeError("fixture state belongs to a different package or display")
        return value

    def wait_state(self, display=0, predicate=lambda state: state.get("ready"), timeout=5):
        deadline = time.monotonic() + timeout
        last = None
        while time.monotonic() < deadline:
            try:
                last = self.state(display)
                if predicate(last):
                    return last
            except (RuntimeError, ValueError):
                pass
            time.sleep(0.08)
        diagnostic = {}
        if last:
            diagnostic = {key: last.get(key) for key in ["displayId", "ready", "windowFocus", "text", "counter", "keyDowns", "keyUps", "backPresses", "lifecycle"]}
            diagnostic["input"] = last.get("controls", {}).get("input", {})
        raise AssertionError("fixture condition timeout: " + json.dumps(diagnostic, ensure_ascii=False))

    def top_activity(self):
        raw = self.adb("shell", "dumpsys", "activity", "activities").stdout
        display = None
        for line in raw.splitlines():
            match = re.search(r"Display #(\d+)", line)
            if match:
                display = int(match.group(1))
            if display == 0 and "topResumedActivity=" in line:
                match = re.search(r"u\d+\s+(\S+)\s+t\d+", line)
                if match:
                    return match.group(1)
        return None

    def displays(self):
        text = self.adb("shell", "cmd", "display", "get-displays", "--ids-only").stdout
        return {int(line) for line in text.splitlines() if line.strip().isdigit()}

    def guard(self, display, require_primary=True):
        if require_primary and not (self.top_activity() or "").startswith(PACKAGE + "/"):
            raise RuntimeError("primary activity changed; refusing to send test input")
        state = self.state(display)
        if not state.get("ready") or state.get("destroyed"):
            raise RuntimeError("fixture is not ready")
        if display and (
            display != self.owned_display
            or state.get("role") != "presentation"
            or state.get("ownerInstance") != self.owner_instance
        ):
            raise RuntimeError("secondary target is not the display owned by this test")
        return state

    def zuse(self, code, display=0, guard=True, expected_failure=False):
        if guard:
            self.guard(display)
        prefix = (
            f"android.connect(serial={json.dumps(self.args.serial)}, "
            f"mode={json.dumps('background' if display else 'preempt')}, display={display}); "
        )
        result = self.command([str(self.binary), "eval", prefix + code], timeout=45, check=False)
        try:
            value = json.loads(result.stdout)
        except ValueError as error:
            raise RuntimeError("zuse returned non-JSON output") from error
        failed = result.returncode != 0 or value.get("status") == "failed"
        if expected_failure:
            if not failed:
                raise AssertionError("operation unexpectedly reported success")
            return {"rejected": True, "error": value.get("error", "")[:1000]}
        if failed:
            raise RuntimeError(value.get("error") or result.stderr or "zuse failed")
        return value["results"][-1]["value"]

    def tap(self, control, display=0):
        state = self.guard(display)
        point = state["controls"][control]
        if not point["visible"] or point["width"] < 1 or point["height"] < 1:
            raise RuntimeError("test control is not visible")
        return self.zuse(f"android.tap(x={point['centerX']}, y={point['centerY']})", display)

    def record(self, name, function):
        began = time.monotonic()
        item = {"name": name}
        try:
            detail = function()
            item.update(status="passed", detail=detail)
        except Exception as error:
            item.update(status="failed", error=str(error).replace(self.args.serial, "<device>")[:1800])
        item["elapsed_ms"] = round((time.monotonic() - began) * 1000, 2)
        self.results.append(item)
        print(json.dumps(item, ensure_ascii=False), flush=True)
        return item["status"] == "passed"

    def button(self, display=0):
        before = self.guard(display)["counter"]
        self.tap("counter", display)
        after = self.wait_state(display, lambda s: s["counter"] == before + 1)
        return {"counter_delta": after["counter"] - before, "display": display}

    def text(self, display=0):
        self.tap("input", display)
        before = self.wait_state(display, lambda s: s["ready"] and s["controls"]["input"]["focused"] and (display != 0 or s["windowFocus"]))
        text = "Background 42!" if display else "Zuse 42!"
        self.zuse(f"android.type(text={json.dumps(text)})", display)
        state = self.wait_state(display, lambda s: s["text"] == before["text"] + text)
        return {"text": state["text"], "display": display}

    def keys(self):
        before = self.guard(0)
        if not before["text"]:
            raise AssertionError("cannot validate deletion after the earlier text input failed")
        self.zuse('android.key(key="backspace"); android.key(key="enter"); android.key(key="back")')
        after = self.wait_state(0, lambda s: s["backPresses"] > before["backPresses"])
        if after["text"] != before["text"][:-1]:
            raise AssertionError("backspace did not delete exactly the final character")
        if not (
            after["editorActions"] > before["editorActions"]
            or any(k["name"] == "KEYCODE_ENTER" for k in after["keys"])
        ):
            raise AssertionError("Enter was not received")
        return {
            "text": after["text"],
            "back_delta": after["backPresses"] - before["backPresses"],
            "key_down_delta": after["keyDowns"] - before["keyDowns"],
        }

    def gestures(self, display=0):
        before = self.guard(display)
        self.tap("gesture", display)
        self.wait_state(display, lambda s: s["gesture"]["taps"] == before["gesture"]["taps"] + 1)
        swipe = self.state(display)["gesture"]["suggestedSwipe"]
        args = ", ".join(f"{key}={swipe[key]}" for key in ["x1", "y1", "x2", "y2"])
        self.zuse(f"android.swipe({args}, duration=240)", display)
        after = self.wait_state(display, lambda s: s["gesture"]["swipes"] == before["gesture"]["swipes"] + 1)
        if after["gesture"]["recipientDisplayId"] != display:
            raise AssertionError("gesture reached the wrong display")
        return {
            "display": display,
            "taps_delta": after["gesture"]["taps"] - before["gesture"]["taps"],
            "swipes_delta": after["gesture"]["swipes"] - before["gesture"]["swipes"],
            "duration_ms": after["gesture"]["durationMs"],
        }

    def printable_ascii(self):
        self.tap("reset")
        self.wait_state(0, lambda s: not s["text"])
        self.tap("input")
        text = "".join(chr(i) for i in range(32, 127))
        self.zuse(f"android.type(text={json.dumps(text)})")
        state = self.wait_state(0, lambda s: s["text"] == text)
        return {"characters": len(state["text"]), "exact_match": True}

    def snapshot(self, display=0, verify_focus=False):
        state = self.guard(display, require_primary=(display == 0))
        snapshot = self.zuse("android.snapshot()", display, guard=False)
        if snapshot.get("display") != display:
            raise AssertionError("snapshot identifies a different display")
        windows = snapshot.get("windows", [])
        if any(window.get("display") != display for window in windows):
            raise AssertionError("snapshot leaked another display's window records")
        owned = [window for window in windows if PACKAGE in window.get("window", "")]
        if not owned:
            raise AssertionError("fixture window was missing from snapshot")
        if verify_focus and state.get("windowFocus") and not any(w.get("focused") for w in owned):
            raise AssertionError("focused fixture is reported as unfocused")
        return {
            "display": display,
            "fixture_windows": len(owned),
            "fixture_visible": any(w.get("visible") for w in owned),
            "fixture_focused": any(w.get("focused") for w in owned),
            "accessibility_xml": snapshot.get("accessibilityXml") is not None,
            "observation": snapshot.get("observation"),
        }

    def screenshot(self, display=0):
        state = self.guard(display, require_primary=(display == 0))
        path = self.output / f"fixture-display-{display}.png"
        self.zuse(f"android.capture(path={json.dumps(str(path))})", display, guard=False)
        try:
            self.guard(display, require_primary=(display == 0))
            with Image.open(path) as source:
                image = source.convert("RGB")
                if image.size != (state["displayWidth"], state["displayHeight"]):
                    raise AssertionError(f"PNG dimensions {image.size} differ from display dimensions")
                actual = []
                for point in state["pixelSamples"]:
                    rgb = image.getpixel((point["x"], point["y"]))
                    expected = (point["red"], point["green"], point["blue"])
                    if any(abs(a - b) > 2 for a, b in zip(rgb, expected)):
                        raise AssertionError(f"fixture color mismatch: {rgb} != {expected}")
                    actual.append(list(rgb))
                size = list(image.size)
            os.chmod(path, 0o600)
            return {
                "display": display, "dimensions": size, "pixel_samples": actual,
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
                "file": path.name,
            }
        except BaseException:
            path.unlink(missing_ok=True)
            raise

    def invalid_preflight(self):
        code = 'android.tap(x=1,y=1); android.type(text="中文")'
        with tempfile.NamedTemporaryFile(mode="w", suffix=".zuse", encoding="utf-8") as source:
            source.write(code)
            source.flush()
            result = self.command([str(self.binary), "check", source.name], check=False)
        if result.returncode == 0:
            raise AssertionError("check accepts unsupported Android Unicode after an earlier input step")
        return {"rejected_before_execution": True}

    def create_background(self):
        before = self.displays()
        self.adb("shell", "am", "start", "--display", "0", "-n", COMPONENT,
                 "--ez", "virtual_display", "true")
        owner = self.wait_state(0, lambda s: s["ready"] and (s["ownedVirtualDisplayId"] >= 0 or bool(s["virtualError"])))
        if owner["virtualError"]:
            raise RuntimeError("fixture cannot create its own-content display: " + owner["virtualError"])
        display = owner["ownedVirtualDisplayId"]
        if display == 0 or display in before:
            raise RuntimeError("fixture did not create a new secondary display")
        self.owned_display = display
        self.owner_instance = owner["instance"]
        state = self.wait_state(display)
        self.guard(display)
        return {"display": display, "role": state["role"],
                "dimensions": [state["displayWidth"], state["displayHeight"]]}

    def collect_metadata(self):
        directory = self.output / "metadata"
        directory.mkdir(mode=0o700, exist_ok=True)
        display_text = self.adb("shell", "cmd", "display", "get-displays").stdout
        (directory / "fixture-display.txt").write_text(
            "\n".join(line for line in display_text.splitlines() if "zuse-fixture-" in line) + "\n")
        window_text = self.adb("shell", "dumpsys", "window", "windows").stdout
        owned = ["  Window #" + block for block in window_text.split("  Window #")[1:]
                 if PACKAGE in block.splitlines()[0]]
        (directory / "fixture-windows.txt").write_text("\n".join(owned))
        focus_text = self.adb("shell", "dumpsys", "window", "displays").stdout
        focus_lines = []
        for line in focus_text.splitlines():
            if "Display: mDisplayId=" in line:
                focus_lines.append(line)
            elif "mCurrentFocus=" in line:
                focus_lines.append(line if PACKAGE in line or line.strip().endswith("=null")
                                   else " mCurrentFocus=<unrelated window redacted>")
        (directory / "fixture-focus.txt").write_text("\n".join(focus_lines) + "\n")
        sf = self.adb("shell", "dumpsys", "SurfaceFlinger", "--displays").stdout
        parts = re.split("(?m)(?=^ *(?:Virtual )?Display [0-9]+)", sf)
        selected = [part for part in parts if "zuse-fixture-" in part]
        if not selected:
            selected = [line for line in sf.splitlines() if "zuse-fixture-" in line]
        (directory / "fixture-surfaceflinger.txt").write_text("\n".join(selected) + "\n")
        for display in [0, self.owned_display]:
            if display is not None:
                (directory / f"fixture-state-{display}.json").write_text(
                    json.dumps(self.state(display), ensure_ascii=False, indent=2) + "\n")
        for path in directory.iterdir():
            os.chmod(path, 0o600)
        return {"saved": directory.name, "only_fixture_window_content": True}

    def background_isolation(self):
        display = self.owned_display
        before = self.guard(0)
        counter = self.button(display)
        text = self.text(display)
        gestures = self.gestures(display)
        after = self.guard(0)
        for key in ["counter", "text", "keyDowns", "keyUps"]:
            if after[key] != before[key]:
                raise AssertionError("background action changed primary fixture " + key)
        event_keys = [key for key in before["gesture"] if key != "suggestedSwipe"]
        changed = {key: {"before": before["gesture"][key], "after": after["gesture"][key]}
                   for key in event_keys if after["gesture"][key] != before["gesture"][key]}
        if changed:
            raise AssertionError("background changed primary gesture events: " + json.dumps(changed))
        return {"counter": counter, "text": text, "gestures": gestures,
                "primary_input_unchanged": True,
                "primary_layout_changed": before["gesture"]["suggestedSwipe"] != after["gesture"]["suggestedSwipe"]}

    def run(self):
        if self.adb("shell", "pm", "list", "packages", PACKAGE).stdout.strip():
            raise RuntimeError("fixture already exists; refusing to replace or remove an existing installation")
        self.original_activity = self.top_activity()
        initial_displays = self.displays()
        self.report["android_api"] = self.adb("shell", "getprop", "ro.build.version.sdk").stdout.strip()
        self.report["version"] = self.command([str(self.binary), "--version"]).stdout.strip()
        try:
            self.adb("install", str(Path(self.args.apk).resolve()), timeout=90)
            self.installed = True
            self.zuse(f"android.start(component={json.dumps(COMPONENT)})", guard=False)
            self.wait_state()
            self.guard(0)
            self.results.append({"name": "native_app_start", "status": "passed"})
            if not self.args.only_background:
                self.record("primary_button", self.button)
                self.record("primary_text", self.text)
                self.record("primary_keys", self.keys)
                self.record("primary_gestures", self.gestures)
                self.record("primary_printable_ascii", self.printable_ascii)
                self.record("primary_snapshot", self.snapshot)
                self.record("primary_focus_metadata", lambda: self.snapshot(verify_focus=True))
                self.record("primary_screenshot_pixels", self.screenshot)
                self.record("unicode_preflight", self.invalid_preflight)
                self.record("missing_component_rejected", lambda: self.zuse(
                    f"android.start(component={json.dumps(PACKAGE + '/.DoesNotExist')})",
                    expected_failure=True))
            if (self.args.background or self.args.only_background) and self.record("owned_background_display", self.create_background):
                self.record("fixture_metadata", self.collect_metadata)
                self.record("background_input_isolation", self.background_isolation)
                self.record("background_snapshot", lambda: self.snapshot(self.owned_display))
                self.record("background_screenshot_pixels", lambda: self.screenshot(self.owned_display))
        except Exception as error:
            self.results.append({"name": "setup", "status": "failed",
                                 "error": str(error).replace(self.args.serial, "<device>")[:1800]})
        finally:
            if self.installed:
                self.adb("shell", "am", "force-stop", PACKAGE, check=False)
                removed = self.adb("uninstall", PACKAGE, check=False)
                absent = not self.adb("shell", "pm", "list", "packages", PACKAGE).stdout.strip()
                displays = self.displays()
                cleanup = {
                    "name": "cleanup", "status": "passed" if absent and self.owned_display not in displays else "failed",
                    "fixture_uninstalled": absent,
                    "owned_display_removed": self.owned_display is None or self.owned_display not in displays,
                    "original_display_set_retained": initial_displays.issubset(displays),
                    "original_primary_activity_restored": self.top_activity() == self.original_activity,
                }
                if removed.returncode:
                    cleanup["error"] = removed.stderr.strip()[:500]
                self.results.append(cleanup)
        self.report["passed"] = sum(item["status"] == "passed" for item in self.results)
        self.report["failed"] = sum(item["status"] == "failed" for item in self.results)
        target = self.output / "report.json"
        target.write_text(json.dumps(self.report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        os.chmod(target, 0o600)
        print(json.dumps(self.report, ensure_ascii=False, indent=2))
        return 1 if self.report["failed"] else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--serial", required=True, help="explicit authorized physical device serial")
    parser.add_argument("--binary", default=str(ROOT / "target/native-validation/debug/zuse"))
    parser.add_argument("--apk", default=str(ROOT / "target/android-fixture/zuse-fixture.apk"))
    parser.add_argument("--output", default=str(ROOT / "target/native-android"))
    parser.add_argument("--label", default="local")
    parser.add_argument("--background", action="store_true", help="create and test an owned secondary display")
    parser.add_argument("--only-background", action="store_true", help="run only the owned secondary display cases")
    args = parser.parse_args()
    return Acceptance(args).run()


if __name__ == "__main__":
    raise SystemExit(main())
