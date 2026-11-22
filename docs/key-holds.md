# Bounded key holds

`keyboard.hold` and `browser.hold` keep one supported key down for a requested interval, then release it. Use them for applications that track a pressed key over time, such as a game. A successful receipt reports the submitted hold and release; confirm the resulting action through application state or an observation.

```text
keyboard.hold(key="Up", duration=400, window="0x123456")
browser.hold(key="ArrowRight", duration=300)
```

Both commands require `duration` in milliseconds, from 1 through 10000. Both accept `Left`, `Up`, `Right`, `Down` and their `ArrowLeft`, `ArrowUp`, `ArrowRight`, `ArrowDown` aliases. Native `keyboard.hold` also accepts `slash` or `/`, `period` or `.`, `m` and `n`; letter names are case-insensitive. `browser.hold` remains direction-only. Modifiers, lock keys and chords are rejected before input. Hold durations count toward the program's aggregate `max_wait_ms` budget. Read-only execution rejects these commands, including in dry-run mode.

## Two browser directions together

`browser.hold_keys` supports a browser game that needs movement and steering at the same time:

```text
browser.hold_keys(keys="ArrowUp,ArrowRight", duration=500)
```

`keys` contains one or two comma-separated directions, with the aliases above. The complete set is validated before input: duplicates, opposite directions, extra keys and non-direction keys are rejected. The duration remains 1–10000 ms and is counted once against the wait budget. This separate command does not expand `browser.hold` or change native keyboard sharing rules.

Both keydown requests are written consecutively under one input permit, without waiting for a reply between them. They are separate CDP events, not an atomic simultaneous operation. The existing document, active-element, visibility, focus and viewport checks remain active. Completion, cancellation or a handled error attempts to release every key this operation tried to press; all keyup requests are sent before waiting for acknowledgements. A broken transport reports unconfirmed cleanup and does not trigger automatic replay. Receipts contain a `keys` array and still require application confirmation.

A target-check reply that is still pending at the requested hold end does not postpone keyup. After release, the adapter checks that reply within a separate bounded cleanup budget before reporting success. A genuine protocol failure, a lost connection or an unconfirmed final check remains an error; reaching the hold end alone is not treated as a failed application action.

Coverage is in [the browser hold protocol tests](../src/browser/key_hold.rs), [preflight tests](../tests/browser_hold_keys.rs), and [the isolated Chromium receiver](../tests/key_hold_browser.rs). The current real-game acceptance boundary is recorded [separately](validation-e2e-20261122.zh-CN.md).

## Hold until an observed condition

`browser.hold_until` releases its direction keys when a synchronous boolean expression becomes true:

```text
browser.hold_until(keys="ArrowUp,ArrowRight", condition="document.querySelector('#status')?.textContent === 'Ready'", timeout=3000)
```

The direction rules are the same as `hold_keys`. `timeout` is required, ranges from 1 through 10000 ms, and counts once toward `max_wait_ms`. It includes the initial target and condition observation. An initially true condition returns `reason="condition"`, `held_ms=0`, `pressed=false` and `released=true` without sending a key. An ongoing hold returns success only after observing true before its deadline and confirming its key releases. A late true reply received during cleanup remains a timeout. Release acknowledgement work has a separate bounded budget, so the command may return after its input deadline.

Use a side-effect-free expression over visible DOM or an explicitly installed observer. The expression must return a boolean, rather than a truthy value or Promise, and fit within 16384 UTF-8 bytes. It executes as ordinary page JavaScript. Zuse checks locally through the existing CDP connection; network and renderer scheduling still affect when it observes a change. Navigation, focus or geometry changes, cancellation, expression errors and transport failures follow the existing release path. A failed release cannot produce a successful condition receipt, and input is never automatically replayed.

For a named [canvas observer](canvas-observer.md), add `canvas="motion"` and write the condition using the supplied `state`. That binding uses this Browser's observer in its selected document and reserves space for the binding within the same 16 KiB expression limit. Read the observer's freshness and coverage rules before using drawing records as a stop condition.

The [condition protocol tests](../tests/support/browser_hold_until.rs) and [preflight tests](../tests/browser_hold_until.rs) cover the API boundaries. Their presence is not a claim that a particular game or real page has passed acceptance.

## Adjust keys from observed feedback

`browser.control_keys` runs a short synchronous policy locally and changes the active directions according to its result. Use it when a single fixed chord cannot respond to changing position, direction or another public observation:

```text
browser.control_keys(allowed_keys="ArrowLeft,ArrowRight", policy="(()=>{const x=Number(document.querySelector('#position').textContent);memory.samples=(memory.samples||0)+1;return {keys:x<100?['ArrowRight']:[],done:x>=100};})()", timeout=3000)
```

The example assumes that the page exposes the observed position as `#position` text. The core contains no game-specific coordinates, steering model or completion condition. The caller supplies those decisions using observations available to the policy.

