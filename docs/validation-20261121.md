# Local-program and input-state validation — 2026-11-21

This record distinguishes native observations from pure planning or mock tests.
Tests used owned displays, applications and browser profiles. They did not install
the candidate onto the operator's running desktop or send messages in WeChat.
Screenshots, built executables and full transient logs remain outside Git.

## Observed native results

| Environment | Result | What was established |
| --- | --- | --- |
| Private nested Hyprland, scale 1.0 | 9 checks passed | Native window coordinates, pointer motion, Unicode/emoji, background ASCII without focus, rejected unsafe input, capture, lock/chord/held-state handling, eight-other-device isolation, marker lifecycle |
| Same, scale 1.25 | 9 checks passed | The same checks using fractional output scale |
| Private Xvfb/GTK, vision enabled | 4 tests passed | X11 input-surface hit checks, real NumLock set/readback, multiscale retained image targets, visual episode/timeline behavior |
| Private Xvfb and private AT-SPI bus | 1 native test passed | Subtree-scoped events, local wait, cancellation, continued observation; events outside scope ignored |
| Owned sandboxed Chromium | 4 appearance-click tests passed | Unique target, per-appearance clicks, target invalidation/cancellation, trusted click receipt and local latency sample |
| Owned Chromium, existing attachment and tab focus loss | 2 native tests passed | Existing attachment neither creates nor activates a page; a different focused tab stops the retained watch |
| Owned Chromium, basic observations | 2 native tests passed | Scoped observation and event-based waiting without screenshots |

### Wayland keyboard and marker

Both scale runs created eight non-current virtual keyboards with NumLock on while
the actual current keyboard had NumLock off and was not last in device order.
Text succeeded through the eligible current-keyboard route; the complete before/
after keyboard state remained unchanged. This exercises the original reported
failure rather than merely testing a device-list parser.

The owned marker produced 281 colored pixels at scale 1.0 and 454 at scale 1.25.
Movement changed its position, while explicit release and Drop removed it. An
owned virtual pointer clicked the GTK target twice through the marker. Protocol
traces confirmed an empty input region, zero keyboard interactivity and exclusive
zone -1. Marker images were inspected by the fixture and retained only as local
artifacts.

These tests did not validate production ydotool button dispatch. The fixture used
a private virtual-pointer client and a deliberately nonexistent private ydotool
socket. It made no host-display connection and opened no physical-input or KMS
device file descriptors. The marker is a shared-pointer overlay, not an
independent Wayland input device.

The two Wayland reports used the same immutable binary:

```text
SHA256 e54ae9d2f6bf2fe8583542eb01f09d4cf7c00aa2a9eada88be7a4f123365231e
```

They validate the keyboard/marker implementation captured at that point. Later
module and X11 changes were compiled and tested separately; the hash is not
presented as a fingerprint of a later rebuilt executable.

### Native X11 target and timeline checks

The image fixture independently drew a 25-pixel icon and matched it with a
20-pixel template at scale 1.25. Duplicate icons returned ambiguity and cleared
the previous cached target. Moving/resizing the target window rejected clicks.

An InputOnly covering window changed neither screenshot pixels nor focus. Tests
covered both root-level occlusion and a sibling inside an owned reparenting frame.
The latter found and fixed an ancestor-hit bug: a frame hit must not automatically
authorize its client. The corrected implementation descends the server's input
window hierarchy until it actually enters the target subtree. Removing the cover
allows the retained target to be used again without another template search.

NumLock tests started with CapsLock set, exercised on/idempotent-on/off/toggle,
and compared the complete locked-modifier mask after each real readback. Only the
requested NumLock bit changed.

The timeline wait returned while the first appearance was still visible, about
21.7 ms after the test received the fixture's paint-complete acknowledgement.
This is post-paint-acknowledgement-to-wait-response time, not full appearance-to-
response latency. Twelve visible samples
formed one episode; disappearance and reappearance formed a second. No SVG was
created until explicit render, which embedded exactly one baseline image. This
number reflects this fixture and sampling interval, not a guaranteed display rate.

### Browser latency sample

The owned headless page recorded **24/24 trusted clicks** in each measured run.
The initial appearance-to-received-click P95 was **6.000 ms**, with a 2.800 ms
minimum. The final complete-gate rerun measured **8.400 ms P95** and a 2.700 ms
minimum. Both runs used zero screenshots and one initial container query. The
report also preserves the separate event-to-dispatch measurement.

