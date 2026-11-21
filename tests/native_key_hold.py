"""Bounded hold checks for the private native Wayland fixture runner.

This module only accepts the runner's owned compositor and fixture windows.
All timing and release claims come from GTK receiver events and compositor state.
"""
import json
from pathlib import Path
import select
import shlex
import signal
import subprocess
import time


def run_private_holds(binary, env, directory, first, second, focus, compositor, private_held):
    if env.get("ZUSE_PRIVATE_WAYLAND_FIXTURE") != "1":
        raise RuntimeError("key hold tests require the owned private compositor")
    directory = Path(directory)
    root = Path(__file__).resolve().parents[1]

    def events(index=0):
        values = []
        for line in (directory / f"{index}.log").read_text().splitlines():
            if line.startswith("HELD "):
                _, action, key, code, state, micros = line.split()
                values.append(dict(action=action, key=int(key), code=int(code), state=int(state), micros=int(micros)))
        return values

    def invoke(code):
        result = subprocess.run([str(binary), "eval", code], env=env, capture_output=True, text=True, timeout=10)
        receipt = json.loads(result.stdout)
        if result.returncode:
            raise RuntimeError(receipt)
        return receipt

    def wait_for(predicate, timeout=3):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            value = predicate()
            if value:
                return value
            time.sleep(.01)
        raise AssertionError("fixture event deadline expired")

    def held_up_since(before):
        return any(event["action"] == "UP" for event in events()[before:])

    def held_case(key, symbol, stop=None):
        focus(first)
        before = len(events())
        duration = 5000 if stop else 180
        process = subprocess.Popen([str(binary), "eval", f'keyboard.hold(key={json.dumps(key)},duration={duration},window={json.dumps(first)})'],
                                   env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            wait_for(lambda: any(e["action"] == "DOWN" and e["key"] == symbol for e in events()[before:]))
            if stop:
                time.sleep(.06)
                if stop == "cancel":
                    process.send_signal(signal.SIGINT)
                elif stop == "focus":
                    focus(second)
            stdout, stderr = process.communicate(timeout=5)
            receipt = json.loads(stdout)
            wait_for(lambda: not private_held())
            received = events()[before:]
            downs = [e for e in received if e["action"] == "DOWN" and e["key"] == symbol]
            assert downs, received
            if stop != "focus":
                wait_for(lambda: held_up_since(before))
                received = events()[before:]
                ups = [e for e in received if e["action"] == "UP" and e["key"] == symbol]
                assert len(ups) == 1, received
                elapsed = (ups[0]["micros"] - downs[0]["micros"]) / 1000
                if not stop:
                    assert 140 <= elapsed <= 800, (elapsed, received)
            else:
                elapsed = None
            if stop:
                assert process.returncode != 0 and receipt["status"] == "failed", receipt
                assert receipt["error_code"] == ("cancelled" if stop == "cancel" else "stale_target"), receipt
            else:
                assert process.returncode == 0 and receipt["status"] == "completed", (key, stop, receipt, stderr)
                assert receipt["results"][0]["value"]["released"] is True, receipt
            return {"key": key, "receiver_events": received, "receiver_hold_ms": elapsed,
                    "compositor_keys_released": True, "receipt": receipt}
        finally:
            if process.poll() is None:
                process.send_signal(signal.SIGINT)
                try:
                    process.communicate(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.communicate(timeout=3)

    results = {"duration": held_case("ArrowRight", 0xff53),
               "cancellation": held_case("Up", 0xff52, "cancel"),
               "focus_change": held_case("Left", 0xff51, "focus")}

    # Retain one original keyboard while exercising each lock state, then
    # verify its device state and the keypad's actual delivered interpretation.
    helper_path = directory / "hold-keyboard-fixture"
    flags = shlex.split(subprocess.check_output(["pkg-config", "--cflags", "--libs", "wayland-client", "xkbcommon"], text=True))
    subprocess.run(["cc", str(root / "tests/native_wayland_keyboard_fixture.c"), "-o", str(helper_path), *flags], check=True)
    helper = subprocess.Popen([str(helper_path)], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    def response(expected):
        if not select.select([helper.stdout], [], [], 3)[0]:
            raise RuntimeError("owned keyboard fixture did not respond")
        assert helper.stdout.readline().strip() == expected
    def send(line):
        helper.stdin.write(line + "\n")
        helper.stdin.flush()
        response("OK")
    def device_states():
        return sorted((k["address"], k["main"], k["capsLock"], k["numLock"], k["active_layout_index"])
                      for k in compositor("devices")["keyboards"])

    def concurrent_game_keys():
        """The second player's input must leave the first player's key down."""
        focus(first)
        send("neutral")
        send("down 25")  # The fixture's own W key, not the host keyboard.
        wait_for(lambda: set(private_held()) == {25})
        baseline_devices = device_states()
        evidence = {}

        def submit(key, allow):
            option = ",allow_other_keys=true" if allow else ""
            source = (f'keyboard.hold(key={json.dumps(key)},duration=180,'
                      f'window={json.dumps(first)}{option})')
            result = subprocess.run([str(binary), "script", "-"], input=source,
                                    env=env, capture_output=True, text=True, timeout=5)
            assert result.stdout.strip(), (result.returncode, result.stderr)
            return result, json.loads(result.stdout)

        try:
            for key, symbol in (("Right", 0xff53), ("slash", 0x2f),
                                ("period", 0x2e), ("m", 0x6d), ("n", 0x6e)):
                before = len(events())
                result, receipt = submit(key, True)
                assert result.returncode == 0 and receipt["status"] == "completed", (key, "concurrent_game", receipt, result.stderr)
                wait_for(lambda: set(private_held()) == {25})
                wait_for(lambda: any(e["action"] == "UP" and e["key"] == symbol
                                    for e in events()[before:]))
                received = events()[before:]
                downs = [e for e in received if e["action"] == "DOWN" and e["key"] == symbol]
                ups = [e for e in received if e["action"] == "UP" and e["key"] == symbol]
                assert downs and len(ups) == 1, received
                assert not any(e["action"] == "UP" and e["code"] == 25 for e in received), received
                assert device_states() == baseline_devices
                elapsed = (ups[0]["micros"] - downs[0]["micros"]) / 1000
                assert 140 <= elapsed <= 800, (key, elapsed, received)
                evidence[key] = {"receiver_hold_ms": elapsed,
                                 "other_player_key_still_down": True,
                                 "receiver_events": received, "receipt": receipt}

            # The default remains exclusive; opting in does not permit a
            # modifier chord or taking ownership of an already-held target.
            for name, extra in (("default_exclusive", None),
                                ("modifier_rejected", 50),  # Shift_L
                                ("target_already_held", 114)):  # Right
                if extra is not None:
                    send(f"down {extra}")
                expected = {25} | ({extra} if extra is not None else set())
                try:
                    wait_for(lambda: set(private_held()) == expected)
                    before = len(events())
                    result, receipt = submit("Right", name != "default_exclusive")
                    assert result.returncode != 0 and receipt["status"] == "failed", receipt
                    assert set(private_held()) == expected
                    received = events()[before:]
                    assert not any(e["action"] == "UP" and e["code"] in expected for e in received), received
                    if extra != 114:
                        assert not any(e["action"] == "DOWN" and e["key"] == 0xff53 for e in received), received
                    evidence[name] = {"held_keys_preserved": sorted(expected), "receipt": receipt}
                finally:
                    if extra is not None:
                        send(f"up {extra}")
            return evidence
        finally:
            send("up 25")
            wait_for(lambda: not private_held())

    try:
        response("READY")
        results["concurrent_game_keys"] = concurrent_game_keys()
        for mode in ("neutral", "num"):
            send(mode)
            before_devices = device_states()
            results[mode] = held_case("Down", 0xff54)
            assert device_states() == before_devices
            focus(first)
            before = len(events())
            send("press 87")
            expected = 0xffb1 if mode == "num" else 0xff9c
            wait_for(lambda: any(e["action"] == "DOWN" and e["key"] == expected for e in events()[before:]))
            results[mode]["original_keypad_symbol"] = expected
            results[mode]["original_keyboard_states_unchanged"] = True
        send("neutral")
    finally:
        if helper.poll() is None:
            try:
                send("neutral")
                send("quit")
            finally:
                try:
                    helper.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    helper.kill()
                    helper.wait(timeout=3)
        for stream in (helper.stdin, helper.stdout, helper.stderr):
            stream.close()
    return results
