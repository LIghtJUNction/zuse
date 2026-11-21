"""Run the CLI against a private Hyprland IPC stub. No Wayland display is opened."""
import argparse
from contextlib import ExitStack
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import time

# Exercise the actual Lua predicates and edge commands sent by the Rust backend.
LUA_FIXTURE = """
local window = {address="0xcafe", stable_id=arg[1], visible=true, accepts_input=true}
hl = {
    get_active_window=function() return window end,
    is_key_down=function() return false end,
    dispatch=function(action) return action() end,
    dsp={}
}
if arg[2] ~= "unsupported" then
    hl.dsp.send_key_state=function(edge)
        return function()
            assert(edge.window == "address:" .. window.address)
            assert(edge.key == "Return" and edge.mods == "CTRL")
            assert(edge.state == "up" or edge.state == "down")
            print("edge " .. edge.state .. " " .. window.stable_id)
            return {ok=true}
        end
    end
end
print(tostring(assert(load(io.read("*a")))()))
"""


def run_case(binary, lua, case, expected_edges, expected_error):
    with tempfile.TemporaryDirectory(prefix="zuse-hold-") as temp:
        root = Path(temp)
        script = root / "fixture.lua"
        script.write_text(LUA_FIXTURE)
        socket_dir = root / "hypr" / "fixture"
        socket_dir.mkdir(parents=True)
        env = dict(os.environ, ZUSE_BACKEND="wayland", XDG_RUNTIME_DIR=temp,
                   HYPRLAND_INSTANCE_SIGNATURE="fixture", WAYLAND_DISPLAY="fixture-only")
        env.pop("DISPLAY", None)
        env.pop("DBUS_SESSION_BUS_ADDRESS", None)
        duration = 80 if case == "normal" else 30000
        window = "0xdead" if case == "wrong-window" else "active"
        command = f'keyboard.hold(key="ctrl+Enter", duration={duration}, window="{window}"); timer.start()'
        with socket.socket(socket.AF_UNIX) as listener, ExitStack() as queued:
            listener.bind(str(socket_dir / ".socket.sock"))
            listener.listen(1)
            listener.settimeout(.05)
            proc = subprocess.Popen([binary, "eval", command], stdin=subprocess.DEVNULL,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
            stable = "abc"
            edges = []
            times = []
            deadline = time.monotonic() + 4
            try:
                while proc.poll() is None:
                    if time.monotonic() >= deadline:
                        raise TimeoutError(case)
                    if case == "full-queue" and edges:
                        time.sleep(.01)
                        continue
                    try:
                        connection, _ = listener.accept()
                    except TimeoutError:
                        continue
                    with connection:
                        connection.settimeout(1)
                        request = connection.recv(8192).decode()
                        if request == "j/version":
                            reply = '{"tag":"v0.57.2"}'
                        elif request == "j/devices":
                            reply = json.dumps({"keyboards": [{"main": True, "capsLock": False,
                                "numLock": case == "lock-on", "active_layout_index": 0}]})
                        elif request == "j/activewindow":
                            reply = json.dumps({"address": "0xcafe", "stableId": stable})
                        else:
                            assert request.startswith("/repl "), request
                            code = request.removeprefix("/repl ")
                            result = subprocess.run([lua, script, stable, case], input=code,
                                capture_output=True, text=True, check=True, timeout=1)
                            *events, reply = result.stdout.splitlines()
                            for event in events:
                                _, edge, recipient = event.split()
                                assert recipient == "abc", (case, event)
                                edges.append(edge)
                                times.append(time.monotonic())
                                if edge == "down":
                                    if case == "cancel":
                                        proc.send_signal(signal.SIGINT)
                                    elif case == "replace":
                                        stable = "def"
                                    elif case == "lost-ack":
                                        reply = ""
                                    elif case == "full-queue":
                                        # Linux allows backlog + 1 queued connections.
                                        # Keep both open without accepting them.
                                        for _ in range(2):
                                            sock = queued.enter_context(socket.socket(socket.AF_UNIX))
                                            sock.setblocking(False)
                                            sock.connect(str(socket_dir / ".socket.sock"))
                            if case == "before-down" and reply == "1" and not edges:
                                stable = "def"
                        connection.sendall(reply.encode())
                out, err = proc.communicate(timeout=1)
            finally:
                if proc.poll() is None:
                    proc.kill()
                    proc.wait()
        receipt = json.loads(out)
        assert edges == expected_edges, (case, edges, receipt)
        assert receipt.get("error_code") == expected_error, (case, receipt, err)
        assert proc.returncode == (1 if expected_error else 0), (case, receipt)
        assert receipt["completed"] == (0 if expected_error else 2), (case, receipt)
        if case == "full-queue":
            assert "release unconfirmed" in receipt["error"], receipt
        if case == "normal":
            assert times[1] - times[0] >= .078, times
        print(f"{case}: {edges}, {expected_error or 'completed'}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    args = parser.parse_args()
    lua = shutil.which("lua") or shutil.which("lua5.4") or shutil.which("texlua")
    if not lua:
        parser.error("the IPC fixture needs Lua 5.3+, lua5.4 or texlua")
    for case, events, error in [
        ("normal", ["down", "up"], None),
        ("cancel", ["down", "up"], "cancelled"),
        ("lost-ack", ["down", "up"], "execution_error"),
        ("full-queue", ["down"], "execution_error"),
        ("replace", ["down"], "stale_target"),
        ("before-down", [], "stale_target"),
        ("wrong-window", [], "stale_target"),
        ("lock-on", [], "unsupported_state"),
        ("unsupported", [], "unsupported_state"),
    ]:
        run_case(args.binary.resolve(), lua, case, events, error)


if __name__ == "__main__":
    main()
