"""Private high-display leases and Xauthority; callers own process cleanup."""
import os
from pathlib import Path
import secrets
import subprocess


def private_xvfb_command(screen, auth_file, display_fd=1):
    auth_file = Path(auth_file).resolve()
    # Share the UTF-8 fixture's per-user atomic lease namespace. Hold our lease
    # until Xvfb exits; never unlink another X server's socket or lock.
    for _ in range(32):
        number = str(10_000 + secrets.randbelow(20_000))
        lease = Path("/tmp") / f"project-z-xvfb-{os.getuid()}-{number}.lock"
        try:
            lease.mkdir(mode=0o700)
        except FileExistsError:
            continue
        break
    else:
        raise RuntimeError("could not reserve a private high-number display")
    auth_created = False
    try:
        auth_file.touch(mode=0o600, exist_ok=False)
        auth_created = True
        auth_file.chmod(0o600)
        authorized = subprocess.run(
            ["xauth", "-f", str(auth_file), "add", ":" + number,
             "MIT-MAGIC-COOKIE-1", secrets.token_hex(16)],
            capture_output=True, timeout=5)
        if authorized.returncode:
            raise RuntimeError("could not create the private Xvfb authorization file")
        return (["Xvfb", ":" + number, "-displayfd", str(display_fd),
                 "-screen", "0", screen, "-nolisten", "tcp", "-nopn",
                 "-auth", str(auth_file)], number, lease)
    except BaseException:
        if auth_created:
            auth_file.unlink(missing_ok=True)
        lease.rmdir()
        raise


def verify_private_xvfb(process, number, ready, auth_file):
    if not isinstance(number, str) or not number.isascii() or not number.isdigit() or not 10000 <= int(number) < 30000:
        raise RuntimeError("private X display number must be in 10000..29999")
    if isinstance(ready, bytes):
        ready = ready.decode("ascii")
    if ready.strip() != number or process.poll() is not None:
        raise RuntimeError("Xvfb did not confirm the requested private display")
    display = ":" + number
    env = dict(os.environ, DISPLAY=display, XAUTHORITY=str(Path(auth_file).resolve()))
    # Some Xvfb builds omit .XN-lock with -displayfd. Authenticate the exact
    # reserved display instead of requiring that optional server lock file.
    subprocess.run(["xdpyinfo", "-display", display], env=env,
                   stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                   check=True, timeout=5)
    if process.poll() is not None:
        raise RuntimeError("private Xvfb exited during its authenticated probe")
    return display


def release_private_xvfb(lease, auth_file):
    """Release only successfully prepared resources, after Xvfb has exited."""
    if lease is not None:
        Path(auth_file).unlink(missing_ok=True)
        lease.rmdir()
