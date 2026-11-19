# Changelog

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
