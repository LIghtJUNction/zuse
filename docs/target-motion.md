# Target selection and pointer paths

`mouse.move_to` locates a desktop text node or button and moves the pointer from its current position in one operation. The runtime reads a fresh accessibility snapshot, resolves one target, reads the live pointer position, validates the complete path, and submits its samples. The final point is the target rectangle's center plus the requested offset. This operation only moves the pointer.

## Preview and execute

Validate a request without reading the desktop or moving the pointer:

~~~sh
zuse eval 'mouse.move_to(text="Save",role="button",duration=650,curve="bezier",jitter=1.2,speed_variation=0.25,seed=42,offset_x=4,offset_y=-3)' --dry-run
~~~

Remove `--dry-run` to locate the live target and execute the movement. A plain request uses a 350 ms smooth path:

~~~text
mouse.move_to(text="Save", role="button")
~~~

The operation is available through the CLI, persistent shell, and MCP `zuse.execute`, which share the same executor. `zuse.check` and `--dry-run` validate parameters and the static wait budget; they do not resolve a target or return its live coordinates. `--read-only` rejects movement operations, including in dry-run mode.

## Selecting a target

Targets come from the operating system's accessibility tree: AT-SPI on Linux, UI Automation on Windows, and System Events on macOS. The application must expose an accessible name or text and usable screen bounds. The locator does not run OCR or a vision model, so image-only content and canvas text without accessibility information are outside this operation's scope. A text query selects the matching node's rectangle; it does not calculate a separate rectangle for a substring inside that node.

| Argument | Default | Behavior |
| --- | --- | --- |
| `text` | Required | Nonblank accessible name or text, at most 65,536 UTF-8 bytes. |
| `role` | Any role | Optional native role. `button` also recognizes `push button`, `ControlType.Button`, and `AXButton`. Role comparison ignores ASCII case. |
| `scope` | Whole snapshot | Current accessibility path such as `0/2/1`; includes that path and its descendants. |
| `exact` | `true` | Whole-string match. `false` accepts a substring. |
| `case_sensitive` | `true` | `false` compares Unicode lowercase forms. |
| `index` | Require a unique match | Zero-based index in the observed matching nodes, in snapshot order; the language accepts 0..4095. |

Use `role="button"` when a button and its child label expose the same text. Without an explicit index, multiple matches fail before movement. A complete snapshot with one matching target succeeds; a truncated snapshot or one containing observation errors requires an explicit index. `index=0` means “select the first observed match,” including in a partial snapshot. Supplying `scope` alone does not establish that a partial snapshot contains every match.

Inspect `accessibility.tree(limit=4096,depth=32)` to obtain a current scope. Accessibility child paths can change when the UI changes, so refresh them after navigation or structural edits. Missing bounds, invalid geometry, nodes marked hidden, and redacted password text do not provide a usable text target. Observation and movement form one call, but the application can still move or remove a target after observation; there is no continuous target tracking during the path.

## Geometry and timing

All coordinates use the selected native desktop backend's screen coordinate space. Negative coordinates are valid on desktops with monitors left of or above the primary display. The operation performs no independent DPI conversion or screenshot-coordinate inference. `via` points use absolute screen coordinates; `offset_x` and `offset_y` are relative to the resolved target center.

| Argument | Default | Units and limits |
| --- | --- | --- |
| `duration` | 350 when `speed` is absent | Movement milliseconds, 0..60,000. Zero produces an immediate two-endpoint move. |
| `speed` | Unset | Finite value greater than zero, in sampled screen-coordinate units per second. Exclusive with `duration`. |
| `steps` | `clamp(duration / 8, 1, 4096)`, or 120 with `speed` | Sampling intervals, 1..4096; the path has `steps + 1` samples. A positive explicit duration requires `steps <= duration`; zero duration requires one interval. |
| `curve` | `"smooth"` | `"linear"`, `"smooth"`, `"ease-in-out"`, or `"bezier"`. |
| `jitter` | 0 | Maximum per-axis displacement, 0..1000 screen-coordinate units. Smoothed seeded noise tapers to zero at both endpoints. |
| `speed_variation` | 0 | Seeded variation in path progress, 0..1. Progress remains monotone. |
| `seed` | 1 | Integer seed. Identical inputs and options reproduce the same sample path. |
| `offset_x`, `offset_y` | 0 | Offset added to the target center. |
| `via` | Empty | Up to 64 absolute points encoded as `"x,y;x,y"`; decimals and negative coordinates are accepted. |

Coordinates and offsets must be finite and within ±16,777,216. The resolved endpoint and every generated sample must also fit that range. The native input backend rounds each sample to integer coordinates before submitting it.

`via` defines a piecewise path with progress distributed by segment chord length. `curve="bezier"` bows each segment; the other curves change progress timing. The number of samples determines the resolution of the delivered path. Intermediate waypoints can fall between samples, and jitter can displace intermediate samples; the first and last samples retain the exact planned endpoints.

`speed` measures average speed over the complete sampled polyline, including curvature, waypoints, and jitter. Its planned duration is `ceil(path_length / speed * 1000)` milliseconds, with a minimum of one millisecond for a nonzero path. Curves and `speed_variation` control how progress changes within that duration. A speed-derived path longer than 60 seconds is rejected. Fast paths may have several samples scheduled in the same millisecond, with nondecreasing timestamps.

