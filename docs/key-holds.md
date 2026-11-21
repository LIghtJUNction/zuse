# Bounded key holds

`keyboard.hold` and `browser.hold` keep one supported key down for a requested interval, then release it. Use them for applications that track a pressed key over time, such as a game. A successful receipt reports the submitted hold and release; confirm the resulting action through application state or an observation.

```text
keyboard.hold(key="Up", duration=400, window="0x123456")
browser.hold(key="ArrowRight", duration=300)
```

Both commands require `duration` in milliseconds, from 1 through 10000. Both accept `Left`, `Up`, `Right`, `Down` and their `ArrowLeft`, `ArrowUp`, `ArrowRight`, `ArrowDown` aliases. Native `keyboard.hold` also accepts `slash` or `/`, `period` or `.`, `m` and `n`; letter names are case-insensitive. `browser.hold` remains direction-only. Modifiers, lock keys and chords are rejected before input. Hold durations count toward the program's aggregate `max_wait_ms` budget. Read-only execution rejects these commands, including in dry-run mode.

## Sharing a keyboard during local multiplayer

Native holds default to `allow_other_keys=false`: a held foreign key prevents input, and pressing another key during the hold ends it. For an authorized game with separate player controls, explicitly opt in on each call:

```text
keyboard.hold(key="Up", duration=180, window="0x123456", allow_other_keys=true)
keyboard.hold(key="slash", duration=80, window="0x123456", allow_other_keys=true)
```

Use the observed focused window ID. These calls run in sequence; they do not hold movement and fire simultaneously. With the opt-in, the other player's ordinary keys, such as WASD, Space, Q and E, may already be down or change during the hold. Zuse checks the current keymap and continues to reject modifiers, lock/layout changes and unsupported key actions. It refuses a requested key that is already held before its own press. Cleanup releases only the key pressed by this call; it never sends keyup for the other player's keys. Choose disjoint controls: a shared native keyboard cannot distinguish two players pressing the same key.

The option applies only to `keyboard.hold`. It does not change `keyboard.key`, typing or browser holds. Use the short `slash` hold for a game's `/` binding; confirm whether a `>` icon means the physical period key before choosing `period`. Shift/Ctrl game actions remain outside this API.

On Hyprland, creating a virtual keyboard is rejected when removing it could select a different main keyboard. With `allow_other_keys=true`, that case can instead use the existing native keyboard through one retained evdev file descriptor. This fallback requires an exact, globally unique mapping from the compositor name to the kernel's device name, the requested key capability, and existing read/write permission. It rejects virtual/reserved names, ambiguous numeric suffixes, duplicate normalized names, incomplete inventories and unsupported names. It checks the captured current XKB map, kernel pressed-key state and compositor state before input; it never grabs a device, selects another keyboard or changes permissions. A successful fallback receipt includes `transport: "native_evdev"`. Default exclusive holds retain their existing behavior.

The supported name normalization and collision rules follow Hyprland v0.57.2: [`deviceNameToInternalString`](https://github.com/hyprwm/Hyprland/blob/v0.57.2/src/helpers/MiscFunctions.cpp) and [`getNameForNewDevice`](https://github.com/hyprwm/Hyprland/blob/v0.57.2/src/managers/input/InputManager.cpp). Zuse accepts only the unambiguous ASCII subset and checks names across the complete device inventory.

## Target and cancellation

`keyboard.hold` supports Linux X11 and Hyprland. It observes the current foreground window once, retaining its native ID, process ID, bounds and scale. An explicit `window` must already have focus. With no `window`, the observed foreground window is retained for the whole call. The command does not focus a window. Identity, focus, geometry or scale changes end the hold even with `allow_other_keys=true`. The existing Num Lock state is preserved. Browser tabs within one native window share its ID; stop input and observe again when the user changes pages. Other native platforms return an explicit unsupported-state error before input.

`browser.hold` operates on the already selected desktop page. It retains the document, URL, active element and viewport. The page must be visible and focused. Navigation, a different active element, a changed viewport or a disconnected target ends the hold. The command never activates a tab and does not enable a remote debugging endpoint. It uses the existing browser connection.

The input permission is acquired for keydown and rechecked during the hold. Waiting and protocol acknowledgements do not keep the dispatch lock. Cancellation, permission revocation and errors run keyup cleanup for the key owned by the call. Cleanup has a separate bounded protocol timeout so cancellation does not skip release. A disconnected transport can return a cleanup failure; the receipt does not claim a confirmed release in that case.

The native evdev fallback releases only its own key on ordinary completion, cancellation and handled failure. A hard process termination, such as `SIGKILL`, cannot confirm cleanup on an existing physical device; closing that descriptor does not synthesize keyup. Do not treat process death as a release acknowledgement.

The native event loop and transport scheduling determine the exact observed interval. The result includes `held_ms`, while receiver-side measurements record the interval the application actually saw. Browser input sends one keydown and one keyup; it does not synthesize periodic repeat events.

## Focused validation

```sh
cargo test --locked --lib --test key_hold key_hold
cargo test --locked --test key_hold_browser -- --ignored --nocapture
```

The first command covers key/option validation, aggregate wait accounting, read-only rejection, concurrent key ownership, cancellation, revoked permission, target changes and release after a failed press. XKB unit fixtures classify ordinary keys and reject remapped modifier actions without native input. The second starts a separate sandboxed Chromium profile and checks trusted keydown/keyup events, receiver timing, cancellation and focus changes.

For an explicitly authorized host preflight that sends no input, run `cargo test --locked --lib native_evdev_preflight_current_keyboard_without_input -- --ignored --nocapture`. It only inspects the currently selected keyboard, opens its verified existing device and checks identity/capability/pressed-state access. A virtual current keyboard is reported as skipped; the test does not switch keyboards. The evdev ownership and name-collision unit tests use in-memory writers and inventories. Private Wayland tests below exercise the virtual transport, so they do not establish successful evdev input or live-game behavior.

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
