#!/usr/bin/env python3
"""Bump one version, optionally commit/tag/push it atomically."""
import argparse
import json
import os
import pathlib
import re
import subprocess
import tomllib
ROOT = pathlib.Path(__file__).resolve().parents[1]
SEMVER = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")

def git(*args):
    return subprocess.check_output(["git", *args], cwd=ROOT, text=True).strip()

def next_version(current, requested):
    if not SEMVER.fullmatch(current):
        raise ValueError("current version must be a stable MAJOR.MINOR.PATCH version")
    values = list(map(int, current.split(".")))
    if requested in ("major", "minor", "patch"):
        index = ("major", "minor", "patch").index(requested)
        values[index] += 1
        values[index + 1:] = [0] * (2 - index)
        version = ".".join(map(str, values))
    else:
        version = requested.removeprefix("v")
    if not SEMVER.fullmatch(version) or tuple(map(int, version.split("."))) <= tuple(map(int, current.split("."))):
        raise ValueError("new version must be a greater stable MAJOR.MINOR.PATCH version")
    return version

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version", help="patch, minor, major or an explicit version")
    parser.add_argument("--commit", action="store_true")
    parser.add_argument("--tag", action="store_true")
    parser.add_argument("--push", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    if args.tag and not args.commit:
        parser.error("--tag requires --commit")
    if args.push and not (args.commit and args.tag):
        parser.error("--push requires --commit and --tag")
    manifest = ROOT / "Cargo.toml"
    original = manifest.read_text()
    current = tomllib.loads(original)["package"]["version"]
    version = next_version(current, args.version)
    tag = "v" + version
    if args.dry_run:
        print(json.dumps({"previous": current, "version": version, "tag": tag}))
        return
    if git("status", "--porcelain"):
        raise RuntimeError("working tree must be clean before bumping")
    if args.push and git("branch", "--show-current") != "main":
        raise RuntimeError("--push requires the main branch")
    if subprocess.run(["git", "show-ref", "--verify", "--quiet", "refs/tags/" + tag], cwd=ROOT).returncode == 0:
        raise RuntimeError("tag already exists: " + tag)
    lines = original.splitlines(keepends=True)
    in_package = False
    changes = 0
    for index, line in enumerate(lines):
        if line.strip().startswith("["):
            in_package = line.strip() == "[package]"
        if in_package and re.match(r'^\s*version\s*=', line):
            lines[index] = re.sub(r'(")[^"]+(")', lambda m: '"' + version + '"', line, count=1)
            changes += 1
    if changes != 1:
        raise RuntimeError("expected exactly one package version")
    lock = ROOT / "Cargo.lock"
    previous_lock = lock.read_bytes() if lock.exists() else None
    manifest.write_text("".join(lines))
    try:
        subprocess.run(["cargo", "generate-lockfile"], cwd=ROOT, check=True)
    except BaseException:
        manifest.write_text(original)
        if previous_lock is None:
            lock.unlink(missing_ok=True)
        else:
            lock.write_bytes(previous_lock)
        raise
    if args.commit:
        subprocess.run(["git", "add", "--", "Cargo.toml", "Cargo.lock"], cwd=ROOT, check=True)
        subprocess.run(["git", "commit", "-m", "chore: release " + tag], cwd=ROOT, check=True)
    if args.tag:
        subprocess.run(["git", "tag", "-a", tag, "-m", "zuse " + version], cwd=ROOT, check=True)
    if args.push:
        subprocess.run(["git", "push", "--atomic", "origin", "HEAD:refs/heads/main", "refs/tags/" + tag], cwd=ROOT, check=True)
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
            output.write("version=" + version + "\ntag=" + tag + "\ncommit=" + git("rev-parse", "HEAD") + "\n")
    print(json.dumps({"previous": current, "version": version, "tag": tag, "committed": args.commit, "pushed": args.push}))

if __name__ == "__main__":
    main()
