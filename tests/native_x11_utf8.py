#!/usr/bin/env python3
"""Actual UTF-8 receipt with private Xvfb, GTK and AT-SPI; no host input.

Requires a C compiler, GTK3 development files, XTest, Xvfb, dbus-run-session,
gdbus and at-spi2-core. By default builds this crate's ignored process driver;
--driver reuses it, while --binary exercises an actual zuse CLI without Cargo.
The CLI's persistent shell acknowledges focus before the input statement, so
the receiver-stop case retains the driver's pre-input barrier in one process.
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
import secrets
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


def reserve_display():
    # Atomic directory creation is the per-user display lease. It remains held
    # until Xvfb exits; unlike unlinking a flock file, it has no open-inode race.
    for _ in range(32):
        number = str(10_000 + secrets.randbelow(20_000))
        lease = Path("/tmp") / f"project-z-xvfb-{os.getuid()}-{number}.lock"
        try:
            lease.mkdir(mode=0o700)
        except FileExistsError:
            continue
        return number, lease
    raise RuntimeError("could not reserve a private high-number display")


def cli_reply(output, operation):
    """Accept one actual Runtime result, never stderr or a process failure."""
    try:
        reply = json.loads(output)
    except (ValueError, UnicodeError) as error:
        raise RuntimeError(f"CLI returned no single JSON result for {operation}: {output!r}") from error
    if not isinstance(reply, dict):
        raise RuntimeError(f"CLI result for {operation} is not an object: {reply!r}")
    results = reply.get("results")
    if reply.get("status") == "completed":
        if reply.get("completed") != 1 or not isinstance(results, list) or len(results) != 1:
            raise RuntimeError(f"CLI did not complete exactly one {operation}: {reply!r}")
        if not isinstance(results[0], dict) or results[0].get("step") != 0 or results[0].get("operation") != operation:
            raise RuntimeError(f"CLI completed the wrong operation: {reply!r}")
    elif reply.get("status") == "failed":
        if reply.get("failed_step") != 0 or reply.get("completed") != 0 or results != [] or reply.get("operation") != operation or not isinstance(reply.get("error"), str) or not reply["error"]:
            raise RuntimeError(f"CLI failure does not identify {operation}: {reply!r}")
    else:
        raise RuntimeError(f"unexpected CLI status for {operation}: {reply!r}")
    return reply


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--inside", action="store_true")
    parser.add_argument("--output", type=Path, default=ROOT / "target/x11-utf8-proof")
    sender = parser.add_mutually_exclusive_group()
    sender.add_argument("--driver", type=Path, help="reuse an already compiled ignored test binary")
    sender.add_argument("--binary", type=Path, help="test an actual zuse CLI, including a release artifact")
    args = parser.parse_args()
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    if not args.inside:
        command = ["dbus-run-session", "--", sys.executable, str(Path(__file__).resolve()), "--inside", "--output", str(args.output)]
        if args.driver:
            command += ["--driver", str(args.driver.resolve())]
        if args.binary:
            command += ["--binary", str(args.binary.resolve())]
        return subprocess.call(command)
    report = {"cases": [], "payload": PAYLOAD, "transport": "local X11 + confirmed AT-SPI UTF-8", "sender": "cli-shell" if args.binary else "ignored-test-driver"}
    fixture = bus = registry = xvfb = None
    logs = []
    display_lease = None
    try:
        binary = args.binary.resolve() if args.binary else None
        if binary:
            report["binary"] = str(binary)
            report["binary_sha256"] = hashlib.sha256(binary.read_bytes()).hexdigest()
            report["cli_calls"] = []
        elif args.driver:
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
        if not binary:
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
        # An unspecified -displayfd starts searching at :0, which can occupy a
        # lazy host XWayland display. Use a high explicit number and -nopn so
        # partial socket binding cannot make a collision appear successful.
        requested_display, display_lease = reserve_display()
        auth_file = args.output / "Xauthority"
        auth_file.touch(mode=0o600, exist_ok=True)
        auth_file.chmod(0o600)
        authorized = subprocess.run(["xauth", "-f", str(auth_file), "add", ":" + requested_display, "MIT-MAGIC-COOKIE-1", secrets.token_hex(16)], capture_output=True, timeout=5)
        if authorized.returncode:
            raise RuntimeError("could not create the private Xvfb authorization file")
        xvfb = subprocess.Popen(["Xvfb", ":" + requested_display, "-displayfd", "1", "-screen", "0", "960x360x24", "-nolisten", "tcp", "-nopn", "-auth", str(auth_file)], stdout=subprocess.PIPE, stderr=log("xvfb.log"), bufsize=0)
        display_number = line(xvfb)
        if display_number != requested_display or xvfb.poll() is not None:
            raise RuntimeError("Xvfb did not allocate a private display")
        bus = subprocess.Popen(["dbus-daemon", "--config-file=/usr/share/defaults/at-spi2/accessibility.conf", "--nofork", "--print-address=1"], stdout=subprocess.PIPE, stderr=log("bus.log"), bufsize=0)
        address = line(bus)
        if not address.startswith("unix:"):
            raise RuntimeError("private accessibility bus did not start")
        env = dict(os.environ, DISPLAY=":" + display_number, XAUTHORITY=str(auth_file), ZUSE_BACKEND="x11", GDK_BACKEND="x11", NO_AT_BRIDGE="0", AT_SPI_BUS_ADDRESS=address, GIO_USE_VFS="local", ZUSE_X11_UTF8_FIXTURE="1")
        env.pop("WAYLAND_DISPLAY", None)
        subprocess.run(["xdpyinfo", "-display", env["DISPLAY"]], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, check=True, timeout=5)
        report["display_isolation"] = {"display": env["DISPLAY"], "high_number_lease": True, "private_authorization": True, "ready_matches_requested": True}
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
            if binary:
                return invoke_binary(value, action, window, pause_before)
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

        def invoke_binary(value, action, window, pause_before):
            quote = lambda text: json.dumps(text, ensure_ascii=False)
            if action in ("type", "background"):
                operation = "keyboard.type"
                code = f"keyboard.type(text={quote(value)}"
                if action == "background":
                    code += f",window={quote(window)}"
                code += ")"
            elif action == "key":
                operation = "keyboard.key"
                code = f"keyboard.key(key={quote(value)})"
            elif action == "num_lock":
                operation = "keyboard.num_lock"
                code = f"keyboard.num_lock(state={quote(value)})"
            elif action == "keyboard_state":
                operation = "keyboard.state"
                code = "keyboard.state()"
            else:
                raise RuntimeError(f"unknown CLI fixture action: {action}")
            if pause_before and action != "type":
                raise RuntimeError("the pre-input barrier only applies to foreground typing")
            record = {"action": action, "operation": operation, "pause_before": pause_before}
            report["cli_calls"].append(record)
            started = time.monotonic()
            child = subprocess.Popen([str(binary), "shell"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
            timer = None
            try:
                setup = [("system.capabilities", "system.capabilities()")]
                if action in ("type", "key"):
                    setup.append(("window.focus", f"window.focus(id={quote(window)})"))
                for setup_operation, source in setup:
                    child.stdin.write((source + "\n").encode())
                    child.stdin.flush()
                    reply = cli_reply(line(child), setup_operation)
                    if reply["status"] != "completed":
                        raise RuntimeError(f"CLI setup failed before input: {reply!r}")
                    if setup_operation == "system.capabilities":
                        capabilities = reply["results"][0]["value"]
                        record["backend"] = capabilities.get("desktop", {}).get("backend")
                        record["version"] = capabilities.get("version")
                        if record["backend"] != "x11":
                            raise RuntimeError(f"CLI fixture did not use X11: {record['backend']!r}")
                    else:
                        record["focus_reply"] = reply
                if pause_before:
                    # The shell is waiting for its next line after confirmed
                    # focus, with the same live Runtime/Desktop as the driver.
                    os.kill(fixture.pid, signal.SIGSTOP)
                    timer = threading.Timer(pause_before, lambda: os.kill(fixture.pid, signal.SIGCONT))
                    timer.start()
                out, err = child.communicate((code + "\n").encode(), timeout=10)
                record.update(exit_code=child.returncode, stdout=out.decode("utf-8", errors="replace"), stderr=err.decode("utf-8", errors="replace"))
                reply = cli_reply(out, operation)
                expected_exit = 0 if reply["status"] == "completed" else 1
                if child.returncode != expected_exit:
                    raise RuntimeError(f"CLI exit code disagrees with its result: {record!r}")
                result = "ok" if expected_exit == 0 else "error: " + reply["error"]
                if expected_exit == 0 and action in ("num_lock", "keyboard_state"):
                    result = json.dumps(reply["results"][0]["value"])
                return result, time.monotonic() - started
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
        reset()
        assert command("FOCUS_ON_CHANGE") == "OK"
        moved = check("focus changes after insertion report uncertainty", "你好🙂", "你好🙂", error=True)
        assert "not fully confirmed" in moved["result"] and "no input was sent" not in moved["result"], moved
        assert moved["observed"]["changes"] == 1, moved
        reset("左旧右")
        assert command("SELECT 1 2") == "OK"
        assert command("MUTATE_SELECTION") == "OK"
        rewritten = check("changed old selection is preserved", "🙂", "左🙂新右", error=True)
        assert "not fully confirmed" in rewritten["result"] and rewritten["observed"]["changes"] == 2, rewritten
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
            check("mapped ASCII uses confirmed insertion while preserving held Shift", "ASCII", "你好🙂ASCII")
        finally:
            assert command("SHIFT_UP") == "OK"

        def keyboard_result(value="", action="keyboard_state"):
            result, elapsed = invoke(value, action)
            assert not result.startswith("error:"), result
            status = json.loads(result)
            assert status["backend"] == "x11" and status["scope"] == "core_keyboard", status
            assert status["keys_held"] is False, status
            assert type(status["current"]["numLock"]) is bool, status
            assert type(status["current"]["capsLock"]) is bool, status
            return status, elapsed

        def keyboard_fields(snapshot):
            fields = tuple(map(int, snapshot["keyboard"].split(",")))
            assert len(fields) == 8, snapshot
            return fields

        reset()
        initial_status, _ = keyboard_result()
        initial_native = native()
        report["num_lock_text"] = {"initial": initial_status, "restored": False}
        num_mask = None
        try:
            if not initial_status["current"]["capsLock"]:
                result, _ = invoke("Caps_Lock", "key")
                assert result == "ok", result
            caps_status, _ = keyboard_result()
            assert caps_status["current"]["capsLock"] is True, caps_status
            for index, (action, expected) in enumerate([
                ("on", True), ("on", True), ("off", False),
                ("toggle", True), ("off", False),
            ]):
                before_status, _ = keyboard_result()
                before_native = native()
                before_text = state()
                result, elapsed = keyboard_result(action, "num_lock")
                readback, _ = keyboard_result()
                after_native = native()
                observed = state()
                receipt = {"name": f"Num Lock {action} step {index + 1}", "seconds": elapsed,
                           "result": result, "readback": readback,
                           "state_before": before_native, "state_after": after_native}
                report["cases"].append(receipt)
                assert result == readback, receipt
                assert readback["current"]["numLock"] is expected, receipt
                assert readback["current"]["capsLock"] is True, receipt
                assert observed["text"] == before_text["text"], receipt
                assert observed["other"] == "other window untouched", receipt
                assert observed["clipboard_clears"] == 0, receipt
                for field in ["mapping", "keys", "clipboard_owner"]:
                    assert before_native[field] == after_native[field], (field, receipt)
                before_fields, after_fields = keyboard_fields(before_native), keyboard_fields(after_native)
                # Groups, depressed and latched modifiers must survive. Only
                # the Num Lock bit may change in effective/locked modifiers.
                for field in [0, 1, 2, 3, 5, 6]:
                    assert before_fields[field] == after_fields[field], (field, receipt)
                changed = before_fields[7] ^ after_fields[7]
                if before_status["current"]["numLock"] is expected:
                    assert before_fields == after_fields, receipt
                else:
                    assert changed and changed & (changed - 1) == 0, receipt
                    if num_mask is None:
                        num_mask = changed
                    assert changed == num_mask, receipt
                    assert bool(after_fields[7] & num_mask) is expected, receipt
                    assert before_fields[4] ^ after_fields[4] == num_mask, receipt
                    assert after_fields[7] & ~num_mask, "Caps Lock must remain set"
                if index == 0:
                    check("ASCII receipt preserves Num Lock and Caps Lock", "ASCII 42! aAzZ", "ASCII 42! aAzZ")
                elif index == 1:
                    check("mixed UTF-8 receipt after idempotent Num Lock on", " 中文🙂 e\u0301", "ASCII 42! aAzZ 中文🙂 e\u0301")
                elif action == "toggle":
                    check("ASCII receipt after Num Lock toggle", " next 7!", "ASCII 42! aAzZ 中文🙂 e\u0301 next 7!")
            assert num_mask is not None, "fixture never observed a native Num Lock transition"
            report["num_lock_text"]["native_num_mask"] = num_mask
        finally:
            restored_status, _ = keyboard_result("on" if initial_status["current"]["numLock"] else "off", "num_lock")
            if restored_status["current"]["capsLock"] != initial_status["current"]["capsLock"]:
                result, _ = invoke("Caps_Lock", "key")
                assert result == "ok", result
            restored_status, _ = keyboard_result()
            restored_native = native()
            report["num_lock_text"].update(readback=restored_status, state_after=restored_native)
            assert restored_status == initial_status, report["num_lock_text"]
            for field in ["mapping", "keys", "keyboard", "clipboard_owner"]:
                assert restored_native[field] == initial_native[field], (field, report["num_lock_text"])
            report["num_lock_text"]["restored"] = True
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
        (args.output / "Xauthority").unlink(missing_ok=True)
        if display_lease is not None:
            display_lease.rmdir()
        (args.output / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
        print(json.dumps(report, ensure_ascii=False, indent=2), flush=True)


if __name__ == "__main__":
    raise SystemExit(main())
