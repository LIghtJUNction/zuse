# 0.2.0 validation — 2026-11-20

This release adds Android Chrome, an iOS/WDA adapter, native serial and USB
interfaces, mobile MCP images, and macOS permission diagnostics. The host Rust
dependency tree remains the same six Project Z crates; zsys 0.1.1 supplies the
shared dynamic-library boundary. The no_std parser/motion core remains
allocation-free and dependency-free.

## Results and scope

| Validation | Result | What it establishes |
| --- | --- | --- |
| Rust default suite (Linux) | 109 passed, 2 graphical tests ignored by default | Parser/runtime, 15 Android-browser protocol tests, 20 iOS tests, existing Android/human/process regressions |
| Physical Android Chrome | 13 checks passed | Owned background tab on Android 16/API 36, Chrome 157.0.8103.50 |
| Native desktop Chromium | Explicit ignored test passed | Isolated profile, Unicode, input, AX/text and 640×480 PNG |
| Native serial | 5 groups passed | Real Linux PTY kernel configuration, data, partial writes, timeout and restoration |
| USB C ABI | 11 groups passed | Isolated fake libusb, transfer buffers and partial/error/cleanup semantics |
| Real read-only device inventory | Passed | 32 serial candidate paths and 9 USB descriptors; no device opened or interface claimed |
| MCP transports | 12 passed, no skips | stdio, HTTP, Unix socket, constraints, human handoff and native PNG content |
| Formatting / Clippy | Passed with warnings denied | Current native code and tests |
| Windows compilation | x86_64-pc-windows-gnu Clippy/all-targets passed | Compile validation, not a live Windows desktop |
| macOS compilation | aarch64 and x86_64 apple-darwin Clippy/all-targets passed | Compile validation, not live Mac permissions or input |
| no_std | Native and wasm32-unknown-unknown checks passed | Host adapters remain behind std |

The default Rust count includes the 15 browser and 20 WDA protocol tests; they
are not added a second time. AT-SPI's graphical test remains ignored by the
default invocation and was explicitly exercised in the preceding
[0.1.1 acceptance](validation-0.1.1.md). The desktop Chromium test was explicitly
run again after the mobile browser fix.

Native fixtures run on the development machine. Windows/macOS checks are cross
compilation. iOS uses a protocol fixture because no iPhone was available.
USB transfer tests use a C library fixture; the physical system check only
enumerates descriptors. No unidentified physical serial or USB device received
test traffic.

## Physical Android browser iteration

The first actual runs failed at Chrome HTTP discovery. Socket discovery alone
was not a sufficient readiness check. A separate diagnostic brought Chrome to
the foreground, confirmed its activity, and compared the native ADB stream
with an independently managed temporary adb forward; the diagnostic forward
was removed afterward.

The decisive trace showed HTTP 200 and a complete 447-byte version body,
followed by an open stream. The client had waited for EOF instead of honoring
Content-Length. The code now reads exactly the declared body under the same
absolute deadline and closes its own discovery tunnel. Regression fixtures
cover a connection kept alive through the later CDP session, truncated bodies,
duplicate/conflicting/invalid lengths and unsupported framing.

The final complete run passed 13 checks using binary SHA-256:

~~~text
852655fa9a0441dd2220e80a07b5dd613d75792478bae8c13c31e78c178b654e
~~~

The binary reports 0.1.1 because it was built before the release version bump.
It contains the 0.2.0 candidate adapters and framing fix. The evidence records
the tested source revision and binary hash independently of the final package
version.

The fixture temporarily activated Chrome, restored the original primary
activity, and then performed navigation/input/capture in its owned tab while
the original activity stayed in front. It verified zuse 中文 ✓ 🙂, an Enter
event, a trusted click with counter=1, 16 AX nodes with the fixture password
value omitted, rendered text, a 640×480 PNG and exact RGB [32,160,96].
Foreign target selection was rejected without losing the owned target.
Target closure was acknowledged and foreground restoration was observed.

The shell's final exit code is intentionally 1 because the test deliberately
attempts a forbidden foreign selection. The overall test exits 0 only when
that expected rejection and all success/cleanup checks pass.

[Raw sanitized evidence](evidence/android-chrome-2026-11-20.json) retains prior
failed runs separately from the final full passing run. [The image](assets/android-chrome-2026-11-20.png)
contains only the offline fixture.

## Serial and USB iteration

The serial test transmits all 256 byte values in both directions through an
owned PTY and checks raw 8N1/115200 against the kernel. Empty reads honor the
requested timeout. Explicit close, Drop and a failed write restore captured
termios settings. A 32768-byte backpressure write timed out after 15360 bytes;
reported progress matched bytes observed at the peer and subsequent steps
did not execute.

USB checks use a C fixture compiled against the installed libusb headers.
A read-only enumeration probe confirms that the child loaded the fake library
before any open or transfer test. The fixture checks exact selection, busy
interfaces without driver detach, bulk/interrupt/control buffers, partial
writes without retries, partial timeout data, invalid native counts and
release/close/exit/unload ordering.

Initial harness failures are retained: preflight errors were correctly emitted
on stderr, but the harness expected stdout; the first serial backpressure
payload exceeded the DSL string limit after base64 encoding. Those harness
assumptions were corrected, then the full 5 and 11 groups passed. The device
implementations did not require a change for those two failures.

See [device evidence](evidence/device-io-2026-11-20.json) for separate binary
fingerprints and [device I/O semantics](device-io.md) for transfer and cleanup
limits. libusb transfer timeouts do not impose a hard wall-clock bound on
enumeration, open or interface release.

## Linux and Android display coverage carried forward

The [0.1.1 validation](validation-0.1.1.md) remains the source for physical Android
foreground/secondary-display tests and native Linux X11, AT-SPI and
Hyprland/Fcitx tests. Those desktop/input implementation files are unchanged
by the mobile/device additions.

The shared desktop test observed real windows and pointer operations.
Keyboard input was rejected before effects because Num Lock was enabled.
Private Hyprland sessions verified foreground Unicode, background ASCII,
input-method interaction, PNG pixels and unsupported lock/layout/held-key
states. Do not describe the shared desktop keyboard step as successful input.

## Reproduction

~~~sh
python3 scripts/check.py
cargo test --locked --test browser_native -- --ignored
python3 tests/transports.py --binary target/debug/zuse
python3 tests/native_serial.py --binary target/debug/zuse --output target/native-serial
python3 tests/native_usb.py --binary target/debug/zuse --output target/native-usb
~~~

Physical phone testing is opt-in and requires an explicit serial; see
[mobile browser setup](mobile-browser.md) and [native display tests](native-testing.md).
CI now runs the deterministic Linux PTY/libusb fixtures after installing their
build headers. Device interaction tests are not run against arbitrary CI
hardware. Routine hosted CI was not dispatched for this development session;
the official multi-platform release workflow publishes the release artifacts.

## Outstanding live coverage

No iPhone, Mac desktop, Windows desktop or physical serial peripheral was
available for this acceptance. iOS requires a signed/running WDA, explicit
endpoint and session coordination; it does not provide Safari DOM/CDP.
macOS permission probes are implemented and compiled, but granting permissions
and actual widget delivery still need a Mac. Wine-specific applications,
other Wayland compositors and arbitrary USB hardware remain separate
acceptance targets.
