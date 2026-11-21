#!/usr/bin/env python3
"""Run fixture input in an owned Hyprland inside a headless parent.

This runner never connects to the current desktop, reads its focus/pointer, or
restores host state. Cage/Weston uses only its headless backend; Hyprland connects
only to that owned socket. Both processes have host display/session variables
removed, and libseat is restricted to an absent private seatd socket. A private
D-Bus server has no service activation directories. Missing headless/GL/seat
support is a startup failure, never a fallback to the host desktop.

Primary interfaces:
  https://github.com/cage-kiosk/cage/blob/v0.3.1/cage.c
  https://gitlab.freedesktop.org/wlroots/wlroots/-/blob/0.20.0/docs/env_vars.md
  https://wayland.pages.freedesktop.org/weston/toc/running-weston.html
  https://github.com/hyprwm/Hyprland/blob/v0.57.2/src/Compositor.cpp
  https://github.com/hyprwm/aquamarine/blob/v0.16.0/src/backend/Wayland.cpp
  https://github.com/kennylevinsen/seatd/blob/master/libseat/libseat.c
"""
import argparse
import json
import os
from pathlib import Path
import resource
import select
import shlex
import shutil
import signal
import socket
import struct
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
CONFIG = """
hl.monitor({output="", mode="800x600@60", position="0x0", scale=1})
hl.config({
  input={kb_layout="us", kb_variant="", kb_options="", follow_mouse=0},
  general={gaps_in=0, gaps_out=0, border_size=0},
  decoration={rounding=0, blur={enabled=false}, shadow={enabled=false}},
  animations={enabled=false},
  misc={disable_hyprland_logo=true, disable_splash_rendering=true},
  xwayland={enabled=false},
  cursor={no_warps=true},
  debug={enable_stdout_logs=true}
})
"""
BUS_CONFIG = """<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen><auth>EXTERNAL</auth>
<policy context="default"><allow own="*"/><allow send_destination="*"/><allow receive_sender="*"/></policy>
</busconfig>
"""


def run(args, **kwargs):
    return subprocess.run(args, capture_output=True, text=True, timeout=20, **kwargs)


def terminate(process):
    if process is not None and process.poll() is None:
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=3)


def keyboard_response(process, expected):
    if not select.select([process.stdout], [], [], 3)[0]:
        raise RuntimeError("owned baseline keyboard response timed out")
    response = process.stdout.readline().strip()
    if response != expected:
        raise RuntimeError(f"owned baseline keyboard expected {expected!r}, received {response!r}")


def stop_keyboard(process):
    if process is None:
        return
    try:
        if process.poll() is None:
            for command in ("neutral", "quit"):
                process.stdin.write(command + "\n")
                process.stdin.flush()
                keyboard_response(process, "OK")
            process.wait(timeout=3)
    except (OSError, RuntimeError, subprocess.TimeoutExpired):
        # Teardown must still reach the owned compositors if the protocol
        # connection was lost or the fixture stopped acknowledging commands.
        pass
    finally:
        terminate(process)
        for stream in (process.stdin, process.stdout):
            try:
                stream.close()
            except OSError:
                pass


def private_environment(source, directory, runtime):
    """Construct the child environment without retaining a host connection."""
    env = source.copy()
    for name in (
        "DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET", "XAUTHORITY",
        "HYPRLAND_INSTANCE_SIGNATURE", "NOTIFY_SOCKET", "DBUS_SESSION_BUS_ADDRESS",
        "DBUS_SESSION_BUS_PID", "DBUS_STARTER_ADDRESS", "DBUS_STARTER_BUS_TYPE",
        "AT_SPI_BUS_ADDRESS", "GTK_IM_MODULE", "QT_IM_MODULE", "XMODIFIERS",
        "IBUS_ADDRESS", "XDG_SEAT", "XDG_SESSION_ID", "XDG_VTNR",
        "AQ_NO_KMS_REQUIREMENT", "AQ_BACKEND",
    ):
        env.pop(name, None)
    for name in tuple(env):
        if name.startswith("WLR_"):
            env.pop(name)
    env.update(
        XDG_RUNTIME_DIR=str(runtime),
        XDG_CONFIG_HOME=str(directory / "config"),
        XDG_CACHE_HOME=str(directory / "cache"),
        XDG_DATA_HOME=str(directory / "data"),
        XDG_CURRENT_DESKTOP="zuse-private-headless", XDG_SESSION_TYPE="wayland",
        HYPRLAND_NO_SD_VARS="1", HYPRLAND_NO_SD_NOTIFY="1", HYPRLAND_NO_RT="1",
        HYPRLAND_NO_CRASHREPORTER="1", AQ_DRM_DEVICES="/dev/null",
        # An explicit libseat backend never tries logind/builtin on failure.
        # Do not run a seatd process at this path: failure is intentional.
        LIBSEAT_BACKEND="seatd", SEATD_SOCK=str(runtime / "disabled-seatd.sock"),
        YDOTOOL_SOCKET=str(runtime / "disabled-ydotool.sock"),
        DBUS_SYSTEM_BUS_ADDRESS="unix:path=" + str(runtime / "disabled-system-bus.sock"),
        NO_AT_BRIDGE="1", GIO_USE_VFS="local", GSETTINGS_BACKEND="memory",
        GTK_A11Y="none", GTK_USE_PORTAL="0",
    )
    return env


