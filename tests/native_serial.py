#!/usr/bin/env python3
"""Exercise native serial I/O against freshly created Linux PTYs only.

No user-supplied device path is accepted. The CLI receives only a slave allocated
by this process; all peer bytes and termios settings are synthetic.
"""
import argparse
import base64
from contextlib import contextmanager
from copy import deepcopy
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import select
import stat
import subprocess
import sys
import termios
import time

ROOT = Path(__file__).resolve().parents[1]


def require(condition, message):
    if not condition:
        raise AssertionError(message)


@contextmanager
def owned_pty():
    master, slave = pty.openpty()
    original = None
    try:
        os.set_blocking(master, False)
        path = os.ttyname(slave)
        require(path.startswith("/dev/pts/"), "allocator did not return a Linux PTY")
        original = termios.tcgetattr(slave)
        configured = deepcopy(original)
        configured[0] |= termios.IXON | termios.IXOFF
        configured[1] |= termios.OPOST | termios.ONLCR
        configured[2] &= ~(termios.CSIZE | termios.PARENB)
        configured[2] |= termios.CS8 | termios.CREAD | termios.CSTOPB
        if hasattr(termios, "CRTSCTS"):
            configured[2] |= termios.CRTSCTS
        configured[3] |= termios.ICANON | termios.ECHO | termios.ISIG
        configured[4] = termios.B9600
        configured[5] = termios.B9600
        configured[6][termios.VMIN] = 7
        configured[6][termios.VTIME] = 3
        termios.tcsetattr(slave, termios.TCSANOW, configured)
        baseline = termios.tcgetattr(slave)
        yield master, slave, path, baseline
    finally:
        if original is not None:
            try:
                termios.tcsetattr(slave, termios.TCSANOW, original)
            except OSError:
                pass
        os.close(slave)
        os.close(master)


def drain(master):
    chunks = []
    while select.select([master], [], [], 0)[0]:
        try:
            part = os.read(master, 65536)
        except BlockingIOError:
            break
        if not part:
            break
        chunks.append(part)
    return b"".join(chunks)


def receive(master, count, timeout=3):
    output = bytearray()
    deadline = time.monotonic() + timeout
    while len(output) < count:
        remaining = deadline - time.monotonic()
        require(remaining > 0, "PTY peer did not receive the expected bytes")
        require(bool(select.select([master], [], [], remaining)[0]), "PTY peer timed out")
        output.extend(os.read(master, 65536))
    return bytes(output)


