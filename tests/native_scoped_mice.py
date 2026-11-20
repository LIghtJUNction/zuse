#!/usr/bin/env python3
"""CLI pointer lifetimes on an authenticated, privately owned X11 display."""

import argparse
import ctypes as C
import ctypes.util
import json
import os
from pathlib import Path
import re
import select
import shutil
import signal
import subprocess
import tempfile
import time

from private_xvfb import private_xvfb_command, release_private_xvfb, verify_private_xvfb


class Device(C.Structure):
    _fields_ = [("id", C.c_int), ("name", C.c_char_p), ("use", C.c_int),
                ("attachment", C.c_int), ("enabled", C.c_int), ("classes_count", C.c_int),
                ("classes", C.POINTER(C.c_void_p))]


class RemoveMaster(C.Structure):
    _fields_ = [(name, C.c_int) for name in
                ("type", "device", "return_mode", "return_pointer", "return_keyboard")]


class Buttons(C.Structure):
    _fields_ = [("length", C.c_int), ("mask", C.POINTER(C.c_ubyte))]


class State(C.Structure):
    _fields_ = [(name, C.c_int) for name in ("base", "latched", "locked", "effective")]


class XError(C.Structure):
    _fields_ = [("type", C.c_int), ("display", C.c_void_p), ("resource", C.c_ulong),
                ("serial", C.c_ulong), ("code", C.c_ubyte), ("request", C.c_ubyte),
                ("minor", C.c_ubyte)]


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def bind(library, name, result, arguments):
    function = getattr(library, name)
    function.restype, function.argtypes = result, arguments
    return function


class Display:
    def __init__(self, name):
        self.x = C.CDLL(ctypes.util.find_library("X11") or "libX11.so.6")
        self.xi = C.CDLL(ctypes.util.find_library("Xi") or "libXi.so.6")
        self.xt = C.CDLL(ctypes.util.find_library("Xtst") or "libXtst.so.6")
        p, i, w, q = C.c_void_p, C.c_int, C.c_ulong, C.POINTER
        for name_, result, args in [
            ("XOpenDisplay", p, [C.c_char_p]), ("XCloseDisplay", i, [p]),
            ("XDefaultRootWindow", w, [p]), ("XSync", i, [p, i]),
            ("XFree", i, [p]), ("XGrabServer", i, [p]), ("XUngrabServer", i, [p]),
            ("XSetErrorHandler", p, [p]),
            ("XWarpPointer", i, [p, w, w, i, i, C.c_uint, C.c_uint, i, i]),
            ("XQueryPointer", i, [p, w, q(w), q(w), q(i), q(i), q(i), q(i), q(C.c_uint)]),
        ]:
            bind(self.x, name_, result, args)
        bind(self.xi, "XIQueryVersion", i, [p, q(i), q(i)])
        bind(self.xi, "XIQueryDevice", q(Device), [p, i, q(i)])
        bind(self.xi, "XIFreeDeviceInfo", None, [q(Device)])
        bind(self.xi, "XIChangeHierarchy", i, [p, q(RemoveMaster), i])
        bind(self.xi, "XIQueryPointer", i, [
            p, i, w, q(w), q(w), q(C.c_double), q(C.c_double), q(C.c_double),
            q(C.c_double), q(Buttons), q(State), q(State),
        ])
        bind(self.xt, "XTestFakeButtonEvent", i, [p, C.c_uint, i, w])
        self.handle = self.x.XOpenDisplay(name.encode("ascii"))
        require(self.handle, "cannot open the authorized private X display")
        self.root = self.x.XDefaultRootWindow(self.handle)
        self.errors = []
        handler = C.CFUNCTYPE(i, p, q(XError))
        self.handler = handler(lambda _display, error: self.errors.append(error.contents.code) or 0)
        self.previous = self.x.XSetErrorHandler(C.cast(self.handler, p))
        try:
            major, minor = i(2), i(0)
            require(self.xi.XIQueryVersion(self.handle, C.byref(major), C.byref(minor)) == 0,
                    "private X display requires XInput 2")
            self.sync()
        except BaseException:
            self.close()
            raise

    def sync(self):
        self.x.XSync(self.handle, 0)
        if self.errors:
            codes, self.errors = self.errors, []
            raise RuntimeError(f"private X11 request failed: {codes}")

    def devices(self):
        count = C.c_int()
        devices = self.xi.XIQueryDevice(self.handle, 0, C.byref(count))
        try:
            self.sync()
            require(devices and 0 <= count.value <= 4096, "invalid XInput device inventory")
            return {devices[n].id: (devices[n].name.decode("utf-8", "replace"),
                                   devices[n].use, devices[n].attachment)
                    for n in range(count.value)}
        finally:
            if devices:
                self.xi.XIFreeDeviceInfo(devices)

    def core(self):
        root, child, mask = C.c_ulong(), C.c_ulong(), C.c_uint()
        x, y, wx, wy = (C.c_int() for _ in range(4))
        result = self.x.XQueryPointer(self.handle, self.root, C.byref(root), C.byref(child),
                                     C.byref(x), C.byref(y), C.byref(wx), C.byref(wy), C.byref(mask))
        self.sync()
        require(result, "private core pointer is on another X11 screen")
        return x.value, y.value, mask.value & 0x1F00

    def warp(self, point):
        self.x.XWarpPointer(self.handle, 0, self.root, 0, 0, 0, 0, *point)
        self.sync()

    def left_down(self, device):
        root, child = C.c_ulong(), C.c_ulong()
        rx, ry, wx, wy = (C.c_double() for _ in range(4))
        buttons, mods, group = Buttons(), State(), State()
        try:
            result = self.xi.XIQueryPointer(
                self.handle, device, self.root, C.byref(root), C.byref(child),
                C.byref(rx), C.byref(ry), C.byref(wx), C.byref(wy),
                C.byref(buttons), C.byref(mods), C.byref(group))
            self.sync()
            require(result and 0 <= buttons.length <= 4096, "cannot read test pointer buttons")
            return bool(buttons.length and buttons.mask and buttons.mask[0] & 2)
        finally:
            if buttons.mask:
                self.x.XFree(buttons.mask)

    def remove_owned(self, processes, baseline):
        # A failed/forcibly killed CLI can leave server-global master pairs.
        # Only remove names belonging to our exact child PIDs, never baseline devices.
        prefixes = tuple(f"zuse-{process.pid}-" for process in processes)
        self.x.XGrabServer(self.handle)
        try:
            for device, info in self.devices().items():
                if device not in baseline and info[1] == 1 and info[0].startswith(prefixes):
                    removal = RemoveMaster(2, device, 2, 0, 0)
                    require(self.xi.XIChangeHierarchy(self.handle, C.byref(removal), 1) == 0,
                            "could not remove a test-owned leaked pointer")
                    self.sync()
        finally:
            self.x.XUngrabServer(self.handle)
            self.sync()

    def restore(self, point):
        # Tests begin with no held core buttons. Only our test's left button can
        # need fallback release if a broken binary redirected independent input.
        if self.core()[2] & 0x100:
            self.xt.XTestFakeButtonEvent(self.handle, 1, 0, 0)
            self.sync()
        self.warp(point)

    def close(self):
        if self.handle:
            self.x.XCloseDisplay(self.handle)
            self.x.XSetErrorHandler(self.previous)
            self.handle = None


