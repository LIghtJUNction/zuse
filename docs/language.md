# Flat execution language

This page describes `zuse eval`, `zuse run`, `zuse check`, the persistent shell, and MCP flat batches. Use [`zuse script`](scoped-mice.md) for mouse variables, block/function lifetimes, and parallel or sequential pointer tracks. Validate that separate language with `zuse script FILE --dry-run`.

A program is a sequence of named calls. Newlines and semicolons separate calls; `#` and `//` start comments outside strings. Argument lists can span lines and end in a trailing comma.

~~~text
mouse.move(
  x=320,
  y=180,
  duration=300,
  curve="smooth",
)
~~~

Values are decimal numbers, booleans, quoted strings, or bare identifiers. Quoted strings use JSON-style escapes, including Unicode surrogate pairs. Duplicate names, positional arguments, malformed numbers, unknown operations, and unknown parameters are rejected. Error locations use byte offsets with one-based line and character columns.

`zuse shell` accepts one complete program per input line. Use `zuse run FILE` for files containing multiline calls.

This command language uses flat bounded call batches. Control flow for these batches belongs to the agent or embedding application; scoped pointer programs have their own `zuse script` entry point. There are no implicit shell substitutions, JavaScript evaluation, or variable interpolation.

## Discovery and limits

Run `zuse schema` to obtain the authoritative command list. The default execution limit is 256 calls, a 1 MiB source, 24 arguments per call, 65,536 bytes per string, and 60 seconds of cumulative scheduled waits. CLI flags can set the per-execution step and wait budgets.

The entire source is parsed and validated before any effect. Fixed durations count toward the scheduled-wait budget during validation. For `mouse.move_to(speed=...)`, the duration becomes known after target resolution and complete path sampling; the runtime checks and deducts it from the remaining budget before moving the pointer. Dynamic failures, such as a window disappearing, stop subsequent calls and include the failed step and previous results.

## Command families

| Family | Operations |
| --- | --- |
| mouse | use, release, position, move, move_to, circle, click, down, up, scroll |
| keyboard | type, key, hold, optional explicit window target |
| window / screen | list, focus, capture, text |
| wine | windows |
| accessibility | tree, focus, action, set_text |
| browser | launch, connect, tabs, select, navigate, snapshot, text, evaluate, click, fill, key, hold, viewport, capture, close |
| android | devices, connect, status, snapshot, capture, tap, swipe, key, type, start |
| human | request, pending |
| process / harness | start, list/read, write/send, stop |
| ide | open, diff, extensions |
| omarchy | status, workspaces, workspace, themes, theme |
| system | capabilities |
| standalone | wait, notify |

## Input and observation

Pointer actions create an independent runtime mouse by default, currently requiring native Linux X11 MPX. `mouse.use()` selects one explicitly; `mouse.use(kind="user", appearance="system")` selects the user's normal system pointer on supported desktop backends. `mouse.position()` only observes the selected handle and returns an error in a fresh runtime without one. `mouse.release()` ends its lifetime; persistent shell, MCP, and Rust runtimes otherwise retain it across completed batches. See [mouse ownership and platform limits](scoped-mice.md).

`touchpad.capabilities()` reports the current gesture capabilities without a display. `touchpad.swipe`, `touchpad.pinch`, `touchpad.rotate`, and `touchpad.cancel` support validation and dry-run planning; native gesture injection is currently unsupported and fails whole-program preflight. See [gesture arguments and planning semantics](scoped-mice.md#touchpad-gesture-planning).

`keyboard.key(key="ctrl+a")` sends a chord. `keyboard.type(text="...")` sends text. On backends that support targeted window events, supplying `window="ID"` directs input to that window without changing the active window. Applications can reject synthetic targeted events; accessibility actions and browser commands are preferred when the target exposes them.

Use `keyboard.hold(key="Return", duration=250)` or `browser.hold(key="Space", duration=250)` for a single press and release. Both commands require 1–60,000 milliseconds and reserve that time from the program wait budget. The hold starts after the press acknowledgement. Target checks and transport acknowledgements add elapsed time. The runtime sends one key-down and one matching key-up on completion or cancellation while the original target is valid. Applications may implement their own key repeat. A successful receipt reports submission.

`keyboard.hold` uses Hyprland's targeted `send_key_state` dispatcher. It requires an inspectable, idle keyboard with Caps Lock and Num Lock off, layout group zero, and a focused window with a stable ID. An explicit `window="0x…"` must match that focused window. Each edge checks the original address and stable ID in the compositor. Other native desktop backends currently return `unsupported_state`.

`browser.hold` binds the selected desktop page, main document and focused element. Frame and author shadow-host focus are unsupported. A detected focus or document change stops execution with `stale_target` and suppresses further key events. CDP checks and input use separate protocol commands, so a renderer change can race the final send. A transport failure can leave delivery unconfirmed.

`screen.text()` reads accessibility text. `screen.text(ocr=true)` captures the display and uses the installed Tesseract executable. Screenshots are PNG files. MCP's screenshot tool also returns an image content block.

Native accessibility snapshots contain bounded tree paths. Pass a path from the current snapshot to `accessibility.focus`, `accessibility.action`, or `accessibility.set_text`. Observe again after navigation or structural changes.

## Targeted pointer movement

`mouse.move_to` finds an accessible name or text and moves from the current pointer position to that node's center. It does not click. Its default is an exact, case-sensitive, unique match and a 350 ms smooth movement.

~~~text
mouse.move_to(
  text="Save",
  role="button",
  duration=650,
  curve="bezier",
  jitter=1.2,
  speed_variation=0.25,
  seed=42,
  offset_x=4,
  offset_y=-3,
)
~~~

`scope="0/2/1"` restricts matching to a fresh accessibility path and its descendants. `exact=false` matches a substring; `case_sensitive=false` compares Unicode lowercase forms. `index=0` explicitly selects the first observed match. A partial accessibility snapshot requires an explicit index, even when a scope is supplied.

`duration` is movement time in milliseconds, excluding target lookup. `speed` is the average sampled path length per second and is exclusive with `duration`. `offset_x` and `offset_y` are added to the target center. `via="320,180;480,220"` specifies absolute screen-coordinate waypoints. The backend's native coordinate space applies to all points, including negative coordinates on multi-monitor desktops. `steps` sets sampling resolution, and the backend rounds samples to integer input coordinates.

`zuse eval 'mouse.move_to(text="Save",role="button")' --dry-run` validates the request without locating a live target. See [target motion](target-motion.md) for all parameter bounds, programmatic paths, output fields, and failure behavior.

## Processes and harnesses

`process.start(program="...",args="...")` starts the exact executable with a quoted argument parser. It does not invoke a shell. A shell can still be selected explicitly as the executable. Process reads return bounded incremental stdout/stderr and lost-byte counts.

Managed processes use pipes. Use the harness's documented pipe or machine protocol mode. Programs that require a terminal need a terminal-aware integration. The manager bounds active processes and output tails, and owns process cleanup.

## Human continuations

A human request saves the remaining program and pauses. The state directory is `ZUSE_STATE_DIR` when set, otherwise the platform state location. On Unix, request directories use 0700 and files use 0600. Decisions are explicit, expire, and are consumed once.

The runtime executes with the caller's OS permissions. Human handoff coordinates an operator and agent; it does not isolate an untrusted process from the operator's account.
