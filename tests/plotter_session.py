#!/usr/bin/env python3
"""Exercise two real session clients against a synthetic GRBL controller on a PTY.

Run on Unix from a Zuse checkout: python3 tests/plotter_session.py /path/to/zuse
"""

import argparse
import errno
import json
import os
from pathlib import Path
import pty
import re
import select
import stat
import subprocess
import sys
import tempfile
import time
import tty
from contextlib import ExitStack

DEADLINE = 8
JOG = 'plotter.jog(axis="x",delta=2,feed=100)'
CLIENT = Path(__file__).resolve().parents[1] / "examples" / "session_client.py"


class Controller:
    def __init__(self, stack):
        self.master, slave = pty.openpty()
        stack.callback(os.close, self.master)
        try:
            tty.setraw(slave)
            self.path = os.ttyname(slave)
        finally:
            os.close(slave)
        self.buffer = bytearray()
        self.received = bytearray()
        self.commands = []
        self.x = 4
        self.held_jog = False

    def step(self, wait):
        if not select.select([self.master], [], [], wait)[0]:
            return
        try:
            chunk = os.read(self.master, 4096)
        except OSError as exc:
            if exc.errno != errno.EIO:  # Unix PTY has no slave open yet.
                raise
            time.sleep(min(0.005, wait))
            return
        self.received.extend(chunk)
        for byte in chunk:
            if byte == ord("?") and not self.buffer:
                self.commands.append("?")
                self.reply(f"<Idle|WPos:{self.x}.000,1.000,0.000|FS:0,0>")
            elif byte == ord("\n"):
                line = self.buffer.decode("ascii").rstrip("\r")
                self.buffer.clear()
                self.commands.append(line)
                if line == "$I":
                    self.reply("[VER:1.1h.20190825:]\r\n[OPT:V,15,128]\r\nok")
                elif line == "$$":
                    self.reply("$13=0\r\nok")
                else:
                    match = re.fullmatch(r"\$J=G21G90X([\d.]+)F([\d.]+)", line)
                    assert match and tuple(map(float, match.groups())) == (6, 100), line
                    assert not self.held_jog and self.x == 4, "unexpected duplicate jog"
                    self.held_jog = True
            else:
                self.buffer.append(byte)

    def reply(self, line):
        os.write(self.master, (line + "\r\n").encode("ascii"))

    def acknowledge_jog(self):
        assert self.held_jog
        self.x = 6
        self.held_jog = False
        self.reply("ok")

    def until(self, condition):
        deadline = time.monotonic() + DEADLINE
        while not condition():
            if time.monotonic() >= deadline:
                raise TimeoutError(f"mock controller saw {self.commands!r}")
            self.step(0.01)


class Session:
    def __init__(self, binary, directory, name, stack):
        self.binary = binary
        self.name = name
        self.stack = stack
        self.env = dict(os.environ, ZUSE_SESSION_DIR=directory)
        for display in ("DISPLAY", "WAYLAND_DISPLAY"):
            self.env.pop(display, None)
        start = self.cli("start", "--idle-timeout-ms", "30000")
        self.socket = Path(start["socket"])
        stack.callback(self.cli, "close")
        assert start["status"] == "started" and self.socket.parent == Path(directory).resolve(), start
        assert self.socket.is_socket() and stat.S_IMODE(self.socket.stat().st_mode) == 0o600

    def cli(self, op, *args):
        result = subprocess.run(
            [self.binary, "session", op, self.name, *args], input="",
            text=True, capture_output=True, timeout=DEADLINE, env=self.env,
        )
        assert result.returncode == 0, (result.stdout, result.stderr)
        return json.loads(result.stdout)

    def submit(self, source, *options):
        process = subprocess.Popen(
            [sys.executable, str(CLIENT), "--socket", str(self.socket), *options],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env=self.env, text=True,
        )
        # ExitStack reaps even the deliberately stalled sender on assertion failure.
        def reap():
            if process.poll() is None:
                process.kill()
            process.communicate()
        self.stack.callback(reap)
        process.stdin.write(source)
        process.stdin.close()
        process.stdin = None
        return process