This is a small, local fixture sample. It does not predict a real site's network,
renderer load or composited-desktop latency. DOM validation and CDP input are two
protocol commands; renderer changes between them are not atomic.

## Regression coverage

The final `cargo test --all-targets --all-features --locked --no-fail-fast` run
passed **360 tests**, with **18 explicit native/subprocess ignores**. Native runs
listed above were invoked separately. `cargo clippy --all-targets --all-features
--locked -- -D warnings` and `cargo check --no-default-features --locked` passed.

- Module compiler: 11 tests passed, including namespace reuse/replacement,
  declaration-only imports, explicit eval, syntax-tree literal substitution,
  relative file loading, cycles, bounded expansion, aggregate limits and borrowed
  scoped-mouse identity.
- Runtime/CLI modules: 9 tests passed, including persistent sessions, whole-program
  preflight with no early timer effects, dry-run non-commit, cancellation,
  entry-relative imports from another cwd, shell reuse and scoped compatibility.
- Runtime cancellation: 2 tests passed, including a real CLI SIGINT during a wait.
- Browser attachment/cleanup: 3 mock socket tests passed, covering cancelled
  discovery/handshake, bounded stalled cleanup, and reading the original release
  acknowledgement without replaying input.
- Template matching: 8 tests passed. Visual timeline core: 10 tests passed.
- SSMCP computer module: 88 tests passed, with 13 explicitly ignored native or
  child-process fixtures. The separately invoked owned-Chromium integration test
  also passed: real `Computer::perform`/Ticket/worker input produced a trusted
  click and confirmed release; off blocked new input even when the target
  reappeared before the parent processed worker EOF. The native test changed only
  its temporary unit-test input store. Library Clippy passed with warnings denied.
- Bundled skill examples: all five files passed preflight; both owned browser
  examples and the local import/eval/timer example completed. The WeChat functions
  were registered only, never invoked.

The module syntax pass initially rejected `serial` in a flat argument and
`serial.ports()` as a command. This was corrected only in module-parser mode,
preserving the original scoped binding grammar. The runtime/native tests above
were rerun after that correction. A separate original AT-SPI application-root
index issue and Wayland preflight error-string classification were likewise
corrected before their passing runs. Legacy scoped diagnostics retain their source
locations and reserved-name wording. Explicit `zuse script` preserves bounded
event receipts for compatible wait/capability-only programs.

## Reproduction

All Cargo commands on the shared developer machine are enclosed by the workspace's
outer `/var/tmp/project-z-cargo-target/.project-z-cargo-owner.lock`; runners do not
take a second copy of that lock.

```sh
cargo test --test script_modules --test runtime_modules --test runtime_cancellation
cargo test --features vision --test vision --test visual_timeline
python3 tests/run_vision_native.py --report dist/native-x11-vision.json
python3 tests/run_observation_native.py
cargo test --test browser_click_watch -- --ignored --test-threads=1
cargo test --lib browser::click_watch::tests -- --test-threads=1
cargo test --lib browser::click_watch::tests -- --ignored --test-threads=1
cargo test --test observation -- --ignored --test-threads=1
cargo check --all-features
cargo check --no-default-features
python3 skills/zuse/scripts/check_examples.py --binary /path/to/zuse --execute-fixtures
```

For the Wayland fixture, an isolated parent compositor with the required protocol
versions must already be available. The validated run built a small private Cage
v6 source in ignored `dist/` against the machine's existing wlroots; it did not
replace or install the host compositor.

```sh
python3 tests/native_wayland_private.py --headless --headless-parent cage \
  --cage /path/to/private/cage --binary /path/to/immutable/zuse \
  --report dist/native-wayland-input-enhanced-125.json \
  --state-tests --marker-tests --scale 1.25 --ascii-background
```

## Explicit limits

Native touchpad pinch/rotate/swipe input remains unsupported where no backend can
deliver the native protocol. Planning tests do not establish hardware delivery.
No scroll or browser-zoom fallback is substituted. Independent mouse devices remain
X11 MPX only. Strict retained-target point hit testing currently requires X11.
Physical Hyprland NumLock setting still needs a resolvable eligible evdev device
and permissions. The included WeChat module is checked source, not a live-tested
WeChat integration, and no real message was sent.

Source publication, downstream dependency propagation and installation are separate
states. This iteration intentionally leaves the running host installation alone.
