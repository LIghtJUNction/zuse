# Scoped mice and parallel pointer programs

`zuse script` creates named mice with their own position, button state, and visible cursor. An independent mouse is the default. On Linux X11, each one owns an XInput2 master pointer; moving it does not move the user's pointer. The mouse disappears when its owning block or function finishes, and the script closes every remaining mouse before returning.

The following examples describe the current repository implementation. Independent mice and custom native cursors require Linux X11 with XInput2, XTEST, and ARGB cursor support. [Platform selection](#platform-selection-and-limits) explains the explicit user-pointer option for other supported desktop backends.

## Run a first script

Validate the included example before opening a display:

~~~sh
zuse script examples/scoped-mice.zuse --dry-run
~~~

On a supported X11 session, run it with:

~~~sh
zuse script examples/scoped-mice.zuse
~~~

`zuse script FILE` reads a UTF-8 program; `-` reads standard input. Use `zuse script FILE --dry-run` to check this language. `zuse check FILE`, `zuse run FILE`, and `zuse eval CODE` use the existing [flat command language](language.md).

MCP clients use the `zuse.script` tool with a `code` string and optional `dry_run` and `read_only` flags. Each call owns its complete scoped run and closes its mice before returning. The `zuse.execute` tool continues to run flat command batches in the persistent runtime.

~~~text
let cyan = mouse(name="cyan", color="#2FE2FF", shape="arrow");
let violet = mouse(name="violet", color="#B58CFF", shape="ring");

parallel {
    cyan.move(x=320, y=180, duration=600);
    violet.move(x=520, y=320, duration=600);
}

cyan.position();
violet.position();
~~~

Both movements start from their respective live positions. Each newly created independent mouse initially copies the user's current coordinates; an optional `x` and `y` pair overrides that starting position. The two 600 ms movements overlap, and both mice close when the top-level script scope ends.

The compiler checks the whole program before creating a mouse or sending input. It resolves variable ownership, validates arguments and custom images, expands function calls and repeats, and rejects parallel conflicts. Dry-run can read a requested cursor image file, but it does not open a desktop or locate a live text target. Runtime failures still remain possible when a device, window, or text target changes after compilation.

## Variables, blocks, and functions

The variable that calls `mouse(...)` owns that mouse. An alias or function parameter borrows the same mouse; it does not create another device or extend the owner's lifetime.

~~~text
fn inspect(owner) {
    let local = mouse(name="temporary", color="#FFBF69", shape="crosshair");
    local.move(x=420, y=220, duration=250);
    owner.position();
} # local closes here; owner remains alive in its calling scope.

let main = mouse();
let alias = main;
{
    let nearby = mouse(shape="dot");
    nearby.move(x=180, y=160, duration=250);
} # nearby closes here.

inspect(alias);
main.move(x=360, y=240, duration=250);
~~~

Functions are declared at the script's top level and accept mouse variables as parameters. A function reads caller-owned mice through those explicit parameters. It has its own local scope, does not return values or mice, and cannot recurse. This keeps each native mouse's owner known before execution. `repeat 3 { ... }` runs a bounded literal number of iterations, with a new local scope for each iteration.

An out-of-scope variable, duplicate binding in one scope, unsupported argument, or recursive function is a compilation error. The default limits are 256 expanded actions and creations, 60 seconds of cumulative scheduled waits, 32 simultaneously live mice, and 256 created mice per script. `--max-steps` and `--max-wait-ms` adjust the execution budgets within the parser and compiler's hard bounds.

## Parallel and sequential work

Each immediate statement inside `parallel` is one branch. Put several operations inside an explicit block when they must run sequentially on one mouse while another branch progresses.

~~~text
let a = mouse(color="#2FE2FF");
let b = mouse(color="#FFBF69");

parallel {
    {
        a.move(x=200, y=180, duration=400);
        a.move(x=360, y=220, duration=400);
    }
    {
        b.move(x=480, y=300, duration=650);
        b.position();
    }
}

sequence {
    a.move(x=280, y=240, duration=300);
    b.move(x=440, y=260, duration=300);
}
~~~

`sequence` and its alias `serial` complete their children in order. Ordinary top-level statements and ordinary blocks are also sequential. The scheduler interleaves parallel branches on one operating-system thread and submits each path sample against a monotonic deadline. Native handles remain on that thread; no extra mouse-control worker thread is needed.

Two parallel branches cannot access the same mouse, including through aliases or function parameters. For example, `parallel { a.move(...); a.click(); }` is rejected; put the move and click inside one branch block. Different handles for the shared user pointer also represent one resource. A script permits only one live `user_mouse` owner at a time; borrow an alias or end its scope before creating another.

The wait budget counts the sum of scheduled work, including work in parallel branches. Two overlapping 600 ms motions consume 1,200 ms of that budget. Actual elapsed time also includes native calls and scheduling overhead. Parallel pointers do not isolate application state: a legacy application may accept only one active drag or pointer grab, and actions can still change shared application focus or content.

## Appearance and pointer methods

Generated cursors have a colored shape, a contrast outline, and a bounded glow. The hotspot is the pixel whose coordinates define the pointer position; glow padding does not shift that point.

| Constructor argument | Default | Meaning |
| --- | --- | --- |
| `name` | `"zuse"` | Human-readable label, 1..128 non-NUL UTF-8 bytes. Native device names also contain a unique component. |
| `color` | `"#2FE2FF"` | `#RRGGBB` or `#RRGGBBAA`; alpha scales the whole rendered cursor. |
| `shape` | `"arrow"` | `arrow`, `ring`, `crosshair`, or `dot`. |
| `size` | `32` | Shape size, 16..96 pixels, excluding glow padding. |
| `glow` | `0.7` | Strength from 0 to 1. Zero keeps the shape and outline. |
| `x`, `y` | User's current coordinates | Optional initial position; supply both. |
| `shared` | `false` for `mouse` | `true` explicitly selects the user pointer. |
| `appearance` | `"glow"` | `"system"` is available only for an explicit shared user pointer. |
| `image` | Unset | Local P7 PAM image, described below. |
| `hotspot_x`, `hotspot_y` | `0`, `0` | Hotspot for a custom image; each must be inside that image. |

`user_mouse(...)` is the explicit shared constructor and still defaults to a glowing appearance. Use `user_mouse(appearance="system")` to preserve the normal system appearance. `mouse(shared=true, ...)` selects the same shared mode.

| Method | Behavior |
| --- | --- |
| `p.position()` | Record the current position of this mouse. |
| `p.move(x=..., y=..., ...)` | Move to explicit screen coordinates. |
| `p.move_to(text=..., role=..., ...)` | Locate an accessible text or button and move to its center plus offset. |
| `p.click(button="left", count=1, interval=80)` | Click; interval is milliseconds between repeated clicks. |
| `p.down(button="left")`, `p.up(button="left")` | Hold or release a button. Scope cleanup releases synthetic held buttons. |
| `p.scroll(x=0, y=1)` | Scroll by notches; positive x is right, positive y is down, at most 1,000 per axis. |
| `p.appearance(color=..., shape=..., size=..., glow=...)` | Replace the generated appearance using those settings and defaults for omitted settings. |
| `p.appearance(image="cursor.pam", hotspot_x=2, hotspot_y=2)` | Replace the cursor with a custom image. |

Buttons accept `left`, `middle`, `right`, `back`, and `forward`. `wait(ms=...)` pauses its branch. The scoped language exposes these pointer operations and waits; keyboard, browser, device, and process operations remain available through the flat command language.

Appearance methods require a handle created with a generated or custom appearance. A handle created with `appearance="system"` cannot be restyled in place; close its scope and create a styled handle when a different appearance is needed.

### Path and target controls

Both `move` and `move_to` accept `duration`, `speed`, `steps`, `curve`, `jitter`, `speed_variation`, `seed`, `offset_x`, `offset_y`, and `via`. The default is a 350 ms smooth path. `duration` is milliseconds; `speed` is average sampled-path length per second, and the two are mutually exclusive. Coordinates use the selected backend's native screen units.

~~~text
let p = mouse(color="#2FE2FF");
p.move_to(
    text="Save", role="button",
    duration=650, curve="bezier",
    jitter=1.2, speed_variation=0.25, seed=42,
    offset_x=4, offset_y=-3,
    via="320,180;480,220",
);
~~~

This locates a fresh accessible target, reads this mouse's live starting position, computes the full path, then sends its samples. It only moves the mouse. Matching is exact and case-sensitive by default; `scope` and `index` select among multiple matches. Applications must expose usable accessibility text and bounds. See [target selection and path parameters](target-motion.md) for the complete geometry, limits, and failure rules.

### Custom images

Custom files use P7 PAM with `MAXVAL 255`, `RGB` or `RGB_ALPHA` tuples, and dimensions from 1 to 256 pixels on each axis. The loader validates the whole file and hotspot before mouse creation. `image` cannot be combined with generated `color`, `shape`, `size`, or `glow` settings. PNG and SVG decoding are not provided by this loader.

Rust callers can supply `CursorImage::from_rgba(...)`, `CursorImage::from_pam(...)`, or `CursorImage::load_pam(...)`. `CursorImage::pixels` stores premultiplied `0xAARRGGBB` values: each color channel is already multiplied by alpha. `from_rgba` performs that conversion from ordinary RGBA bytes.

## Touchpad gesture planning

Touchpad operations currently provide capability queries and pure gesture plans. No platform has a native gesture backend: `touchpad.capabilities()` executes and reports that native swipe, pinch, rotation, and cancellation are unsupported. A normal run containing `touchpad.swipe`, `touchpad.pinch`, `touchpad.rotate`, or `touchpad.cancel` fails preflight before any mouse input, including mouse actions written earlier in that program. There is no mouse-movement or wheel-event fallback.

Both flat batches and scoped scripts accept these operations for dry-run validation. Save the following as `gestures.zuse` and use `zuse script gestures.zuse --dry-run`:

~~~text
touchpad.capabilities();
touchpad.swipe(dx=240, dy=-80, fingers=3, duration=500, steps=60);
touchpad.pinch(scale=1.25);
touchpad.rotate(degrees=30);
touchpad.cancel();
~~~

`swipe` takes finite `dx` and `dy` deltas within ±1,000,000; `pinch` takes a total multiplicative `scale` from 0.01 to 100; `rotate` takes `degrees` from -3,600 to 3,600. Each gesture accepts `fingers=2..5`, `duration=0..60000` milliseconds, and `steps=1..4096`. Defaults are two fingers, 350 ms, and approximately 125 update intervals per second. Explicit steps must fit the duration in milliseconds; zero duration requires one interval. Gesture deltas have no native coordinate mapping yet.

Rust `GesturePlan` produces `Begin`, incremental `Update`, and `End` samples without device input. Update translations and rotation angles add; update scale factors multiply to the requested total scale. `cancel_sample` constructs a planned `Cancel` sample only. Neither it nor the dry-run `touchpad.cancel()` sends cancellation to a device.

## Rust ownership and custom paths

`Mouse` uses Rust ownership directly. A local mouse closes when its value is dropped, including when `?` returns an error from its function. `close()` allows a caller to receive cleanup errors explicitly and is safe to call more than once.

~~~rust
use zuse::{
    cursor::{CursorColor, CursorShape, CursorStyle},
    motion::Point,
    mouse::{Mouse, MouseOptions},
    pointer::MoveOptions,
};

fn run() -> Result<(), String> {
    let style = CursorStyle {
        color: CursorColor::parse("#B58CFF")?,
        shape: CursorShape::Ring,
        ..CursorStyle::default()
    };
    let mut pointer = Mouse::create(&MouseOptions {
        name: "rust-path".into(),
        appearance: Some(style),
        ..MouseOptions::default()
    })?;
    let motion = MoveOptions {
        duration_ms: Some(650),
        ..MoveOptions::default()
    };
    pointer.move_with(Point { x: 400.0, y: 240.0 }, &motion, |u, start, end| {
        Point {
            x: start.x + (end.x - start.x) * u,
            y: start.y + (end.y - start.y) * u - 48.0 * 4.0 * u * (1.0 - u),
        }
    })?;
    pointer.close()?;
    Ok(())
}
~~~

The callback receives path progress, the actual starting point, and the final endpoint after offsets. The planner evaluates and validates the samples before sending input. `move_to` takes coordinates; `move_to_text` takes a `TargetQuery`; `move_to_text_with` combines target lookup with a callback. Use `MovePlan` when only a path preview is needed.

Embedded callers can execute the scoped language with `script::execute(source, Options)`. `script::compile` returns a checked plan and summary without desktop input. `script::execute_with_cancel(source, options, || token.is_cancelled())` accepts an embedding application's cancellation policy; `CancellationToken` can be cloned and cancelled from another thread. The mouse handles themselves remain on the execution thread. Rust code permits only one live user-pointer owner in a process; borrow that `Mouse` or close it before creating another.

## Existing CLI, shell, and MCP programs

The flat runtime's pointer actions now use an independent mouse by default. The first move, click, button, or scroll action creates it lazily; `mouse.use()` creates or replaces the selected runtime mouse explicitly. `mouse.position()` is observation-only and requires a selected handle, so a fresh runtime must select a mouse before querying its position.

~~~text
mouse.use(kind="independent", color="#2FE2FF", shape="arrow", glow=0.7)
mouse.position()
mouse.move_to(text="Save", role="button", duration=450)
mouse.release()
~~~

`mouse.use(kind="user", appearance="system")` explicitly selects the user's pointer with its normal appearance. `mouse.use(kind="user", color="#FFBF69")` temporarily styles that shared pointer on X11. `mouse.release()` closes the current runtime handle; a later pointer mutation creates an independent mouse again unless another `mouse.use` selects a mode first.

A persistent `zuse shell`, MCP server, or Rust `Runtime` retains its selected mouse across completed flat batches. Explicit release or destruction of that runtime closes it. A one-shot `eval` or `run` closes its runtime as the invocation ends. Use scoped scripts when block, function, and whole-program cleanup are part of the program's structure.

## Cleanup, receipts, and interruption

A scoped run closes mice on normal scope exit, runtime error, and cooperative cancellation. Cleanup releases synthetic held buttons, removes this handle's cursor overrides, and removes its independent master pair. It does not delete the user's master pointer. Explicit `close()` failures are returned; destructors perform best-effort cleanup when no return channel remains.

The JSON receipt contains a plan summary, elapsed time, and events such as `create`, `move`, `position`, and `close`. Creation events include the native device ID when available. A close event reports whether release succeeded. A runtime failure returns `status="failed"` and an error after cleanup has been attempted; compile errors occur before native creation. The CLI exits nonzero for a failed receipt. A submitted click or path does not prove that the application accepted it; observe the target application's state when confirmation matters.

On supported 64-bit Linux and macOS builds, `zuse script` installs temporary SIGINT and SIGTERM handlers. They set a flag, and the scheduler returns through normal cleanup when it observes cancellation. Embedded library use does not install signal handlers. Windows retains its native Ctrl+C behavior; use a cancellation callback for cooperative library execution there.

SIGKILL, abort, and process crashes cannot run Rust destructors. XInput master devices are server-global and can outlive the creating connection, so cleanup after those terminations is not guaranteed. This implementation does not include a separate guardian process. A lost display connection can also prevent native cleanup.

## Platform selection and limits

| Mode | Linux native X11 | Wayland, Windows, macOS |
| --- | --- | --- |
| `mouse()` / independent `Mouse` | Independent XI2/MPX device and visible custom cursor | Explicit unsupported error |
| `user_mouse()` / shared glowing `Mouse` | Existing user pointer, temporary per-device appearance | Explicit unsupported error |
| `user_mouse(appearance="system")` | Existing user pointer with normal appearance | Uses the supported native system-pointer backend and its permissions |

X11 needs runtime libraries `libX11`, `libXi`, `libXtst`, and `libXcursor`. `ZUSE_BACKEND=x11` explicitly selects X11 when both display environments are present; the selected server must actually support the required operations. zuse does not substitute the user pointer if independent creation fails, and an XWayland display alone does not establish native Wayland independent-pointer support.

The X11 backend assigns a cursor specifically to each master pointer along its current window path. Closing a shared styled mouse removes those overrides so ordinary application cursors resume. XInput provides no query for a previous per-device cursor override, so an override installed by another XI-aware tool cannot be recovered. Applications' active grabs can also constrain motion or cursor presentation. See [platform support](platforms.md) for system-backend requirements and the scope of native verification.
