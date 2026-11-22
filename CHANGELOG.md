# Changelog

## Unreleased

- Keep the persistent shell's SIGINT/SIGTERM handler active while waiting for input and cleaning up its Runtime. Interrupting an idle shell now reaps managed child processes; interrupting active work cancels the command, discards queued input, and exits with status 1 on supported native signal platforms.
- Add isolated subprocess regressions for both signals with idle input, incomplete lines, active commands and managed-process cleanup; preserve existing idle-timeout and CLI execution behavior.

## 0.6.0 — 2026-11-22

- Add `zuse -c` / `--code` for direct inline programs and `shell --idle-timeout-ms` for bounded persistent sessions. Idle shells close after 15 minutes by default; running commands do not consume idle time.
- Add immutable scalar bindings, arithmetic, numeric comparisons, `if`/`else`, bounded integer `for` ranges, pure math helpers and recursive functions. Evaluate them before input with call-depth, expansion-work and runtime budgets; preserve lexical mouse ownership and transactional module preparation.
- Add window-local anchors, `mouse.click_at`, `mouse.stroke`, and `draw.line/triangle/rect/circle/polygon`. Validate geometry and point budgets before input, reject stale window anchors, and release stroke buttons on completion, failure, or cancellation. Include a recursive Sierpinski example using numeric triangle vertices.
- Add named Unix sessions with `start/serve/status/script/observe/result/cancel/close` and a public JSON-lines v1 protocol. Retain Runtime state across clients, distinguish accepted jobs from results, support bounded request replay and revision checks, and exclude running work from idle expiry. Execute flat and expanded numeric programs; scoped mouse programs support dry-run preflight in this channel.
- Extend native `keyboard.hold` to slash, period, M and N, with explicit `allow_other_keys=true` for concurrent ordinary keys. Preserve the other player's held keys, reject modifiers and an already-held target key, and keep cancellation, focus checks and owned-key release. Treat the hold's end as normal completion when its final read-only target check reaches that deadline; transport failures remain errors.
- For explicit concurrent holds on Hyprland, support the existing native keyboard when a temporary virtual device cannot restore the same active keyboard. Require unique compositor/kernel identity, existing permissions, key capability and ownership; retain one verified evdev descriptor and report `transport: "native_evdev"` on success.
- Send one target sample for zero-duration pointer moves instead of querying the old position and sending a redundant initial sample.
- Default flat/scoped point moves, Rust `MoveOptions` and `mouse.click_at` approaches to 350 ms, smooth easing and 0.6 px jitter. Keep explicit instant movement available and add click-at motion overrides; drawing-stroke trajectories retain their existing behavior.
- Add Git-backed `pkg init/add/install/update/remove/list/publish`, strict manifests, immutable commit locks, bounded transitive dependencies, and local checkout verification. Publication plans and pushes version tags without creating repositories or pushing branches.
- Add declaration-only `pkg:NAME` and `pkg:NAME/EXPORT` imports from locally installed packages. Keep the entry project's lock context through nested imports; imports never install dependencies or fetch source.
- Document the separately versioned [standard library](https://github.com/LIghtJUNction/zuse-std) and include three focused skills for computer operation, package development/publication, and package discovery/use.

## 0.5.0 — 2026-11-22

- Add bounded direction-key holds for Linux X11, Hyprland and selected Chromium pages. Hold duration counts toward the program's wait budget; cancellation, permission revocation and target changes attempt to release the owned key.
- Preserve native Num Lock state and bind each hold to its observed target. Browser cleanup sends keyup before waiting for acknowledgements and reports errors from either event, including when a very short hold ends before the keydown reply arrives.
- Add a declaration-only ChatGPT desktop module with native window inspection, scoped conversation and draft observations, guarded draft preparation, and a separately authorized send helper. Importing the module performs no desktop actions.
- Add optional exact text and process preconditions to Linux accessibility edits. Guarded writes retain one native object, reject mismatched drafts before input, and verify the resulting text. Real application accessibility availability remains a separate requirement.
- Submit native accessibility edits while holding the input permit, then wait for the application's reply outside it. Cancellation after submission returns promptly and reports that the draft may already have changed.
- Add private browser, X11, Wayland and GTK fixtures for receiver timing, cancellation, target changes, draft protection and cleanup. Document actual Cargo artifact discovery for updater acceptance tests.

## 0.4.2 — 2026-11-21

- Add `zuse update`, `update --check`, and `update --dry-run` with JSON receipts and explicit progress. Preserve the running CLI's tracked Cargo root, source, features, profile, and target.
- Verify the official stable package or Git release/tag before installation; retain newer development versions and require manual updates for system, portable, untracked, or explicitly pinned installations.
- Bound metadata requests and installation subprocesses, reuse CLI cancellation and owned-process cleanup, and verify the installed package identity and executable version after Cargo succeeds.

## 0.4.1 — Num Lock regression fixtures

- Extend isolated X11/GTK fixtures to check Num Lock on/off/toggle and repeated
  on requests, keyboard-state readback, ASCII/UTF-8 input, Caps Lock preservation
  and state restoration. Runtime code and public APIs are unchanged.
- Refresh the locked zmcpio default-branch revision for release-tooling path
  fixes. Its dependency version and published source contents are unchanged.

## 0.4.0 — local programs and input-state iteration

- Align the Git development lock with zmcpio 0.5.0, zcall 0.4.1 and zconv 0.4.1. A separate registry projection must preserve this dependency graph and the complete published source contents.

- Add read-only function catalogs to Runtime/CLI and MCP, with explicit `///` descriptions, ordered parameters, source locations and namespace filtering. Keep the existing Rust name-list methods compatible.
- Preserve lexical module names across local mouse variables, blocks, sequences, repeats and parallel branches; detect recursive functions even when a later local binding uses the same name.
- Separate the active Wayland keyboard from other devices; preserve lock state during text entry and add explicit Num Lock read/set/toggle with native readback.
- Add a click-through, non-focusable Wayland marker for the shared user pointer, including fractional output coordinates and cleanup. Independent pointer devices remain X11-only.
- Add event-based browser/AT-SPI observation, retained watches, cancellable local waits, monotonic timers and bounded appearance-triggered browser clicks with explicit receipt and cleanup states.
- Add optional multi-scale template matching, cached-target checks, native X11 input-surface hit testing, and explicitly captured/rendered visual timelines with bounded episode history.
- Add transactional local file/source imports, explicit eval, namespaced functions and persistent Runtime/shell reuse. Preserve scoped borrowed mouse ownership and aggregate preflight limits.
- Include a reusable Zuse skill, declaration-only WeChat helpers, owned browser examples, and isolated Wayland/X11/AT-SPI validation fixtures.

See [local programs](docs/local-programs.md) and [validation](docs/validation-20261121.md). Function discovery and lexical-scope checks are recorded in the [additional validation](docs/validation-function-metadata-20261121.md).

## 0.3.0 — 2026-11-21

- Publish GitHub binaries only. Project Z dependencies follow their GitHub default branches; the lockfile records the validated revisions. Registry publication is deferred until a compatible package can be staged separately.

- Update the transitive zcall Git lock to `317c52f1` (release tooling only; no runtime API changes in that dependency).

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