def stop(process):
    if process is not None and process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)


def receipt(process, status):
    output, error = process.communicate(timeout=8)
    expected = 0 if status == "completed" else 1
    require(process.returncode == expected,
            f"CLI exited {process.returncode}, expected {expected}: {error[:1000]} {output[:1000]}")
    try:
        value = json.loads(output)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"CLI did not return one JSON receipt: {output[:1000]}") from error
    require(value.get("status") == status, f"unexpected CLI receipt: {value}")
    return value


def await_master(display, process, held=False):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        require(process.poll() is None, "CLI exited before its independent mouse became ready")
        owned = [device for device, info in display.devices().items()
                 if info[1] == 1 and info[0].startswith(f"zuse-{process.pid}-")]
        if len(owned) == 1 and (not held or display.left_down(owned[0])):
            return owned[0]
        time.sleep(0.01)
    raise RuntimeError("CLI did not create its independent mouse / hold left within 5 seconds")


def check_scripts(binary, directory, environment):
    display = Display(environment["DISPLAY"])
    processes, baseline, original = [], {}, None
    failure, cleanup_errors, report = None, [], {}

    def launch(command, source):
        if command == "script":
            path = directory / f"program-{len(processes)}.zuse"
            path.write_text(source, encoding="utf-8")
            arguments = [str(binary), "script", str(path)]
        else:
            arguments = [str(binary), "eval", source]
        process = subprocess.Popen(arguments, env=environment, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, text=True)
        processes.append(process)
        return process

    def unchanged(point):
        require(display.devices() == baseline, "CLI left XInput devices behind or changed core devices")
        require(display.core() == (*point, 0), "independent CLI changed the core pointer/buttons")

    try:
        baseline, original = display.devices(), display.core()
        require(original[2] == 0, "private test display must start without held core buttons")
        display.warp((67, 89))
        core = display.core()[:2]
        normal = receipt(launch("script", """
            let p=mouse(name="cli-normal");
            p.move(x=113,y=149,duration=0);
            p.position();
        """), "completed")
        events = normal.get("events", [])
        require([event["event"] for event in events].count("create") == 1, "normal CLI did not create a mouse")
        require(events[-1]["event"] == "close" and events[-1]["detail"]["released"],
                "normal CLI did not close its mouse")
        unchanged(core)
        report["script_normal_exit"] = "pass"

        for name, number in [("sigint", signal.SIGINT), ("sigterm", signal.SIGTERM)]:
            process = launch("script", f"""
                let p=mouse(name="cli-{name}");
                p.down();
                wait(ms=30000);
            """)
            await_master(display, process, held=True)
            started = time.monotonic()
            process.send_signal(number)
            result = receipt(process, "failed")
            require("cancel" in result.get("error", "").lower(), f"{name} did not report cancellation")
            events = result.get("events", [])
            require(any(event["event"] == "down" for event in events), f"{name} arrived before down")
            require(events[-1]["event"] == "close" and events[-1]["detail"]["released"],
                    f"{name} did not close its mouse")
            unchanged(core)
            report[f"script_{name}"] = {"status": "pass", "exit_code": process.returncode,
                                        "cancel_ms": round((time.monotonic() - started) * 1000)}

        process = launch("eval", "mouse.move(x=187,y=143,duration=0); mouse.position(); wait(ms=500);")
        await_master(display, process)
        require(display.core() == (*core, 0), "legacy default moved the core pointer while alive")
        result = receipt(process, "completed")
        positions = [step["value"] for step in result["results"] if step["operation"] == "mouse.position"]
        require(positions == [{"x": 187, "y": 143}], "legacy independent position was incorrect")
        unchanged(core)
        report["legacy_default_independent"] = "pass"

        result = receipt(launch("eval", """
            mouse.use(kind="user",appearance="system");
            mouse.move(x=239,y=181,duration=0);
            mouse.position();
            mouse.release();
        """), "completed")
        positions = [step["value"] for step in result["results"] if step["operation"] == "mouse.position"]
        require(positions == [{"x": 239, "y": 181}], "explicit user position was incorrect")
        unchanged((239, 181))
        report["legacy_explicit_user_release"] = "pass"
        report["baseline_devices"] = len(baseline)
    except BaseException as error:
        failure = error
    finally:
        for process in processes:
            try:
                stop(process)
                for stream in (process.stdout, process.stderr):
                    if stream:
                        stream.close()
            except Exception as error:
                cleanup_errors.append(str(error))
        cleanups = [lambda: display.remove_owned(processes, baseline)]
        if original is not None and original[2] == 0:
            cleanups.append(lambda: display.restore(original[:2]))
        cleanups.append(display.close)
        for cleanup in cleanups:
            try:
                cleanup()
            except Exception as error:
                cleanup_errors.append(str(error))
    if failure or cleanup_errors:
        raise RuntimeError("; ".join([str(failure)] if failure else []) +
                           ("; cleanup: " + "; ".join(cleanup_errors) if cleanup_errors else ""))
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True, help="built zuse CLI")
    parser.add_argument("--inside", action="store_true", help="reuse an explicitly marked private X11 display")
    args = parser.parse_args()
    binary = args.binary.resolve()
    require(binary.is_file() and os.access(binary, os.X_OK), "--binary must be an executable zuse file")
    if args.inside:
        match = re.fullmatch(r":([0-9]+)(?:\.[0-9]+)?", os.environ.get("DISPLAY", ""))
        require(os.environ.get("ZUSE_TEST_PRIVATE_X11") == "1" and match and int(match[1]) > 0,
                "--inside requires ZUSE_TEST_PRIVATE_X11=1 and a nonzero local private DISPLAY")
    else:
        for tool in ("Xvfb", "xauth", "xdpyinfo"):
            require(shutil.which(tool), f"required private-display tool is missing: {tool}")
    with tempfile.TemporaryDirectory(prefix="zuse-scoped-cli-") as directory:
        directory = Path(directory)
        server, lease, auth = None, None, directory / "Xauthority"
        previous_authority = os.environ.get("XAUTHORITY")
        try:
            environment = dict(os.environ, ZUSE_BACKEND="x11", XDG_SESSION_TYPE="x11")
            environment.pop("WAYLAND_DISPLAY", None)
            if not args.inside:
                command, number, lease = private_xvfb_command("640x480x24", auth)
                server = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                require(select.select([server.stdout], [], [], 5)[0], "private Xvfb startup timed out")
                display = verify_private_xvfb(server, number, server.stdout.readline(), auth)
                environment.update(DISPLAY=display, XAUTHORITY=str(auth), ZUSE_TEST_PRIVATE_X11="1")
                # XOpenDisplay in this helper reads the same private authority as the CLI.
                os.environ["XAUTHORITY"] = str(auth)
            report = check_scripts(binary, directory, environment)
            return {"native_scoped_mice": "pass", **report}
        finally:
            stop(server)
            if server is not None:
                for stream in (server.stdout, server.stderr):
                    if stream:
                        stream.close()
            release_private_xvfb(lease, auth)
            if previous_authority is None:
                os.environ.pop("XAUTHORITY", None)
            else:
                os.environ["XAUTHORITY"] = previous_authority


if __name__ == "__main__":
    try:
        print(json.dumps(main(), sort_keys=True))
    except (Exception, KeyboardInterrupt) as error:
        print(json.dumps({"native_scoped_mice": "fail", "error": str(error)}, sort_keys=True))
        raise SystemExit(1)
