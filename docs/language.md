# Execution language

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

The first release uses flat bounded call batches. Control flow belongs to the agent or embedding application. There are no implicit shell substitutions, JavaScript evaluation, or variable interpolation.

## Discovery and limits

Run `zuse schema` to obtain the authoritative command list. The default execution limit is 256 calls, a 1 MiB source, 24 arguments per call, 65,536 bytes per string, and 60 seconds of cumulative scheduled waits. CLI flags can set the per-execution step and wait budgets.

The entire source is parsed and validated before any effect. Dynamic failures, such as a window disappearing, stop subsequent calls and include the failed step and previous results.

## Command families

| Family | Operations |
| --- | --- |
| mouse | position, move, click, down, up, scroll |
| keyboard | type, key, optional explicit window target |
| window / screen | list, focus, capture, text |
| wine | windows |
| accessibility | tree, focus, action, set_text |
| browser | launch, connect, tabs, select, navigate, snapshot, text, evaluate, click, fill, key, viewport, capture, close |
| android | devices, connect, status, snapshot, capture, tap, swipe, key, type, start |
| human | request, pending |
| process / harness | start, list/read, write/send, stop |
| ide | open, diff, extensions |
| omarchy | status, workspaces, workspace, themes, theme |
| system | capabilities |
| standalone | wait, notify |

## Input and observation

`keyboard.key(key="ctrl+a")` sends a chord. `keyboard.type(text="...")` sends text. On backends that support targeted window events, supplying `window="ID"` directs input to that window without changing the active window. Applications can reject synthetic targeted events; accessibility actions and browser commands are preferred when the target exposes them.

`screen.text()` reads accessibility text. `screen.text(ocr=true)` captures the display and uses the installed Tesseract executable. Screenshots are PNG files. MCP's screenshot tool also returns an image content block.

Native accessibility snapshots contain bounded tree paths. Pass a path from the current snapshot to `accessibility.focus`, `accessibility.action`, or `accessibility.set_text`. Observe again after navigation or structural changes.

## Processes and harnesses

`process.start(program="...",args="...")` starts the exact executable with a quoted argument parser. It does not invoke a shell. A shell can still be selected explicitly as the executable. Process reads return bounded incremental stdout/stderr and lost-byte counts.

Managed processes use pipes. Use the harness's documented pipe or machine protocol mode. Programs that require a terminal need a terminal-aware integration. The manager bounds active processes and output tails, and owns process cleanup.

## Human continuations

A human request saves the remaining program and pauses. The state directory is `ZUSE_STATE_DIR` when set, otherwise the platform state location. On Unix, request directories use 0700 and files use 0600. Decisions are explicit, expire, and are consumed once.

The runtime executes with the caller's OS permissions. Human handoff coordinates an operator and agent; it does not isolate an untrusted process from the operator's account.
