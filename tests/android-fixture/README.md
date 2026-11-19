# Android test fixture

This is a disposable, native Java app for testing zuse against synthetic UI content. The package is `best.lmm.zuse.fixture`, and the exported activity is `best.lmm.zuse.fixture/.MainActivity`. It requests no Android permissions, contains no network client, and reads no other application's data.

## Build

Use JDK 17 or newer and an installed Android SDK with platform `android-35` and build tools `35.0.0`. No Gradle, Maven, AndroidX, or downloads are involved.

```sh
python3 tests/build_android_fixture.py
```

SDK paths can be selected with `--sdk`, `--platform`, and `--build-tools`. The default SDK root is `ANDROID_SDK_ROOT`, then `ANDROID_HOME`, then `/opt/android-sdk`. The fixture targets API 35 and supports API 26 or newer.

The signed APK is `target/android-fixture/zuse-fixture.apk`. `build.json` records its SHA-256 and source hashes; `SHA256SUMS` contains the archive checksum. The disposable signing key and password also stay under this ignored directory, with mode 0600 on Unix. The builder checks that Git ignores the output directory, verifies the APK signature, and rejects requested permissions. It never installs the APK or contacts an Android device.

Java compiler, D8, and signer heaps are bounded to 256, 384, and 192 MiB respectively. Build steps run sequentially.

## Install and launch explicitly

These commands are for the operator. Substitute the selected, authorized serial. They are not run by the build script.

```sh
adb -s SERIAL install -r target/android-fixture/zuse-fixture.apk
adb -s SERIAL shell am start --display 0 -n best.lmm.zuse.fixture/.MainActivity
adb -s SERIAL shell run-as best.lmm.zuse.fixture cat files/display-0.json
```

Wait for `ready: true`, confirm `displayId`, and use the current `controls` coordinates. Pending layout prevents `ready` from becoming true. After tapping the input, wait for `ready`, `windowFocus`, and `controls.input.focused` before injecting text. Each launch creates a new document task and independent instance. The activity supports resizing and secondary displays; it does not select or take over any existing secondary display itself.

The fixture hides system bars in its own window and disables the soft keyboard for its input field. The screen contains a fixed heading, an EditText, Count/Reset/Exit buttons, colored pixel targets, and a gesture area. BACK is consumed and counted so a key test stays inside the fixture. Exit finishes the current fixture task. A system gesture can temporarily reveal Android's bars, so validate the current window before taking a display-wide screenshot.

## State files and coordinates

State is written atomically in the app's private `files` directory, normally within 24 ms of a UI change. The build deliberately marks this fixture debuggable so `run-as` can read that directory.

| File | Meaning |
| --- | --- |
| `display-N.json` | Active fixture instance on logical display N; focused instance wins, then the most recently created instance |
| `state-UUID.json` | One instance's state, retained after that instance is destroyed |
| `latest.json` | Most recently written state across all fixture instances; do not use it to identify a specific display |
| `instances.json` | Instance IDs, roles, logical display IDs, and state filenames |

The complete record shape is in [state.schema.json](state.schema.json). `controls.input`, `counter`, `reset`, `exit`, `gesture`, and `swatches` contain `x`, `y`, `width`, `height`, `centerX`, and `centerY`. These values come from `View.getLocationOnScreen` and measured view sizes, in pixels on that logical display. Observe again after layout, orientation, or display changes. Each control also reports `focused` and `visible`.

| Assertion | Fields |
| --- | --- |
| Input and selection | `text`, `selectionStart`, `selectionEnd`, `controls.input.focused` |
| Counter button | `counter` |
| Enter and BACK | `editorActions`, `backPresses` |
| Delivered key events | `keyDowns`, `keyUps`, bounded `keys` history including `eventTimeMs` |
| Input focus/touch diagnostics | `inputDiagnostics.focusChanges`, `lastFocusUptimeMs`, `touchEvents`, `lastTouchAction`, `lastTouchUptimeMs` |
| Tap or swipe in gesture area | `gesture.taps`, `swipes`, `lastKind`, `downX`, `downY`, `upX`, `upY`, `durationMs` |
| Suggested swipe coordinates | `gesture.suggestedSwipe` with `x1`, `y1`, `x2`, `y2` |
| Session and display | `instance`, `role`, `displayId`, `displayName`, `taskId`, `lifecycle`, `windowFocus` |
| PNG color samples | `pixelSamples`, each with screen `x`/`y` and exact `red`/`green`/`blue` values |

