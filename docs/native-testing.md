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

The serial is mandatory. Before its first ADB call, the runner verifies the APK hash against its build record, source hashes, package/activity identity, and absence of requested permissions using aapt2. The runner refuses to replace an existing installation of its fixture package. It installs its own fixture, runs the test, then force-stops and uninstalls that installation. Its optional secondary display is created by the fixture with PUBLIC, OWN_CONTENT_ONLY and PRESENTATION flags. It never selects an unrelated pre-existing secondary display and does not mirror the primary screen.

The runner checks:

- Native application launch, button state, exact text, Backspace/Enter/Back, tap and swipe delivery.
- All 95 printable ASCII characters, including shell metacharacters.
- Window scope, actual focus metadata, and PNG dimensions plus four known color samples.
- Unsupported input rejection during whole-program validation.
- Explicit secondary-display input while the primary fixture's text, counters and key history remain unchanged.
- Removal of the fixture and its owned display.

Foreground input stops when another primary activity takes over. Read-only background observation may continue only against the fixture-owned secondary display. The runner does not reclaim focus from a user who switches applications. Reports distinguish fixture removal, display cleanup, and whether the original primary activity is still the active one. A timeout during installation still triggers cleanup of the previously absent, verified fixture package. Cleanup attempts each independent step even if an earlier command fails, and reports whether the original activity returned or an external activity selection was preserved. Unknown activity state is never accepted as successful restoration.

Outputs stay under the ignored `target/native-android` directory with private permissions. Reports omit the device serial and other applications' UI. Primary screenshot validation checks both fixture state and the system's focused window before and after capture. Screenshot validation requires the expected fixture dimensions and instance-specific colors; an unexpected or partially written image is removed, including when the capture command fails. Raw personal screenshots should never be copied into public test evidence.

## Linux

Successful native key submission is not an application acknowledgement. X11 XSendEvent can be accepted by the X server while the target widget ignores the synthetic event. Keyboard results expose delivery=submitted and application_confirmation_required=true; typed_bytes is retained for compatibility and counts submitted UTF-8 bytes. Verify application state through accessibility, a fixture record, or an observation before assuming text changed. Use an explicit foreground operation when a target application cannot accept background events.

The Wayland acceptance fixture uses dedicated native GTK windows with unique identities. It compares delivered text and pointer events with fixture state, and validates screenshots against a known color patch. Tests verify the intended window before sending input and stop on focus changes.

A real shared desktop can receive events from several sources. An unexpected character is evidence of an uncontrolled or incorrectly mapped event, not proof that the operator typed it. Diagnose the event path before attributing it. In particular, virtual keyboards can install temporary keymaps, so a test should exercise background typing after foreground Unicode input.

Use a separate target directory for native validation. Do not share the executable output directory with `cargo package` verification, which can leave an executable built from the unpacked package source:

```sh
CARGO_TARGET_DIR=target/native-validation cargo build --locked
```

The acceptance reports must distinguish shared-desktop tests, isolated graphical fixtures, and cross-target compilation. Passing a compiler check does not establish live desktop compatibility.

### Window capture without Python

The [private Cage builder](../tests/build_private_cage.sh) and [capture runner](../tests/run_native_wayland_capture.sh) reproduce the Rust [window-identity fixture](../tests/native_wayland_capture.rs) without Python or a connection to the host desktop. Use installed Bash/GNU tools, `cc`, `pkg-config`, Git, `jq`, Rust/Cargo, wlroots **0.20.x** development files, Wayland server and xkbcommon development files, Hyprland, D-Bus, and the GTK 3/Cairo runtime libraries. The headless parent also needs working GL/render-node support. Missing support fails explicitly; the runner does not fall back to the host display.

Download the pinned source archive in the sandbox first, then transfer only that small archive to the development machine. The reviewed Cage v0.3.1 archive is **25,363 bytes**. For example, run this download and hash check in the sandbox:

```sh
curl --fail --location --proto '=https' --tlsv1.2 \
  https://codeload.github.com/cage-kiosk/cage/tar.gz/refs/tags/v0.3.1 \
  --output cage-v0.3.1.tar.gz
printf '%s  %s\n' \
  6dc1619665acd367e0174c93b234002549a66f55f1de9197d67f0305415babc8 \
  cage-v0.3.1.tar.gz | sha256sum --check -
```