Every sample uses an absolute deadline relative to one monotonic start instant. The runtime sleeps until that deadline when it is still in the future. Input overhead therefore does not add another full interval at every step. System scheduling and backend delivery still determine the observed elapsed time.

Fixed movement durations count toward the program's scheduled-wait budget during validation. Speed-derived durations are checked against the remaining budget after target lookup and complete sampling, before the first event of that movement. An earlier operation may already have completed when a dynamic target or budget error stops the program. Target lookup time is separate from the movement duration and scheduled-wait budget.

## Rust: plan without desktop access

`MovePlan` generates and validates all samples without opening a desktop connection. Its endpoint argument is the target center; `MoveOptions::offset` is applied once during planning.

~~~rust
use zuse::{
    motion::{Curve, Point},
    pointer::{MoveOptions, MovePlan},
};

let options = MoveOptions {
    duration_ms: Some(650),
    curve: Curve::Bezier,
    jitter: 1.2,
    seed: 42,
    offset: Point { x: 4.0, y: -3.0 },
    via: vec![Point { x: 240.0, y: 140.0 }],
    ..MoveOptions::default()
};
let plan = MovePlan::new(
    Point { x: 80.0, y: 200.0 },
    Point { x: 420.0, y: 160.0 },
    &options,
)?;
assert_eq!(plan.to(), Point { x: 424.0, y: 157.0 });
for sample in plan.samples() {
    // sample.point and sample.at_ms are available for inspection or scheduling.
}
~~~

The executable [Rust example](../examples/target_motion.rs) defaults to a pure path preview:

~~~sh
cargo run --example target_motion
~~~

It uses supplied sample coordinates and prints plan metadata. It does not locate a live target or send native input in this mode.

## Rust: resolve and move in one call

`Runtime::move_to(&query, &options)` executes the built-in geometry. `Runtime::move_to_with(&query, &options, path)` accepts a `FnMut(f64, Point, Point) -> Point`. Its callback receives eased progress `u` from 0 through 1, the live starting position, and the offset-adjusted endpoint. The callback owns the geometry, so `via` must be empty. With a callback, `curve="bezier"` uses smooth progress timing; the callback defines any curved shape itself.

Return the supplied endpoints directly at `u == 0` and `u == 1`. Every callback sample, including the endpoints, is validated before the first native input event. A nonfinite coordinate, out-of-range point, or changed endpoint rejects the plan. Sampling invokes a stateful callback once per sample; speed calculation does not run it again.

~~~rust
use zuse::{
    motion::Point,
    pointer::MoveOptions,
    runtime::Runtime,
    target::TargetQuery,
};

fn arc(u: f64, from: Point, to: Point) -> Point {
    if u == 0.0 { return from; }
    if u == 1.0 { return to; }
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let bow = 0.5 * u * (1.0 - u);
    Point {
        x: from.x + dx * u - dy * bow,
        y: from.y + dy * u + dx * bow,
    }
}

let query = TargetQuery::button("Save");
let options = MoveOptions {
    duration_ms: Some(650),
    jitter: 1.2,
    speed_variation: 0.25,
    seed: 42,
    ..MoveOptions::default()
};
let receipt = Runtime::new().move_to_with(&query, &options, arc)?;
println!("{}", receipt.to_json());
~~~

To opt into real movement with the example:

~~~sh
cargo run --example target_motion -- --execute "Save"
~~~

The named button must be available in the current desktop's accessibility tree. Typed `Runtime::move_to` methods apply the movement options' bounds, including the 60-second per-move limit. Program-wide `Options::max_wait_ms` accounting belongs to `Runtime::execute`, which is also used by CLI and MCP programs.

## Results and failures

`MoveReceipt` returns the located target's path, native role, name and bounds, plus the live start, planned endpoint, planned duration, measured movement time, sample count, and sampled path length. Its JSON adds integer `x` and `y` fields for the submitted endpoint and `delivery="submitted"`. These are input-submission records; the receipt does not read back the final physical pointer position or confirm an application response. No click is part of this operation.

Static parameter errors are detected during whole-program validation. Target lookup and complete path generation finish before the operation sends pointer events. A native backend failure stops at the failing sample and reports how many preceding samples were submitted; the runtime then stops subsequent program operations.

The pure selection and planning implementations are in [target.rs](../src/target.rs) and [pointer.rs](../src/pointer.rs); execution and wait-budget handling are in [runtime.rs](../src/runtime.rs). Their regression cases are in [target tests](../tests/target.rs) and [pointer tests](../tests/pointer.rs). Platform-specific validation scope remains in the [platform documentation](platforms.md).

## Allocation-free paths and circles

`motion::FunctionalMotion` is the `no_std`, allocation-free sampler for a caller-defined `FnMut(f64) -> Point` and `PathOptions`. It produces timestamped samples without desktop access. The host `MovePlan` collects and validates samples so an invalid later point is caught before native execution; target lookup and `Runtime` require the `std` feature.

`mouse.circle` supplies a closed circular path through the shared sampler:

~~~sh
zuse eval 'mouse.circle(x=320,y=220,radius=100,duration=1600,speed_variation=0.25,jitter=0.7,seed=42)' --dry-run
~~~

Here `x,y` is the circle center, the first and last point is `(x, y - radius)`, and `duration` covers the circular trajectory. This command begins at that first circle point; use target movement or a coordinate move when the intended path must begin at the current pointer position.

## Validation

See the [source validation record](validation-target-motion.md) for exact local checks and platform coverage.
