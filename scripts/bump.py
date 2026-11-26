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

def bump_lockfile(original, package, current, version):
    # Cargo owns the lockfile syntax. Edit only the local package's version;
    # a registry or Git package with the same name has its own source and pin.
    records = re.split(r"(?m)(?=^\[\[package\]\][ \t]*\r?$)", original)
    found = 0
    for index, record in enumerate(records[1:], 1):
        entry = tomllib.loads(record)["package"][0]
        if entry["name"] != package or "source" in entry:
            continue
        if entry["version"] != current:
            raise ValueError(f"expected local {package} {current}, found {entry['version']}")
        pattern = rf'(?m)^(version[ \t]*=[ \t]*"){re.escape(current)}(")'
        records[index], count = re.subn(pattern, lambda m: m[1] + version + m[2], record)
        if count != 1:
            raise ValueError(f"expected one local version field for {package}")
        found += 1
    if found != 1:
        raise ValueError(f"expected one local {package} lockfile entry, found {found}")
    # Cargo may disambiguate dependencies by version when a second source uses this name.
    dependency = rf'(?m)^([ \t]+"){re.escape(package + " " + current)}("[ \t]*,?[ \t]*\r?$)'
    return re.sub(dependency, lambda m: m[1] + package + " " + version + m[2], "".join(records))

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
    original = manifest.read_bytes().decode("utf-8")
    package = tomllib.loads(original)["package"]
    current = package["version"]
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
    paths = ("Cargo.toml", "Cargo.lock", "apps/writer/Cargo.lock")
    originals = {path: (ROOT / path).read_bytes() for path in paths}
    outputs = {"Cargo.toml": "".join(lines).encode("utf-8")}
    for path in paths[1:]:
        try:
            outputs[path] = bump_lockfile(originals[path].decode("utf-8"), package["name"], current, version).encode("utf-8")
        except ValueError as error:
            raise ValueError(f"{path}: {error}") from error
    head = git("rev-parse", "HEAD") if args.commit else None
    try:
        for path in paths:
            (ROOT / path).write_bytes(outputs[path])
        if args.commit:
            git("add", "--", *paths)
            git("commit", "-m", "chore: release " + tag)
    except BaseException:
        # A successful commit is a recovery point for a later tag/push retry.
        if not args.commit or git("rev-parse", "HEAD") == head:
            for path, content in originals.items():
                target = ROOT / path
                if not target.exists() or target.read_bytes() != content:
                    target.write_bytes(content)
            if args.commit:
                git("reset", "--quiet", "HEAD", "--", *paths)
        raise
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
