# Bounded direction-key holds

`keyboard.hold` and `browser.hold` keep one direction key down for a requested interval, then release it. Use them for applications that track a pressed key over time, such as a game. A successful receipt reports the submitted hold and release; the application can confirm the resulting movement through its own state or a scoped observation.

```text
keyboard.hold(key="Up", duration=400, window="0x123456")
browser.hold(key="ArrowRight", duration=300)
```

Both commands require `duration` in milliseconds, from 1 through 10000. They accept `Left`, `Up`, `Right`, `Down` and the corresponding `ArrowLeft`, `ArrowUp`, `ArrowRight`, `ArrowDown` names. Chords and lock keys are rejected before input. Hold durations count toward the program's aggregate `max_wait_ms` budget. Read-only execution rejects these commands, including in dry-run mode.

## Target and cancellation

`keyboard.hold` supports Linux X11 and Hyprland. It observes the current foreground window once, retaining its native ID, process ID, bounds and scale. An explicit `window` must already have focus. With no `window`, the observed foreground window is retained for the whole call. The command does not focus a window. It checks the retained window during the hold and releases the key when its identity, focus, geometry or scale changes, or another key is pressed. The existing Num Lock state is preserved. Other native platforms return an explicit unsupported-state error before input.

`browser.hold` operates on the already selected desktop page. It retains the document, URL, active element and viewport. The page must be visible and focused. Navigation, a different active element, a changed viewport or a disconnected target ends the hold. The command never activates a tab and does not enable a remote debugging endpoint. It uses the existing browser connection.

The input permission is acquired for keydown and rechecked during the hold. Waiting and protocol acknowledgements do not keep the dispatch lock. Cancellation, permission revocation and errors run keyup cleanup for the key owned by the call. Cleanup has a separate bounded protocol timeout so cancellation does not skip release. A disconnected transport can return a cleanup failure; the receipt does not claim a confirmed release in that case.

The native event loop and transport scheduling determine the exact observed interval. The result includes `held_ms`, while receiver-side measurements record the interval the application actually saw. Browser input sends one keydown and one keyup; it does not synthesize periodic repeat events.

## Focused validation

```sh
cargo test --locked --lib --test key_hold key_hold
cargo test --locked --test key_hold_browser -- --ignored --nocapture
```

The first command covers duration limits, aggregate wait accounting, read-only rejection, cancellation, revoked permission, target changes and release after a failed press. The second starts a separate sandboxed Chromium profile and checks trusted keydown/keyup events, receiver timing, cancellation and focus changes.

The private Wayland runner has a hold-only mode. Supply the actual binary path from the Cargo build output:

```sh
python3 tests/native_wayland_private.py --headless --hold-tests \
  --binary "$ZUSE_BINARY" --report /tmp/zuse-key-holds.json
```

This runner uses its owned headless compositor and GTK windows. It records actual GTK keydown/keyup timestamps and compositor held-key state, tests cancellation and focus changes, and checks Num Lock both off and on with the original keyboard's keypad input after each hold.

For X11, the separate fixture starts its own authenticated Xvfb server and checks receiver events, cancellation, focus changes and both Num Lock states:

```sh
python3 tests/native_key_hold_x11.py --binary "$ZUSE_BINARY" \
  --report /tmp/zuse-key-holds-x11.json
```
