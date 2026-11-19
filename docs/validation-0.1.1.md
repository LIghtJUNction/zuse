# Validation — zuse 0.1.1

Recorded on 2026-11-20 (Asia/Taipei), Rust 1.100.0, x86_64 Linux and Hyprland 0.57.2. This patch follows actual physical Android and Linux tests. It also replaces duplicated native dynamic loaders with the exact optional Project Z dependency zsys 0.1.1.

## Automated and native checks

| Check | Result |
| --- | --- |
| Rust default suite | 74 passed: 39 module, 3 Android integration, 17 parser/motion, 9 runtime, 6 runtime/human regressions |
| Android acceptance-runner regression | 17 passed with all external commands mocked; no phone input |
| MCP transport integration | 12 passed, zero skips; real disposable servers and an isolated PNG screenshot |
| no_std core | Native and wasm32-unknown-unknown checks passed; no runtime dependencies when std is disabled |
| Code checks | rustfmt and Clippy with warnings denied passed |
| Windows | Full x86_64-pc-windows-gnu type check passed |
| macOS | Full aarch64-apple-darwin type check passed |
| Native X11 | Private Xvfb; exact foreground/background fixture input, pointer, click/scroll and PNG validation passed |
| Native AT-SPI | Private D-Bus/Xvfb GTK fixture: observation, edits, action and password redaction passed |
| Private Wayland/Fcitx | Actual GTK4 widgets received foreground Unicode and targeted background ASCII; PNG pixel checks passed |
| Shared Wayland desktop | Window metadata and pointer passed; keyboard stopped before effects because Num Lock was enabled |
| Physical Android | Foreground and fixture-owned background tests verified; detailed source-run accounting below |

The default Rust suite intentionally leaves two graphical integration tests ignored. AT-SPI was run explicitly in this patch. The Chromium desktop integration was already exercised for 0.1.0 and was not needlessly repeated for the loader-only changes; Android Chrome is a separate later feature.

## Physical Android evidence

The zero-permission test APK ran on an explicitly selected, authorized Android 16/API 36 physical device. Another attached device and pre-existing secondary displays were not targeted. The fixture contains only synthetic data and exposes its own diagnostic state through run-as.

Verified behavior included exact text, Backspace/Enter/Back, a native counter button, taps and timed swipes, all 95 printable ASCII characters, focused-window metadata, and image pixels. The primary PNG measured 1240 × 2772. The fixture's own-content secondary display measured 640 × 960.

Background input changed the secondary fixture's text, counter and gesture records while the primary fixture's text, counters, key history and actual gesture events remained unchanged. The background screenshot's four RGB samples matched the fixture. After the targeted run, the fixture was uninstalled, its display disappeared, the original display set remained and the original primary activity returned.

The final full run had 16 passed checks and 1 failed isolation comparison. That comparison included derived layout information; the exact earlier delta was not preserved. A later targeted run refined the assertion and passed 7 checks. The [published evidence](evidence/android-physical-2026-11-20.json) selects the latest verified result for each of 17 checks from these two runs on the same Android-tested binary. It preserves both source counts and does not claim that the unchanged full runner passed in one run.

The acceptance runner was then hardened through 17 mock regressions: pre-ADB APK identity/hash/permission checks, continued cleanup after timeout, cleanup after uncertain installation, preserving an external activity selection, rejecting unknown restoration state, deleting failed captures, distinguishing gesture events from derived layout, recursive identifier redaction, and private report permissions. These harness changes were not presented as a new physical-device run.

The [reviewed secondary-display image](assets/android-background-2026-11-20.png) contains only fixture data.

## Android fixes driven by these results

- Device selection now considers the complete inventory. Offline or unauthorized peers cannot silently turn an ambiguous selection into a ready emulator.
- A failed retarget clears the previous selected device.
- Key, text and component constraints are checked before a program's first effect.
- A zero shell exit status no longer hides recognized Android command failures written to stderr.
- Window focus is correlated with the correct display and window token. Unknown focus remains null.
- Virtual screenshot mapping requires a verified virtual logical display and a unique matching SurfaceFlinger virtual name. Physical and virtual IDs are kept distinct; ambiguous or prefix-only matches are rejected.
- A shared ADB I/O deadline covers connection, handshakes, partial reads/writes and the commands inside that operation. Trickle-response tests exercise this behavior.

