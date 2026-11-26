#!/usr/bin/env python3
"""Exercise a first install, managed upgrades, and a preserved third-party command."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]


def main():
    with tempfile.TemporaryDirectory() as temporary:
        sandbox = Path(temporary)
        home = sandbox / "user home"
        data = sandbox / "data with spaces"
        bundle = sandbox / "bundle"
        output = sandbox / "launched"
        command = home / ".local/bin/zuse-writer"
        command.parent.mkdir(parents=True)
        generic = command.with_name("zuse")
        generic.write_text("separately installed engine")
        old = home / ".local/share/zuse-writer/old/zuse-writer"
        old.parent.mkdir(parents=True)
        old.touch()
        command.symlink_to(old)
        desktop = data / "applications/zuse-writer.desktop"
        for source, dest in (("apps/writer/install-writer", "install-writer"),
                             ("apps/writer/zuse-writer.desktop", "apps/writer/zuse-writer.desktop"),
                             ("scripts/import_gfont.py", "scripts/import_gfont.py")):
            target = bundle / dest
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / source, target)
        writer = bundle / "zuse-writer"
        writer.write_text('#!/bin/sh\nset -eu\nbin=$(readlink -f "$0")\n"$(dirname "$bin")/zuse" > "$RECEIPT"\n')
        writer.chmod(0o755)
        engine = bundle / "zuse"
        env = {**os.environ, "HOME": str(home), "XDG_DATA_HOME": str(data), "RECEIPT": str(output)}
        def run_installer():
            subprocess.run([bundle / "install-writer"], env=env, check=True, stdout=subprocess.PIPE)
        def check_command(executable, expected):
            subprocess.run([executable], env=env, check=True)
            assert output.read_text().strip() == expected
        for marker in ("original", "updated"):
            engine.write_text("#!/bin/sh\necho " + marker + "\n")
            engine.chmod(0o755)
            run_installer()
            resolved = command.resolve()
            assert resolved == (data / "zuse-writer/current/zuse-writer").resolve()
            check_command(command, marker)
            assert old.exists() and generic.read_text() == "separately installed engine"
            assert 'Exec="' + str(data / "zuse-writer/current/zuse-writer") + '"' in desktop.read_text()
            if shutil.which("gio"):
                output.unlink()
                subprocess.run(["gio", "launch", desktop], env=env, check=True)
                for _ in range(100):
                    if output.exists() and output.read_text().strip() == marker:
                        break
                    time.sleep(0.02)
                assert output.read_text().strip() == marker
            run_installer()
            assert resolved == command.resolve()
        command.unlink()
        command.write_text("independent command")
        run_installer()
        assert command.read_text() == "independent command"
        assert generic.read_text() == "separately installed engine"
        print("Writer install, menu launch, upgrade, reinstall, and independent commands: OK")

if __name__ == "__main__":
    main()
