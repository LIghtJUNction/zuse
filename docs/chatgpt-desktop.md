# ChatGPT desktop module

[`chatgpt-desktop.zuse`](../skills/zuse/assets/chatgpt-desktop.zuse) provides
importable native desktop helpers for inspecting a conversation, preparing a
draft, and invoking a separately authorized send action. Importing the module
registers its functions without observing or changing an application. The
helpers use native windows and desktop accessibility; they do not connect to a
browser, navigate, focus a window, type keys, or move the pointer.

## Load and inspect the functions

Run the catalog from the repository root. It needs no desktop connection:

```sh
zuse run examples/chatgpt-desktop-catalog.zuse --read-only
```

For live work, use one persistent shell and import the module once:

```text
import(path="skills/zuse/assets/chatgpt-desktop.zuse", as="chatgpt")
functions(namespace="chatgpt")
chatgpt.windows()
```

The returned catalog describes every parameter and each function's intended
effect. The current scoped desktop implementation uses Linux AT-SPI. Confirm
the actual backend with `zuse doctor`; a window title alone does not establish
which desktop application owns it. This module contains no built-in ChatGPT
installation path, account, conversation title, node path, or button index.

| Function | Effect |
| --- | --- |
| `windows()` | List native windows for target selection. |
| `inspect_window(scope)` | Observe one selected native window subtree. |
| `read_conversation(scope)` | Observe one selected conversation subtree. |
| `read_draft(editor)` | Return the selected editor's current native observation. |
| `prepare_draft(editor, pid, message)` | Require the selected process and an empty editor, write and read back the same native object, then observe the editor. |
| `send_prepared(send_button, action_index)` | Invoke exactly one native action at the supplied node and index. |

## Check the target and empty draft before writing

The calling agent identifies the intended conversation. The native edit
primitive then checks the editor's process and exact empty text immediately
before writing, and verifies the written text on the same resolved object.

1. **Select the unique intended desktop conversation.** Match the actual native
   window and process to its accessibility window, inspect that window, and
   identify the requested conversation and editor within it. Use a dedicated
   test conversation for acceptance work. Require a complete observation:
   `truncated` must be false and `errors` must be zero. Stop when the candidate
   is absent, ambiguous, stale, or belongs to a browser instead of the intended
   desktop application.
2. **Read the exact editor text and require it to be empty.** Call
   `read_draft(editor)` immediately before preparation. Check the selected
   editor's `text` field is exactly `""`; whitespace, existing text,
   `[redacted]`, a missing field, and an unavailable node all require stopping.
   Check the same observed path, process, role and editor identity. Pass the
   observed positive process ID as `pid`. The module also supplies
   `expected_text=""` and `expected_pid=pid` to the native edit operation; a
   failed precondition stops the operation before any text is written.
3. **Compare the prepared draft with the intended text.** Call `prepare_draft`
   once. Its first result contains the native edit receipt with the complete
   bounded `text`, checked `pid`, and `verified=true`; its second result observes
   the editor subtree. Check that both belong to the original target. Native
   snapshots preview at most 4096 characters, while the edit check and receipt
   cover up to 64 KiB of text. If preparation fails or its result is uncertain,
   observe the editor before deciding what to do next.

Supply the actual observed paths as string parameters. Paths such as
`0/application/window/editor` are explanatory notation, not usable AT-SPI paths.
Do not hardcode paths from an earlier application launch or infer a send action
index from its position in a screenshot. Obtain the node's advertised action
index from the current native snapshot.

`prepare_draft` never requests focus. Accessibility paths use child indices;
the native edit resolves its object once and keeps that object for its checks,
write and readback. The process check catches a path that now points into a
different application. The caller still establishes conversation identity and
stops when the original window or conversation changes.

## Conditional native edits

