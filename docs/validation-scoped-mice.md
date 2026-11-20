# Scoped mice validation — 2026-11-21

This record covers the unreleased `feat/scoped-mice-20261121` candidate based on `a1bb8fd2cbce0a9d85134999883c578358065242`. The candidate adds owned native mice, scoped script execution, touchpad planning, and a bounded foreground Num Lock chord path. It has not been published as a release or installed into the active SSMCP service. The pull request identifies the exact source commit to reuse.

## Behavior and evidence

| Claim | Evidence | Scope |
| --- | --- | --- |
| Independent mice keep separate positions and visible cursor images | Two real XInput2 master pointers, cursor-image inspection, application click/scroll events, unchanged core pointer | Private Linux Xvfb, XInput2/XTEST/ARGB support |
| Blocks and functions release local mice while borrowed owners survive | Compiler ownership checks, native device inventories, scope ordering | Scoped scripts and Rust ownership |
| Parallel trajectories overlap and serial trajectories run in order | Real native 250 ms paths, sample-interleaving tests, exact endpoint checks | Cooperative scheduler on one thread |
| Cancellation releases synthetic held buttons and removes owned masters | Native error/cancellation cases; separate CLI SIGINT/SIGTERM subprocesses holding a button | Normal error and cooperative exit; excludes hard kill and crash |
| Target motion uses fresh accessible bounds and the live start position | Real GTK/AT-SPI current position, offset, duration, ambiguity, visibility, callback and CLI/MCP tests | Applications exposing usable accessibility information |
| Unsupported touchpad input fails before earlier mouse effects | Six CLI/MCP entrypoint tests, pure gesture tests, compiler and flat-runtime validation | No current native touchpad backend |
| Deeply expanded scripts continue to make progress | Accepted eight-function/eight-block regression beyond the 64-transition quantum | Bounded expanded trees; no unbounded recursion |

## Portable validation

The final source passed `cargo test --offline --locked --all-targets`: **286 passed, 0 failed, 5 ignored**. Optional desktop fixtures are not counted as native acceptance merely because an ordinary test run returns successfully; the separate native runs below establish their results. `cargo test --offline --locked --no-default-features` passed **20 tests**.

Formatting, `git diff --check`, and `cargo clippy --offline --locked --all-targets -- -D warnings` passed. The compiler was Rust 1.100.0. Cross-target checks used the locked dependency graph and passed for:

| Target | Gate |
| --- | --- |
| `wasm32-unknown-unknown` | `cargo check --no-default-features --target ...` |
| `x86_64-pc-windows-gnu` | `cargo check --all-targets --target ...` |
| `x86_64-apple-darwin` | `cargo check --all-targets --target ...` |
| `aarch64-apple-darwin` | `cargo check --all-targets --target ...` |
| `aarch64-unknown-linux-gnu` | `cargo check --all-targets --target ...` |

All commands also used `--offline --locked`. These are compilation results, not live Windows, macOS, or Linux ARM desktop acceptance.

## Native mouse and target acceptance

The two `private_x11` Rust tests passed on an owned disposable Xvfb display with `ZUSE_TEST_PRIVATE_X11=1`. They exercised independent creation, native positions, device-specific appearance, real application button/wheel delivery, explicit shared-pointer mode, function and block lifetimes, concurrent versus serial motion, externally removed-device errors, cancellation, and cleanup. The final rerun completed in 1.03 seconds.

`python3 tests/native_scoped_mice.py --binary <candidate>` passed on its own authenticated Xvfb. The initial device inventory contained six devices and was restored after every case. Normal script exit, default independent flat actions, explicit user-pointer actions, and explicit user-pointer release passed. SIGINT and SIGTERM interrupted scripts after the fixture observed a held left button; cancellation cleanup took 5 ms and 3 ms respectively in this run. Both returned a failed/cancelled JSON receipt and process exit code 1. Those timings describe the measured run, not a real-time guarantee.

`python3 tests/run_accessibility_native.py` passed against private D-Bus, GTK, AT-SPI and Xvfb. It verified live starting coordinates, target-center offsets, timing, movement without an implicit click, missing/hidden/ambiguous targets, cumulative speed budgets, callback validation, and CLI/MCP entrypoints. This compatibility fixture selects `mouse.use(kind="user", appearance="system")` explicitly; the independent-device tests above validate the separate device backend.

The final development-machine binary used for the native mouse and accessibility runs had SHA-256 `17d0a6867ee3dc9d176b5d8c3921069e51f06648c29459083cee16084f326c0b`. Build products, fixture archives, images and recordings are excluded from the source commit.

## Private Wayland acceptance

