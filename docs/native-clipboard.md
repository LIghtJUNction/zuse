# Native Wayland text clipboard

`clipboard.read`, `clipboard.write` and `clipboard.restore` use a private in-process Wayland connection. They prefer `ext_data_control_manager_v1` and also support `zwlr_data_control_manager_v1` at version 1. Missing data-control access, an ambiguous seat or another unsupported session returns an error. There is no external clipboard command fallback, and these commands do not change primary selection.

```text
clipboard.read()
clipboard.write(text="中文\nsecond line 🙂")
clipboard.restore(path="/explicit/private/original-clipboard.txt")
```

`read` returns `present`, `mimeType`, `bytes` and UTF-8 `text`. No selection returns `present=false` and `text=null`; a nontext offer, invalid UTF-8, a changed selection or a failed transfer returns an error. Empty text is a distinct successful selection. Text and restore files are limited to 1 MiB and cannot contain NUL. `restore` accepts an explicit regular UTF-8 file and reads back the selected bytes for comparison. Its successful receipt contains `verified=true` and the byte count, without echoing the restored text. It never deletes the restore file.

`write` changes the clipboard, then keeps serving its source from a private Zuse worker thread. Its receipt does not echo the text and does not claim an application pasted it. Each native operation and source transfer has a three-second deadline; polling checks cancellation at intervals of at most 20 ms. Restore performs a bounded write followed by a separate bounded readback. Failure after a selection request may mean the clipboard already changed; the operation is not automatically replayed.

Keep the owning Runtime alive for as long as its text must remain available. A named persistent session can own the restored clipboard; ending that Runtime drops its native source. Another application may replace the selection at any time, at which point Zuse releases the old source. Before restoring a backup after an acceptance test, check whether the current selection is still the test text, so a later user copy is not intentionally overwritten. This observation and a following restore are separate operations.

The initial tests cover bounded text/file handling, command preflight, missing protocol, nontext selection, cancellation before writing, and a native wire fixture that requests UTF-8 text after `write` has returned. They do not control the user's desktop. Real compositor acceptance is recorded separately and is not implied by this implementation.

Implementation: [public API](../src/clipboard.rs), [Wayland owner and protocol tests](../src/clipboard/wayland.rs), [runtime preflight tests](../tests/clipboard.rs). Protocol definitions: [ext-data-control-v1](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/blob/main/staging/ext-data-control/ext-data-control-v1.xml), [wlr-data-control-v1](https://gitlab.freedesktop.org/wlroots/wlr-protocols/-/blob/master/unstable/wlr-data-control-unstable-v1.xml).
