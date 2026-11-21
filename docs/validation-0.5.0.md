# Validation for 0.5.0

## Native accessibility

The developer-machine run passed both tests in
[`accessibility_guard_native.rs`](../tests/accessibility_guard_native.rs).
The launcher created a private Xvfb display, accessibility bus, AT-SPI registry,
and GTK application. These results cover native editing and cancellation in
that owned fixture.

| Native check | Evidence and passing condition | Result |
| --- | --- | --- |
| Guarded draft preparation | `conditional_draft_edits_use_the_native_object_and_preserve_other_actions` rejects existing text, whitespace, wrong process identity, unavailable text, password input, denied permission, and prior cancellation. It checks full text beyond the observation preview, detects application-modified readback, and prepares a module draft without invoking the fixture button or changing native focus. | Passed |
| Cancellation after submission | `cancelling_a_submitted_edit_does_not_wait_for_the_gtk_reply` waits for a marker from an 800 ms GTK callback. The input permit must already be released, `cancel()` must return within 250 ms, and the cancelled execution must return within 300 ms after `cancel()` while the callback is still pending. The result must report submission, the following button action must remain unexecuted, and the submitted text must be observable after the callback finishes. | Passed |

The timing values above are assertions for this fixture. They do not establish
a general latency bound for native callbacks, readback, or arbitrary desktop
applications. The implementation separates request submission from reply
waiting in [`text_edit.rs`](../src/accessibility/native/text_edit.rs); cancelling
local reply tracking cannot undo a request already submitted to the app.

To reproduce the native run from the repository root:

```sh
python3 tests/run_accessibility_native.py --test-target accessibility_guard_native
```

The tests are ignored by ordinary `cargo test` because they need the private
native environment. The launcher requires Xvfb, xauth, xdpyinfo, dbus-daemon,
dbus-run-session, gdbus, AT-SPI libraries and registry, libdbus, and
`/usr/bin/python3` with Gtk 3 PyGObject. Supply
`--test-binary /absolute/path/to/accessibility_guard_native-HASH` to reuse an
already compiled test harness. The sandbox could not create Unix sockets, so
its failed environment startup was not counted as a native test result.

No live ChatGPT desktop conversation was used for this validation. Acceptance
of that integration still needs the actual native application, a uniquely
identified dedicated conversation, an empty editor, and explicitly authorized
draft text. A send requires separate authorization and observation of the
resulting message. The module's [usage and boundaries](chatgpt-desktop.md)
remain applicable: AT-SPI checks, edits, and readback are separate calls, and
the process guard alone does not identify a conversation.

## Integrated runtime and private input fixtures

The first integrated developer-machine candidate passed 395 ordinary Rust
tests, with 19 native opt-in tests left ignored by that command. The browser
hold test and both accessibility guard tests were then run explicitly in
their own private environments. The no-std library check and all 12 updater
CLI fixtures also passed. The updater fixtures use private installation
prefixes and simulated metadata; they do not update a real installation.

The isolated Chromium test observed trusted keydown/keyup events, hold timing,
cancellation and changed focus. A fake CDP regression also covers all eight
combinations of down/up success or failure and reply ordering. Its server
waits until keyup is submitted before replying, so the test demonstrates that
an outstanding or rejected keydown cannot prevent release submission.

A private Cage parent and Hyprland 0.57.2 child passed the Wayland hold
fixtures. For a requested 180 ms hold, the GTK receiver observed 172.205 ms;
the cancellation case released after 66.850 ms. The other two timed holds
observed 174.144 and 174.367 ms with Num Lock off and on. These are measurements
from that run, not timing guarantees. Focus change stopped execution with
`stale_target`; compositor state confirmed no remaining held keys. The
original keyboard's keypad symbols and lock states were preserved. The
private compositors opened no physical input or KMS devices and did not
connect to the operator's desktop.

The initial X11 fixture rejected a decimal window ID even though
`window.focus` accepted it. Its receiver observed no input, and the CLI
reported `stale_target`. X11 target selection now normalizes decimal and
hexadecimal IDs through the same parser before applying the existing target
checks. Pure regressions cover both forms, invalid IDs and invalid target
states. The final native run is recorded below.

The final integrated candidate passed **397 Rust tests**, with the same 19
native opt-in tests ignored by the ordinary command, and passed
`cargo clippy --locked --all-targets -- -D warnings`. Its X11 fixture then
passed all four cases: Num Lock off, Num Lock on, cancellation, and focus
change. The receiver measured 179 and 181 ms for the two 180 ms requests,
70 ms before cancellation cleanup, and 84 ms before focus-change cleanup.
Both lock states were preserved and no synthetic key remained held.

Native validation used the executable paths reported by Cargo with
`CARGO_TARGET_DIR` set by the developer environment. No binary was transferred
from the sandbox or added to Git. These tests used owned private targets;
they did not operate the user's running browser, chat windows or drawing.
