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
        tsv = command("tesseract", str(img), "stdout", "-l", "eng", "--psm", "11", "tsv")
        groups = {}
        for word in csv.DictReader(io.StringIO(tsv), delimiter="\t"):
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
                        return ((left + right) // 2, (top + bottom) // 2)
                    if not wanted.startswith(part):
                        break
        return None

    def locate(self, label):
        try:
            return await_value(lambda: self.find(label), label)
        except RuntimeError:
            self.snap(Path(os.environ["WRITER_QA_ARTIFACTS"]) / "failure.png")
            raise

    def click(self, pos):
        command("xdotool", "mousemove", "--window", self.win, *map(str, pos), "click", "1")

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
        w.locate("Connect your writing machine")
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
        w.locate("Find the top-left corner")
        w.click_label("Record as top left")
        w.locate("Recorded top left")
        w.locate("Find the bottom-right corner")
        command(str(fixture), "inject", "plotter.jog", "plotter.jog_held", "plotter.status")
        w.locate("Recorded top left")
    finally:
        w.close()
    w = Window(binary, root)
    try:
        w.locate("Find the bottom-right corner")
        w.locate("Holder now")
        w.locate("Recorded top left")
        assert corners() == [0, 0]
        w.save_chinese(output / "02-resume-top-left.png")
        w.click_label("Record as bottom right")
        w.locate("Set the holder height")
        w.locate("Recorded bottom right")
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
