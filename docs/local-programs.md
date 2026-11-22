# Local programs, observations, and reusable functions

Zuse expands a small program, checks the entire result, and executes it locally.
Observation and input remain separate operations. A subscription can wait for a
change without another model turn; input requires an explicit action command.

## Functions and imports

Place reusable declarations in a UTF-8 `.zuse` file:

```text
fn read_chat(scope) {
    observe(source="desktop", scope=scope)
}

fn prepare_message(editor, message) {
    accessibility.set_text(path=editor, text=message)
}

fn send_prepared(send_button, action_index) {
    accessibility.action(path=send_button, index=action_index)
}
```

Load it into one persistent `zuse shell` session, then invoke the functions with
verified node paths. A function definition is not authorization to send a message.
Confirm the application, conversation, recipient and intended content before an
authorized call to `send_prepared`.

```text
import(path="wechat.zuse", as="wechat")
wechat.read_chat(scope="0/1")
```

The path above is illustrative. `skills/zuse/assets/wechat.zuse` contains the
declarations; it does not discover a recipient or send anything when loaded.
AT-SPI paths are application-instance data and must be obtained from the actual
selected window. This is a reusable starting point, not a universal WeChat adapter.

An import accepts exactly one explicit local `path` or `source` string. Importing
registers functions only; imported top-level actions, including actions nested in
an eval, are rejected. Entry-file paths resolve against the entry file's directory,
and nested paths against each module's own directory. The loader never changes
the process working directory or downloads a module.

```text
import(source="fn inspect(){timer.status()}", as="tools")
tools.inspect()
eval(source="fn pause(ms){wait(ms=ms)} pause(ms=1)", as="local")
local.pause(ms=2)
```

Explicit `eval(source=...)` may register definitions and execute actions in the same
submission. Actions join the same preflight, read-only policy, step count and
cumulative wait budget as the outer program. Arguments are substituted through
the syntax tree; quotes inside a string cannot inject a second command. Parameters
are literals or parameters from the enclosing function, not arbitrary expressions.

Functions remain available in the same `Runtime` or shell session. Separate CLI
processes do not share definitions. An identical import is reusable; replacing a
namespace requires `replace=true`. Failed preflight and dry runs leave retained
definitions unchanged. A later execution failure returns completed steps and
retains definitions from the successful preflight.

The loader rejects recursive calls and cyclic imports. Limits include 64 modules,
256 functions, 16 import/eval levels, 32 call levels, 1 MiB per source or expanded
program, and 4 MiB of source read or retained per preparation/session. Expansion
work is bounded even when a repeat body emits no actions.

`zuse run`, `zuse script`, and `zuse check` select the executor from syntax.
Scoped `let`/mouse programs retain their existing ownership, borrowing and parallel
conflict checks. Imported scoped functions keep their borrowed mouse identity.
Words such as `parallel` inside quoted text do not select another executor.
The explicit `script` command also preserves the original scoped engine's bounded
event receipts for compatible wait/capability-only programs, using pure preflight
of the already expanded source to decide.

## Discover callable functions

Write a description as consecutive `///` lines immediately before a function.
The loader keeps those lines with the declaration and derives its name, ordered
parameters and source location from the parsed program.

~~~text
/// Inspect the timer without starting it.
fn inspect() {
    timer.status()
}

/// Wait for the requested number of milliseconds.
fn pause(ms) {
    wait(ms=ms)
}
~~~

After importing the module, query its catalog before choosing a function:

~~~text
import(path="examples/function-library.zuse", as="tools")
functions(namespace="tools")
~~~

`functions()` lists every retained function. `namespace="tools"` includes
`tools.inspect` and nested modules under `tools`, while excluding `tools_extra`.
The result contains `functions` and `count`. Each entry has `name`, `parameters`,
`description`, `source`, `line` and `column`, sorted by fully qualified name.
It does not include function bodies, reopen source files or invoke functions.
Run the complete read-only example with:

