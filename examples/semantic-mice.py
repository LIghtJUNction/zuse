#!/usr/bin/env python3
"""Show parallel Zuse cursors activating owned GTK buttons on Hyprland."""
import argparse
import json
import os
from pathlib import Path
import select
import shutil
import signal
import subprocess
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
TITLE = "Zuse · Semantic mice"
NAMES = ("Blue target", "Pink target", "Mint target", "Gold target", "Violet target")
COLORS = ("#22D3EE", "#F472B6", "#6EE7B7", "#FBBF24", "#C4B5FD")
FILLS = ("#12333F", "#3C2140", "#132C29", "#372A19", "#282344")


def run(command, **options):
    completed = subprocess.run(command, capture_output=True, text=True, timeout=12, **options)
    if completed.returncode:
        raise RuntimeError(completed.stderr.strip() or completed.stdout.strip())
    return completed.stdout


def hyprland(query):
    return json.loads(run(["hyprctl", "-j", query]))


def start_accessibility():
    registry = shutil.which("at-spi2-registryd") or next(
        p for p in ("/usr/lib/at-spi2-registryd", "/usr/libexec/at-spi2-registryd")
        if Path(p).is_file()
    )
    bus = subprocess.Popen(
        ["dbus-daemon", "--config-file=/usr/share/defaults/at-spi2/accessibility.conf",
         "--nofork", "--print-address=1"],
        stdout=subprocess.PIPE, text=True,
    )
    try:
        if not select.select([bus.stdout], [], [], 4)[0]:
            raise RuntimeError("fixture accessibility bus did not start")
        address = bus.stdout.readline().strip()
        if not address.startswith("unix:"):
            raise RuntimeError("fixture accessibility bus address is invalid")
        os.environ.update(AT_SPI_BUS_ADDRESS=address, NO_AT_BRIDGE="0", GDK_BACKEND="wayland")
        service = subprocess.Popen([registry], stdout=subprocess.DEVNULL)
    except BaseException:
        bus.terminate()
        bus.wait(timeout=4)
        raise
    return bus, service


