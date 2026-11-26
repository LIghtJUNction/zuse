#!/usr/bin/env python3
"""Verify release identity, package native builds, and publish GitHub assets."""
import argparse
import gzip
import io
import json
import pathlib
import re
import subprocess
import tarfile
import tomllib
import zipfile
ROOT = pathlib.Path(__file__).resolve().parents[1]
TARGETS = (
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "aarch64-linux-android",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
)
WRITER_TARGETS = {"x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"}

def git(*args):
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True).strip()

def metadata():
    return tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]

def verify(tag):
    if not re.fullmatch(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", tag):
        raise ValueError("release tag must be vMAJOR.MINOR.PATCH")
    version = metadata()["version"]
    if tag != "v" + version:
        raise ValueError("tag does not match Cargo.toml")
    commit = git("rev-parse", "HEAD")
    if git("rev-parse", "--verify", "refs/tags/" + tag + "^{commit}") != commit:
        raise ValueError("release tag does not point to the checked-out commit")
    return {"tag": tag, "version": version, "commit": commit}

def built_binary(project, target, name):
    cargo = json.loads(subprocess.check_output(["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"], cwd=project, text=True))
    executable = pathlib.Path(cargo["target_directory"]) / target / "release" / name
    if not executable.is_file():
        raise FileNotFoundError("build the release target first: " + str(executable))
    return executable

def package(target):
    if not re.fullmatch(r"[A-Za-z0-9_-]+", target):
        raise ValueError("invalid Rust target")
    version = metadata()["version"]
    name = "zuse.exe" if "windows" in target else "zuse"
    prefix = "zuse-v" + version + "-" + target
    files = [(built_binary(ROOT, target, name), name, 0o755)]
    files.append((ROOT / "scripts" / "import_gfont.py", "scripts/import_gfont.py", 0o755))
    if target in WRITER_TARGETS:
        writer = ROOT / "apps" / "writer"
        files.append((built_binary(writer, target, "zuse-writer"), "zuse-writer", 0o755))
        files.append((writer / "install-writer", "install-writer", 0o755))
        files.append((writer / "README.md", "apps/writer/README.md", 0o644))
        files.append((writer / "zuse-writer.desktop", "apps/writer/zuse-writer.desktop", 0o644))
    for pattern in ("web/*", "README*.md", "LICENSE", "CHANGELOG.md", "docs/*.md", "examples/*.zuse", "examples/*.py", "skills/**/*"):
        for path in sorted(ROOT.glob(pattern)):
            if path.is_file():
                files.append((path, str(path.relative_to(ROOT)).replace("\\", "/"), 0o644))
    dist = ROOT / "dist"
    dist.mkdir(exist_ok=True)
    if "windows" in target:
        output = dist / (prefix + ".zip")
        with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
            for path, relative, mode in files:
                info = zipfile.ZipInfo(prefix + "/" + relative, (1980, 1, 1, 0, 0, 0))
                info.external_attr = (0o100000 | mode) << 16
                info.compress_type = zipfile.ZIP_DEFLATED
                archive.writestr(info, path.read_bytes())
    else:
        output = dist / (prefix + ".tar.gz")
        with output.open("wb") as raw:
            with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0, compresslevel=9) as compressed:
                with tarfile.open(fileobj=compressed, mode="w") as archive:
                    for path, relative, mode in files:
                        data = path.read_bytes()
                        info = tarfile.TarInfo(prefix + "/" + relative)
                        info.size = len(data)
                        info.mode = mode
                        info.mtime = 0
                        info.uid = info.gid = 0
                        archive.addfile(info, io.BytesIO(data))
    print(output)

def publish(tag):
    identity = verify(tag)
    files = []
    for target in TARGETS:
        extension = ".zip" if "windows" in target else ".tar.gz"
        path = ROOT / "dist" / ("zuse-" + tag + "-" + target + extension)
        if not path.is_file():
            raise FileNotFoundError("missing native release artifact: " + str(path))
        files.append(path)
    exists = subprocess.run(["gh", "release", "view", tag], cwd=ROOT, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0
    if not exists:
        subprocess.run(["gh", "release", "create", tag, "--verify-tag", "--title", "zuse " + identity["version"], "--generate-notes"], cwd=ROOT, check=True)
    subprocess.run(["gh", "release", "upload", tag, *map(str, files), "--clobber"], cwd=ROOT, check=True)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    actions = parser.add_subparsers(dest="action", required=True)
    actions.add_parser("verify").add_argument("tag")
    actions.add_parser("package").add_argument("target")
    actions.add_parser("publish").add_argument("tag")
    args = parser.parse_args()
    if args.action == "verify":
        for key, value in verify(args.tag).items():
            print(key + "=" + value)
    elif args.action == "package":
        package(args.target)
    else:
        publish(args.tag)

if __name__ == "__main__":
    main()