The first three color patches are red, green, and blue; the fourth is derived from the instance ID. `recipientDisplayId` records the display of the view that received a key or gesture, using public view APIs. It does not claim access to Android's hidden event-display metadata. The gesture area records gestures only inside that view. Input text is bounded to 512 characters and the key history to 24 events.

For a dump without reading a file:

```sh
adb -s SERIAL shell dumpsys activity best.lmm.zuse.fixture/.MainActivity
```

The activity emits `ZUSE_FIXTURE_STATE ` followed by one compact JSON record per panel. Android can add its own process/task envelope around these lines. Use `run-as` when an unambiguous JSON file is preferable.

## Optional owned offscreen display

The default launch creates no virtual display. This explicit option creates one public, own-content-only display backed by an ImageReader surface owned by the fixture process:

```sh
adb -s SERIAL shell am start --display 0 -n best.lmm.zuse.fixture/.MainActivity --ez virtual_display true
adb -s SERIAL shell run-as best.lmm.zuse.fixture cat files/display-0.json
```

Read `ownedVirtualDisplayId` and `virtualError` from the new owner instance. A successful creation reports a nonnegative ID and an empty error. Then read `files/display-ID.json` and confirm `role: "presentation"`, `ready: true`, and its actual display ID before using zuse's background mode. The fixture uses its own Presentation and does not move another app or reuse another display.

The creation flags are exactly `PUBLIC | OWN_CONTENT_ONLY | PRESENTATION`; the public flag allows shell discovery while the own-content-only flag prevents mirroring when the display has no windows. A device can still reject creation; `SecurityException` is recorded rather than triggering a permission request.

The display name begins with `zuse-fixture-`. The default size is 640 by 960 pixels at 160 dpi. Optional integer extras are `virtual_width` (320–1024), `virtual_height` (480–1280), and `virtual_dpi` (120–320). Creation errors are recorded; the fixture never requests MediaProjection, capture, overlay, input-injection, or system privileges as a fallback.

The fixture drains and discards frames from its own render surface. It does not mirror or read the primary display. The virtual display remains owned by the creating activity until Exit, activity destruction, or process termination. The Presentation's Exit button releases only that owned display. Whether a particular Android version permits shell input, app launch, or external screenshot mapping on this owned public display must be established by the device test; creation alone does not prove those capabilities.

## Cleanup

```sh
adb -s SERIAL shell am force-stop best.lmm.zuse.fixture
adb -s SERIAL uninstall best.lmm.zuse.fixture
```

Force-stop or uninstall ends the fixture process and its owned display. Remove `target/android-fixture` to discard local build artifacts and signing identity. A fresh identity requires uninstalling the old test APK before reinstalling it. Preserve the operator's original task/display and restore it after the test.

## Android API references

- [Activity dump](https://developer.android.com/reference/android/app/Activity#dump(java.lang.String,java.io.FileDescriptor,java.io.PrintWriter,java.lang.String%5B%5D)) provides the explicit activity-state output hook.
- [Activity manifest](https://developer.android.com/guide/topics/manifest/activity-element#dlmode) describes the independent document tasks used by this fixture.
- [DisplayManager](https://developer.android.com/reference/android/hardware/display/DisplayManager) defines public/private displays, own-content-only rendering, creation flags, and lifecycle.
- [AAPT2](https://developer.android.com/tools/aapt2) and [command-line builds](https://developer.android.com/build/building-cmdline) describe the resource, DEX, alignment, and APK-signing pipeline.
