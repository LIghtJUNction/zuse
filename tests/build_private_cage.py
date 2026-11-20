#!/usr/bin/env python3
"""Build the pinned Cage fixture without downloads or system installation.

Stock Cage 0.3.1 chooses XDG-shell v5. Aquamarine 0.16 unconditionally binds
v6. The matching wlroots 0.20 library implements v7, including the sole v6
addition, the suspended state. This fixture changes only Cage's requested
XDG-shell version from 5 to 6. It does not alter or intercept protocol traffic.

Obtain the archive in the sandbox, then pass its local path with --source.
The output directory must not exist. The script never launches a compositor.
It reproduces the upstream C source list and generated config.h using cc and
pkg-config directly; Meson and Ninja are unnecessary for this fixture build.

Primary sources:
  https://github.com/cage-kiosk/cage/tree/v0.3.1
  https://gitlab.freedesktop.org/wlroots/wlroots/-/blob/0.20.0/types/xdg_shell/wlr_xdg_shell.c
  https://gitlab.freedesktop.org/wlroots/wlroots/-/blob/0.20.0/types/xdg_shell/wlr_xdg_toplevel.c
  https://gitlab.freedesktop.org/wayland/wayland-protocols/-/blob/1.45/stable/xdg-shell/xdg-shell.xml
"""
import argparse
import difflib
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import resource
import shlex
import shutil
import subprocess
import tarfile

SOURCE_URL = "https://codeload.github.com/cage-kiosk/cage/tar.gz/refs/tags/v0.3.1"
SOURCE_SHA256 = "6dc1619665acd367e0174c93b234002549a66f55f1de9197d67f0305415babc8"
SOURCE_COMMIT = "b7b774ac957b6d8c7b39f0dcb9ebe19de3e8a45d"
ORIGINAL_SHA256 = "fa2fa2ca03b657216b350728e02f716a043f4f66ef6c76f727b5b484d931611c"
PATCHED_SHA256 = "9ffd623b192a9c9830f670757f0c8559069e3f5de38e9de5f52ccecebbf5022c"
OLD = "wlr_xdg_shell_create(server.wl_display, 5)"
NEW = "wlr_xdg_shell_create(server.wl_display, 6)"
PROBE = """#define WLR_USE_UNSTABLE
#include <stdio.h>
#include <wayland-server-core.h>
#include <wlr/types/wlr_xdg_shell.h>

int main(void) {
    struct wl_display *display = wl_display_create();
    if (!display) return 1;
    struct wlr_xdg_shell *shell = wlr_xdg_shell_create(display, 6);
    if (!shell || shell->version != 6 || wl_global_get_version(shell->global) != 6) return 2;
    puts("installed wlroots created an XDG-shell v6 global without a socket, backend, renderer or seat");
    wl_display_destroy(display);
    return 0;
}
"""


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(command, log, env):
    result = subprocess.run(command, env=env, text=True, capture_output=True,
                            stdin=subprocess.DEVNULL, timeout=180)
    log.write("$ " + shlex.join(command) + "\n" + result.stdout + result.stderr)
    log.flush()
    if result.returncode:
        raise RuntimeError(f"command failed ({result.returncode}): {shlex.join(command)}; see build.log")
    return result.stdout.strip()


