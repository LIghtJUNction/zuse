# Changelog

## 0.2.0 — 2026-11-20

- Added native Android Chrome over ADB with an owned tab, Unicode/selector input, AX/text and viewport screenshots; 13 physical-device checks passed.
- Fixed real Chrome discovery timeouts by honoring HTTP Content-Length instead of waiting for a persistent ADB stream to close.
- Added explicit iOS/WDA touch, text, screenshot, source and session operations, native secure-field redaction and 20 protocol/MCP regressions; no iPhone was available.
- Added native Linux/macOS/Windows serial adapters and system-libusb USB bulk, interrupt and control transfers, with bounded buffers and explicit partial completion.
- Verified 5 Linux PTY groups, 11 isolated USB C ABI groups and real read-only device enumeration.
- Added Android/iOS PNG sources to the MCP image tool and non-prompting macOS permission diagnostics.
- Preserved the no_std zero-dependency core and reused the existing six Project Z dependencies.
- Verified 109 Rust tests, desktop Chromium, 12 MCP wire tests, no_std/wasm, Windows and both macOS target checks; wired deterministic device I/O fixtures into Linux CI.

See [validation](docs/validation-0.2.0.md) for exact evidence, initial failures and live-platform limits.

## 0.1.1 — 2026-11-20

- Validated foreground and owned-secondary-display actions on a physical Android 16 device, with published sanitized evidence and a zero-permission fixture.
- Fixed Android focus correlation, virtual screenshot ID mapping, command error detection, target invalidation and whole-operation ADB deadlines.
- Added complete Android argument preflight and stricter multi-device selection.
- Replaced Wayland text utility calls with native virtual-keyboard handling and original-keymap restoration.
- Resolved background text against the actual XKB map before dispatch, fixing Fcitx mapping/punctuation failures.
- Added explicit keyboard-state rejection and machine-readable execution error codes.
- Clarified that native keyboard success reports event submission, requiring application observation for confirmation.
- Reused exact zsys 0.1.1 for X11, AT-SPI, Wayland and XKB dynamic loading.
- Added reproducible private Wayland/Fcitx/state tests and hardened Android cleanup, screenshot and redaction regressions.
- Verified 74 Rust tests, 17 acceptance-runner regressions, 12 MCP tests, X11/AT-SPI live fixtures, no_std/wasm, and Windows/macOS type checks.

See [validation](docs/validation-0.1.1.md) for run accounting and limits. Shared-desktop keyboard input was rejected before effects because Num Lock was enabled; it is not recorded as delivered input.

## 0.1.0 — 2026-11-20

- Added the allocation-free no_std computer-use parser and deterministic motion kernel.
- Added a shared CLI, persistent shell, embedded runtime, and MCP executor.
- Added native Linux X11/Hyprland, Windows, and macOS desktop backends.
- Added native AT-SPI and system accessibility adapters, text observation, and PNG capture.
- Added an isolated Chromium/CDP browser controller with visible and headless modes.
- Added a native ADB client with explicit foreground and existing-secondary-display modes.
- Added bounded process/harness management, IDE interfaces, and Omarchy adapters.
- Added durable human assistance, local password handling, and MCP form elicitation.
- Added local checks, native fixture tests, multi-platform releases, and version bump workflows.

The Cargo distribution is named zuse-computer. The project, binary, and library remain zuse.