def physical_device_fds(process):
    """Check owned children only. Render nodes are allowed; input/KMS are not."""
    if process.poll() is not None:
        raise RuntimeError(f"owned compositor {process.pid} exited during startup")
    unsafe = []
    for entry in Path(f"/proc/{process.pid}/fd").iterdir():
        try:
            target = os.readlink(entry)
        except FileNotFoundError:
            continue
        if target.startswith(("/dev/input/", "/dev/dri/card", "/dev/tty", "/dev/uinput")):
            unsafe.append(target)
    if unsafe:
        raise RuntimeError("headless compositor opened a physical input/KMS device: " + repr(sorted(set(unsafe))))
    return 0


def wait_socket(path, process, label, seconds=12):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"{label} exited before its private socket was ready")
        if path.is_socket():
            return
        time.sleep(.05)
    raise RuntimeError(f"{label} socket startup timed out")


def wait_parent_socket(runtime, process):
    """Cage chooses a socket name; this directory belongs only to this runner."""
    deadline = time.monotonic() + 12
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError("headless Cage exited before its private socket was ready")
        displays = [path for path in runtime.glob("wayland-*") if path.is_socket()]
        if len(displays) == 1:
            return displays[0]
        if len(displays) > 1:
            raise RuntimeError("headless parent sockets are ambiguous")
        time.sleep(.05)
    raise RuntimeError("headless Cage socket startup timed out")


def registry_versions(path, runtime):
    """Read registry announcements only, using the core Wayland wire protocol.

    No global is bound and no surface/input object is created. Connecting by an
    absolute path inside our owned runtime avoids environment-based selection.
    https://wayland.freedesktop.org/docs/html/ch04.html#sect-Protocol-Wire-Format
    """
    if not path.resolve().is_relative_to(runtime.resolve()) or not path.is_socket():
        raise RuntimeError("registry probe requires an owned private socket")

    def receive(connection, size):
        data = bytearray()
        while len(data) < size:
            part = connection.recv(size - len(data))
            if not part:
                raise RuntimeError("private registry connection closed early")
            data.extend(part)
        return data

    versions = {}
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(3)
        connection.connect(str(path))
        # wl_display.get_registry(new_id=2), then wl_display.sync(new_id=3).
        connection.sendall(struct.pack("=IIIIII", 1, (12 << 16) | 1, 2,
                                       1, 12 << 16, 3))
        for _ in range(4096):
            object_id, word = struct.unpack("=II", receive(connection, 8))
            size, opcode = word >> 16, word & 0xffff
            if size < 8 or size % 4:
                raise RuntimeError("invalid private registry message size")
            payload = receive(connection, size - 8)
            if object_id == 3 and opcode == 0:
                return versions
            if object_id == 1 and opcode == 0:
                raise RuntimeError("private display reported a registry protocol error")
            if object_id == 2 and opcode == 0:
                if len(payload) < 12:
                    raise RuntimeError("truncated private registry announcement")
                name_size = struct.unpack_from("=I", payload, 4)[0]
                version_offset = 8 + ((name_size + 3) & ~3)
                if not name_size or version_offset + 4 != len(payload) or payload[7 + name_size] != 0:
                    raise RuntimeError("invalid private registry interface name")
                name = payload[8:7 + name_size].decode("ascii")
                version = struct.unpack_from("=I", payload, version_offset)[0]
                # Aquamarine binds every matching global it sees. Retain the
                # lowest advertised version if a name occurs more than once.
                versions[name] = min(version, versions.get(name, version))
    raise RuntimeError("private registry produced too many messages")