| Input or binding | Contract |
| --- | --- |
| `allowed_keys` | Required, one to four unique comma-separated directions; the existing direction aliases are accepted |
| `policy` | Required synchronous JavaScript expression, 1–16384 non-NUL UTF-8 bytes |
| `timeout` | Required, 1–10000 ms; setup and all policy iterations count toward this deadline and once toward `max_wait_ms` |
| `canvas` | Optional existing watch name; binds this Browser's public drawing snapshot to `state` |
| `state` | Detached, read-only canvas snapshot; `null` when no watch is supplied |
| `memory` | A fresh object initially `{}`; accepted JSON contents are carried to the next iteration, at most 4096 UTF-8 bytes, depth 8 and 512 values |
| `control` | Read-only metadata with zero-based `tick`, `elapsed_ms`, current owned `keys` and observation-assembly `sample_age_ms` |

The result must be a plain object with exactly `{keys:[...],done:bool}`. `keys=[]` is a valid wait. An active chord may contain at most two unique, non-opposite directions from the allowlist. Unknown fields, unknown or unlisted keys, duplicate directions, opposite directions, wrong types, getters and non-JSON memory fail before that iteration sends new input. `done=true` validates the complete decision, then releases any owned keys; its valid `keys` array does not cause a new press. An initially done policy sends no input.

The policy runs in an isolated JavaScript world with `throwOnSideEffect=true`. It can read public DOM, its supplied observation and its own fresh memory. Page-world JavaScript globals are not supplied. Zuse copies observation objects before starting the checked evaluation, so modifying their existing objects is rejected; only the policy's own fresh memory is persisted. Some DOM getters or JavaScript operations cannot be established as side-effect-free by Chromium and are rejected. There is no fallback that disables the check. In particular, do not rely on dynamic `eval` or `Object.freeze` inside a policy. See [the implementation](../src/browser/control_keys.rs) and the [official Runtime evaluation contract](https://chromedevtools.github.io/devtools-protocol/tot/Runtime/#method-evaluate).

Use `querySelector(...).textContent` for the DOM text reads shown here. In the owned fixture on Chrome/158.0.8127.36 with V8 15.8.62.31, this read, the retained target checks and the complete policy wrapper with fresh JSON memory passed with `throwOnSideEffect=true`. The equivalent `getElementById(...).textContent` read returned `EvalError: Possible side-effect in debug-evaluate`. Treat support as a property of the exact operation and browser build, and check the intended expression with the guard enabled. Blink's [side-effect annotations](https://chromium.googlesource.com/chromium/src/+/refs/heads/main/third_party/blink/renderer/bindings/IDLExtendedAttributes.md) explain why a read can be conservatively refused. Zuse reports that refusal and releases its owned keys through the existing cleanup path.

Offline `zuse check` validates the entire DSL program, argument types, permissions and aggregate wait budget. It does not parse arbitrary JavaScript. At execution time, Zuse compiles the policy source and runs its first complete checked evaluation before the first keydown. Subsequent evaluations have a 50 ms VM budget, clipped to the remaining total deadline. Polling includes a local wait of at most 20 ms; this is not a real-time control frequency. Renderer and protocol scheduling affect when a change is observed. Drawing ages and `control` are snapshots, not live clocks: `sample_age_ms` is measured while assembling the observation before the isolated binding is submitted. Policies must leave room for later dispatch latency and reject stale or insufficient drawing coverage themselves. The JSON limits bound retained controller data and replies; they are not a separate JavaScript heap quota.

Rust sends trusted CDP input according to the difference between successive valid chords. An unchanged key remains down without repeated keydown events. All removed keys receive keyup before Zuse waits for any release acknowledgement; replacements are pressed only after those releases are confirmed and the retained target is checked again. No policy-generated DOM keyboard events are used. Document, URL, active element, focus, visibility, viewport and scale checks remain active, and the command does not enable background focus automatically.

Completion, cancellation, permission revocation, policy errors, navigation and deadline exhaustion all enter the same owned-key cleanup. Cleanup sends remaining keyups before waiting for pending policy or input replies, ignores cancellation, and has a separate bounded acknowledgement budget. It never replays an ambiguous input frame. A late `done` result cannot turn a timeout into success, and a rejected or unconfirmed release remains an error. Success returns `reason="done"`, `elapsed_ms`, `ticks`, `keydowns`, `keyups`, `pressed`, `released=true`, final `memory` and `delivery`; application completion still requires an observation.

The [protocol fixtures](../tests/support/browser_control_keys.rs) include differential switching, empty-key waiting, invalid initial decisions, cancellation, policy failures, navigation, delayed replies and a deadline reached while acquiring the final input permit. Two opt-in tests use owned Chromium pages: the capability probe checks the guarded reads and wrapper without sending input; the feedback fixture exercises DOM and canvas observations, fresh memory carried across iterations, trusted direction changes and confirmed releases. The feedback fixture also checks that DOM/global writes, observation/control writes and invalid memory fail without input from those policies. [Preflight tests](../tests/browser_control_keys.rs) cover schema, permissions and aggregate timeout accounting. These receiver checks exercise the controller on its own fixture; a real application's completion condition requires its own observation, recorded in the [acceptance record](validation-e2e-20261122.zh-CN.md).

