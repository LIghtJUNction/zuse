"""Linux PTY integration for retained chart-plotter device names; no real device required."""
import json
import os
import select
import subprocess
import sys
import tempfile
import time
from contextlib import ExitStack
from pathlib import Path

from plotter_session import Controller


def run(binary):
    with ExitStack() as stack:
        first, second = Controller(stack), Controller(stack)
        first.x, second.x = 4, 9
        directory = Path(stack.enter_context(tempfile.TemporaryDirectory()))
        directory_fd = os.open(directory, os.O_DIRECTORY | os.O_RDONLY)
        stack.callback(os.close, directory_fd)
        symlink = directory / "by-id-plotter"
        symlink.symlink_to(first.path)
        # The child resolves this private fixture under /dev without changing the system's udev tree.
        alias = f"/dev/fd/{directory_fd}/{symlink.name}"
        with subprocess.Popen([binary, "shell"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                              text=True, pass_fds=(directory_fd,)) as runtime:
            def request(source, expect="completed"):
                runtime.stdin.write(source + "\n")
                runtime.stdin.flush()
                deadline = time.monotonic() + 6
                while not select.select([runtime.stdout], [], [], 0.005)[0]:
                    if time.monotonic() > deadline:
                        raise TimeoutError(source)
                    for controller in (first, second):
                        controller.step(0.002)
                result = json.loads(runtime.stdout.readline())
                assert result["status"] == expect, result
                return result if expect == "failed" else result["results"][0]["value"]

            def connect(path, baud=115200, expect="completed"):
                return request(f"plotter.connect(port={json.dumps(path)},baud={baud})", expect)

            try:
                assert connect(first.path)["work_position"][0] == 4
                assert connect(alias)["port"] == first.path
                assert "plotter.disconnect" in str(connect(alias, 9600, "failed"))
                assert request("plotter.status()")["work_position"][0] == 4
                # A newly requested distinct machine can still replace the connection after probing.
                assert connect(second.path)["work_position"][0] == 9
                assert connect(alias)["port"] == alias
                # Rebinding even the exact same spelled path must not redirect a held handle.
                symlink.unlink()
                symlink.symlink_to(second.path)
                before = len(second.received)
                assert "plotter.disconnect" in str(connect(alias, expect="failed"))
                assert request("plotter.status()")["work_position"][0] == 4
                assert len(second.received) == before
                request("plotter.disconnect()")
                assert connect(alias)["work_position"][0] == 9
                # A disappeared new name leaves the originally verified handle usable.
                symlink.unlink()
                assert connect(alias, expect="failed")["completed"] == 0
                assert request("plotter.status()")["work_position"][0] == 9
                request("plotter.disconnect()")
            finally:
                runtime.stdin.close()

    print("PASS: same-device aliases, alternate baud, verified switch, and rebound/missing symlinks")


if __name__ == "__main__":
    run(sys.argv[1])