class Acceptance:
    def __init__(self, args):
        self.binary = Path(args.binary).resolve()
        mode = self.binary.stat().st_mode
        require(stat.S_ISREG(mode) and not mode & (stat.S_ISUID | stat.S_ISGID),
                "test requires a regular unprivileged CLI binary")
        self.output = Path(args.output).resolve()
        self.output.mkdir(parents=True, exist_ok=True)
        os.chmod(self.output, 0o700)
        self.results = []
        self.report = {
            "schema": 1,
            "platform": "linux",
            "backend": "owned-kernel-pty",
            "binary_sha256": hashlib.sha256(self.binary.read_bytes()).hexdigest(),
            "physical_devices_opened": False,
            "tests": self.results,
        }

    def launch(self, program):
        return subprocess.Popen([str(self.binary), "eval", program],
                                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, text=True)

    def finish(self, process, timeout=5):
        try:
            stdout, stderr = process.communicate(timeout=timeout)
        except BaseException:
            process.kill()
            process.communicate()
            raise
        error_stream = not stdout.strip() and process.returncode != 0
        try:
            result = json.loads(stderr if error_stream else stdout)
        except ValueError as error:
            raise AssertionError("CLI did not return JSON: " + stderr[:400]) from error
        require(isinstance(result, dict), "CLI response is not an object")
        if error_stream:
            require(isinstance(result.get("error"), str),
                    "nonzero CLI stderr was not a structured validation error")
        return process.returncode, result

    def evaluate(self, program, timeout=5):
        return self.finish(self.launch(program), timeout)

    @staticmethod
    def success(code, result):
        require(code == 0 and result.get("status") != "failed",
                "expected successful CLI execution: " + json.dumps(result)[:1000])

    @staticmethod
    def failed(code, result):
        require(code != 0 or result.get("status") == "failed",
                "invalid or incomplete operation was reported as success")

    @staticmethod
    def read_values(result):
        return [item["value"] for item in result.get("results", [])
                if isinstance(item.get("value"), dict)
                and {"bytes", "base64", "text", "timed_out"}.issubset(item["value"])]

    @staticmethod
    def opened(path, baud=115200):
        return f"serial.open(path={json.dumps(path)}, baud={baud}); "

    def duplex(self):
        payload = bytes(range(256))
        encoded = base64.b64encode(payload).decode("ascii")
        with owned_pty() as (master, slave, path, baseline):
            program = (
                self.opened(path) + "serial.status(); "
                f"serial.write(base64={json.dumps(encoded)}, timeout=1000); "
                "serial.read(length=256, timeout=1500); "
                "serial.read(length=256, timeout=25); "
                "serial.read(length=256, timeout=25); serial.close()"
            )
            process = self.launch(program)
            try:
                require(receive(master, 256) == payload,
                        "kernel PTY output did not preserve all 256 byte values")
                active = termios.tcgetattr(slave)
                require(not active[0] & (termios.IXON | termios.IXOFF | termios.ICRNL),
                        "input translation or software flow control remained enabled")
                require(not active[1] & termios.OPOST, "output postprocessing remained enabled")
                require(not active[3] & (termios.ICANON | termios.ECHO | termios.ISIG),
                        "canonical, echo, or signal processing remained enabled")
                require(active[2] & termios.CSIZE == termios.CS8
                        and not active[2] & (termios.PARENB | termios.CSTOPB
                                             | getattr(termios, "CRTSCTS", 0)),
                        "serial port was not configured as 8N1 without hardware flow control")
                require(active[4] == termios.B115200 and active[5] == termios.B115200,
                        "kernel baud does not match the requested 115200 baud")
                require(os.write(master, payload) == len(payload), "short fixture peer write")
                code, result = self.finish(process)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.communicate()
            self.success(code, result)
            reads = self.read_values(result)
            require(bool(reads), "CLI omitted serial read results")
            decoded = [base64.b64decode(value["base64"], validate=True) for value in reads]
            require(all(value["bytes"] == len(data) for value, data in zip(reads, decoded)),
                    "read count and base64 payload disagree")
            require(b"".join(decoded) == payload, "serial reads did not preserve all 256 bytes")
            require(any(value["text"] is None for value in reads if value["bytes"]),
                    "invalid UTF-8 binary data was not represented by text=null")
            require(termios.tcgetattr(slave) == baseline,
                    "explicit close did not restore complete termios/baud/control characters")
            require(drain(master) == b"", "input unexpectedly echoed back to the PTY peer")
        return {"bytes_each_direction": 256, "raw_8n1": True,
                "kernel_baud": 115200, "complete_configuration_restored": True}

    def read_timeout(self):
        with owned_pty() as (_, slave, path, baseline):
            started = time.monotonic()
            code, result = self.evaluate(
                self.opened(path) + "serial.read(length=256, timeout=80); serial.close()"
            )
            elapsed = time.monotonic() - started
            self.success(code, result)
            require(self.read_values(result) == [
                {"bytes": 0, "base64": "", "text": "", "timed_out": True}
            ], "idle serial read did not report an empty timeout")
            require(elapsed >= 0.05, "serial timeout returned before its waiting budget")
            require(termios.tcgetattr(slave) == baseline, "timeout path did not restore termios")
        return {"requested_timeout_ms": 80, "elapsed_ms": round(elapsed * 1000),
                "configuration_restored": True}

    def implicit_drop(self):
        with owned_pty() as (_, slave, path, baseline):
            code, result = self.evaluate(self.opened(path) + "serial.status()")
            self.success(code, result)
            require(termios.tcgetattr(slave) == baseline,
                    "end-of-session Drop did not restore the original configuration")
        return {"explicit_close": False, "configuration_restored": True}

    def partial_write_timeout(self):
        # Stay below the DSL's 65536-character string bound after base64 encoding.
        payload = bytes(range(256)) * 128
        encoded = base64.b64encode(payload).decode("ascii")
        with owned_pty() as (master, slave, path, baseline):
            # Deliberately leave this owned peer undrained until the write deadline.
            code, result = self.evaluate(
                self.opened(path)
                + f"serial.write(base64={json.dumps(encoded)}, timeout=60); "
                + 'serial.write(text="FORBIDDEN_FOLLOWUP", timeout=60); serial.close()'
            )
            self.failed(code, result)
            text = json.dumps(result)
            counts = re.findall(r"bytes_written=(\d+)", text)
            require(bool(counts), "partial write failure omitted bytes_written")
            count = int(counts[0])
            observed = drain(master)
            require(0 < count < len(payload), "PTY backpressure did not produce a partial write")
            require(len(observed) == count and observed == payload[:count],
                    "reported partial count does not match bytes received by the kernel peer")
            require(b"FORBIDDEN_FOLLOWUP" not in observed,
                    "execution continued after a partial write failure")
            require(termios.tcgetattr(slave) == baseline,
                    "runtime failure Drop did not restore the original configuration")
        return {"requested_bytes": len(payload), "bytes_before_timeout": count,
                "later_step_stopped": True, "configuration_restored": True}

    def preflight(self):
        suffixes = [
            "serial.read(length=0, timeout=10)",
            "serial.read(length=65537, timeout=10)",
            "serial.read(length=1, timeout=0)",
            "serial.write(base64=\"AA?=\", timeout=10)",
            "serial.write(base64=\"AB==\", timeout=10)",
            "serial.write(text=\"a\", base64=\"YQ==\", timeout=10)",
            "serial.open(path=PATH, baud=12345)",
        ]
        for index, suffix in enumerate(suffixes):
            with owned_pty() as (master, slave, path, baseline):
                suffix = suffix.replace("PATH", json.dumps(path))
                program = (self.opened(path)
                           + 'serial.write(text="MUST_NOT_BE_WRITTEN", timeout=100); '
                           + suffix)
                source = self.output / f"preflight-{index}.zuse"
                try:
                    source.write_text(program + "\n", encoding="utf-8")
                    os.chmod(source, 0o600)
                    checked = subprocess.run([str(self.binary), "check", str(source)],
                                             capture_output=True, text=True, timeout=5)
                    require(checked.returncode != 0, "check accepted invalid serial arguments")
                    code, result = self.evaluate(program)
                    self.failed(code, result)
                    require(drain(master) == b"",
                            "an earlier serial write ran before whole-program validation")
                    require(termios.tcgetattr(slave) == baseline,
                            "preflight rejection changed the owned PTY configuration")
                finally:
                    source.unlink(missing_ok=True)
        return {"invalid_cases": len(suffixes), "prior_effects": 0}

    def record(self, name, function):
        try:
            detail = function()
            item = {"name": name, "status": "passed", "detail": detail}
        except Exception as error:
            item = {"name": name, "status": "failed", "error": str(error)[:1800]}
        self.results.append(item)
        print(json.dumps(item), flush=True)

    def run(self):
        try:
            for name, function in [
                ("all_256_bytes_bidirectional_and_termios_restore", self.duplex),
                ("read_timeout", self.read_timeout),
                ("implicit_drop_restores_configuration", self.implicit_drop),
                ("partial_write_timeout_stops_and_restores", self.partial_write_timeout),
                ("whole_program_preflight_has_no_prior_effects", self.preflight),
            ]:
                self.record(name, function)
        finally:
            self.report["passed"] = sum(item["status"] == "passed" for item in self.results)
            self.report["failed"] = sum(item["status"] == "failed" for item in self.results)
            path = self.output / "report.json"
            path.write_text(json.dumps(self.report, indent=2) + "\n", encoding="utf-8")
            os.chmod(path, 0o600)
        return int(bool(self.report["failed"]))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, help="newly built zuse CLI")
    parser.add_argument("--output", default=str(ROOT / "target/native-serial"))
    args = parser.parse_args()
    if sys.platform != "linux":
        parser.error("this acceptance fixture requires Linux PTYs")
    return Acceptance(args).run()


if __name__ == "__main__":
    raise SystemExit(main())