~~~sh
zuse run examples/function-catalog.zuse --read-only
~~~

MCP clients use `zuse.functions` with an optional `namespace` after loading
declarations through `zuse.execute` in the same session. Rust integrations use
`ModuleSession::function_metadata()` or `Runtime::function_metadata()` for typed
`FunctionMetadata` values; `Runtime::function_catalog(namespace)` returns the
same JSON catalog. The existing `functions()` Rust methods still return names.

A blank line or ordinary comment ends a documentation block. Trailing `///`
comments, text inside strings and block comments are not function descriptions.
Descriptions are UTF-8, limited to 4096 bytes per function, and count toward the
session's retained-source budget. Missing documentation produces an empty
description. The metadata states what the author wrote; it does not infer
parameter types, grant input permission or establish runtime success.

Changing a retained description follows the same replacement rules as changing
its function. Failed preflight, dry runs and already cancelled submissions leave
the existing catalog unchanged. Imported module names follow lexical scope:
a same-named mouse variable shadows a module only after its declaration and
within its own block or parallel branch.

## Structured observation and local waiting

```text
observe(source="browser", selector="#conversation")
watch(source="browser", selector="#conversation", name="chat")
wait_until(watch="chat", condition="text", text="ready", timeout=5000)
observe(watch="chat")
watch.close(name="chat")
```

Connect an explicit browser session first. A browser watch uses mutations in the
selected container. A native desktop watch uses AT-SPI events in its selected
application/subtree. Their default results are structured text; neither path takes
screenshots. `condition="text"` requires a text filter; `changed` rejects text/role
filters, while `visible` and `gone` accept optional text/role filters. Timeout and
cancellation wake the local wait and return
a reason. A successful wait continues the remaining original program.

## Bounded appearance-triggered clicking

```text
watch_until_click(source="browser", scope="#board", selector=".ready",
                 origin="http://127.0.0.1:8000", max_clicks=1, timeout=5000)
```

This explicit action binds the original document, origin, tab and container. It
uses simple compound selectors and requires one unique target. A continuous
visible episode produces at most one click; disappearance and reappearance start
another episode, within the requested count and deadline. Focus loss, navigation,
scope/container detachment, occlusion, ambiguity and cancellation stop the run. Default count is
one. There is no model roundtrip, screenshot or repeat selector search in the
event-to-input path.

DOM verification and CDP input are separate protocol commands; they cannot make
renderer state and dispatch atomic. Receipts distinguish event-to-dispatch time
from trusted page click receipt. A written socket frame alone is not confirmation
that the application processed input. Interrupted cleanup only reads the original
release response, without replaying input. An unconfirmed release remains an
explicit uncertain cleanup result.

SSMCP's `computer.watch_until_click` additionally requires an explicit `host`
desktop, loopback endpoint, existing tab, exact HTTP(S) origin, scope, selector,
count and timeout. It reuses the original host Ticket and per-dispatch file gate.
Disabling host input revokes that Ticket; a running watch cannot reacquire a fresh
permission. Waiting for changes or browser replies never holds the dispatch gate.

`Browser::connect_existing_cancellable` attaches without creating, activating or
navigating a page. Cancellation also covers discovery, WebSocket handshake and
attachment; closing the wrapper preserves the externally owned tab.

## Retained targets and explicit image matching

```text
target.locate(text="Save", role="button", window="active", name="save")
mouse.use(kind="user", appearance="system")
mouse.click(target="save")
```

Semantic location happens once. A click checks the retained window identity,
geometry, scale, focus and the original accessibility node, followed by native
point hit checks. A changed or covered target stops. Ambiguous semantic matches
return bounded candidate details; they do not silently choose the first match.

The optional `vision` feature adds explicit multi-scale template matching:

```text
target.locate(template="send-icon.png", scales="1,1.25,1.5",
              mode="edges", min_score=0.90, min_margin=0.05,
              window="active", name="send")
```

