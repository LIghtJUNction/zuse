#!/usr/bin/env python3
"""Offline regressions for the native Android acceptance runner.

Every external process is blocked unless a test replaces its exact boundary.
The APK, device serial, activity names, state, and screenshot bytes are synthetic.
Run with: python3 -m unittest discover -s tests -p test_native_android.py -v
"""
from contextlib import redirect_stdout
from copy import deepcopy
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import stat
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch


_spec = importlib.util.spec_from_file_location(
    "zuse_native_android_acceptance", Path(__file__).with_name("native_android.py")
)
native = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(native)


class AcceptanceRegressionTests(unittest.TestCase):
    SERIAL = "ZUSE-SYNTHETIC-SERIAL-ONLY"
    ORIGINAL = "example.original/.Main"
    EXTERNAL = "example.other/.Main"

    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="zuse-android-mock-")
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        self.binary = self.directory / "zuse"
        self.binary.write_bytes(b"synthetic binary; never execute\n")
        self.apk = self.directory / "fixture.apk"
        self.apk.write_bytes(b"synthetic APK; never install\n")
        self.args = SimpleNamespace(
            serial=self.SERIAL,
            binary=str(self.binary),
            apk=str(self.apk),
            aapt2="zuse-mock-aapt2",
            output=str(self.directory / "report"),
            label="mock",
            background=False,
            only_background=False,
        )
        self.acceptance = native.Acceptance(self.args)
        process_guard = patch.object(
            native.subprocess,
            "run",
            side_effect=AssertionError("external processes are forbidden in this test"),
        )
        self.processes = process_guard.start()
        self.addCleanup(process_guard.stop)

    @staticmethod
    def completed(stdout="", stderr="", returncode=0):
        return subprocess.CompletedProcess(["mock"], returncode, stdout, stderr)

    def read_report(self):
        return json.loads((self.acceptance.output / "report.json").read_text())

    def fixture_metadata(self):
        root = self.directory / "fixture-source"
        names = [
            "tests/android-fixture/AndroidManifest.xml",
            "tests/android-fixture/src/best/lmm/zuse/fixture/MainActivity.java",
        ]
        hashes = {}
        for name in names:
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("synthetic source for " + name + "\n")
            hashes[name] = hashlib.sha256(path.read_bytes()).hexdigest()
        metadata = {
            "package": native.PACKAGE,
            "component": native.COMPONENT,
            "sha256": hashlib.sha256(self.apk.read_bytes()).hexdigest(),
            "permissions": [],
            "sources": hashes,
        }
        return root, metadata

    def write_metadata(self, metadata):
        (self.apk.parent / "build.json").write_text(json.dumps(metadata))

    def aapt(self, argv, **_kwargs):
        self.assertEqual(argv[0], self.args.aapt2)
        if argv[1:3] == ["dump", "badging"]:
            return self.completed(
                "package: name='" + native.PACKAGE + "' versionCode='1'\n"
                "launchable-activity: name='" + native.PACKAGE + ".MainActivity'\n"
            )
        if argv[1:3] == ["dump", "permissions"]:
            return self.completed("package: " + native.PACKAGE + "\n")
        self.fail("unexpected manifest inspection command: " + repr(argv))

    def cleanup_state(self, activity=None):
        acceptance = self.acceptance
        acceptance.original_activity = self.ORIGINAL
        acceptance.initial_displays = {0, 5}
        acceptance.owned_display = 7
        acceptance.install_attempted = True
        acceptance.installed = True
        active = self.ORIGINAL if activity is None else activity
        acceptance.top_activity = Mock(side_effect=[active, active])
        acceptance.displays = Mock(return_value={0, 5})
        acceptance.adb = Mock(return_value=self.completed())
        return acceptance

    @staticmethod
    def primary_state():
        return {
            "counter": 2, "text": "fixture text", "keyDowns": 3, "keyUps": 3,
            "windowFocus": True, "ready": True,
            "gesture": {
                "taps": 1, "swipes": 2, "moves": 3, "cancelled": 0,
                "lastKind": "swipe", "durationMs": 240, "recipientDisplayId": 0,
                "downX": 20, "downY": 100, "upX": 80, "upY": 100,
                "suggestedSwipe": {"x1": 20, "y1": 100, "x2": 80, "y2": 100},
            },
        }

    def test_adb_always_uses_explicit_serial(self):
        self.acceptance.command = Mock(return_value=self.completed())
        self.acceptance.adb("shell", "getprop", "ro.build.version.sdk", timeout=2)
        self.acceptance.command.assert_called_once_with(
            ["adb", "-s", self.SERIAL, "shell", "getprop", "ro.build.version.sdk"],
            timeout=2,
        )
        self.processes.assert_not_called()

    def test_apk_hash_failure_is_rejected_before_any_adb(self):
        root, metadata = self.fixture_metadata()
        metadata["sha256"] = "0" * 64
        self.write_metadata(metadata)
        self.acceptance.adb = Mock()
        self.acceptance.command = Mock()
        with patch.object(native, "ROOT", root), redirect_stdout(io.StringIO()):
            self.assertEqual(self.acceptance.run(), 1)
        self.acceptance.adb.assert_not_called()
        self.acceptance.command.assert_not_called()
        report = self.read_report()
        self.assertEqual(report["failed"], 1)
        self.assertFalse(report["fixture_install_attempted"])

    def test_apk_manifest_identity_failure_is_rejected_before_any_adb(self):
        root, metadata = self.fixture_metadata()
        self.write_metadata(metadata)
        self.acceptance.adb = Mock()
        self.acceptance.command = Mock(side_effect=lambda argv, **kwargs: (
            self.completed("package: name='example.wrong'\n"
                           "launchable-activity: name='example.wrong.Main'\n")
            if argv[1:3] == ["dump", "badging"] else self.aapt(argv, **kwargs)
        ))
        with patch.object(native, "ROOT", root), redirect_stdout(io.StringIO()):
            self.assertEqual(self.acceptance.run(), 1)
        self.acceptance.adb.assert_not_called()
        self.assertTrue(self.acceptance.command.called, "must reach manifest inspection")
        self.assertFalse(self.read_report()["fixture_install_attempted"])

    def test_verified_fixture_has_no_permissions_and_matches_sources(self):
        root, metadata = self.fixture_metadata()
        self.write_metadata(metadata)
        self.acceptance.command = Mock(side_effect=self.aapt)
        self.acceptance.adb = Mock()
        with patch.object(native, "ROOT", root):
            result = self.acceptance.verify_apk()
        self.assertEqual(result["package"], native.PACKAGE)
        self.assertEqual(result["permissions"], [])
        self.assertTrue(result["source_hashes_verified"])
        self.acceptance.adb.assert_not_called()
        self.processes.assert_not_called()

    def test_added_permission_is_rejected_before_any_adb(self):
        root, metadata = self.fixture_metadata()
        self.write_metadata(metadata)
        self.acceptance.adb = Mock()
        self.acceptance.command = Mock(side_effect=lambda argv, **kwargs: (
            self.completed("uses-permission: name='android.permission.INTERNET'\n")
            if argv[1:3] == ["dump", "permissions"] else self.aapt(argv, **kwargs)
        ))
        with patch.object(native, "ROOT", root), redirect_stdout(io.StringIO()):
            self.assertEqual(self.acceptance.run(), 1)
        self.acceptance.adb.assert_not_called()
        self.assertFalse(self.read_report()["fixture_install_attempted"])

    def test_existing_installation_is_never_replaced_or_uninstalled(self):
        self.acceptance.verify_apk = Mock(return_value={"package": native.PACKAGE})
        self.acceptance.adb = Mock(
            return_value=self.completed("package:" + native.PACKAGE + "\n")
        )
        with redirect_stdout(io.StringIO()):
            self.assertEqual(self.acceptance.run(), 1)
        self.acceptance.adb.assert_called_once_with(
            "shell", "pm", "list", "packages", native.PACKAGE
        )
        self.assertFalse(self.read_report()["fixture_install_attempted"])

    def test_install_timeout_still_uninstalls_and_saves_redacted_report(self):
        acceptance = self.acceptance
        acceptance.verify_apk = Mock(return_value={"package": native.PACKAGE})
        acceptance.top_activity = Mock(return_value=self.ORIGINAL)
        acceptance.displays = Mock(return_value={0, 5})
        acceptance.command = Mock(return_value=self.completed("zuse test\n"))
        acceptance.zuse = Mock()

        def adb(*args, **_kwargs):
            if args[0] == "install":
                raise subprocess.TimeoutExpired(
                    ["adb", "-s", self.SERIAL, "install", str(self.apk)], 90
                )
            if args == ("shell", "getprop", "ro.build.version.sdk"):
                return self.completed("36\n")
            if args in [
                ("shell", "pm", "list", "packages", native.PACKAGE),
                ("shell", "am", "force-stop", native.PACKAGE),
                ("uninstall", native.PACKAGE),
            ]:
                return self.completed()
            self.fail("unexpected mock ADB operation: " + repr(args))

        acceptance.adb = Mock(side_effect=adb)
        output = io.StringIO()
        with redirect_stdout(output):
            self.assertEqual(acceptance.run(), 1)
        acceptance.adb.assert_any_call("uninstall", native.PACKAGE, check=False)
        acceptance.zuse.assert_not_called()
        report = self.read_report()
        self.assertTrue(report["fixture_install_attempted"])
        self.assertFalse(acceptance.installed)
        self.assertEqual({item["name"]: item["status"] for item in report["tests"]},
                         {"setup": "failed", "cleanup": "passed"})
        self.assertNotIn(self.SERIAL, output.getvalue())
        self.assertNotIn(self.SERIAL, json.dumps(report))
        self.assertIn("<device>", json.dumps(report))
        self.processes.assert_not_called()

    def test_cleanup_continues_after_separate_command_failures(self):
        acceptance = self.cleanup_state()
        acceptance.top_activity = Mock(side_effect=[native.COMPONENT, self.ORIGINAL])

        def adb(*args, **_kwargs):
            if args[:3] == ("shell", "am", "force-stop"):
                raise subprocess.TimeoutExpired(["adb", "-s", self.SERIAL], 1)
            if args[0] == "uninstall":
                return self.completed(stderr="mock uninstall failed", returncode=1)
            return self.completed("package:" + native.PACKAGE + "\n")

        acceptance.adb = Mock(side_effect=adb)
        acceptance.displays = Mock(side_effect=RuntimeError("mock display query failed"))
        result = acceptance.cleanup()
        acceptance.adb.assert_any_call("uninstall", native.PACKAGE, check=False)
        acceptance.adb.assert_any_call("shell", "pm", "list", "packages", native.PACKAGE)
        acceptance.displays.assert_called_once()
        self.assertEqual(acceptance.top_activity.call_count, 2)
        self.assertEqual(result["status"], "failed")
        self.assertEqual({error["step"] for error in result["command_errors"]},
                         {"force_stop", "uninstall", "verify_displays"})
        self.assertFalse(result["fixture_uninstalled"])
        self.assertFalse(result["owned_display_removed"])

    def test_cleanup_preserves_an_external_activity_without_starting_anything(self):
        acceptance = self.cleanup_state(self.EXTERNAL)
        result = acceptance.cleanup()
        self.assertEqual(result["status"], "passed")
        self.assertEqual(result["activity_policy"], "preserve_external")
        self.assertTrue(result["external_activity_preserved"])
        self.assertFalse(result["original_primary_activity_restored"])
        self.assertEqual([call.args for call in acceptance.adb.call_args_list], [
            ("shell", "am", "force-stop", native.PACKAGE),
            ("uninstall", native.PACKAGE),
            ("shell", "pm", "list", "packages", native.PACKAGE),
        ])

    def test_cleanup_unknown_activity_is_not_a_pass(self):
        acceptance = self.cleanup_state()
        acceptance.top_activity = Mock(return_value=None)
        result = acceptance.cleanup()
        self.assertEqual(result["activity_policy"], "unknown")
        self.assertEqual(result["status"], "failed")
        self.assertFalse(result["activity_selection_preserved"])

    def test_cleanup_missing_original_display_is_not_a_pass(self):
        acceptance = self.cleanup_state()
        acceptance.displays = Mock(return_value={0})
        result = acceptance.cleanup()
        self.assertEqual(result["status"], "failed")
        self.assertFalse(result["original_display_set_retained"])

    def test_capture_error_deletes_partial_png(self):
        acceptance = self.acceptance
        acceptance.guard = Mock(return_value=self.primary_state())
        acceptance.primary_fixture_focused = Mock(return_value=True)
        path = acceptance.output / "fixture-display-0.png"

        def capture(*_args, **_kwargs):
            path.write_bytes(b"\x89PNG\r\n\x1a\npartial synthetic data")
            raise RuntimeError("mock capture transport failure")

        acceptance.zuse = Mock(side_effect=capture)
        with self.assertRaisesRegex(RuntimeError, "capture transport"):
            acceptance.screenshot()
        self.assertFalse(path.exists())
        acceptance.zuse.assert_called_once()

    def test_capture_focus_loss_deletes_returned_png(self):
        acceptance = self.acceptance
        before = self.primary_state()
        after = deepcopy(before)
        after["windowFocus"] = False
        acceptance.guard = Mock(side_effect=[before, after])
        acceptance.primary_fixture_focused = Mock(return_value=True)
        path = acceptance.output / "fixture-display-0.png"
        acceptance.zuse = Mock(side_effect=lambda *_args, **_kwargs: path.write_bytes(b"synthetic"))
        with self.assertRaisesRegex(RuntimeError, "focus changed"):
            acceptance.screenshot()
        self.assertFalse(path.exists())

    def test_either_missing_focus_signal_blocks_screenshot(self):
        for state_focus, observed_focus in [(False, True), (True, False)]:
            with self.subTest(state_focus=state_focus, observed_focus=observed_focus):
                state = self.primary_state()
                state["windowFocus"] = state_focus
                self.acceptance.guard = Mock(return_value=state)
                self.acceptance.primary_fixture_focused = Mock(return_value=observed_focus)
                self.acceptance.zuse = Mock()
                with self.assertRaisesRegex(RuntimeError, "exclusive focus"):
                    self.acceptance.screenshot()
                self.acceptance.zuse.assert_not_called()

    def isolate(self, before, after):
        acceptance = self.acceptance
        acceptance.owned_display = 7
        acceptance.guard = Mock(side_effect=[before, after])
        acceptance.button = Mock(return_value={"display": 7})
        acceptance.text = Mock(return_value={"display": 7})
        acceptance.gestures = Mock(return_value={"display": 7})
        return acceptance.background_isolation()

    def test_primary_layout_changes_do_not_count_as_input(self):
        before = self.primary_state()
        after = deepcopy(before)
        for name in ["downX", "downY", "upX", "upY"]:
            after["gesture"][name] += 30
        after["gesture"]["suggestedSwipe"]["y1"] += 30
        result = self.isolate(before, after)
        self.assertTrue(result["primary_input_unchanged"])
        self.assertTrue(result["primary_layout_changed"])
        self.acceptance.button.assert_called_once_with(7)
        self.acceptance.text.assert_called_once_with(7)
        self.acceptance.gestures.assert_called_once_with(7)

    def test_each_real_gesture_event_change_fails_isolation(self):
        for name in ["taps", "swipes", "moves", "cancelled", "lastKind",
                     "durationMs", "recipientDisplayId"]:
            with self.subTest(field=name):
                before = self.primary_state()
                after = deepcopy(before)
                value = after["gesture"][name]
                after["gesture"][name] = "cancel" if isinstance(value, str) else value + 1
                with self.assertRaisesRegex(AssertionError, "primary gesture events"):
                    self.isolate(before, after)

    def test_success_error_cleanup_and_report_output_redact_serial_recursively(self):
        acceptance = self.cleanup_state()
        acceptance.report["label"] = self.SERIAL
        acceptance.adb = Mock(side_effect=[
            subprocess.TimeoutExpired(["adb", "-s", self.SERIAL], 1),
            self.completed(),
            self.completed(),
        ])
        output = io.StringIO()
        with redirect_stdout(output):
            self.assertTrue(acceptance.record(
                "nested_success", lambda: {self.SERIAL: [self.SERIAL, {"value": self.SERIAL}]}
            ))
            def failure():
                raise RuntimeError("mock transport failed for " + self.SERIAL)
            self.assertFalse(acceptance.record("operation_error", failure))
            acceptance.results.append(acceptance.cleanup())
            acceptance.save_report()
        report_text = (acceptance.output / "report.json").read_text()
        self.assertNotIn(self.SERIAL, report_text)
        self.assertNotIn(self.SERIAL, output.getvalue())
        self.assertIn("<device>", report_text)
        self.assertEqual(stat.S_IMODE((acceptance.output / "report.json").stat().st_mode), 0o600)
        self.assertTrue(self.read_report()["fixture_install_attempted"])
        self.processes.assert_not_called()


if __name__ == "__main__":
    unittest.main()