def extract_source(archive, destination):
    """Extract regular source files only from the already hash-checked archive."""
    with tarfile.open(archive, "r:gz") as source:
        if source.pax_headers.get("comment") != SOURCE_COMMIT:
            raise RuntimeError("source archive commit does not match the pinned release")
        members = source.getmembers()
        if len(members) > 100 or sum(item.size for item in members) > 1_000_000:
            raise RuntimeError("unexpected Cage source archive size")
        for member in members:
            relative = PurePosixPath(member.name)
            if relative.is_absolute() or ".." in relative.parts or relative.parts[0] != "cage-0.3.1":
                raise RuntimeError("unexpected source archive path")
            target = destination.joinpath(*relative.parts[1:])
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            elif member.isfile():
                target.parent.mkdir(parents=True, exist_ok=True)
                with source.extractfile(member) as incoming:
                    target.write_bytes(incoming.read())
            else:
                raise RuntimeError("source archive contains a link or special file")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True, help="local, unmodified Cage v0.3.1 tar.gz")
    parser.add_argument("--output", type=Path, required=True, help="new private build directory")
    args = parser.parse_args()
    archive = args.source.resolve()
    if not archive.is_file() or digest(archive) != SOURCE_SHA256:
        parser.error("--source must be the pinned, unmodified Cage v0.3.1 archive")
    for executable in ("cc", "pkg-config"):
        if shutil.which(executable) is None:
            parser.error(f"required installed build tool is missing: {executable}")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    env = os.environ.copy()
    for name in ("DISPLAY", "WAYLAND_DISPLAY", "WAYLAND_SOCKET", "HYPRLAND_INSTANCE_SIGNATURE"):
        env.pop(name, None)
    receipt = {
        "status": "building", "source_url": SOURCE_URL, "source_sha256": SOURCE_SHA256,
        "upstream_version": "0.3.1", "upstream_commit": SOURCE_COMMIT,
        "patch": {"file": "cage.c", "old": OLD, "new": NEW,
                  "before_sha256": ORIGINAL_SHA256, "after_sha256": PATCHED_SHA256},
        "system_install": False, "downloads": False, "compositor_launched": False,
    }
    receipt_path = output / "receipt.json"
    try:
        with (output / "build.log").open("w") as log:
            version = run(["pkg-config", "--modversion", "wlroots-0.20"], log, env)
            if not re.fullmatch(r"0\.20\.\d+(?:[.+~-].*)?", version):
                raise RuntimeError("the reviewed fixture requires installed wlroots 0.20.x")
            receipt["wlroots_version"] = version
            receipt["tool_versions"] = {
                "pkg_config": run(["pkg-config", "--version"], log, env),
                "compiler": run(["cc", "--version"], log, env).splitlines()[0],
            }
            # This checks the installed library, not just headers or advertised
            # package metadata. No display socket or input/output backend exists.
            probe_source = output / "xdg_v6_probe.c"
            probe_source.write_text(PROBE)
            flags = shlex.split(run(["pkg-config", "--cflags", "--libs", "wlroots-0.20", "wayland-server"], log, env))
            probe_binary = output / "xdg-v6-probe"
            run(["cc", "-std=c11", str(probe_source), "-o", str(probe_binary), *flags], log, env)
            receipt["installed_library_probe"] = run([str(probe_binary)], log, env)

            source = output / "source"
            extract_source(archive, source)
            cage_source = source / "cage.c"
            if digest(cage_source) != ORIGINAL_SHA256:
                raise RuntimeError("Cage source does not match the reviewed file")
            original = cage_source.read_text()
            if original.count(OLD) != 1:
                raise RuntimeError("expected exactly one XDG-shell version selection")
            patched = original.replace(OLD, NEW)
            cage_source.write_text(patched)
            if digest(cage_source) != PATCHED_SHA256:
                raise RuntimeError("patched Cage source does not match the reviewed change")
            patch = "".join(difflib.unified_diff(original.splitlines(True), patched.splitlines(True),
                                                 fromfile="a/cage.c", tofile="b/cage.c"))
            (output / "xdg-v6.patch").write_text(patch)
            receipt["patch_sha256"] = digest(output / "xdg-v6.patch")
            build = output / "build"
            build.mkdir()
            have_xwayland = run(["pkg-config", "--variable=have_xwayland", "wlroots-0.20"], log, env)
            if have_xwayland not in ("true", "false"):
                raise RuntimeError("installed wlroots did not expose its Xwayland build setting")
            # These are the only two substitutions in upstream config.h.in.
            template = (source / "config.h.in").read_text()
            config = template.replace("#mesondefine CAGE_HAS_XWAYLAND",
                                      "#define CAGE_HAS_XWAYLAND " + ("1" if have_xwayland == "true" else "0"))
            config = config.replace("#mesondefine CAGE_VERSION", '#define CAGE_VERSION "0.3.1"')
            if "#mesondefine" in config:
                raise RuntimeError("unexpected upstream configuration substitution")
            (build / "config.h").write_text(config)
            receipt["config_sha256"] = digest(build / "config.h")
            receipt["have_xwayland"] = have_xwayland == "true"
            sources = ["cage.c", "idle_inhibit_v1.c", "output.c", "seat.c", "view.c", "xdg_shell.c"]
            if have_xwayland == "true":
                sources.append("xwayland.c")
            flags = shlex.split(run(["pkg-config", "--cflags", "--libs", "wlroots-0.20",
                                     "wayland-server", "xkbcommon"], log, env))
            binary = build / "cage"
            run(["cc", "-std=c11", "-O2", "-DWLR_USE_UNSTABLE", "-Wundef",
                 "-Wno-unused-parameter", "-Wall", "-Wextra", "-Werror",
                 "-I", str(build), "-I", str(source),
                 *(str(source / name) for name in sources), "-o", str(binary), *flags, "-lm"], log, env)
            receipt.update(status="built", binary=str(binary), binary_sha256=digest(binary),
                           binary_version=run([str(binary), "-v"], log, env))
    except Exception as error:
        receipt.update(status="failed", error=str(error))
        raise
    finally:
        receipt_path.write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps(receipt, indent=2))


if __name__ == "__main__":
    main()
