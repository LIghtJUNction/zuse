"""High-display Xvfb commands and ownership checks; callers own process cleanup."""
import os
from pathlib import Path


def private_xvfb_command(screen, display_fd=1, *, extra=()):
    # Never remove an existing lock/socket. Xvfb acquires the actual server
    # lock atomically; a race must fail startup rather than reuse another X.
    for offset in range(64):
        number = str(20000 + (os.getpid() + offset) % 10000)
        if not any(os.path.lexists(path) for path in
                   (f"/tmp/.X{number}-lock", f"/tmp/.X11-unix/X{number}")):
            return (["Xvfb", ":" + number, "-displayfd", str(display_fd),
                     "-screen", "0", screen, "-nolisten", "tcp", "-nopn", *extra], number)
    raise RuntimeError("no unused private high X display candidate")


def verify_private_xvfb(process, number, ready):
    if not isinstance(number, str) or not number.isascii() or not number.isdigit() or not 20000 <= int(number) < 30000:
        raise RuntimeError("private X display number must be in 20000..29999")
    if isinstance(ready, bytes):
        ready = ready.decode("ascii")
    if ready.strip() != number:
        raise RuntimeError("Xvfb did not confirm the requested private display")
    try:
        owner = Path(f"/tmp/.X{number}-lock").read_text().strip()
    except OSError as error:
        raise RuntimeError("private X display has no readable server lock") from error
    if owner != str(process.pid) or process.poll() is not None:
        raise RuntimeError("private X display is not owned by this live Xvfb")
    return ":" + number
