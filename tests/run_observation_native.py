#!/usr/bin/env python3
"""Run the event-driven Linux observation test in its own X server and D-Bus session.

Requires Xvfb, xauth, xdpyinfo, dbus-run-session and Gtk 3 development files.
No pointer/keyboard events are sent to the user's desktop.
"""
import argparse
import os
import select
import subprocess
import sys
import tempfile
import time

from private_xvfb import private_xvfb_command, release_private_xvfb, verify_private_xvfb


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--inside', action='store_true')
    parser.add_argument('--manifest-path')
    args = parser.parse_args()
    if not args.inside:
        command = ['dbus-run-session', '--', sys.executable, os.path.abspath(__file__), '--inside']
        if args.manifest_path: command += ['--manifest-path', args.manifest_path]
        return subprocess.call(command)
    workspace = tempfile.TemporaryDirectory(prefix='zuse-a11y-x11-')
    auth_file = os.path.join(workspace.name, 'Xauthority')
    read_fd = write_fd = None
    xvfb = fixture = accessibility_bus = registry = None
    display_lease = None
    try:
        read_fd, write_fd = os.pipe()
        xvfb_command, display_number, display_lease = private_xvfb_command('800x600x24', auth_file, write_fd)
        xvfb = subprocess.Popen(xvfb_command, pass_fds=(write_fd,), stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        os.close(write_fd)
        write_fd = None
        with os.fdopen(read_fd) as output:
            read_fd = None
            if not select.select([output], [], [], 5)[0]:
                raise RuntimeError('private Xvfb did not become ready within 5s')
            display = verify_private_xvfb(xvfb, display_number, output.readline(), auth_file)
        accessibility_bus = subprocess.Popen(['dbus-daemon', '--config-file=/usr/share/defaults/at-spi2/accessibility.conf', '--nofork', '--print-address=1'], stdout=subprocess.PIPE, text=True)
        address = accessibility_bus.stdout.readline().strip()
        if not address.startswith('unix:'): raise RuntimeError('isolated accessibility bus did not start')
        env = dict(os.environ, DISPLAY=display, XAUTHORITY=auth_file, GDK_BACKEND='x11', ZUSE_BACKEND='x11', NO_AT_BRIDGE='0', ZUSE_ATSPI_FIXTURE='1', AT_SPI_BUS_ADDRESS=address, GIO_USE_VFS='local')
        env.pop('WAYLAND_DISPLAY', None)
        registry = subprocess.Popen(['/usr/lib/at-spi2-registryd'], env=env, stdout=subprocess.DEVNULL)
        for _ in range(50):
            owner = subprocess.run(['gdbus', 'call', '--address', address, '--dest', 'org.freedesktop.DBus', '--object-path', '/org/freedesktop/DBus', '--method', 'org.freedesktop.DBus.NameHasOwner', 'org.a11y.atspi.Registry'], capture_output=True, text=True)
            if 'true' in owner.stdout: break
            if registry.poll() is not None: raise RuntimeError('isolated accessibility registry exited')
            time.sleep(0.05)
        else: raise RuntimeError('isolated accessibility registry did not own its bus name')
        fixture_path = os.path.join(workspace.name, 'observation-fixture')
        import shlex
        flags = subprocess.check_output(['pkg-config', '--cflags', '--libs', 'gtk+-3.0'], text=True)
        subprocess.run(['cc', os.path.join(os.path.dirname(__file__), 'native_observation_fixture.c'), '-o', fixture_path, *shlex.split(flags)], check=True)
        env['ZUSE_OBSERVATION_FIXTURE'] = fixture_path
        command = ['cargo', 'test', '--offline', '--locked']
        if args.manifest_path: command += ['--manifest-path', args.manifest_path]
        command += ['--test', 'observation_native', '--', '--ignored', '--test-threads=1', '--nocapture']
        return subprocess.call(command, env=env)
    finally:
        if fixture is not None:
            fixture.terminate()
            try: fixture.wait(timeout=3)
            except subprocess.TimeoutExpired: fixture.kill(); fixture.wait()
        if registry is not None:
            registry.terminate()
            try: registry.wait(timeout=3)
            except subprocess.TimeoutExpired: registry.kill(); registry.wait()
        if accessibility_bus is not None:
            accessibility_bus.terminate()
            try: accessibility_bus.wait(timeout=3)
            except subprocess.TimeoutExpired: accessibility_bus.kill(); accessibility_bus.wait()
        if xvfb is not None:
            xvfb.terminate()
            try: xvfb.wait(timeout=3)
            except subprocess.TimeoutExpired: xvfb.kill(); xvfb.wait()
        for fd in (read_fd, write_fd):
            if fd is not None:
                os.close(fd)
        release_private_xvfb(display_lease, auth_file)
        workspace.cleanup()

if __name__ == '__main__':
    raise SystemExit(main())
