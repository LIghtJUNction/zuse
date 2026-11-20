#!/usr/bin/env python3
"""Actual UTF-8 receipt with private Xvfb, GTK and AT-SPI; no host input.

Requires a C compiler, GTK3 development files, XTest, Xvfb, dbus-run-session,
gdbus and at-spi2-core. Builds only this crate's small ignored process driver.
Use a private CARGO_TARGET_DIR/CARGO_HOME and CARGO_BUILD_JOBS=1 as appropriate.
The receiver's pause is deliberate test input, not an implementation delay.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import threading
import time
import traceback

ROOT = Path(__file__).resolve().parents[1]
PAYLOAD = "Z 你好こんにちは مرحبا नमस्ते 🙂"


def line(process, timeout=5):
    if not select.select([process.stdout], [], [], timeout)[0]:
        raise RuntimeError(f"fixture did not reply within {timeout}s")
    value = process.stdout.readline()
    if not value:
        raise RuntimeError(f"fixture exited with {process.poll()}")
    return value.decode("utf-8").strip()


def stop(process):
    if process is None:
        return
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=3)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--inside", action="store_true")
    parser.add_argument("--output", type=Path, default=ROOT / "target/x11-utf8-proof")
    parser.add_argument("--driver", type=Path, help="reuse an already compiled ignored test binary")
    args = parser.parse_args()
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    if not args.inside:
        command = ["dbus-run-session", "--", sys.executable, str(Path(__file__).resolve()), "--inside", "--output", str(args.output)]
        if args.driver:
            command += ["--driver", str(args.driver.resolve())]
        return subprocess.call(command)
    report = {"cases": [], "payload": PAYLOAD, "transport": "local X11 + confirmed AT-SPI UTF-8"}
    fixture = bus = registry = xvfb = None
    logs = []
    try:
        if args.driver:
            driver = args.driver.resolve()
        else:
            build = subprocess.run(["cargo", "test", "--locked", "--offline", "--test", "x11_utf8_native", "--no-run", "--message-format=json"], cwd=ROOT, capture_output=True, text=True, timeout=180)
            (args.output / "build.stdout").write_text(build.stdout)
            (args.output / "build.stderr").write_text(build.stderr)
            build.check_returncode()
            binaries = [j["executable"] for value in build.stdout.splitlines() if (j := json.loads(value)).get("reason") == "compiler-artifact" and j.get("executable") and j.get("target", {}).get("name") == "x11_utf8_native"]
            if len(binaries) != 1:
                raise RuntimeError(f"expected one native driver, got {binaries}")
            driver = Path(binaries[0])
        report["driver"] = str(driver)
        report["driver_sha256"] = hashlib.sha256(driver.read_bytes()).hexdigest()
        fixture_binary = args.output / "receiver"
        flags = subprocess.check_output(["pkg-config", "--cflags", "--libs", "gtk+-3.0", "xtst"], text=True).split()
        subprocess.run(["cc", "-O1", "-Wall", "-Wextra", "-Werror", str(ROOT / "tests/native_x11_utf8_fixture.c"), "-o", str(fixture_binary), *flags, "-lX11"], check=True, timeout=60)
        report["gtk_version"] = subprocess.check_output(["pkg-config", "--modversion", "gtk+-3.0"], text=True).strip()
        report["atspi_version"] = subprocess.check_output(["pkg-config", "--modversion", "atspi-2"], text=True).strip()
        def log(name):
            handle = (args.output / name).open("wb")
            logs.append(handle)
            return handle
        xvfb = subprocess.Popen(["Xvfb", "-displayfd", "1", "-screen", "0", "960x360x24", "-nolisten", "tcp"], stdout=subprocess.PIPE, stderr=log("xvfb.log"), bufsize=0)
        display_number = line(xvfb)
        if not display_number.isdigit():
            raise RuntimeError("Xvfb did not allocate a private display")
        bus = subprocess.Popen(["dbus-daemon", "--config-file=/usr/share/defaults/at-spi2/accessibility.conf", "--nofork", "--print-address=1"], stdout=subprocess.PIPE, stderr=log("bus.log"), bufsize=0)
        address = line(bus)
        if not address.startswith("unix:"):
            raise RuntimeError("private accessibility bus did not start")
        env = dict(os.environ, DISPLAY=":" + display_number, ZUSE_BACKEND="x11", GDK_BACKEND="x11", NO_AT_BRIDGE="0", AT_SPI_BUS_ADDRESS=address, GIO_USE_VFS="local", ZUSE_X11_UTF8_FIXTURE="1")
        env.pop("WAYLAND_DISPLAY", None)
        registry_path = next((str(p) for p in [Path("/usr/lib/at-spi2-registryd"), Path("/usr/libexec/at-spi2-registryd")] if p.is_file()), None)
        if registry_path is None:
            raise RuntimeError("at-spi2-registryd is missing")
        registry = subprocess.Popen([registry_path], env=env, stdout=log("registry.stdout"), stderr=log("registry.stderr"))
        deadline = time.monotonic() + 5
        while True:
            owner = subprocess.run(["gdbus", "call", "--address", address, "--dest", "org.freedesktop.DBus", "--object-path", "/org/freedesktop/DBus", "--method", "org.freedesktop.DBus.NameHasOwner", "org.a11y.atspi.Registry"], capture_output=True, text=True, timeout=2)
            if "true" in owner.stdout:
                break
            if time.monotonic() >= deadline:
                raise RuntimeError("private accessibility registry was not ready")
            time.sleep(0.02)
        fixture = subprocess.Popen([str(fixture_binary)], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log("receiver.stderr"), bufsize=0)
        ready = line(fixture).split()
        if len(ready) != 4 or ready[0] != "READY":
            raise RuntimeError(f"unexpected receiver startup: {ready}")
        target, other, pid = ready[1:]
        # Observe registration, rather than assuming a fixed startup delay.
        deadline = time.monotonic() + 5
        while True:
            children = subprocess.run(["gdbus", "call", "--address", address, "--dest", "org.a11y.atspi.Registry", "--object-path", "/org/a11y/atspi/accessible/root", "--method", "org.a11y.atspi.Accessible.GetChildren"], capture_output=True, text=True, timeout=2)
            if "/org/a11y/atspi/accessible/root" in children.stdout:
                break
            if time.monotonic() >= deadline:
                raise RuntimeError(f"GTK accessibility registration missing: {children.stdout} {children.stderr}")
            time.sleep(0.02)

        def command(value):
            fixture.stdin.write((value + "\n").encode())
            fixture.stdin.flush()
            return line(fixture)

        def reset(text=""):
            assert command("RESET " + (base64.b64encode(text.encode()).decode() or "-")) == "OK"

        def state():
            parts = command("STATE").split()
            assert parts[0] == "STATE"
            decode = lambda s: "" if s == "-" else base64.b64decode(s).decode()
            return {"text": decode(parts[1]), "other": decode(parts[2]), "caret": int(parts[3]), "selection": [int(parts[4]), int(parts[5])], "changes": int(parts[6]), "clipboard_clears": int(parts[7])}

        def native():
            parts = command("NATIVE").split()
            assert parts[0] == "NATIVE"
            return {"mapping": hashlib.sha256(base64.b64decode(parts[4])).hexdigest(), "keys": parts[5], "keyboard": parts[6], "clipboard_owner": parts[7], "primary_owner": parts[8]}

        def invoke(value, action="type", window=target, pause_before=0):
            result_path = args.output / "last-result.txt"
            result_path.unlink(missing_ok=True)
            local = dict(env, ZUSE_X11_UTF8_TARGET=window, ZUSE_X11_UTF8_VALUE=value, ZUSE_X11_UTF8_ACTION=action, ZUSE_X11_UTF8_RESULT=str(result_path), ZUSE_X11_UTF8_BARRIER="1" if pause_before else "0")
            started = time.monotonic()
            child = subprocess.Popen([str(driver), "--ignored", "--exact", "input", "--nocapture"], env=local, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
            timer = None
            try:
                if pause_before:
                    deadline = time.monotonic() + 5
                    while "PROBE_READY" not in line(child):
                        if time.monotonic() >= deadline:
                            raise RuntimeError("sender did not reach the pre-input barrier")
                    os.kill(fixture.pid, signal.SIGSTOP)
                    timer = threading.Timer(pause_before, lambda: os.kill(fixture.pid, signal.SIGCONT))
                    timer.start()
                    child.stdin.write(b"GO\n")
                    child.stdin.flush()
                out, err = child.communicate(timeout=10)
                if child.returncode:
                    raise RuntimeError(f"driver failed: {out!r} {err!r}")
                return result_path.read_text(), time.monotonic() - started
            finally:
                if timer:
                    timer.join(timeout=2)
                    os.kill(fixture.pid, signal.SIGCONT)
                stop(child)

        def check(name, value, expected, *, error=False, action="type", window=target, pause_before=0):
            before = native()
            result, elapsed = invoke(value, action, window, pause_before)
            # Observation may wait for ordinary X key events; it never retries
            # the input call, which executes exactly once in every case.
            deadline = time.monotonic() + 2
            observed = state()
            while observed["text"] != expected and time.monotonic() < deadline:
                time.sleep(0.01)
                observed = state()
            after = native()
            receipt = {"name": name, "result": result, "seconds": elapsed, "observed": observed, "state_before": before, "state_after": after}
            report["cases"].append(receipt)
            assert (result.startswith("error:")) == error, receipt
            assert observed["text"] == expected, receipt
            assert observed["other"] == "other window untouched", receipt
            assert observed["clipboard_clears"] == 0, receipt
            for field in ["mapping", "keys", "keyboard", "clipboard_owner"]:
                assert before[field] == after[field], (field, receipt)
            return receipt

        reset()
        check("normal multilingual UTF-8", PAYLOAD, PAYLOAD)
        reset()
        check("receiver stopped before input", PAYLOAD, PAYLOAD, pause_before=0.12)
        reset()
        assert command("PAUSE 120") == "OK"
        pause = check("receiver pauses inside insertion callback", PAYLOAD, PAYLOAD)
        assert pause["seconds"] >= 0.12 and pause["observed"]["changes"] == 1, pause
        reset("左é右")
        assert command("SELECT 1 2") == "OK"
        selected = check("selection uses character offsets and byte length", "🙂नम", "左🙂नम右")
        assert selected["observed"]["caret"] == 4, selected
        appended = check("caret advances for repeated commits", "e\u0301", "左🙂नमe\u0301右")
        assert appended["observed"]["caret"] == 6, appended
        reset("keep")
        check("background target rejected", "ASCII prefix🙂", "keep", error=True, action="background", window=other)
        reset("keep")
        assert command("BUTTON") == "OK"
        check("noneditable target rejects whole string", "ASCII prefix🙂", "keep", error=True)
        reset("keep")
        assert command("PID " + str(os.getpid())) == "OK"
        try:
            check("mismatched process identity rejected", "ASCII prefix🙂", "keep", error=True)
        finally:
            assert command("PID " + pid) == "OK"
        reset("keep")
        assert command("MACHINE unrelated-remote-host.invalid") == "OK"
        try:
            check("remote X client identity rejected", "ASCII prefix🙂", "keep", error=True)
        finally:
            assert command("MACHINE local") == "OK"
        reset()
        assert command("SHIFT_DOWN") == "OK"
        try:
            check("UTF-8 preserves a held modifier", "你好🙂", "你好🙂")
            check("native keys reject a held modifier without releasing it", "ASCII", "你好🙂", error=True)
        finally:
            assert command("SHIFT_UP") == "OK"
        reset()
        check("ASCII keyboard path", "Hello Zuse 123!", "Hello Zuse 123!")
        result, _ = invoke("Ctrl+a", "key")
        assert result == "ok", result
        check("existing select-all shortcut and ASCII replacement", "Replaced!", "Replaced!")
        reset()
        assert command("LIMIT 3") == "OK"
        truncated = check("filtered insertion fails without key fallback", "ABCDE🙂", "ABC", error=True)
        assert "not fully confirmed" in truncated["result"] and truncated["observed"]["changes"] == 1, truncated
        reset()
        assert command("PAUSE 1800") == "OK"
        timed_out = check("uncertain reply does not resend or roll back", PAYLOAD, PAYLOAD, error=True)
        assert "do not retry automatically" in timed_out["result"] and timed_out["observed"]["changes"] == 1, timed_out
        clipboard = command("CLIPBOARD").split()
        assert clipboard[0] == "CLIPBOARD" and base64.b64decode(clipboard[1]).decode() == "keep my clipboard 中文"
        assert base64.b64decode(clipboard[2]) == bytes([0, 255, 42, 128]) and clipboard[3] == "0"
        report["clipboard_text_and_binary_preserved"] = True
        report["status"] = "pass"
        return 0
    except Exception:
        report["status"] = "fail"
        report["error"] = traceback.format_exc()
        return 1
    finally:
        if fixture is not None and fixture.poll() is None:
            os.kill(fixture.pid, signal.SIGCONT)
        for process in (fixture, registry, bus, xvfb):
            stop(process)
        for handle in logs:
            handle.close()
        (args.output / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
        print(json.dumps(report, ensure_ascii=False, indent=2), flush=True)


if __name__ == "__main__":
    raise SystemExit(main())