Place the transferred archive at `target/native-validation/sources/cage-v0.3.1.tar.gz`. From the repository root on the development machine:

```sh
mkdir -p target/native-validation
zuse_capture_work=$(mktemp -d "$PWD/target/native-validation/capture-XXXXXX")
bash tests/build_private_cage.sh \
  --source target/native-validation/sources/cage-v0.3.1.tar.gz \
  --output "$zuse_capture_work/cage"

CARGO_TARGET_DIR=target/native-validation cargo test --locked --features vision \
  --test native_wayland_capture --no-run --message-format=json \
  > "$zuse_capture_work/cargo.jsonl"
zuse_capture_test=$(jq -ers '
  [.[] | select(.reason == "compiler-artifact"
    and .target.name == "native_wayland_capture" and .profile.test == true)
    | .executable // empty] | unique
  | if length == 1 then .[0] else error("expected one capture test executable") end
' "$zuse_capture_work/cargo.jsonl")
bash tests/run_native_wayland_capture.sh \
  "$zuse_capture_test" "$zuse_capture_work/cage/build/cage" \
  "$(command -v Hyprland)" 1.25 "$zuse_capture_work/logs"
```

The builder accepts an existing local archive and a new output directory. It verifies the archive hash and upstream commit, then changes only Cage's requested XDG-shell protocol version from 5 to 6. It compiles with installed native tools; it performs no download, system installation, Meson invocation, or compositor launch. The runner subsequently creates its own headless Cage parent, nested Hyprland, D-Bus, and two synthetic GTK windows. An optional sixth runner argument selects an explicitly chosen `/dev/dri/renderDNUMBER` render node; physical input devices and KMS card descriptors remain forbidden in the inspected compositor processes.

`1.25` is the **requested** output scale. The fixture checks actual image dimensions against `logicalRegion`, checks both returned pixel/logical ratios, and requires window scale to remain stable across the focus transition. It does **not** assert that the effective ratio must equal 1.25. A passing result must not be reported as a separate proof of that exact fractional scale.

The fixture requires successful captures of two differently colored owned windows, checks identity across focus changes without coordinate-based selection, and rejects a closed window ID while preserving an existing destination file. A background capture must succeed when the observed placement and monitor are unchanged; an observed placement change permits a bounded explicit failure that preserves the file. Keep this branch distinction when interpreting a passing run.

Build products, source archives, images, and raw logs stay under ignored `target/` directories. Do not add them to Git. The [2026-11-22 evidence](evidence/native-window-capture-20261122.json) records binary/source hashes and the passing test summary for an **uncommitted candidate**, not a release. It also retains the exact isolation message, runner exit code, and the fact that `cleanup.log` was empty; an empty cleanup log alone is not evidence of additional cleanup assertions.

## Evidence and references

Record the tested binary hash, version, platform/API version, delivered values, screenshot dimensions, failure behavior, and cleanup result. Keep measurement claims separate from inferred causes. A failed fixture observation can indicate a stale test-state record rather than a failed product input operation.

Android platform sources used when interpreting device output:

- [SurfaceFlinger display diagnostics](https://android.googlesource.com/platform/frameworks/native/+/refs/heads/main/services/surfaceflinger/SurfaceFlinger.cpp)
- [DisplayContent window focus diagnostics](https://android.googlesource.com/platform/frameworks/base/+/refs/heads/main/services/core/java/com/android/server/wm/DisplayContent.java)
- [Virtual display API](https://developer.android.com/reference/android/hardware/display/DisplayManager)

Release-specific results are recorded separately from this procedure. The [physical Android evidence](evidence/android-physical-2026-11-20.json) retains the source run counts and identifies which checks were verified by a later targeted rerun; it does not present merged results as a single unchanged passing run.

## Mobile browsers and peripheral fixtures

Android Chrome now has a separate owned-tab acceptance runner. It uses an offline
page, checks real DOM/input/AX/image state, and preserves private target identities.
See [mobile browser testing](mobile-browser.md#reproduce) for explicit serial and
optional readiness activation. The final evidence retains prior failures rather
than merging them into a fictitious unchanged passing run.

Linux serial tests use owned PTYs; USB tests compile an isolated C libusb fixture
and verify its identity before sending transfers. Neither test accepts a physical
peripheral path or opens an unidentified USB device. Run instructions and exact
limits are in [device I/O](device-io.md) and [0.2.0 validation](validation-0.2.0.md).
