# X11 text delivery

Text outside an X11 keyboard layout, or foreground text whose key events
would conflict with held keys, modifiers or the active layout, uses the existing
AT-SPI adapter. The complete UTF-8 string is sent in one `EditableText.InsertText`
request. The adapter checks the inserted text and final caret before returning
success. It never replaces a global keycode mapping or temporarily owns the
clipboard.

## Why the previous path was unsafe

The previous implementation reused the last X11 keycode for each missing
Unicode symbol, emitted a key press, then restored the keycode. A fixed wait
cannot tell whether another application has consumed that event. `XSync`
confirms that the X server processed the sender's requests; it does not confirm
that the receiving application decoded its queued key events. A slow receiver
can therefore decode an older event using a newer mapping.

The same X11 implementation shipped in 0.1.1 and 0.2.0. A fixed comparison of
both releases, each with an ordinary GTK receiver and a deliberately paused GTK
receiver, observed wrong or missing characters in all four cases. The calls
returned success. This is a shared correctness fix, not a regression introduced
by 0.2.0's mobile-device changes.

## Delivery contract

| Operation | Behavior |
| --- | --- |
| All characters exist in the first two levels of the current map, and foreground keyboard state is safe | Preflight the entire string, then emit the existing native key events. Safe foreground state means released keys, no locked or latched modifiers, and layout group zero. |
| A character is absent, or a foreground key is held/modifiers or layout make key emission unsafe | Send the complete printable string through confirmed AT-SPI insertion. Choose this path before any native key event; never emit an ASCII prefix, release a user's key or wait for it to be released. |
| Background Unicode target | Refuse unless the requested window is already the actual X11 focus window. The caller can explicitly focus its chosen target, then call `type_text`. |
| Missing or ambiguous accessibility target | Return an error before sending text. The adapter does not guess a target or retry through keyboard events. |
| Timeout or rejection after insertion starts | Return an error explaining that text may have changed. Callers must inspect the target and must not automatically resend. |

The Unicode path accepts up to 64 KiB of NUL-free, non-control UTF-8. Control
keys remain separate `key` operations. X11 key-event submission remains
application-dependent; the stronger text/caret confirmation belongs to the
AT-SPI path. This does not add a system-wide input-method frontend or establish
compatibility with every application.

## Binding the editable object to the target

The caller's actual X11 focus window is captured. Its client ancestor must have
a local process identity and a `WM_CLIENT_MACHINE` matching the local kernel
hostname. A local X server may also have remote clients, whose numeric PIDs
must not be correlated with local accessibility applications. The accessibility
search is bounded to 4096 nodes,
32 levels and five seconds, with per-call AT-SPI timeouts. It considers only
that process and requires exactly one object claiming keyboard focus. A process
ID alone is insufficient: the active accessibility top-level window must also
match the X11 client rectangle or its standard EWMH frame rectangle. The focused
control must be visible, enabled, editable and inside the client rectangle.

Focus state is queried without relying on cached focus events. The caller's
X11 focus, process hint and geometry are checked again before mutation. A moved,
ambiguous or unsupported window fails explicitly. Password controls are rejected
before reading their text. These are conservative application-identity checks,
not an authentication boundary against a malicious desktop client.

## Character positions and selection replacement

AT-SPI caret and selection offsets count Unicode characters. `InsertText`'s
length argument counts UTF-8 **bytes**. The adapter sends `text.len()` for that
argument and uses `text.chars().count()` when advancing the caret and selection
offsets. Emoji, combining sequences and multibyte scripts are not byte-sliced.

A selection replacement is not an AT-SPI transaction. Before mutation, the
adapter snapshots the original selected text, limited to 65,536 Unicode
characters and 262,144 UTF-8 bytes. Larger selections are refused before input.
It first inserts the new string and confirms its length and exact contents.
Before deleting the displaced selection, it compares that range with the
snapshot. If an application callback rewrites the old selection, the new content
is preserved and the operation returns uncertainty. Successful replacement then
clears selection state and sets the final caret.

These checks avoid deleting a selection after a rejected insertion or knowingly
deleting text rewritten by a callback. The comparison and deletion are separate
requests, so a concurrent edit between them is still possible. A later failure
can leave a partial edit; the returned error makes that uncertainty explicit and
does not roll back or retry. A focus change before the first mutation reports
that no input was sent; one detected after insertion never makes that claim.

## Reproduce the native regression

Run `python3 tests/native_x11_utf8.py` on Linux with Xvfb, GTK3 development
headers, XTest, xauth, xdpyinfo, D-Bus tools and at-spi2-core installed. Use a
private Cargo target directory and one build job. The test reserves an explicit
display number between 10,000 and 29,999 with an atomic per-user directory lease,
kept until its Xvfb process exits. It checks the returned display number and
process, authenticates with a private cookie, and creates its own session bus,
accessibility bus and GTK receiver. It never searches from display `:0` or sends
input to the host desktop. The lease and cookie are removed during teardown.

The report is saved under `target/x11-utf8-proof/report.json`. It records actual
receiver text, caret positions, call results, a keymap digest, held-key and XKB
state, clipboard owner, and library versions. The cases cover an ordinary
receiver, a receiver stopped before input, a receiver paused inside its insert
callback, selection replacement, a callback rewriting the displaced selection,
a focus change after insertion, repeated commits, same-process windows,
unsupported targets, held-modifier preservation for both Unicode and mapped
ASCII, ordinary ASCII/shortcuts, filtered text and an uncertain reply. Input calls execute once per case; observation does not
resend text. Text and a binary clipboard format are checked after the run.

## Primary API references

- [Xlib `XSync`](https://www.x.org/releases/X11R7.5/doc/man/man3/XSync.3.html): server request completion and client event queues.
- [AT-SPI `EditableText.InsertText`](https://gnome.pages.gitlab.gnome.org/at-spi2-core/libatspi/method.EditableText.insert_text.html): UTF-8 data, character position and byte length.
- [AT-SPI `Text.get_caret_offset`](https://gnome.pages.gitlab.gnome.org/at-spi2-core/libatspi/method.Text.get_caret_offset.html): caret offset semantics.
- [AT-SPI `Accessible.get_process_id`](https://gnome.pages.gitlab.gnome.org/at-spi2-core/libatspi/method.Accessible.get_process_id.html): application process identity from its D-Bus connection.
- [AT-SPI state definitions](https://gnome.pages.gitlab.gnome.org/at-spi2-core/libatspi/enum.StateType.html): focused, editable, active, visible and read-only states.
- [AT-SPI cache invalidation](https://gnome.pages.gitlab.gnome.org/at-spi2-core/libatspi/method.Accessible.clear_cache_single.html): requesting a live state snapshot without changing the application's cache policy.
- [Extended Window Manager Hints](https://specifications.freedesktop.org/wm-spec/latest/ar01s05.html): client process and frame-extents properties.

## Consumer check

The actual ztype panel commits from its Enter key-press handler. An explicit
held-Return test against the first repair (`afcb4544`) reached six successful
native checks, then left the literal ASCII candidate unsent. This established
why printable mapped text also needs the confirmed AT-SPI route when a user key
is still down. The follow-up consumer proof checks the same single Enter press,
multilingual/plugin commits, candidate navigation and rejection by a read-only
GTK target. It uses an explicit private Cargo patch only during validation;
that source override is not an application release dependency.
