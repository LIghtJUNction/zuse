# Observe public canvas drawing

The canvas observer records subsequent public 2D `drawImage` calls on one selected HTML canvas. It reports the image source metadata, drawing arguments and transform used by those calls. It does not read game variables, collision boxes, progress counters or canvas pixels, and it does not turn drawings into native controls or game entities. The implementation is in [canvas.rs](../src/browser/canvas.rs).

## Commands and limits

```text
canvas.watch(selector="#scene", name="motion", ttl_ms=10000, history=false)
canvas.read(name="motion")
canvas.close(name="motion")
```

The selector must match exactly one connected HTML canvas in the selected document. Watch installation wraps the public drawing method without calling `getContext`; calls still reach the original method with their original arguments, receiver, return value and exception behavior. One active watch is allowed per document.

| Parameter or budget | Limit |
| --- | --- |
| `name` | 1–64 ASCII letters, digits, `_` or `-` |
| `selector` | Nonempty, NUL-free, at most 4096 bytes |
| `ttl_ms` | 1–15000 ms; default 5000 |
| `history` | Boolean; default `true` |
| Retained event history | At most 4096 events with `history=true`; none with `history=false` |
| Source objects | At most 64 |
| Recorded data and returned receipt | At most 256 KiB |

Reaching a data budget stops observation and attempts to restore the original drawing method. A busy renderer may reach the history budget before the requested TTL. `watch` installs an observer and `close` removes it, so both are blocked by read-only execution. `read` is an observation operation. The [runtime registration](../src/runtime.rs) supplies the command defaults and preflight rules.

Use `history=false` when a continuing control loop needs the most recent draw from each source. This mode retains source metadata and one latest record per source; replacing that record subtracts its previous size from the retained-data budget. Repeated draws from the same sources therefore do not accumulate an event history. `event_count` still counts all accepted draws during the watch, while `retained_event_count=0`, `history_recorded=false` and `canvas.read` returns `events=[]`. The 64-source, 256 KiB and TTL limits still apply. A newly encountered source or larger latest record can still exhaust a budget.

Omitting `history`, or setting it to `true`, preserves full-history behavior. In that mode `retained_event_count` reports the retained history length and `history_recorded=true`. Neither mode reconstructs drawing before watch installation. Choosing not to retain history also does not establish which drawings remain visible.

## Read the evidence accurately

Each read reports `status`, `active`, `available`, `stop_reason`, `truncated`, `restore_conflict`, the count/history fields above, `sources`, `events` and `latest`. A newly installed watch can have `available=false` with `no_matching_2d_drawImage_observed`; it has not reconstructed earlier frames. `latest` contains the last recorded draw for each source object, with its `age_ms`, transform `matrix`, destination rectangle, transformed `quad` and `center`. These coordinates are in canvas bitmap units, as declared by `coordinate_space="canvas_bitmap"`; they are not CSS viewport input coordinates.

A source object may be drawn several times in one frame or reused for several sprites. Its last draw does not enumerate every instance. A retained draw also does not prove that it remains visible after a later clear, clipping, compositing or redraw. The receipt therefore declares `coverage="intercepted_2d_drawImage"` and `visibility="unverified"`. WebGL drawing, worker rendering and calls through references cached before the wrapper was installed are outside that coverage. `sources` contains bounded public metadata; data-URI contents are omitted.

## Use a drawing condition

`browser.hold_until` can bind a synchronous condition to the named watch without returning its opaque owner token to the controller:

```text
canvas.watch(selector="#scene", name="motion", ttl_ms=10000, history=false)
browser.hold_until(keys="ArrowRight", canvas="motion", condition="state.available && state.latest.some(draw => draw.source.src.endsWith('/marker.svg') && draw.age_ms <= 100 && draw.center[0] >= 240)", timeout=3000)
canvas.read(name="motion")
canvas.close(name="motion")
```

Replace the selector, source match and threshold with values established by observation of the intended page. The example tests a recent draw in bitmap coordinates; the 100 ms freshness threshold is an example policy, not a visibility guarantee. `state` is a new synchronous snapshot of this Browser's observer on each check. It contains `sources` and `latest`; retained event history is returned only by `canvas.read` when history recording is enabled. The expression must be boolean, and its observer binding plus expression must fit within the hold command's 16 KiB limit. See [conditional key holds](key-holds.md#hold-until-an-observed-condition) for release and timeout semantics.

For a policy that must change its active keys as observations change, use [`browser.control_keys`](key-holds.md#adjust-keys-from-observed-feedback) with the same `canvas` name. The controller obtains a fresh owner-bound public-draw snapshot on each iteration and copies it into an isolated world before invoking the side-effect-checked policy. `state` is read-only there; the separate fresh `memory` object carries at most 4 KiB of the policy's own JSON history. A result of `{keys:[],done:false}` waits without holding a key. Direction changes become differential trusted CDP key events from Rust.

Choose a TTL long enough for the bounded controller, including setup. Watch expiry or an invalid observation fails the controller and enters its key-release path. Latest-mode records still describe each source's last intercepted drawing call, which can remain after the source stops drawing. A policy must check age, expected source identity and coverage; neither the canvas binding nor a successful control receipt establishes visibility or game completion. Policy memory may calculate motion from successive public positions and timestamps, but no private application state is supplied.

When the authorized target must keep running while another application has focus, explicitly enable [selected-page background mode](key-holds.md#explicit-background-focus-for-the-selected-page) in the same persistent Runtime. A canvas watch itself does not emulate focus. After each actual run, submit `browser.background(enabled=false)` separately on both success and failure and check its cleanup result. An error stops a Zuse program, so a cleanup line after the failed command is not sufficient. Also close the canvas watch independently; failure of one cleanup must not prevent attempting the other.

## Lifetime and cleanup

The watch is bound to its Browser owner and document. Wrong-owner access, canvas removal or resizing, context loss, expiry and a replaced drawing method invalidate the state used by conditions. Explicit close discards the retained records. Page selection changes, navigation and Browser close attempt owner-specific cleanup; direct page WebSocket connections are included. If the page replaced the method, cleanup preserves that replacement and reports `restore_conflict` rather than claiming restoration.

TTL provides a fallback when the controller stops reading or disconnects; page scheduling can defer timer callbacks, and the observer also checks expiry before recording or serving active state. It does not release held input by itself. A lost CDP transport can leave key delivery or cleanup unconfirmed, which remains an error. These are ordinary page hooks and owner tags for cooperating Browser connections, not a security boundary against the page itself.

The [isolated Chromium fixtures](../tests/browser_canvas.rs) exercise public drawing, owner binding, expiry, restoration conflicts and data limits. The latest-mode fixture requests 5000 repeated draws followed by a fresh draw and separately checks the source, byte and TTL bounds. These tests are marked ignored unless explicitly run with Chromium available; the presence of a fixture is not an execution result. Implementation and fixture coverage do not establish completion of the real-game acceptance task; its current evidence is recorded [separately](validation-e2e-20261122.zh-CN.md).
