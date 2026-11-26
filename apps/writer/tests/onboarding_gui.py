#!/usr/bin/env python3
"""Run Writer onboarding on a private Xvfb display with the read-only CLI fixture.

Usage: python3 apps/writer/tests/onboarding_gui.py /path/to/zuse-writer /path/to/screenshots
Ubuntu packages: xvfb xauth xdotool imagemagick tesseract-ocr tesseract-ocr-eng fonts-noto-cjk
"""
import csv
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def command(*args):
    return subprocess.run(args, check=True, text=True, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE).stdout.rstrip("\r\n")


def await_value(read, description, timeout=15):
    end = time.monotonic() + timeout
    last = None
    while time.monotonic() < end:
        try:
            last = read()
            if last:
                return last
        except (FileNotFoundError, subprocess.CalledProcessError):
            pass
        time.sleep(.15)
    raise RuntimeError(f"Timed out waiting for {description}, last result {last!r}")


class Window:
    def __init__(self, binary, root):
        self.root = root
        self.proc = subprocess.Popen([binary, "--align"], stdout=sys.stderr)
        try:
            self.win = await_value(lambda: command("xdotool", "search", "--onlyvisible",
                                                   "--name", "^Zuse Writer").splitlines()[-1],
                                   "Writer window")
        except BaseException:
            self.close()
            raise
        command("xdotool", "windowfocus", "--sync", self.win)
        self.language = self.locate("English")
        self.click(self.language)

    def close(self):
        self.proc.terminate()
        self.proc.wait(timeout=10)

    def snap(self, path):
        command("import", "-window", self.win, str(path))

    def save_chinese(self, path):
        self.click(self.language)
        self.locate("English")
        self.snap(path)
        self.click(self.language)

    def lines(self):
        img = self.root / "frame.png"
        self.snap(img)
        enlarged = self.root / "frame-2x.png"
        command("convert", str(img), "-resize", "200%", str(enlarged))
        tsv = command("tesseract", str(enlarged), "stdout", "-l", "eng", "--psm", "11", "tsv")
        groups = {}
        for word in csv.DictReader(io.StringIO(tsv), delimiter="\t", quoting=csv.QUOTE_NONE):
            if not (word.get("text") or "").strip():
                continue
            key = tuple(word[k] for k in ("page_num","block_num","par_num","line_num"))
            groups.setdefault(key, []).append(word)
        return list(groups.values())

    def find(self, label):
        wanted = re.sub(r"[^a-z0-9]", "", label.lower())
        for words in self.lines():
            phrase = "".join(re.sub(r"[^a-z0-9]", "", w["text"].lower()) for w in words)
            if wanted not in phrase:
                continue
            # Locate the requested substring even when OCR groups adjacent buttons.
            for i in range(len(words)):
                part = ""
                for w in words[i:]:
                    part += re.sub(r"[^a-z0-9]", "", w["text"].lower())
                    if wanted == part:
                        left, top = int(words[i]["left"]), int(words[i]["top"])
                        right, bottom = int(w["left"]) + int(w["width"]), int(w["top"]) + int(w["height"])
                        return ((left + right) // 4, (top + bottom) // 4)
                    if not wanted.startswith(part):
                        break
        return None

    def locate(self, label):
        try:
            return await_value(lambda: self.find(label), label)
        except Exception:
            dest = Path(os.environ["WRITER_QA_ARTIFACTS"])
            self.snap(dest / "failure.png")
            words = "\n".join(" ".join(w["text"] for w in line) for line in self.lines())
            (dest / "failure-ocr.txt").write_text(words)
            print(f"Looking for {label!r}. OCR on the current window:\n{words}", file=sys.stderr)
            raise

    def click(self, pos):
        command("xdotool", "mousemove", "--window", self.win, *map(str, pos), "click", "1")

    def record_corner(self):
        # At either corner the enabled recording button spans the paper pane.
        # Follow its rendered rectangle, then check the saved coordinates independently.
        def position():
            img = self.root / "controls.png"
            self.snap(img)
            width, height = map(int, command("identify", "-format", "%w %h", str(img)).split())
            pixels = subprocess.check_output(["convert", str(img), "-depth", "8", "rgb:-"])
            xs = range(width * 46 // 100, width * 92 // 100, 8)
            rows = []
            for y in range(height // 6, height * 9 // 10, 2):
                count = 0
                for x in xs:
                    offset = 3 * (y * width + x)
                    red, green, blue = pixels[offset:offset + 3]
                    count += red < 120 and blue > 130 and blue > red + 55 and blue > green + 25
                if count >= len(xs) * .75:
                    rows.append(y)
            if len(rows) < 8:
                return None
            return (width * 70 // 100, rows[len(rows) // 2])
        self.click(await_value(position, "enabled paper-corner recording button"))

    def click_label(self, label):
        self.click(self.locate(label))


def exercise(binary, output, root):
    os.environ["WRITER_QA_ARTIFACTS"] = str(output)
    fixture = root / "zuse"
    shutil.copy(Path(__file__).with_name("corner_session.py"), fixture)
    fixture.chmod(0o700)
    os.environ.update(HOME=str(root), XDG_CONFIG_HOME=str(root), ZUSE_BIN=str(fixture),
                      ZUSE_WRITER_SESSION="corner-test", ZUSE_CORNER_DAEMON_PID=str(os.getpid()))
    # Keep the fixture executable outside the config directory named zuse.
    os.environ["XDG_CONFIG_HOME"] = str(root / "config")
    cfg = Path(os.environ["XDG_CONFIG_HOME"])
    cfg.mkdir()
    prefs = cfg / "zuse" / "writer" / "jog-distance-mm"
    draft = cfg / "zuse" / "writer" / "corner-drafts" / "corner-test.draft"

    def corners():
        return [float(item) for item in draft.read_text().splitlines()[3].split()]
    unplugged = cfg / "unplugged"
    unplugged.touch()
    w = Window(binary, root)
    try:
        fine = w.locate("Fine")
        coarse = w.locate("Coarse")
        w.click(coarse)
        await_value(lambda: float(prefs.read_text()) == 10, "offline coarse step")
        w.click((2 * fine[0] - coarse[0], fine[1]))
        command("xdotool", "key", "ctrl+a")
        command("xdotool", "type", "37.5")
        await_value(lambda: float(prefs.read_text()) == 37.5, "offline custom step")
        w.save_chinese(output / "01-connect.png")
    finally:
        w.close()
    unplugged.unlink()
    w = Window(binary, root)
    try:
        w.record_corner()
        await_value(lambda: corners() == [0, 0], "first logged corner")
        command(str(fixture), "inject", "plotter.jog", "plotter.jog_held", "plotter.status")
    finally:
        w.close()
    w = Window(binary, root)
    try:
        assert corners() == [0, 0]
        w.save_chinese(output / "02-resume-top-left.png")
        w.record_corner()
        await_value(lambda: corners() == [0, 0, 2, 2], "both saved paper corners")
        w.save_chinese(output / "03-corners.png")
    finally:
        w.close()
    journal = [json.loads(line) for line in (cfg / "job-log").read_text().splitlines()]
    assert journal[-1]["position"] == [2, 2, 0], journal[-1]


def main():
    if len(sys.argv) == 3:
        binary, output = map(lambda s: str(Path(s).resolve()), sys.argv[1:])
        Path(output).mkdir(parents=True, exist_ok=True)
        subprocess.run(["xvfb-run", "-a", "-s", "-screen 0 1280x900x24 -nolisten tcp",
                        sys.executable, __file__, "--private-xvfb", binary, output], check=True)
    elif len(sys.argv) == 4 and sys.argv[1] == "--private-xvfb":
        with tempfile.TemporaryDirectory(prefix="writer-onboarding-") as scratch:
            exercise(Path(sys.argv[2]), Path(sys.argv[3]), Path(scratch))
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    main()
