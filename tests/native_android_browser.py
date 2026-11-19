#!/usr/bin/env python3
"""Opt-in physical Android Chrome acceptance using an owned offline tab.

Requires an explicitly selected device and an already running Chrome with an
observable DevTools abstract socket. Never lists pre-existing browser targets.
Report data excludes the device serial and foreground application identities.
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import subprocess
import sys
import time

HTML = """<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>zuse Android browser fixture</title>
<style>html,body{margin:0;background:rgb(32,160,96);font:18px sans-serif}
main{margin:24px;padding:24px;background:white;border-radius:12px}
input,button{font:inherit;padding:8px;margin:8px 0;display:block}
</style></head><body><main><h1>zuse mobile browser</h1>
<label for="entry">Fixture input</label><input id="entry">
<button id="run">Run fixture</button><p id="result">waiting</p>
<input id="secret" type="password" aria-label="Fixture secret">
<script>window.clicks=0;window.trusted=false;window.lastKey="";
document.addEventListener("keydown",e=>window.lastKey=e.key);
document.querySelector("#run").addEventListener("click",e=>{
window.clicks++;window.trusted=e.isTrusted;
document.querySelector("#result").textContent="clicked "+window.clicks;});
</script></main></body></html>"""


def require(condition, detail):
    if not condition:
        raise AssertionError(detail)


class Shell:
    def __init__(self, binary, folder):
        self.stderr = (folder / "stderr.log").open("w")
        os.chmod(folder / "stderr.log", 0o600)
        self.process = subprocess.Popen(
            [str(binary), "shell"], stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=self.stderr, bufsize=0
        )
        self.reader = selectors.DefaultSelector()
        self.reader.register(self.process.stdout, selectors.EVENT_READ)
        self.buffer = bytearray()

    def run(self, code, *, failed=False):
        self.process.stdin.write((code + "\n").encode())
        self.process.stdin.flush()
        deadline = time.monotonic() + 45
        while b"\n" not in self.buffer:
            remaining = deadline - time.monotonic()
            require(remaining > 0, "CLI response deadline exceeded")
            require(self.reader.select(remaining), "CLI response timeout")
            data = os.read(self.process.stdout.fileno(), 65536)
            require(data, "CLI closed without a complete response")
            self.buffer.extend(data)
            require(len(self.buffer) <= 2 * 1024 * 1024, "CLI response too large")
        line, _, tail = self.buffer.partition(b"\n")
        self.buffer = bytearray(tail)
        value = json.loads(line)
        wanted = "failed" if failed else "completed"
        require(value.get("status") == wanted,
                "unexpected CLI result: " + json.dumps(value, ensure_ascii=False)[:1600])
        return value if failed else value["results"][-1]["value"]

    def finish(self):
        self.process.stdin.close()
        try:
            status = self.process.wait(timeout=40)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)
            raise AssertionError("owned CLI did not finish cleanup")
        finally:
            self.reader.close()
            self.process.stdout.close()
            self.stderr.close()
        return status


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--serial", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--wake-chrome", action="store_true", help="Temporarily show Chrome for readiness, then restore the previous activity before page operations")
    args = parser.parse_args()
    binary = args.binary.resolve()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    os.chmod(output, 0o700)
    from PIL import Image

    def adb(*words):
        return subprocess.run(["adb", "-s", args.serial, *words], check=True,
                              capture_output=True, text=True, timeout=15).stdout

    def foreground():
        # Compare privately. Never copy app identities or activity dumps to report.
        raw = adb("shell", "dumpsys", "activity", "activities")
        found = re.findall(
            r"(?:topResumedActivity|mResumedActivity)\s*[:=]\s*"
            r"ActivityRecord\{[^\n]*?\bu\d+\s+([^\s}]+)", raw
        )
        return sorted(set(found)) or None

    report = {
        "kind": "physical_android_chrome",
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "version": subprocess.check_output([str(binary), "--version"], text=True).strip(),
        "device_model": adb("shell", "getprop", "ro.product.model").strip(),
        "android_api": adb("shell", "getprop", "ro.build.version.sdk").strip(),
        "transport": "native_adb_localabstract",
        "page": "owned offline data URL fixture",
        "profile_isolated": False,
        "preexisting_targets_enumerated": False,
        "readiness_setup": "temporary Chrome foreground, restored before page operations" if args.wake_chrome else "already responsive Chrome",
        "checks": [],
        "cleanup": {},
    }

    def passed(name, **evidence):
        report["checks"].append({"name": name, "passed": True, **evidence})

    shell = None
    connected = False
    wake_attempted = False
    setup_restored = False
    initial_focus = foreground()
    original_component = None
    if args.wake_chrome:
        display = None
        for line in adb("shell", "dumpsys", "activity", "activities").splitlines():
            marker = re.match(r"\s*Display #(\d+)", line)
            if marker:
                display = int(marker.group(1))
            if display == 0 and ("topResumedActivity" in line or "mResumedActivity" in line):
                found = re.search(r"ActivityRecord\{[^\n]*?\bu\d+\s+([^\s}]+)\s+t\d+", line)
                if found:
                    original_component = found.group(1)
                    break

    def restore_setup():
        nonlocal setup_restored
        require(original_component is not None, "original primary activity unavailable")
        value = adb("shell", "am", "start", "--display", "0",
                    "--activity-reorder-to-front", "--activity-single-top",
                    "-n", original_component)
        require("Error:" not in value, "could not restore the original primary activity")
        require(foreground() == initial_focus, "primary activity restoration not observed")
        setup_restored = True

    try:
        shell = Shell(binary, output)
        sockets = shell.run("android.browser.sockets(serial=" + json.dumps(args.serial) + ")")
        require("chrome" in json.dumps(sockets).lower(), "Chrome socket unavailable")
        passed("observed_chrome_socket")
        if args.wake_chrome:
            require(original_component is not None, "cannot wake Chrome without a known restore target")
            candidates = adb("shell", "cmd", "package", "resolve-activity", "--brief",
                             "-a", "android.intent.action.MAIN", "-c",
                             "android.intent.category.LAUNCHER", "-p", "com.android.chrome")
            launch = [line.strip() for line in candidates.splitlines()
                      if line.strip().startswith("com.android.chrome/")]
            require(len(launch) == 1, "Chrome launcher component is not unique")
            wake_attempted = True
            value = adb("shell", "am", "start", "-W", "--display", "0",
                        "--activity-reorder-to-front", "-a", "android.intent.action.MAIN",
                        "-c", "android.intent.category.LAUNCHER", "-n", launch[0])
            require("Error:" not in value, "Chrome readiness launch failed")
        result = shell.run("browser.connect_android(serial=" + json.dumps(args.serial) + ")")
        connected = True
        require(result.get("connected") is True, "browser not connected")
        passed("native_adb_connection", backend=result.get("backend"))
        if wake_attempted:
            restore_setup()
            passed("readiness_focus_restored_before_page_operations")
        tabs = shell.run("browser.tabs()")
        require(tabs.get("scope") == "owned", "mobile target scope not owned")
        require(len(tabs.get("targetInfos", [])) == 1, "expected exactly one owned target")
        target = tabs["targetInfos"][0].get("targetId")
        require(bool(target), "missing owned target identity")
        passed("owned_target_scope")
        shell.run("browser.viewport(width=640,height=480)")
        url = "data:text/html;base64," + base64.b64encode(HTML.encode()).decode()
        shell.run("browser.navigate(url=" + json.dumps(url) + ")")
        title = shell.run('browser.evaluate(expression="document.title")')
        require(title == "zuse Android browser fixture", "fixture navigation mismatch")
        passed("offline_navigation")
        phrase = "zuse 中文 ✓ 🙂"
        shell.run("browser.fill(selector=\"#entry\",text=" +
                  json.dumps(phrase, ensure_ascii=False) + ")")
        value = shell.run('browser.evaluate(expression="document.querySelector(\'#entry\').value")')
        require(value == phrase, "Unicode input mismatch")
        passed("unicode_input", text=phrase)
        shell.run('browser.key(key="Enter")')
        require(shell.run('browser.evaluate(expression="window.lastKey")') == "Enter",
                "key event not delivered")
        passed("keyboard_event")
        shell.run('browser.click(selector="#run")')
        state = shell.run('browser.evaluate(expression="({clicks, trusted, result:document.querySelector(\'#result\').textContent})")')
        require(state == {"clicks": 1, "trusted": True, "result": "clicked 1"},
                "trusted click result mismatch")
        passed("trusted_selector_click", state=state)
        expression = "document.querySelector('#secret').value='fixture-private-value';true"
        shell.run("browser.evaluate(expression=" + json.dumps(expression) + ")")
        snapshot = shell.run("browser.snapshot()")
        encoded = json.dumps(snapshot, ensure_ascii=False)
        require("zuse mobile browser" in encoded, "accessible heading missing")
        require("fixture-private-value" not in encoded, "password value exposed in AX view")
        passed("accessibility_snapshot", nodes=len(snapshot.get("nodes", [])),
               password_value_omitted=True)
        text = shell.run("browser.text()")
        require("clicked 1" in text and "zuse mobile browser" in text, "rendered text mismatch")
        passed("rendered_text")
        picture = output / "fixture.png"
        shell.run("browser.capture(path=" + json.dumps(str(picture)) + ")")
        with Image.open(picture) as image:
            image.load()
            size = list(image.size)
            pixel = list(image.convert("RGB").getpixel((2, 2)))
            require(size == [640, 480], "viewport PNG dimensions mismatch")
            require(pixel == [32, 160, 96], "viewport PNG background pixel mismatch")
        passed("viewport_png", dimensions=size, sample_rgb=pixel,
               sha256=hashlib.sha256(picture.read_bytes()).hexdigest())
        reject = shell.run('browser.select(id="zuse-fixture-foreign-target")', failed=True)
        require("only select their own" in reject.get("error", ""), "foreign target not rejected")
        require(shell.run('browser.evaluate(expression="document.title")') == title,
                "failed selection changed active owned target")
        passed("foreign_target_selection_rejected")
        current = foreground()
        require(initial_focus is not None and current is not None,
                "foreground identity unavailable; cannot assert preservation")
        require(current == initial_focus, "foreground activity changed during background control")
        passed("foreground_activity_preserved")
    except Exception as exc:
        report["checks"].append({
            "name": "execution", "passed": False,
            "error": str(exc).replace(args.serial, "<selected-device>")
        })
    finally:
        if wake_attempted and not setup_restored:
            try:
                restore_setup()
            except Exception:
                report["cleanup"]["setup_focus_restore_error"] = True
        if wake_attempted:
            report["cleanup"]["setup_focus_restored"] = setup_restored
        if shell is not None:
            if connected:
                try:
                    result = shell.run("browser.close()")
                    require(result.get("ok") is True, "owned target close not acknowledged")
                    report["cleanup"]["owned_target_close_acknowledged"] = True
                except Exception as exc:
                    report["cleanup"]["owned_target_close_acknowledged"] = False
                    report["cleanup"]["error"] = str(exc).replace(args.serial, "<selected-device>")
            try:
                report["cleanup"]["shell_exit_code"] = shell.finish()
            except Exception as exc:
                report["cleanup"]["shell_error"] = str(exc).replace(args.serial, "<selected-device>")
        try:
            final_focus = foreground()
            report["cleanup"]["foreground_activity_preserved"] = (
                initial_focus is not None and final_focus == initial_focus
            )
        except Exception:
            report["cleanup"]["foreground_activity_preserved"] = False
        report["passed"] = (
            all(item["passed"] for item in report["checks"])
            and report["cleanup"].get("owned_target_close_acknowledged", False)
            and report["cleanup"].get("foreground_activity_preserved", False)
            and report["cleanup"].get("shell_exit_code") == 1
            # shell exit 1 records the deliberately rejected foreign selection.
        )
        path = output / "report.json"
        path.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
        os.chmod(path, 0o600)
        print(json.dumps(report, ensure_ascii=False))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