The private Hyprland **0.57.2** run passed all seven fixture groups: native window metadata, pointer positioning, foreground Unicode input, background ASCII/punctuation input, unsupported-input rejection, captured window pixels, and keyboard-state handling. Num Lock tests delivered `Ctrl+T` twice, once with an implicit foreground target and once with the explicit focused window. After temporary-device cleanup, the original fixture keyboard sent a raw keypad key without replaying modifiers and the application still received `1`. A following plain letter confirmed that Control was released. The original keyboard identities, main selection, lock states and layout indices matched their pre-call values.

The same run verified that Caps Lock, layout group 1, a held key, and Num Lock text/background operations are rejected before input. The fixture then successfully sent ordinary input after each rejection. Owned windows closed and the private compositor's pointer/focus restoration checks passed. These results cover the documented state gates on this compositor version; they do not establish preservation of arbitrary sticky/custom modifiers or require operating the user's desktop. The [sanitized native evidence](evidence/scoped-mice-native-2026-11-21.json) contains the machine-readable results.

The private environment uses an owned headless parent, private runtime directory and D-Bus session, and a persistent neutral fixture keyboard. Host display names, Wayland socket inheritance and compositor signatures are removed. The helper verifies the private parent registry and rejects physical input/KMS file descriptors; a render-only GPU node may be used. It never falls back to the current desktop.

Stock Weston 15 advertises `wl_compositor` version 5, and stock Cage 0.3.1 requests XDG-shell version 5. Aquamarine 0.16 requires version 6 for both relevant bindings. The test-only builder pins Cage commit `b7b774ac957b6d8c7b39f0dcb9ebe19de3e8a45d` and archive SHA-256 `6dc1619665acd367e0174c93b234002549a66f55f1de9197d67f0305415babc8`, verifies the installed wlroots 0.20.2 library can construct XDG-shell v6, then changes only Cage's requested version from 5 to 6. The underlying library implements that protocol version. No protocol interception, system installation or dependency download occurs in the builder.

To reproduce with an already downloaded source archive:

```sh
python3 tests/build_private_cage.py --source <cage-v0.3.1.tar.gz> --output <new-build-directory>
python3 tests/native_wayland_private.py --headless --headless-parent cage \
  --cage <new-build-directory>/build/cage --render-node <render-node> \
  --binary <candidate> --state-tests --report <report.json>
```

The helper records the actual parent globals and fixture identities. The source builder emits the exact one-line patch, source and binary hashes, and tool versions. Its outputs stay in ignored temporary/build directories.

## Compatibility and downstream handoff

The flat runtime's default pointer selection changes from the user's pointer to an independent mouse. Existing callers that deliberately control the user pointer must select `mouse.use(kind="user", appearance="system")`. `mouse.position()` now requires an existing selected handle and does not create a mouse as an observation side effect. Persistent runtimes retain their selected handle until `mouse.release()` or runtime destruction. [Scoped mice](scoped-mice.md) documents the full API and migration examples.

Linux native X11 is the implemented independent-pointer backend. Wayland, Windows and macOS callers can explicitly use their existing user-pointer backend with system appearance; independent or custom glowing pointer modes return unsupported errors there. XWayland availability is not evidence of independent native Wayland pointers. Native touchpad injection remains unavailable on every backend; planning and capability discovery do not claim input delivery.

SSMCP and ztype currently call the lower-level `platform::Desktop` API, so replacing their dependency alone does not expose the new Runtime/scoped mouse behavior through the SSMCP facade. That adapter work belongs to the next integration stage. The existing SSMCP source/release owner requested an isolated upstream PR and retained the `a1bb8fd` release cohort. The ztype source owner and Project Z aggregate coordinator were notified for subsequent lockfile, adapter, catalog and gitlink work. The aggregate impact command is currently blocked by an uninitialized `zi18n` submodule; no unrelated checkout was initialized or rewritten to hide that blocker. Downstream integration is pending and is not reported as completed propagation.

## Review of the claims

- **Contribution:** the tested change supplies owned OS pointer identities and a scoped programming interface; a visual overlay alone would not satisfy the independent-device tests.
- **Clarity:** ownership, explicit user-pointer selection, timing units, cancellation, unsupported platforms and the difference between planning and execution have documented examples.
- **Evidence:** native acceptance observes device state and application events in isolated fixtures; cross-compilation is labeled separately.
- **Completeness:** hard termination, unknown modifier state, non-X11 independent mice and native touchpad gestures remain outside the implemented guarantees. Downstream source/release integration remains an explicit handoff.
- **Design:** program validation and capability checks precede input; resource conflicts, finite work/receipt budgets and cleanup failures are surfaced. Submitted events still require application-state observation when successful consumption matters.
