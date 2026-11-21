#!/usr/bin/env python3
"""Exercise the real CLI with private Cargo roots and fake network/install tools.

No public request, Cargo build, user installation, or desktop input occurs.
Run: python3 tests/cli_update.py --binary /absolute/path/to/zuse
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest


TOOL = r'''
import json, os, pathlib, subprocess, sys, time
s = pathlib.Path(os.environ["ZUSE_UPDATE_FIXTURE"])
name = pathlib.Path(sys.argv[0]).name
with (s / "calls.jsonl").open("a") as f:
    f.write(json.dumps([name, *sys.argv[1:]]) + "\n")
mode = (s / "mode").read_text()
if name == "curl":
    if mode == "http_failure":
        print("curl: (28) simulated HTTP timeout", file=sys.stderr)
        sys.exit(28)
    print((s / "response.json").read_text())
elif name == "git":
    if mode == "missing_tag":
        print("official stable tag absent", file=sys.stderr)
        sys.exit(2)
    print("a" * 40 + "\t" + sys.argv[-1])
elif name == "cargo":
    if mode == "cargo_failure":
        print("simulated compilation failure", file=sys.stderr)
        sys.exit(7)
    if mode == "cargo_wait":
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
        (s / "child.pid").write_text(str(child.pid))
        (s / "cargo.started").write_text(str(os.getpid()))
        time.sleep(60)
        sys.exit(9)
    root = pathlib.Path(sys.argv[sys.argv.index("--root") + 1])
    if root != s / "a user prefix":
        print("wrong prefix", file=sys.stderr)
        sys.exit(10)
    if "--version" in sys.argv:
        latest = sys.argv[sys.argv.index("--version") + 1].removeprefix("=")
        source = "registry+https://github.com/rust-lang/crates.io-index"
    else:
        tag = sys.argv[sys.argv.index("--tag") + 1]
        latest = tag.removeprefix("v")
        source = "git+https://github.com/LIghtJUNction/zuse.git?tag=" + tag + "#" + "a" * 40
    metadata_path = root / ".crates2.json"
    metadata = json.loads(metadata_path.read_text())
    info = next(iter(metadata["installs"].values()))
    metadata_path.write_text(json.dumps({"installs": {"zuse-computer " + latest + " (" + source + ")": info}}))
    reported = "99.0.0" if mode == "wrong_version" else latest
    replacement = root / "bin/zuse.next"
    replacement.write_text("#!" + sys.executable + "\nprint(" + repr("zuse " + reported) + ")\n")
    replacement.chmod(0o755)
    replacement.replace(root / "bin/zuse")
    print("fixture Cargo installed into private root", file=sys.stderr)
'''


@unittest.skipUnless(os.name == "posix", "private tool fixtures use POSIX executables")
class UpdateCLI(unittest.TestCase):
    binary = None
    current = None
    latest = None

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="zuse-update-fixture-")
        self.addCleanup(self.temp.cleanup)
        self.state = Path(self.temp.name)
        self.prefix = self.state / "a user prefix"
        (self.prefix / "bin").mkdir(parents=True)
        self.exe = self.prefix / "bin/zuse"
        shutil.copy2(self.binary, self.exe)
        self.original_sha = hashlib.sha256(self.exe.read_bytes()).hexdigest()
        self.tools = self.state / "tools"
        self.tools.mkdir()
        for name in ("curl", "cargo", "git"):
            path = self.tools / name
            path.write_text("#!" + sys.executable + "\n" + TOOL)
            path.chmod(0o755)
        self.env = dict(os.environ, PATH=str(self.tools), ZUSE_UPDATE_FIXTURE=str(self.state))
        self.write_mode("success")
        self.write_tracking()
        self.registry_response(self.latest)

    def write_mode(self, mode):
        (self.state / "mode").write_text(mode)

    def write_tracking(self, name="zuse-computer", source=None):
        source = source or "registry+https://github.com/rust-lang/crates.io-index"
        metadata = {"installs": {f"{name} {self.current} ({source})": {
            "bins": ["zuse"], "features": ["vision"], "all_features": False,
            "no_default_features": True, "profile": "release",
            "target": "x86_64-unknown-linux-gnu",
        }}}
        (self.prefix / ".crates2.json").write_text(json.dumps(metadata))

    def registry_response(self, version):
        self.response({"crate": {"name": "zuse-computer", "repository": "https://github.com/LIghtJUNction/zuse", "max_stable_version": version}, "versions": [{"num": version, "yanked": False}]})

    def git_source(self):
        self.write_tracking(source="git+https://github.com/LIghtJUNction/zuse.git#" + "b" * 40)
        self.response({"tag_name": "v" + self.latest, "html_url": "https://github.com/LIghtJUNction/zuse/releases/tag/v" + self.latest, "draft": False, "prerelease": False})

    def response(self, data):
        (self.state / "response.json").write_text(json.dumps(data))

    def calls(self):
        path = self.state / "calls.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def invoke(self, *args, code=0):
        result = subprocess.run([str(self.exe), *args], env=self.env, capture_output=True, text=True, timeout=8)
        self.assertEqual(result.returncode, code, (result.stdout, result.stderr))
        return result

    def assert_unmodified(self):
        self.assertEqual(hashlib.sha256(self.exe.read_bytes()).hexdigest(), self.original_sha)
        self.assertFalse(any(call[0] == "cargo" for call in self.calls()))

    def test_check_and_dry_run_return_plans_without_installing(self):
        for flag in ("--check", "--dry-run"):
            with self.subTest(flag=flag):
                data = json.loads(self.invoke("update", flag).stdout)
                self.assertTrue(data["update_available"])
                self.assertEqual(data["command"][0], "cargo")
                self.assertEqual(data["installation"]["root"], str(self.prefix))
                self.assert_unmodified()

    def test_registry_update_preserves_prefix_and_installed_options(self):
        data = json.loads(self.invoke("update").stdout)
        self.assertEqual(data["status"], "updated")
        self.assertEqual(data["current_version"], self.latest)
        self.assertFalse(data["update_available"])
        cargo = [c for c in self.calls() if c[0] == "cargo"]
        self.assertEqual(len(cargo), 1)
        args = cargo[0]
        for flag, expected in (("--root", str(self.prefix)), ("--version", "=" + self.latest), ("--features", "vision"), ("--profile", "release"), ("--target", "x86_64-unknown-linux-gnu")):
            self.assertEqual(args[args.index(flag) + 1], expected)
        self.assertIn("--no-default-features", args)
        self.assertNotIn("--force", args)
        self.assertNotIn("sudo", args)

    def test_current_or_newer_version_does_not_install(self):
        for version in (self.current, "0.0.1"):
            with self.subTest(version=version):
                self.registry_response(version)
                data = json.loads(self.invoke("update").stdout)
                self.assertEqual(data["status"], "up_to_date")
                self.assertFalse(data["update_available"])
                self.assert_unmodified()

    def test_unknown_installation_reports_manual_update_and_fails_apply(self):
        (self.prefix / ".crates2.json").unlink()
        checked = json.loads(self.invoke("update", "--check").stdout)
        self.assertFalse(checked["can_update"])
        failed = json.loads(self.invoke("update", code=1).stdout)
        self.assertEqual(failed["status"], "failed")
        self.assertIn("manual", failed["reason"])
        self.assert_unmodified()

    def test_wrong_package_is_rejected_before_lookup_or_install(self):
        self.write_tracking(name="other-package")
        result = self.invoke("update", code=1)
        self.assertIn("another package", result.stderr)
        self.assertEqual(self.calls(), [])
        self.assert_unmodified()

    def test_curl_missing_and_http_failure_are_explicit(self):
        self.write_mode("http_failure")
        result = self.invoke("update", "--check", code=1)
        self.assertIn("exit 28", result.stderr)
        self.assertIn("timeout", result.stderr)
        (self.tools / "curl").unlink()
        result = self.invoke("update", "--check", code=1)
        self.assertIn("curl is required", result.stderr)
        self.assert_unmodified()

    def test_git_tag_is_verified_before_cargo_and_the_source_is_preserved(self):
        self.git_source()
        data = json.loads(self.invoke("update").stdout)
        self.assertEqual(data["status"], "updated")
        calls = self.calls()
        self.assertEqual([c[0] for c in calls], ["curl", "git", "cargo"])
        self.assertEqual(calls[1][-1], "refs/tags/v" + self.latest)
        self.assertIn("https://github.com/LIghtJUNction/zuse.git", calls[1])
        self.assertEqual(calls[2][calls[2].index("--tag") + 1], "v" + self.latest)
        self.assertNotIn("--registry", calls[2])

    def test_missing_official_git_tag_never_runs_cargo(self):
        self.git_source()
        self.write_mode("missing_tag")
        result = self.invoke("update", code=1)
        self.assertIn("official stable tag absent", result.stderr)
        self.assert_unmodified()

    def test_failed_install_is_not_reported_as_success(self):
        self.write_mode("cargo_failure")
        result = self.invoke("update", code=1)
        self.assertEqual(result.stdout, "")
        self.assertIn("exit 7", result.stderr)
        self.assertIn("simulated compilation failure", result.stderr)
        self.assertEqual(hashlib.sha256(self.exe.read_bytes()).hexdigest(), self.original_sha)

    def test_installed_version_is_verified_after_cargo_success(self):
        self.write_mode("wrong_version")
        result = self.invoke("update", code=1)
        self.assertIn("unexpected version", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_invalid_flags_and_read_only_apply_never_launch_tools(self):
        for args in (("schema", "--check"), ("update", "extra"), ("update", "--check", "--dry-run"), ("update", "--read-only")):
            with self.subTest(args=args):
                self.invoke(*args, code=1)
                self.assertEqual(self.calls(), [])

    @unittest.skipUnless(sys.platform == "linux", "descendant liveness receipt uses /proc")
    def test_cancel_stops_the_owned_cargo_process_and_descendant(self):
        self.write_mode("cargo_wait")
        child = subprocess.Popen([str(self.exe), "update"], env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            deadline = time.monotonic() + 5
            while not (self.state / "cargo.started").exists() and time.monotonic() < deadline:
                if child.poll() is not None:
                    self.fail(child.communicate())
                time.sleep(0.01)
            self.assertTrue((self.state / "cargo.started").exists())
            cargo_pid = int((self.state / "cargo.started").read_text())
            descendant = int((self.state / "child.pid").read_text())
            child.send_signal(signal.SIGINT)
            stdout, stderr = child.communicate(timeout=3)
            self.assertEqual(child.returncode, 1, (stdout, stderr))
            self.assertIn("cancelled", stderr)
            self.assertEqual(hashlib.sha256(self.exe.read_bytes()).hexdigest(), self.original_sha)
            for pid in (cargo_pid, descendant):
                try:
                    os.kill(pid, 0)
                except ProcessLookupError:
                    continue
                # A killed descendant can await the host's reaper briefly.
                stat = Path(f"/proc/{pid}/stat")
                self.assertTrue(stat.exists() and stat.read_text().split(")", 1)[1].split()[0] == "Z", f"owned child {pid} remains alive")
        finally:
            if child.poll() is None:
                child.kill()
                child.communicate(timeout=3)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    args, remaining = parser.parse_known_args()
    binary = args.binary.resolve(strict=True)
    version = subprocess.check_output([str(binary), "--version"], text=True, timeout=5).strip()
    if not version.startswith("zuse "):
        raise SystemExit("unexpected binary version identity")
    current = version.removeprefix("zuse ")
    major, minor, patch = map(int, current.split("-", 1)[0].split("+", 1)[0].split("."))
    UpdateCLI.binary = binary
    UpdateCLI.current = current
    UpdateCLI.latest = f"{major}.{minor}.{patch + 1}"
    unittest.main(argv=[sys.argv[0], *remaining])


if __name__ == "__main__":
    main()