def write_report(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--headless", action="store_true", help="required: use an owned headless parent")
    parser.add_argument("--binary", type=Path, default=ROOT / "target/native-validation/debug/zuse")
    parser.add_argument("--report", type=Path, default=ROOT / "target/native-wayland-private.json")
    parser.add_argument("--with-ime", action="store_true")
    parser.add_argument("--state-tests", action="store_true")
    parser.add_argument("--marker-tests", action="store_true")
    parser.add_argument("--hold-tests", action="store_true")
    parser.add_argument("--scale", type=float, choices=(1.0, 1.25), default=1.0,
                        help="owned monitor scale for coordinate/marker calibration")
    parser.add_argument("--ascii-background", action="store_true")
    parser.add_argument("--headless-parent", choices=("cage", "weston"), default="cage")
    parser.add_argument("--cage", default="cage", help="Cage parent executable; see build_private_cage.py for the v6 fixture")
    parser.add_argument("--weston", default="weston", help="Weston parent executable")
    parser.add_argument("--render-node", type=Path, help="optional Cage renderer node, for example /dev/dri/renderD129")
    parser.add_argument("--compositor", default="Hyprland", help="private child executable")
    args = parser.parse_args()
    if not args.headless:
        parser.error("--headless is required; nesting on the current desktop is disabled")
    # Startup failures should produce the bounded log report, not a core file.
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    binary = args.binary.resolve()
    if not binary.is_file():
        parser.error(f"build zuse first; missing {binary}")
    parent_executable = args.cage if args.headless_parent == "cage" else args.weston
    for executable in (parent_executable, args.compositor, "hyprctl", "dbus-daemon", "cc", "pkg-config"):
        if shutil.which(executable) is None:
            parser.error(f"headless fixture dependency is unavailable: {executable}")
    report_path = args.report.resolve()
    stage = "prepare-private-environment"
    isolation = {
        "mode": "owned-headless-" + args.headless_parent + "-and-private-hyprland",
        "host_display_connection": False,
        "host_focus_or_pointer_queries": False,
        "host_state_restoration": False,
        "seat_backend": "seatd-with-absent-private-socket",
        "production_pointer_buttons": "disabled-private-ydotool-socket",
        "requested_monitor_scale": args.scale,
    }
    write_report(report_path, {"native_wayland": "starting", "isolation": isolation})
    # Hyprland includes a 40-character revision plus timestamp/random suffix in
    # its IPC directory; a long TMPDIR or prefix can exceed sun_path[108].
    with tempfile.TemporaryDirectory(prefix="zw-", dir="/tmp") as temporary:
        directory = Path(temporary).resolve()
        runtime = directory / "r"
        longest_ipc = runtime / "hypr" / ("x" * 40 + "_" + "9" * 10 + "_" + "9" * 10) / ".socket2.sock"
        if len(os.fsencode(longest_ipc)) >= 108:
            raise RuntimeError("private runtime is too long for Hyprland IPC sockets")
        runtime.mkdir(mode=0o700)
        for name in ("config", "cache", "data"):
            (directory / name).mkdir(mode=0o700)
        config = directory / "hyprland.lua"
        config.write_text(CONFIG.replace("scale=1", f"scale={args.scale:g}", 1))
        bus_config = directory / "dbus.conf"
        bus_config.write_text(BUS_CONFIG.replace("unix:tmpdir=/tmp", "unix:path=" + str(runtime / "dbus.sock")))
        log_path = directory / "private.log"
        log = log_path.open("w")
        env = private_environment(os.environ, directory, runtime)
        assert not Path(env["SEATD_SOCK"]).exists()
        assert not Path(env["YDOTOOL_SOCKET"]).exists()
        bus = parent = child = ime = keyboard = None
        try:
            stage = "check-headless-parent-options"
            if args.headless_parent == "weston":
                help_result = run([args.weston, "--backend=headless", "--help"], env=env)
                help_text = help_result.stdout + help_result.stderr
                if help_result.returncode or "--renderer" not in help_text or "--fake-seat" not in help_text:
                    raise RuntimeError("installed Weston lacks required headless GL/fake-seat options; no host-desktop fallback is permitted")
            version = run([parent_executable, "-v" if args.headless_parent == "cage" else "--version"], env=env)
            version.check_returncode()
            isolation[args.headless_parent + "_version"] = version.stdout.strip()
            isolation["max_expected_ipc_path_bytes"] = len(os.fsencode(longest_ipc))

            stage = "start-private-dbus"
            bus = subprocess.Popen(
                ["dbus-daemon", "--config-file=" + str(bus_config), "--nofork", "--print-address=1"],
                env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                stderr=log, text=True, start_new_session=True,
            )
            if not select.select([bus.stdout], [], [], 3)[0]:
                raise RuntimeError("private D-Bus startup timed out")
            address = bus.stdout.readline().strip()
            if not address.startswith("unix:"):
                raise RuntimeError("private D-Bus did not report a Unix address")
            env["DBUS_SESSION_BUS_ADDRESS"] = address

            stage = "start-headless-" + args.headless_parent
            parent_env = env.copy()
            if args.headless_parent == "cage":
                parent_env.update(WLR_BACKENDS="headless", WLR_HEADLESS_OUTPUTS="1",
                                  WLR_RENDERER="gles2", WLR_DRM_DEVICES="/dev/null",
                                  WLR_XWAYLAND="/bin/false")
                if args.render_node:
                    node = args.render_node.resolve()
                    if node.parent != Path("/dev/dri") or not node.name.startswith("renderD") or not node.name[7:].isdigit() or not node.is_char_device():
                        raise RuntimeError("--render-node must be an existing /dev/dri/renderD<number> character device")
                    parent_env["WLR_RENDER_DRM_DEVICE"] = str(node)
                    isolation["render_node"] = str(node)
                # Cage v0.3.1 supports no primary application. Its own event
                # loop stays alive until our owned process group is stopped.
                parent_command = [args.cage, "-D"]
            else:
                parent_socket = runtime / "zuse-headless-parent"
                parent_command = [args.weston, "--backend=headless", "--renderer=gl", "--fake-seat",
                                  "--no-config", "--shell=kiosk-shell.so", "--idle-time=0",
                                  "--width=800", "--height=600", "--socket=" + parent_socket.name]
            parent = subprocess.Popen(
                parent_command, env=parent_env, stdin=subprocess.DEVNULL,
                stdout=log, stderr=log, start_new_session=True,
            )
            if args.headless_parent == "cage":
                parent_socket = wait_parent_socket(runtime, parent)
            else:
                wait_socket(parent_socket, parent, "headless Weston")
            isolation[args.headless_parent + "_pid"] = parent.pid
            physical_device_fds(parent)

            stage = "check-parent-protocols"
            globals_ = registry_versions(parent_socket, runtime)
            isolation["parent_protocol_versions"] = globals_
            # Aquamarine v0.16.0 binds these exact versions unconditionally.
            required = {"wl_compositor": 6, "wl_seat": 9, "xdg_wm_base": 6,
                        "zwp_linux_dmabuf_v1": 4, "wl_shm": 1}
            missing = [f"{name}: advertised {globals_.get(name, 0)}, required {version}"
                       for name, version in required.items() if globals_.get(name, 0) < version]
            if missing:
                raise RuntimeError("headless parent cannot host Aquamarine 0.16: " + "; ".join(missing))

            stage = "start-private-hyprland"
            env["WAYLAND_DISPLAY"] = str(parent_socket)
            child = subprocess.Popen(
                [args.compositor, "--config", str(config)], env=env,
                stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True,
            )
            deadline = time.monotonic() + 12
            while time.monotonic() < deadline:
                sockets = list((runtime / "hypr").glob("*/.socket.sock")) if (runtime / "hypr").is_dir() else []
                displays = [path for path in runtime.glob("wayland-*") if path.is_socket() and path != parent_socket]
                if sockets and displays:
                    break
                if child.poll() is not None:
                    raise RuntimeError("private Hyprland exited; the headless parent may lack required dmabuf/GL support")
                if parent.poll() is not None:
                    raise RuntimeError("headless parent exited while starting the private child")
                time.sleep(.05)
            else:
                raise RuntimeError("private compositor startup timed out")
            if len(sockets) != 1 or len(displays) != 1:
                raise RuntimeError("private compositor sockets are ambiguous")
            for path in (sockets[0], displays[0], parent_socket):
                if not path.resolve().is_relative_to(runtime.resolve()):
                    raise RuntimeError("compositor socket escaped the owned runtime directory")
            env["HYPRLAND_INSTANCE_SIGNATURE"] = sockets[0].parent.name
            env["WAYLAND_DISPLAY"] = displays[0].name
            env["ZUSE_BACKEND"] = "wayland"
            env["ZUSE_PRIVATE_WAYLAND_FIXTURE"] = "1"
            env["ZUSE_PRIVATE_WAYLAND_HEADLESS"] = "1"
            isolation["hyprland_pid"] = child.pid
            isolation["private_socket_checks"] = True
            isolation["physical_input_or_kms_fds"] = physical_device_fds(parent) + physical_device_fds(child)

            stage = "verify-private-compositor"
            ready = run(["hyprctl", "-j", "version"], env=env)
            ready.check_returncode()
            isolation["hyprland_version"] = json.loads(ready.stdout).get("tag")
            monitors = run(["hyprctl", "-j", "monitors"], env=env)
            monitors.check_returncode()
            monitor_rows = json.loads(monitors.stdout)
            if not monitor_rows:
                raise RuntimeError("private compositor has no virtual monitor")
            if any(abs(item.get("scale", 0) - args.scale) > 0.001 for item in monitor_rows):
                raise RuntimeError("private compositor did not apply the requested monitor scale")
            isolation["monitor_scales"] = [item["scale"] for item in monitor_rows]

            stage = "start-private-baseline-keyboard"
            # A headless parent advertises a seat without a physical keyboard.
            # Keep one owned, neutral virtual keyboard alive throughout the
            # GTK fixtures. The later state-test keyboard becomes the newest
            # main device, and returns to this baseline when it is destroyed.
            keyboard_binary = directory / "baseline-keyboard"
            flags = run(["pkg-config", "--cflags", "--libs", "wayland-client", "xkbcommon"], env=env)
            log.write(flags.stdout + flags.stderr)
            flags.check_returncode()
            compiled = run(["cc", str(ROOT / "tests/native_wayland_keyboard_fixture.c"),
                            "-o", str(keyboard_binary), *shlex.split(flags.stdout)], env=env)
            log.write(compiled.stdout + compiled.stderr)
            compiled.check_returncode()
            keyboard = subprocess.Popen(
                [str(keyboard_binary)], env=env, stdin=subprocess.PIPE,
                stdout=subprocess.PIPE, stderr=log, text=True, start_new_session=True,
            )
            keyboard_response(keyboard, "READY")
            devices = run(["hyprctl", "-j", "devices"], env=env)
            devices.check_returncode()
            keyboards = json.loads(devices.stdout).get("keyboards", [])
            main_keyboards = [item for item in keyboards if item.get("main") is True]
            if len(main_keyboards) != 1 or any(
                item.get("capsLock") is not False or item.get("numLock") is not False
                or item.get("active_layout_index") != 0 for item in keyboards
            ):
                raise RuntimeError("private baseline keyboard did not establish a unique neutral main device")
            isolation["baseline_keyboard_pid"] = keyboard.pid
            isolation["baseline_keyboard"] = "owned-persistent-virtual-us-de-neutral"
            isolation["baseline_keyboard_initial_count"] = len(keyboards)

            if args.with_ime:
                stage = "start-private-ime"
                ime = subprocess.Popen(
                    ["fcitx5", "--disable", "all", "--enable", "wayland,waylandim,keyboard,dbus,dbusfrontend"],
                    env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True,
                )
                time.sleep(.3)
                if ime.poll() is not None:
                    raise RuntimeError("private IME exited")
                env["ZUSE_TEST_EXPECT_FCITX"] = "1"

            stage = "run-private-fixtures"
            command = ["python3", str(ROOT / "tests/native_wayland.py"), "--allow-desktop",
                       "--binary", str(binary), "--report", str(report_path)]
            if args.ascii_background:
                command.append("--ascii-background")
            if args.state_tests:
                command.append("--state-tests")
            if args.marker_tests:
                command.append("--marker-tests")
            if args.hold_tests:
                command.append("--hold-tests")
            result = subprocess.run(command, env=env, stdin=subprocess.DEVNULL, timeout=90)
            if keyboard.poll() is not None:
                raise RuntimeError("owned baseline keyboard exited during the native fixtures")
            isolation["physical_input_or_kms_fds"] = physical_device_fds(parent) + physical_device_fds(child)
            report = json.loads(report_path.read_text())
            report["isolation"] = isolation
            if report.get("native_wayland") == "starting":
                report.update(native_wayland="blocked", stage=stage,
                              error="fixture runner returned without a completed report")
            write_report(report_path, report)
            return result.returncode
        except Exception as error:
            log.flush()
            tail = log_path.read_text(errors="replace")[-6000:]
            write_report(report_path, {"native_wayland": "blocked", "stage": stage,
                                      "error": str(error), "isolation": isolation, "private_log_tail": tail})
            raise RuntimeError(f"{stage}: {error}; private log: {tail}") from error
        finally:
            terminate(ime)
            stop_keyboard(keyboard)
            terminate(child)
            terminate(parent)
            terminate(bus)
            log.close()


if __name__ == "__main__":
    raise SystemExit(main())
