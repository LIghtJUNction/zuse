# Changelog

## Unreleased

- Add `zuse script` and MCP `zuse.script` with owned mouse variables, lexical blocks, borrowed aliases and function parameters, bounded repeats, and cooperative parallel or sequential trajectories.
- Create independent Linux X11 MPX mice by default, with per-device glowing cursors, configurable colors/shapes, bounded PAM images, and explicit shared user-pointer selection.
- Expose the Rust `Mouse`, `MouseOptions`, `CursorStyle`, and `CursorImage` APIs, including validated callback paths and native resource cleanup on scope exit.
- Make flat runtime pointer actions independent by default; add `mouse.use` and `mouse.release`, and require an existing selected handle for `mouse.position`.
- Reject conflicting parallel access and overlapping user-pointer owners before execution; close scoped mice and release held synthetic buttons on runtime errors and cooperative cancellation.
- Add temporary `zuse script` CLI SIGINT/SIGTERM handling on supported Unix builds and document hard-termination, application-grab, and platform limits.
- Add bounded touchpad swipe, pinch, rotation, and cancellation planning to Rust, flat batches, and scoped scripts; report native support as unavailable and reject execution before other input.
- Preserve verified Num Lock state for supported foreground Hyprland key chords using the original XKB map; retain explicit state errors for text, background, and unobservable configurations.
- Keep deeply expanded function scopes progressing within the cooperative scheduler's work budget and bound accumulated execution receipts while retaining cleanup records.
- Add `mouse.move_to` to locate an accessible text or button target and move from the live pointer position in one CLI, shell or MCP operation.
- Support absolute waypoints, seeded smooth jitter and speed variation, easing, target offsets, duration or sampled-path average speed, and explicit selection of duplicate targets.
- Expose typed target queries, complete pointer plans, `Runtime::move_to` and `Runtime::move_to_with` for custom Rust paths validated before native input.
- Add allocation-free `FunctionalMotion` sampling and timed `mouse.circle` paths.
- Include native visibility in desktop accessibility snapshots; preserve exact movement endpoints and enforce the cumulative wait budget before speed-based input.

## 0.2.2 — 2026-11-20

- Preserve the current browser page and session when selecting it again; prepare another page before releasing the previous selection.
- Preserve the previous browser selection on explicit attachment, initialization or cleanup failure, and check target-close confirmation.
- Resolve a unique macOS AX window title before activating the application; reject ambiguous or missing windows before focus effects.
- Validate Android screenshot PNG structure and share staged image replacement with iOS so failed captures preserve the previous file.
- Reject invalid Android capture paths during whole-program validation.
- Add seven stateful CDP, four macOS selection, five Android capture and one staged-output regression; exercise current-page preservation in real isolated Chromium.
- Verify 150 Rust tests, no_std/native and wasm builds, Clippy, and Windows plus both macOS all-target checks.

## 0.2.1 — 2026-11-20

- Fixed X11 Unicode delivery by inserting and confirming complete UTF-8 through the focused AT-SPI editable control instead of temporarily remapping global keycodes.
- Routed foreground text through the same confirmed path when held keys, modifiers or layout make native key emission unsafe; the user's keys remain held and input is not retried.
- Preserved callback edits to a displaced selection and distinguished errors before input from uncertain results after text has changed.
- Fixed Windows background key messages for extended keys and Alt/Ctrl/F10 combinations, with complete validation before sending the first message.
- Propagated macOS window-query, missing-target and AXRaise failures while keeping native objects alive through the operation.
- Rejected signed, malformed and out-of-range Windows COM names before execution.
- Added 21 portable production-logic simulations, 17 isolated GTK scenarios and a CLI entry point for checking downloaded release binaries.
- Kept native X11 and MCP fixtures on explicit high display numbers with startup ownership checks.

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
