#!/usr/bin/env python3
"""Run the Linux accessibility test in its own X server and D-Bus session.

Requires Xvfb, dbus-run-session, /usr/bin/python3 with Gtk 3 PyGObject.
No pointer/keyboard events are sent to the user's desktop.
"""
import argparse
import os
import subprocess
import sys
import time

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
for widget in [entry, password, button, label]: box.pack_start(widget, False, False, 0)
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
    read_fd, write_fd = os.pipe()
    xvfb = subprocess.Popen(['Xvfb', '-displayfd', str(write_fd), '-screen', '0', '800x600x24', '-nolisten', 'tcp'], pass_fds=(write_fd,), stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    os.close(write_fd)
    fixture = None
    accessibility_bus = None
    registry = None
    try:
        with os.fdopen(read_fd) as output:
            display = output.readline().strip()
        if not display.isdigit(): raise RuntimeError('Xvfb did not allocate an isolated display')
        accessibility_bus = subprocess.Popen(['dbus-daemon', '--config-file=/usr/share/defaults/at-spi2/accessibility.conf', '--nofork', '--print-address=1'], stdout=subprocess.PIPE, text=True)
        address = accessibility_bus.stdout.readline().strip()
        if not address.startswith('unix:'): raise RuntimeError('isolated accessibility bus did not start')
        env = dict(os.environ, DISPLAY=':' + display, GDK_BACKEND='x11', NO_AT_BRIDGE='0', ZUSE_ATSPI_FIXTURE='1', AT_SPI_BUS_ADDRESS=address, GIO_USE_VFS='local')
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
        xvfb.terminate()
        try: xvfb.wait(timeout=3)
        except subprocess.TimeoutExpired: xvfb.kill(); xvfb.wait()

if __name__ == '__main__':
    raise SystemExit(main())