## Explicit background focus for the selected page

Browser holds normally require the selected document to report visible and focused. If the user needs to work in another application during an authorized browser task, explicitly enable focus emulation on that selected page:

```text
browser.background(enabled=true)
browser.hold_until(keys="ArrowUp", canvas="motion", condition="state.available && state.latest.some(draw => draw.age_ms <= 100 && draw.center[0] >= 240)", timeout=3000)
```

The example assumes that the intended canvas watch and threshold have already been established. `enabled` is a required boolean, this command requires write permission, and each new Browser connection starts with the mode off. It uses CDP's experimental [`Emulation.setFocusEmulationEnabled`](https://chromedevtools.github.io/devtools-protocol/tot/Emulation/#method-setFocusEmulationEnabled); unsupported methods fail explicitly. The implementation sends the command to the selected desktop page session, verifies the reported focus and visibility, and does not request activation of an OS window or another tab. It does not enable a remote debugging endpoint. Mobile sessions are not supported by this API. See [the implementation](../src/browser/background.rs).

The receipt declares `focus_emulated`, `scope="selected_page_session"` and the observed top-level document's `frameId`, `loaderId` and URL. A direct page WebSocket can have null target/session IDs in the receipt; its connection still identifies the selected page. The mode does not remove the input guards: held input remains bound to its document, URL, active element, viewport and scale. Input continues through that CDP session, not through whichever OS window currently has focus. Focus reported under this explicit mode is emulated focus. Chromium's [trusted input injector](https://chromium.googlesource.com/chromium/src/+/refs/heads/main/content/browser/devtools/protocol/input_handler.cc) also calls `RenderWidgetHost::Focus()` for key and mouse events. That internal page-focus state can remain true after the override is disabled. Zuse does not activate another window or tab to make a focus observation turn false.

Zuse records the document identity when enabling the mode and checks it before and after each ordinary Runtime command. The checks sit outside the hold loop so they do not postpone keyup at the hold deadline. A different document ends the mode and returns `stale_target`; it does not silently extend authorization to the new document. Navigation through Zuse, selecting another page, closing the Browser and dropping it explicitly disable the old session's override. A failed disable keeps the cleanup unconfirmed and reports an error.

**Idle external navigation is detected at the next Zuse command.** This synchronous client has no background socket-reading thread. Chromium can carry focus emulation to a replacement renderer, so an externally initiated navigation can remain emulated while Zuse is idle. The next command boundary checks the actual document and attempts cleanup before proceeding. This is not an instantaneous navigation monitor. The boundary checks belong to Zuse Runtime; direct use of the lower-level Rust Browser API outside Runtime does not add them automatically. Chromium's renderer-switch and disable behavior is visible in its [Emulation handler](https://chromium.googlesource.com/chromium/src/+/main/content/browser/devtools/protocol/emulation_handler.cc).

After **every actual run**, submit the following as a separate cleanup call in the same Runtime, whether the run succeeded or failed:

```text
browser.background(enabled=false)
```

Check `focus_emulation_cleared=true` on success. This acknowledges removal of this connection's CDP override; it does not claim restoration of OS focus or require `document.hasFocus()` to become false. A cleanup line after a failing command in the same program may never run, because execution stops at that failure. A transport or protocol failure does not establish restoration, and no input is automatically replayed. An explicit off also remains available through the Browser API after cancellation. It sends a disable only to the page session on which this Browser attempted an override. Concurrent controllers on that same page are not isolated, and an unknown pre-existing focus-emulation setting cannot be reconstructed by this API.

The [loopback lifecycle tests and opt-in two-page Chromium fixture](../tests/support/browser_background.rs) cover direct-page cleanup, navigation, page selection, failed cleanup and selected-page trusted key delivery. The native fixture records the actual visibility baseline, checks an on/off cycle without input, and verifies the same visibility after input and override removal. It then changes focus only between its two owned fixture pages and requires the target to report unfocused; a residual override would prevent that check from passing. This fixture action is not performed by production cleanup and does not establish OS-window restoration. The [preflight test](../tests/browser_background.rs) checks the explicit boolean and write permission. Their execution and the real task's current evidence are recorded in the [acceptance notes](validation-e2e-20261122.zh-CN.md).

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

`browser.hold` operates on the already selected desktop page. It retains the document, URL, active element and viewport. The page must report visible and focused; only explicit `browser.background(enabled=true)` opts into the focus emulation described above. Navigation, a different active element, a changed viewport or a disconnected target ends the hold. The hold command never activates a tab and does not enable a remote debugging endpoint. It uses the existing browser connection.

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
