# Native device acceptance testing

These opt-in tests exercise the installed operating system and an explicitly selected Android device. Unit tests and fake protocol servers remain useful for malformed responses, timeouts, and error paths; they do not establish that a real window received an event.

## Android

The [fixture](../tests/android-fixture/README.md) is a small native Java APK. It requests no Android permissions, has no network client, and shows only synthetic test data. The host test reads its private diagnostic files through `run-as`; it does not read another application's data.

Build prerequisites are JDK 17+, Android SDK platform 35 / build tools 35.0.0, and Python 3. The acceptance runner additionally needs Pillow and a locally running ADB server with the device already authorized.

```sh
python3 tests/build_android_fixture.py
cargo build --locked
python3 tests/native_android.py \
  --serial YOUR_EXPLICIT_SERIAL \
  --binary target/debug/zuse \
  --background
```

The serial is mandatory. The runner refuses to replace an existing installation of its fixture package. It installs its own fixture, runs the test, then force-stops and uninstalls that installation. Its optional secondary display is created by the fixture with PUBLIC, OWN_CONTENT_ONLY and PRESENTATION flags. It never selects an unrelated pre-existing secondary display and does not mirror the primary screen.

The runner checks:

- Native application launch, button state, exact text, Backspace/Enter/Back, tap and swipe delivery.
- All 95 printable ASCII characters, including shell metacharacters.
- Window scope, actual focus metadata, and PNG dimensions plus four known color samples.
- Unsupported input rejection during whole-program validation.
- Explicit secondary-display input while the primary fixture's text, counters and key history remain unchanged.
- Removal of the fixture and its owned display.

Foreground input stops when another primary activity takes over. Read-only background observation may continue only against the fixture-owned secondary display. The runner does not reclaim focus from a user who switches applications. Reports distinguish fixture removal, display cleanup, and whether the original primary activity is still the active one.

Outputs stay under the ignored `target/native-android` directory with private permissions. Reports omit the device serial and other applications' UI. Screenshot validation requires the expected fixture dimensions and instance-specific colors; an unexpected image is removed. Raw personal screenshots should never be copied into public test evidence.

## Linux

The Wayland acceptance fixture uses dedicated native GTK windows with unique identities. It compares delivered text and pointer events with fixture state, and validates screenshots against a known color patch. Tests verify the intended window before sending input and stop on focus changes.

A real shared desktop can receive events from several sources. An unexpected character is evidence of an uncontrolled or incorrectly mapped event, not proof that the operator typed it. Diagnose the event path before attributing it. In particular, virtual keyboards can install temporary keymaps, so a test should exercise background typing after foreground Unicode input.

Use a separate target directory for native validation. Do not share the executable output directory with `cargo package` verification, which can leave an executable built from the unpacked package source:

```sh
CARGO_TARGET_DIR=target/native-validation cargo build --locked
```

The acceptance reports must distinguish shared-desktop tests, isolated graphical fixtures, and cross-target compilation. Passing a compiler check does not establish live desktop compatibility.

## Evidence and references

Record the tested binary hash, version, platform/API version, delivered values, screenshot dimensions, failure behavior, and cleanup result. Keep measurement claims separate from inferred causes. A failed fixture observation can indicate a stale test-state record rather than a failed product input operation.

Android platform sources used when interpreting device output:

- [SurfaceFlinger display diagnostics](https://android.googlesource.com/platform/frameworks/native/+/refs/heads/main/services/surfaceflinger/SurfaceFlinger.cpp)
- [DisplayContent window focus diagnostics](https://android.googlesource.com/platform/frameworks/base/+/refs/heads/main/services/core/java/com/android/server/wm/DisplayContent.java)
- [Virtual display API](https://developer.android.com/reference/android/hardware/display/DisplayManager)

Release-specific results are recorded separately from this procedure.
