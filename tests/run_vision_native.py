#!/usr/bin/env python3
"""Run native X11/vision regressions only on an owned authenticated Xvfb.

Requires installed Xvfb, xauth, xdpyinfo, cc, pkg-config and GTK 3/X11 headers.
No window manager or desktop session bus is used. Nothing is installed.
The caller must hold the project's Cargo lock when using the Cargo path.
Pass --test-binary and --zuse-binary to run immutable prebuilt executables.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import select
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time

from private_xvfb import private_xvfb_command, release_private_xvfb, verify_private_xvfb

ROOT = Path(__file__).resolve().parents[1]
CORE_TESTS = (
    "native_x11_window_hit_test_respects_invisible_input_cover_without_moving_pointer_or_focus",
    "native_x11_num_lock_cli_changes_only_the_requested_bit_and_reads_back",
)
VISION_TESTS = (
    "native_cli_scaled_template_rejects_ambiguity_geometry_changes_and_input_cover",
    "native_cli_timeline_returns_first_appearance_then_keeps_two_episodes_until_explicit_svg_render",
)


def terminate_owned(process):
    if process is None or process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
        process.wait(timeout=3)
    except ProcessLookupError:
        pass
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=3)


def isolated_environment(workspace, auth):
    env = os.environ.copy()
    for name in (
        "DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET", "HYPRLAND_INSTANCE_SIGNATURE",
        "XAUTHORITY", "DBUS_SESSION_BUS_ADDRESS", "DBUS_SESSION_BUS_PID",
        "DBUS_STARTER_ADDRESS", "DBUS_STARTER_BUS_TYPE", "AT_SPI_BUS_ADDRESS",
        "GTK_IM_MODULE", "QT_IM_MODULE", "XMODIFIERS", "IBUS_ADDRESS",
        "SESSION_MANAGER", "NOTIFY_SOCKET", "GDK_SCALE", "GDK_DPI_SCALE",
    ):
        env.pop(name, None)
    runtime = workspace / "runtime"
    runtime.mkdir(mode=0o700)
    env.update(
        XDG_RUNTIME_DIR=str(runtime), XAUTHORITY=str(auth),
        GDK_BACKEND="x11", ZUSE_BACKEND="x11", GDK_SCALE="1", GDK_DPI_SCALE="1",
        ZUSE_PRIVATE_X11_FIXTURE="1", ZUSE_VISION_NATIVE_WORKSPACE=str(workspace),
        DBUS_SESSION_BUS_ADDRESS="unix:path=" + str(runtime / "disabled-session-bus.sock"),
        DBUS_SYSTEM_BUS_ADDRESS="unix:path=" + str(runtime / "disabled-system-bus.sock"),
        NO_AT_BRIDGE="1", GTK_A11Y="none", GTK_USE_PORTAL="0", GIO_USE_VFS="local",
        GSETTINGS_BACKEND="memory", XDG_CURRENT_DESKTOP="zuse-owned-xvfb",
        CARGO_TERM_COLOR="never",
        YDOTOOL_SOCKET=str(runtime / "disabled-ydotool.sock"),
    )
    return env


def run_tests(command, env, log_path, timeout):
    """Stream the owned harness log without a blocking pipe/readline deadline."""
    process = None
    began = time.monotonic()
    try:
        with log_path.open("wb") as log, log_path.open("rb") as incoming:
            process = subprocess.Popen(command, env=env, cwd=ROOT, stdin=subprocess.DEVNULL,
                                       stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            while True:
                chunk = incoming.read(65536)
                if chunk:
                    sys.stdout.buffer.write(chunk)
                    sys.stdout.buffer.flush()
                code = process.poll()
                if code is not None:
                    sys.stdout.buffer.write(incoming.read())
                    sys.stdout.buffer.flush()
                    return code, round((time.monotonic() - began) * 1000)
                if time.monotonic() - began >= timeout:
                    raise RuntimeError(f"owned native harness exceeded {timeout}s")
                time.sleep(.05)
    finally:
        terminate_owned(process)


def save_report(path, report, workspace):
    if path is None:
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    artifact_dir = path.parent / (path.stem + "-artifacts")
    artifacts = {}
    # Preserve useful evidence, never Xauthority cookies or compiled binaries.
    files = [workspace / "native-vision.log", workspace / "xvfb.log", workspace / "fixture-build.log"]
    files += [item for case in workspace.glob("case-*") if case.is_dir()
              for item in case.iterdir() if item.suffix in (".png", ".svg", ".json", ".log")]
    for source in files:
        if not source.is_file():
            continue
        relative = source.relative_to(workspace)
        target = artifact_dir / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
        artifacts[str(relative)] = {"path": str(target), "sha256": hashlib.sha256(target.read_bytes()).hexdigest()}
    report["artifacts"] = artifacts
    path.write_text(json.dumps(report, indent=2) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest-path", type=Path, default=ROOT / "Cargo.toml")
    parser.add_argument("--without-vision", action="store_true", help="run the two core X11 tests only")
    parser.add_argument("--test-binary", type=Path, help="already compiled Rust integration-test harness")
    parser.add_argument("--zuse-binary", type=Path, help="matching immutable CLI binary")
    parser.add_argument("--report", type=Path, help="JSON evidence; logs and explicit SVG are copied beside it")
    parser.add_argument("--timeout", type=int, default=300, help="bounded harness/build time in seconds")
    args = parser.parse_args()
    if sys.platform != "linux":
        parser.error("this native fixture is Linux X11 only")
    if not 5 <= args.timeout <= 900:
        parser.error("--timeout must be 5..900 seconds")
    if args.test_binary and not args.zuse_binary:
        parser.error("--test-binary requires its matching --zuse-binary")
    required = ["Xvfb", "xauth", "xdpyinfo", "cc", "pkg-config"]
    if not args.test_binary:
        required.append("cargo")
    for name in required:
        if shutil.which(name) is None:
            parser.error("installed fixture dependency is unavailable: " + name)
    for path in (args.test_binary, args.zuse_binary):
        if path and not path.resolve().is_file():
            parser.error("executable does not exist: " + str(path))
    report_path = args.report.resolve() if args.report else None
    report = {"native_x11_vision": "starting", "vision_enabled": not args.without_vision,
              "isolation": {"host_display_connection": False, "window_manager": False,
                            "desktop_bus_connection": False, "server": "owned-authenticated-high-display-Xvfb"}}
    code = 1
    with tempfile.TemporaryDirectory(prefix="zuse-native-vision-") as temporary:
        workspace = Path(temporary).resolve()
        auth = workspace / "Xauthority"
        read_fd = write_fd = None
        xvfb = None
        lease = None
        xvfb_log = None
        try:
            env = isolated_environment(workspace, auth)
            read_fd, write_fd = os.pipe()
            command, number, lease = private_xvfb_command("800x600x24", auth, write_fd)
            xvfb_log = (workspace / "xvfb.log").open("wb")
            xvfb = subprocess.Popen(command, env=env, pass_fds=(write_fd,), stdin=subprocess.DEVNULL,
                                    stdout=subprocess.DEVNULL, stderr=xvfb_log, start_new_session=True)
            os.close(write_fd); write_fd = None
            with os.fdopen(read_fd) as ready:
                read_fd = None
                if not select.select([ready], [], [], 5)[0]:
                    raise RuntimeError("private Xvfb startup exceeded 5s")
                env["DISPLAY"] = verify_private_xvfb(xvfb, number, ready.readline(), auth)
            report["isolation"].update(display=env["DISPLAY"], xvfb_pid=xvfb.pid)

            flags = subprocess.run(["pkg-config", "--cflags", "--libs", "gtk+-3.0", "x11"],
                                   env=env, capture_output=True, text=True, check=True, timeout=10)
            fixture = workspace / "native-vision-fixture"
            source = ROOT / "tests/native_vision_fixture.c"
            compile_command = ["cc", "-std=c11", "-O2", "-Wall", "-Wextra", "-Werror",
                               str(source), "-o", str(fixture), *shlex.split(flags.stdout)]
            compiled = subprocess.run(compile_command, env=env, capture_output=True, text=True, timeout=60)
            (workspace / "fixture-build.log").write_text(compiled.stdout + compiled.stderr)
            compiled.check_returncode()
            env["ZUSE_NATIVE_VISION_FIXTURE"] = str(fixture)
            if args.zuse_binary:
                binary = args.zuse_binary.resolve()
                env["ZUSE_NATIVE_ZUSE"] = str(binary)
                with binary.open("rb") as incoming:
                    report["zuse_binary_sha256"] = hashlib.file_digest(incoming, "sha256").hexdigest()
            else:
                env.pop("ZUSE_NATIVE_ZUSE", None)
            if args.test_binary:
                command = [str(args.test_binary.resolve())]
            else:
                command = ["cargo", "test", "--offline", "--locked", "--manifest-path", str(args.manifest_path.resolve())]
                if not args.without_vision:
                    command += ["--features", "vision"]
                command += ["--test", "vision_runtime_native", "--"]
            command += ["--ignored", "--test-threads=1", "--nocapture", "--color=never"]
            if args.without_vision:
                command += ["native_x11_"]
            log_path = workspace / "native-vision.log"
            code, elapsed = run_tests(command, env, log_path, args.timeout)
            output = log_path.read_text(errors="replace")
            expected = CORE_TESTS + (() if args.without_vision else VISION_TESTS)
            checks = {name: f"test {name} ... ok" in output for name in expected}
            # --nocapture timing output may occur between the test name and ok.
            timeline = VISION_TESTS[1]
            if timeline in checks and not checks[timeline]:
                checks[timeline] = (f"test {timeline} ... first appearance to wait response:" in output and
                                    "12 visible frames -> 1 episode; reappearance -> 2\nok" in output)
            if code == 0 and not all(checks.values()):
                raise RuntimeError("the supplied test harness did not run every requested native test")
            report.update(native_x11_vision="pass" if code == 0 else "fail", checks=checks,
                          elapsed_ms=elapsed, exit_code=code,
                          source_sha256={name: hashlib.sha256((ROOT / name).read_bytes()).hexdigest()
                                         for name in ("tests/vision_runtime_native.rs", "tests/native_vision_fixture.c", "tests/run_vision_native.py")})
        except Exception as error:
            report.update(native_x11_vision="blocked", error=str(error))
            print("native fixture: " + str(error), file=sys.stderr)
            code = 1
        finally:
            terminate_owned(xvfb)
            if xvfb_log:
                xvfb_log.close()
            for fd in (read_fd, write_fd):
                if fd is not None:
                    os.close(fd)
            release_private_xvfb(lease, auth)
            save_report(report_path, report, workspace)
    print(json.dumps(report, indent=2))
    return code


if __name__ == "__main__":
    raise SystemExit(main())