`accessibility.set_text` accepts optional `expected_text` and `expected_pid`
parameters. An expected text may be empty; it is compared exactly without
trimming. A missing or unreadable Text interface is rejected. A process ID must
be a positive integer from the observed native application. Omitting both
parameters keeps the existing unconditional edit behavior.

Linux text edits hold the caller's input permission only while submitting the
native request. Zuse builds the request before acquiring that permission,
queues `SetTextContents` with the public
[`dbus_connection_send_with_reply` API](https://dbus.freedesktop.org/doc/api/html/group__DBusConnection.html),
then releases the permission before waiting for the application reply or
reading the edited object. A slow application reply therefore does not keep
the cancellation dispatch gate or the host's input permission locked.

The final dispatch check rejects a cancelled execution or denied input
permission before queuing the request. Cancellation after submission stops
local waiting but cannot retract the request already queued for the
application. The error starts with `cancelled:`
and explicitly says that text was submitted and completion was not confirmed.
A reply timeout also reports submission. Inspect the current draft before
retrying either outcome; cancellation does not mean the editor stayed empty.

Reply waiting uses 10 ms D-Bus I/O slices and a 1 s deadline. The slice size is
not a general completion-latency guarantee: existing native callbacks and
readback calls can take longer, but they run outside the input permission.
The AT-SPI serialization lock still protects the native connection while these
calls run.

Windows and macOS currently return `unsupported_state` before input when
either condition is supplied; their existing unconditional edit path is
unchanged.

After a guarded write, Zuse reads the same native object and requires its full
bounded text to match the requested value. If an application changes that text
or readback becomes unavailable, the operation fails and subsequent batch
commands stop. The error explicitly says that input was submitted, so the
caller inspects the current editor instead of blindly retrying.

AT-SPI checking, editing and readback use separate application calls. They do
not form an atomic compare-and-set against concurrent user or application
input. A process ID also identifies the application, not a particular
conversation inside it. Keep these native checks together with the caller's
unique conversation selection and normal user takeover mechanism.

## Keep sending separate

Call `send_prepared` only after the user has authorized the specific target and
message, and the draft checks above have passed. The module offers no automatic
send, retry, conversation switching or follow-up loop. After the single action,
call `read_conversation` on the same selected transcript and inspect the actual
message state. When delivery is uncertain, inspect that state before any further
action. Importing this module or preparing a draft grants no sending authority.

## Validation

The module contract is covered by `tests/chatgpt_desktop_module.rs`. These tests
exercise the real module loader and command preflight: imports have no effects,
inspection works under read-only policy, writes and sends are rejected under
that policy, message strings remain literal data, and input helpers contain no
focus or shared-pointer operations.

```sh
cargo test --locked --test chatgpt_desktop_module
zuse check skills/zuse/assets/chatgpt-desktop.zuse
zuse run examples/chatgpt-desktop-catalog.zuse --read-only
```

The private GTK fixture additionally checks exact empty-text refusal, process
mismatch, missing text, password refusal, input-permission cancellation, full
text beyond the observation preview limit, application-modified readback, and
module draft preparation without a send or focus change. A second native test
holds a GTK text-change callback for 800 ms and checks that cancellation and
the cancelled execution return before that application reply, after releasing
the input permission. It also verifies that the following button action does
not run:

```sh
python3 tests/run_accessibility_native.py --test-target accessibility_guard_native
```

Use `--test-binary /absolute/path/to/accessibility_guard_native-HASH` to reuse
an already compiled integration-test harness. The launcher creates its own X
display and accessibility bus; the fixture is a local test application, not a
ChatGPT installation. Both native checks passed on the developer machine for
0.5.0; see the [validation record](validation-0.5.0.md#native-accessibility).

Those checks do not exercise a live ChatGPT desktop installation. Native
acceptance needs an actual application exposing the required accessibility
nodes, a uniquely identified dedicated conversation, an empty editor, and
explicitly authorized draft text. Sending needs separate explicit authority.
Record the observed draft and application result only for actions actually
performed; no private conversation content belongs in the repository.
