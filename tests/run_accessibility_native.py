#!/usr/bin/env python3
"""Run the Linux accessibility test in its own X server and D-Bus session.

Requires Xvfb, xauth, xdpyinfo, dbus-run-session, /usr/bin/python3 with Gtk 3 PyGObject.
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

FIXTURE = """
import gi
gi.require_version('Gtk', '3.0')
from gi.repository import Gtk
window = Gtk.Window(title='zuse-a11y-fixture')
window.set_default_size(420, 220)
box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
entry = Gtk.Entry()
entry.get_accessible().set_name('Zuse editable fixture')
password = Gtk.Entry()
password.set_visibility(False)
password.set_text('never-expose-this')
password.get_accessible().set_name('Zuse secret fixture')
label = Gtk.Label(label='Zuse action waiting')
button = Gtk.Button(label='Zuse action fixture')
button.connect('clicked', lambda _: label.set_text('Zuse action completed'))
duplicate_a = Gtk.Button(label='Zuse duplicate target')
duplicate_b = Gtk.Button(label='Zuse duplicate target')
hidden = Gtk.Button(label='Zuse hidden target')
hidden.set_no_show_all(True)
for widget in [entry, password, button, label, duplicate_a, duplicate_b, hidden]: box.pack_start(widget, False, False, 0)
window.add(box)
window.connect('destroy', Gtk.main_quit)
window.show_all()
print('READY', flush=True)
Gtk.main()
"""

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
        fixture = subprocess.Popen(['/usr/bin/python3', '-c', FIXTURE], env=env, stdout=subprocess.PIPE, text=True)
        if fixture.stdout.readline().strip() != 'READY': raise RuntimeError('GTK fixture did not start')
        command = ['cargo', 'test', '--offline']
        if args.manifest_path: command += ['--manifest-path', args.manifest_path]
        command += ['--test', 'accessibility_native', '--', '--ignored', '--nocapture']
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