It uses bounded normalized cross-correlation over the complete requested search,
then consolidates overlapping candidates. A search that exceeds its budget fails
instead of returning a partially searched unique match. More than one plausible
target is ambiguous. The receipt gives candidates, scores and native coordinates.
Pixel-to-window mapping accounts for the captured image dimensions and native
window size, including 125% scale.

Clicking a retained image target captures the selected window and compares the
small retained pixel patch plus window metadata. It does not run NCC again.
Native point hit testing currently supports X11; stricter retained-target clicks
on other backends fail if that check cannot be established. Linux PNG decoding
uses the system libpng 1.6 library. Image matching is not OCR.

## Visual time slices

```text
timeline.start(window="active", columns=3, rows=3, name="board")
timeline.wait_until(name="board", timeout=5000, interval=32)
timeline.observe(name="board")
timeline.render(name="board", path="board-timeline.svg")
timeline.close(name="board")
```

This is an explicitly selected local capture path, separate from event-based text
observation. The first frame supplies a fixed baseline and the selected window is
divided into cells. Entry/exit thresholds provide hysteresis. Twelve successive
visible samples are one episode; absence followed by reappearance is another.
The first appearance event is available immediately, before the object disappears.
`start` captures only the baseline, `sample` captures one frame, and `wait_until`
samples until the first appearance then returns. There is no background sampler.
Continue with explicit samples or later waits to record disappearance and further
episodes. The interval is a requested wait after each capture/processing pass;
actual frame spacing also includes that work and is not a fixed-period guarantee.

`observe` reads retained metadata and episodes without capturing a frame. Only `render` writes an SVG: one baseline
image, later appearances darker on a shared time scale, and numbered/timestamped
reappearances in the same cell. Runtime sessions, retained baseline bytes, cells,
history and event buffers are bounded; overflow is reported. Geometry, focus or
capture dimensions changing rejects the current sample/wait. Recorded data remains
available to observe/render/close. Explicitly close and start again to choose a new
observation scope.

## Timing and cancellation

```text
timer.start()
wait(ms=10)
timer.status()
timer.reset()
wait(ms=5)
timer.stop()
```

Every ordinary command records `elapsed_us`. While the timer is running, timing
also includes `command_us`, `total_elapsed_us`, `command_total_us`, and `commands`.
Total elapsed time includes idle time between shell submissions; cumulative command
time counts execution only. Measurements use a monotonic clock.

Runtime cancellation wakes waits and stops future frames. The CLI bridges
SIGINT/SIGTERM into the same local cancellation token. SSMCP keeps its emergency
host-input revocation and shared-file dispatch gate. Native pinch, rotate and swipe
planning remains available, but unavailable native touchpad input is explicitly
unsupported; scrolling and browser zoom are not substituted.

The persistent `zuse shell` owns its signal handler until its retained Runtime
has been dropped. On Linux and macOS builds with native signal support,
SIGINT or SIGTERM cancels active work and exits the whole shell with status 1.
An executing program returns its cancellation receipt; queued input is discarded.
Idle input and incomplete lines also exit through normal cleanup, including
termination and reaping of managed child processes. The signal handler only sets
a flag; resource cleanup runs on the ordinary Rust execution path.

The shell checks idle interruption every 25 ms while preserving the configured
total idle timeout. Command execution remains excluded from idle time. Use
`session cancel NAME` when a controller needs to cancel one job and retain a
named session for subsequent work. Linux subprocess checks and their exact
coverage are recorded in [shell signal validation](validation-shell-signals-20261122.md).

## Reproduce the included examples

```sh
python3 skills/zuse/scripts/check_examples.py --binary /path/to/zuse
python3 skills/zuse/scripts/check_examples.py --binary /path/to/zuse --execute-fixtures
```

The second command launches only owned headless browser fixtures and a local timer
example. The WeChat module is checked but never invoked. See
[validation evidence](validation-20261121.md) for observed results and limits.
