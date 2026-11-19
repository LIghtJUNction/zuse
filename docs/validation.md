Latest patch results: [zuse 0.1.1](validation-0.1.1.md). The record below is the original 0.1.0 acceptance.

# Validation — zuse 0.1.0

Recorded on 2026-11-20 with Rust 1.100.0 on an x86_64 Linux development host.

## Automated checks

| Check | Result |
| --- | --- |
| Core and host Rust tests | 53 passed: 21 module tests, 17 parser/motion tests, 9 executor tests, 6 executor/human regression tests |
| no_std library | Native and wasm32-unknown-unknown checks passed |
| Windows target | Full crate x86_64-pc-windows-gnu check passed |
| macOS target | Full crate aarch64-apple-darwin check passed |
| MCP transport integration | 12 passed, zero skips |
| Chromium integration | Isolated sandboxed browser test passed |
| Linux accessibility integration | Private D-Bus/Xvfb GTK fixture passed |
| Native X11 integration | Private Xvfb fixture passed |

The browser and accessibility tests are intentionally ignored in the default unit-test command because they require graphical fixtures. They were run explicitly during this release's validation.

## Native observations and actions

The X11 fixture received the exact foreground and targeted background keyboard events. The pointer reached the requested coordinate, and click/scroll events were observed. Full-screen and window PNG outputs were checked for dimensions, CRCs, decompression, and pixel contents.

The Chromium fixture verified an isolated profile, navigation, Unicode input, key events, trusted clicks, accessibility snapshots, password-field handling, text observation, target listing, viewport capture, and cleanup. A regression verifies that trickling partial WebSocket frames cannot extend the operation deadline.

The AT-SPI fixture verified bounded tree reads, password redaction, text edits, focus, native button invocation, and the resulting label change.

The Android adapter was tested against a fake ADB server that checked real wire framing and display-specific input commands. A real authorized Android device was connected through the Rust protocol client for read-only status checks. Tests did not inject input into the operator's phone.

## MCP coverage

The transport tests launch disposable real zuse processes. They cover legacy and current stdio, HTTP metadata negotiation, Host/Origin/Bearer validation, invalid JSON, oversized bodies, Unix socket framing and 0600 permissions, schema enforcement, read-only restrictions, and a real inline PNG screenshot from a private 64×48 display.

Human-flow cases cover completion forms, denied or missing confirmation, local-only password handling, URL-only client fallback, token tampering, source changes, request-ID replay, cross-server replay, and one-time continuation consumption.

## Reproduce

~~~sh
python3 scripts/check.py
python3 tests/transports.py
python3 tests/native_x11.py
cargo test --locked --test browser_native -- --ignored
python3 tests/run_accessibility_native.py
cargo bench --locked --bench core
~~~

Performance is reported by the benchmark itself. Browser and target-application memory belongs to those separate processes and should be measured separately from the zuse executable. No speed comparison against another computer-use framework is claimed.

## Coverage boundaries

Windows and macOS have implemented native desktop paths and OS accessibility helpers, plus full cross-target type checks. No live Windows/macOS desktop was available for interaction acceptance.

Wine uses the X11/XWayland path; no Wine application was available for a separate compatibility test. Background window input remains subject to the application's event acceptance.

Android background mode requires an existing nonzero display. It does not provision a new virtual display. Screenshot display IDs are resolved explicitly and ambiguous mappings fail. The tested device's uiautomator interface did not offer XML dumping, so window metadata remained the available observation source.

## Documentation self-review

| Review question | Evidence and decision |
| --- | --- |
| What does the implementation add? | One executable language and executor shared by the CLI, Rust API, and MCP; separate native OS, CDP, ADB, and human adapters |
| Can a reader reproduce the behavior? | Runnable examples, command discovery, explicit transport invocations, and isolated test scripts |
| Are performance claims supported? | Core allocation behavior follows the no_std design; benchmark commands are provided; no comparative speed claim |
| Is evaluation coverage explicit? | Linux live tests and device protocol tests are distinguished from Windows/macOS type checks |
| Are the operating assumptions clear? | The program uses the caller's OS permissions; GUI permissions, device authorization, supported display targets, and persistent-session lifetime are documented |