def program(window, targets, positions):
    x, y = window["at"]
    w, h = window["size"]
    constructors, branches = [], []
    for index, (color, node, (px, py)) in enumerate(zip(COLORS, targets, positions)):
        key = f"cursor{index}"
        # Reverse the starting order so the visible trajectories cross.
        start = x + w * (len(targets) - index) // (len(targets) + 1)
        duration = 600 if len(targets) > 2 else 480 + 160 * index
        constructors.append(
            f'let {key}=mouse(kind="semantic", name="demo-{index}", color="{color}", '
            f'x={start}, y={y + h // 4}, size=40)'
        )
        point = f"x={x + px}, y={y + py}"
        action = (f'path={json.dumps(node["path"])}, pid={node["pid"]}, '
                  f'name={json.dumps(node["name"])}, action="click"')
        branches.append(
            "{ " + f"{key}.move({point}, duration={duration}, steps={duration * 60 // 1000}, jitter=0); "
            + f"{key}.position(); wait(ms=80); {key}.activate({action}); " + "}"
        )
    return "\n".join(constructors) + "\nwait(ms=180)\nparallel {\n" + "\n".join(branches) + "\n}\nwait(ms=1800)\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--count", type=int, choices=range(2, 6), default=2)
    parser.add_argument("--once", action="store_true", help="Close the demo after one run")
    parser.add_argument("--artifacts", type=Path, help="Record a private fixture only")
    args = parser.parse_args()
    args.binary = args.binary.resolve()
    if args.artifacts:
        if os.environ.get("ZUSE_PRIVATE_WAYLAND_FIXTURE") != "1":
            parser.error("desktop recording requires the private Wayland fixture")
        args.artifacts = args.artifacts.resolve()
        args.artifacts.mkdir(parents=True, exist_ok=True)

    os.environ["ZUSE_BACKEND"] = "wayland"
    bus, service = start_accessibility()
    try:
        import gi
        gi.require_version("Gtk", "3.0")
        gi.require_version("Gdk", "3.0")
        from gi.repository import Gdk, GLib, Gtk

        css = Gtk.CssProvider()
        styles = """
            window { background: #101626; color: #E8EDF8; }
            #heading { font-size: 30px; font-weight: bold; color: #F2F7FF; }
            #status { color: #B5C2DB; }
            .target { min-height: 104px; font-size: 17px; font-weight: bold;
                border-radius: 14px; border: 2px solid; background-image: none; box-shadow: none; }
            #replay { padding: 7px 22px; }
        """
        for index, (color, fill) in enumerate(zip(COLORS, FILLS)):
            styles += f"#target-{index} {{ color: {color}; border-color: {color}; background: {fill}; }}"
        css.load_from_data(styles.encode())
        Gtk.StyleContext.add_provider_for_screen(
            Gdk.Screen.get_default(), css, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION,
        )
        window = Gtk.Window(title=TITLE)
        window.set_default_size(1080 if args.count > 2 else 720, 400)
        window.connect("destroy", Gtk.main_quit)
        layout = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=16, margin=30)
        window.add(layout)
        title = Gtk.Label(label=f"Zuse  /  {args.count} semantic cursors", xalign=0)
        title.set_name("heading")
        layout.pack_start(title, False, False, 0)
        status = Gtk.Label(label="Preparing the accessibility targets…", xalign=0)
        status.set_name("status")
        layout.pack_start(status, False, False, 0)
        grid = Gtk.Box(spacing=16, homogeneous=True)
        layout.pack_start(grid, True, True, 0)
        names = NAMES[:args.count]
        counts = [0] * args.count
        manual = [0] * args.count
        buttons = []

        def clicked(_button, index):
            if Gtk.get_current_event() is None:
                counts[index] += 1
            else:
                manual[index] += 1
            buttons[index].set_label(
                f"{NAMES[index]}\nAgent {counts[index]}  ·  Manual {manual[index]}"
            )
            if all(counts):
                status.set_text("All application callbacks received")

        for index, name in enumerate(names):
            button = Gtk.Button(label=f"{name}\nAgent 0  ·  Manual 0")
            button.set_name(f"target-{index}")
            button.get_style_context().add_class("target")
            button.get_accessible().set_name(name)
            button.connect("clicked", clicked, index)
            buttons.append(button)
            grid.pack_start(button, True, True, 0)

        replay = Gtk.Button(label="Replay")
        replay.set_name("replay")
        layout.pack_start(replay, False, False, 0)
        failed = []
        active = []

        def finished(error):
            if error:
                failed.append(error)
                status.set_text(error)
                print(error, flush=True)
            else:
                status.set_text("All application callbacks received")
            replay.set_sensitive(True)
            active.clear()
            if args.once:
                GLib.timeout_add(250, lambda: (Gtk.main_quit(), False)[1])
            return False

        def execute(positions):
            error = None
            try:
                pid = os.getpid()
                deadline = time.monotonic() + 7
                while time.monotonic() < deadline:
                    snapshot = json.loads(run(
                        [args.binary, "-c", "accessibility.tree(limit=128,depth=12)"]
                    ))["results"][0]["value"]
                    targets = [next((n for n in snapshot["nodes"]
                                     if n["name"] == name and n["pid"] == pid
                                     and any(a["name"] == "click" for a in n["actions"])), None)
                               for name in names]
                    windows = [w for w in hyprland("clients") if w["pid"] == pid
                               and w["title"] == TITLE]
                    if all(targets) and len(windows) == 1:
                        break
                    time.sleep(0.12)
                else:
                    raise RuntimeError("The owned AT-SPI buttons were not found")

                initial = counts.copy()
                pointer = hyprland("cursorpos")
                focus = hyprland("activewindow").get("address")
                source = program(windows[0], targets, positions)
                with tempfile.TemporaryDirectory(prefix="zuse-semantic-") as temporary:
                    script = Path(temporary) / "demo.zuse"
                    script.write_text(source, encoding="utf-8")
                    if args.artifacts:
                        # A stale name must prevent its action from reaching GTK.
                        x, y = windows[0]["at"]
                        w, h = windows[0]["size"]
                        node = targets[0]
                        stale_program = (
                            f'let p=mouse(kind="semantic",x={x+w//2},y={y+h//3}); '
                            f'p.activate(path={json.dumps(node["path"])},pid={pid},name="stale target");'
                        )
                        script.write_text(stale_program, encoding="utf-8")
                        stale = subprocess.run([args.binary, "script", script], capture_output=True,
                                               text=True, timeout=12)
                        assert stale.returncode and "stale_target" in stale.stdout, stale
                        assert counts == initial, counts
                        script.write_text(source, encoding="utf-8")
                    with subprocess.Popen([args.binary, "script", script],
                                          stdout=subprocess.PIPE, text=True) as process:
                        active.append(process)
                        if args.artifacts:
                            with tempfile.TemporaryDirectory(prefix="frames-", dir=args.artifacts) as frames:
                                frames = Path(frames)
                                n = 0
                                deadline = time.monotonic() + 10
                                while process.poll() is None and time.monotonic() < deadline:
                                    run(["grim", "-c", str(frames / f"{n:03d}.png")])
                                    n += 1
                                    time.sleep(0.05)
                                run(["ffmpeg", "-y", "-loglevel", "error", "-framerate", "9",
                                     "-i", str(frames / "%03d.png"), "-c:v", "libx264",
                                     "-pix_fmt", "yuv420p", "-movflags", "+faststart",
                                     str(args.artifacts / "semantic-mice.mp4")])
                                shutil.copyfile(frames / f"{n - 2:03d}.png",
                                                args.artifacts / "semantic-mice.png")
                        try:
                            output, _ = process.communicate(timeout=8)
                        except subprocess.TimeoutExpired:
                            process.send_signal(signal.SIGINT)
                            try:
                                output, _ = process.communicate(timeout=3)
                            except subprocess.TimeoutExpired:
                                process.kill()
                                output, _ = process.communicate()
                            raise RuntimeError("The parallel cursor program exceeded its demo deadline")
                    assert process.returncode == 0, output
                    receipt = json.loads(output)
                assert counts == [n + 1 for n in initial], counts
                report = dict(
                    callbacks=counts, manual_clicks=manual,
                    pointer_before=pointer, pointer_after=hyprland("cursorpos"),
                    focus_unchanged=focus == hyprland("activewindow").get("address"),
                    results=receipt,
                )
                if args.artifacts:
                    moves = [e for e in receipt["events"] if e["event"] == "move"]
                    assert len(moves) == args.count, receipt
                    latest_start = max(e["at_ms"] - e["detail"]["elapsed_ms"] for e in moves)
                    assert latest_start < min(e["at_ms"] for e in moves), "cursor tracks did not overlap"
                    assert manual == [0] * args.count, manual
                    assert report["pointer_before"] == report["pointer_after"], report
                    assert report["focus_unchanged"], report
                    (args.artifacts / "semantic-mice.json").write_text(json.dumps(report, indent=2))
                print(json.dumps(report), flush=True)
            except Exception as exc:
                error = str(exc)
            GLib.idle_add(finished, error)

        def start(*_):
            if not replay.get_sensitive():
                return False
            replay.set_sensitive(False)
            status.set_text(f"Moving {args.count} cursors in parallel and activating through AT-SPI…")
            positions = []
            for button in buttons:
                allocation = button.get_allocation()
                positions.append(button.translate_coordinates(window, allocation.width // 2,
                                                              allocation.height // 2))
            threading.Thread(target=execute, args=(positions,), daemon=True).start()
            return False

        replay.connect("clicked", start)
        window.show_all()
        GLib.timeout_add(550, start)
        Gtk.main()
        for proc in active:
            if proc.poll() is None:
                proc.send_signal(signal.SIGINT)
                proc.wait(timeout=5)
        return bool(failed)
    finally:
        for proc in (service, bus):
            if proc.poll() is None:
                proc.terminate()
                proc.wait(timeout=4)


if __name__ == "__main__":
    raise SystemExit(main())