## Linux findings and fixes

The original Wayland utility path could leave a temporary keyboard map in the client/input-method path. A first native replacement fixed foreground typing, but a private Fcitx test showed that targeted input could still interpret temporary codes with the original layout. The final implementation therefore uses two explicit mechanisms.

Foreground Unicode uses an owned native Wayland virtual keyboard, restores the captured original map before destruction, and verifies synchronization. Background text does not replace the map: libxkbcommon resolves the complete text to real plain/Shift keycodes before any dispatch. Unsupported characters are rejected before a prefix can be typed.

The current keyboard path requires inspectable Hyprland state, no held keys, Caps/Num off and group 0. A private persistent-keyboard fixture tested Caps, Num, group 1 and a held key. Each state returned unsupported_state with completed=0 and no result entries; then ordinary keys, without an extra modifier replay, demonstrated the original state was preserved. These checks used the pre-Cell synchronization binary; the final Cell callback was separately exercised in the latest actual Fcitx run.

The final shared-desktop run used the latest binary and found Num Lock enabled. Window observation and pointer movement passed. The first keyboard call was rejected before sending any key, the script stopped, and its windows, original focus and pointer were restored. Background keyboard and shared-desktop screenshot checks were not executed. This is a verified stop condition, not successful shared-desktop keyboard delivery.

See the [Linux evidence](evidence/linux-native-2026-11-20.json) for per-run hashes, exact outcomes, source hashes, cleanup and reproducible commands.

## Native submission semantics

Native event APIs do not acknowledge widget state. A server accepting XSendEvent does not establish that a GTK entry or another application consumed it. Keyboard results now include delivery=submitted and application_confirmation_required=true. The old typed_bytes field remains for compatibility and measures submitted bytes; submitted_bytes makes that meaning explicit.

Execution failures include error_code=unsupported_state, unsupported_character or execution_error as applicable. The readable error and completed-step history remain available.

## Reproduce

Use a dedicated Cargo target directory, distinct from cargo package verification:

~~~sh
CARGO_TARGET_DIR=target/native-validation cargo build --locked
python3 scripts/check.py
python3 -m unittest discover -s tests -p test_native_android.py -v
python3 tests/transports.py --binary target/native-validation/debug/zuse
python3 tests/native_x11.py
python3 tests/run_accessibility_native.py
python3 tests/native_wayland_private.py --with-ime --binary target/native-validation/debug/zuse
python3 tests/native_wayland_private.py --state-tests --binary target/native-validation/debug/zuse
~~~

Physical-device and shared-desktop acceptance are opt-in. Follow [native testing](native-testing.md) for fixture preparation, explicit target selection, output handling and cleanup.

## Scope and evidence review

| Claim | Evidence and limit |
| --- | --- |
| Android received the intended events | Fixture state and pixels on a physical Android 16 device; not a general claim for every OEM/device |
| Background Android stayed isolated | Owned secondary-display event records and unchanged primary fixture; product does not automatically provision displays |
| Wayland supports foreground Unicode | Actual private Hyprland/Fcitx input with an eligible keyboard state |
| Wayland supports background text | Actual current-map plain/Shift characters, including all 95 printable ASCII on the tested map; arbitrary Unicode is not promised |
| Locks are not silently cleared | Four private state-rejection cases and shared Num Lock preflight stop |
| Native dependencies are reused | zsys 0.1.1 pinned in Cargo.toml/Cargo.lock; X11, AT-SPI and Wayland use the shared ownership wrapper |
| Cross-platform support | Windows/macOS compilation passed; no live sessions for these operating systems |
| Other desktop actions | Buttons/wheel were exercised in isolated X11; not in these Wayland runs |
| Input accepted by an application | Requires an observation after submission; synthetic background events can be ignored |

No comparative speed or universal desktop-compatibility claim is made.