def finish(controller, process, status="completed"):
    controller.until(lambda: process.poll() is not None)
    stdout, stderr = process.communicate()
    expected = 0 if status == "completed" else 1
    assert process.returncode == expected, (stdout, stderr, controller.commands)
    response = json.loads(stdout)
    assert response["ok"] and response["status"] == status, response
    return response


def value(response):
    return response["result"]["results"][0]["value"]


def run(binary):
    with tempfile.TemporaryDirectory(prefix="zp-") as directory, ExitStack() as stack:
        controller = Controller(stack)
        owner = Session(binary, directory, "owner", stack)
        other = Session(binary, directory, "other", stack)
        connect = f'plotter.connect(port={json.dumps(controller.path)},baud=115200)'

        def execute(session, source, *options, status="completed"):
            return finish(controller, session.submit(source, *options), status)

        def silent(session, source, *options, status="completed"):
            before = bytes(controller.received)
            receipt = execute(session, source, *options, status=status)
            controller.step(0.05)  # Drain queued bytes even if the client has already exited.
            assert bytes(controller.received) == before, (source, controller.commands)
            return receipt

        def verify_connection(receipt, x):
            current = value(receipt)
            assert current["connected"] and current["port"] == controller.path, current
            assert current["baud"] == 115200 and current["state"] == "Idle", current
            assert current["units"] == "mm" and current["report_units"] == "mm", current
            assert current["work_position"] == [x, 1, 0], current

        silent(owner, connect, "--dry-run")
        assert value(silent(owner, "plotter.status()", "--observe"))["connected"] is False
        verify_connection(execute(owner, connect), 4)
        assert controller.commands == ["?", "$I", "$$", "?"], controller.commands
        # Another process observes the port retained after the connecting client exited.
        start = len(controller.commands)
        verify_connection(execute(owner, "plotter.status()", "--observe"), 4)
        assert controller.commands[start:] == ["?", "$I", "$$", "?"], controller.commands
        silent(owner, JOG, "--dry-run")
        silent(owner, JOG, "--observe", status="failed")
        silent(owner, 'plotter.jog(axis="q",delta=2,feed=100)', status="failed")
        denied = silent(other, connect, status="failed")
        assert value(silent(other, "plotter.status()", "--observe"))["connected"] is False, denied

        prefix = len(controller.commands)
        sender = owner.submit(JOG)

        def seen_jog():
            if sender.poll() is not None:
                raise AssertionError(f"client exited before jog: {sender.communicate()!r}")
            return controller.held_jog

        controller.until(seen_jog)
        assert controller.commands[prefix:-1] == ["?", "$I", "$$", "?"], controller.commands
        # The first client is still blocked on the withheld controller acknowledgement.
        status = owner.cli("status")
        assert status["state"] == "running" and type(status["active_job_id"]) is int, status
        before = bytes(controller.received)
        competing = owner.submit(JOG)
        controller.until(lambda: competing.poll() is not None)
        stdout, stderr = competing.communicate()
        assert competing.returncode == 1, (stdout, stderr)
        refusal = json.loads(stdout)
        assert not refusal["ok"] and refusal["error"]["code"] == "busy", refusal
        controller.step(0.05)
        assert bytes(controller.received) == before
        controller.acknowledge_jog()
        done = finish(controller, sender)
        assert done["job_id"] == status["active_job_id"], done
        jog = value(done)
        assert (jog["state"], jog["target_mm"], jog["from_mm"]) == ("Idle", 6, 4), jog
        assert controller.commands[-1] == "?" and not controller.buffer, controller.commands
        verify_connection(execute(owner, "plotter.status()", "--observe"), 6)
        execute(owner, "plotter.disconnect()")
        assert value(silent(owner, "plotter.status()", "--observe"))["connected"] is False
        verify_connection(execute(other, connect), 6)  # Previously excluded session now owns it.
        execute(other, "plotter.disconnect()")
        assert value(silent(other, "plotter.status()", "--observe"))["connected"] is False

    print("PASS: two clients, busy rejection, zero-write guards, exclusive ownership and release")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("zuse", type=Path)
    run(parser.parse_args().zuse.resolve())
