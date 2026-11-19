# macOS permissions and native control

zuse implements macOS desktop control through Quartz, ApplicationServices and System Events. Its permission diagnostics report the current process's access without requesting authorization. Both Apple Silicon and Intel targets have passed Rust type checks; this work has not been exercised on a live Mac. The distinction matters: successful compilation establishes that the code fits the target's types, while permission behavior, application input and captured pixels still need macOS runtime acceptance.

## Check the calling process

Run the diagnostic from the same terminal, IDE or application that will launch zuse:

```sh
zuse doctor
```

The result is JSON. The macOS fields are under `desktop`. The language exposes the same capability query:

```sh
zuse eval 'system.capabilities()'
```

The following is an illustrative fragment of the schema, **not a result observed on a Mac during this validation**:

```json
{
  "desktop": {
    "backend": "quartz",
    "permissions": {
      "accessibility": "denied",
      "screenRecording": "granted"
    },
    "permissionProbe": {
      "mode": "preflight_only",
      "promptRequested": false,
      "scope": "current_process",
      "automation": "not_checked",
      "deniedMeans": "not_currently_granted",
      "errors": {
        "accessibility": null,
        "screenRecording": null
      }
    }
  }
}
```

Each permission has three possible states:

| State | Meaning | Next step |
| --- | --- | --- |
| `granted` | The native query reports that this process currently has access. | Check the result of the requested operation in the target application. |
| `denied` | Access is not currently granted. The query does not distinguish a previous refusal from authorization that has never been given. | Have the human review the appropriate setting below, then run the diagnostic again. |
| `unknown` | zuse could not load the framework or resolve the permission-query symbol. | Inspect the corresponding `permissionProbe.errors` entry. Changing a permission setting alone does not resolve a missing API. |

`scope: "current_process"` means the native query concerns the process executing it. It does not establish permission for every application, another account or another way of launching zuse. Apple defines [AXIsProcessTrusted](https://developer.apple.com/documentation/applicationservices/1460720-axisprocesstrusted) in terms of the current process. A terminal launch, an IDE-hosted MCP server and a packaged application can have different responsible callers; authorize the caller macOS actually identifies and repeat the check from the intended launch context.

The diagnostic calls `AXIsProcessTrusted` and `CGPreflightScreenCaptureAccess`. It does not call an authorization-request API, enable a prompting option or start System Events. Consequently, `capabilities` itself does not ask macOS to display a permission dialog. Apple documents prompting separately through the options to [AXIsProcessTrustedWithOptions](https://developer.apple.com/documentation/applicationservices/1459186-axisprocesstrustedwithoptions).

Both query symbols are resolved at runtime through the shared zsys dynamic-library wrapper. A missing optional query becomes `unknown` instead of a required startup symbol. Apple's [CGPreflightScreenCaptureAccess reference](<https://developer.apple.com/documentation/coregraphics/cgpreflightscreencaptureaccess()>) lists its introduction in macOS 10.15. This handling does not establish a minimum supported OS version for the rest of zuse.

Permission queries run again on each capability check. Native input and capture also recheck their required permission before effects; `denied` and `unknown` produce explicit errors. In capabilities, `pointer` and `keyboard` become true only when Accessibility is granted, and `capture` becomes true only when Screen Recording is granted. These booleans do not confirm that a particular application accepted an event. See the [macOS implementation](../src/platform/macos.rs).

## Human authorization in System Settings

The human grants permissions to the actual calling application. Keep the zuse launch entry consistent while diagnosing access, especially when switching between a terminal and an IDE's MCP configuration.

1. Open **Apple menu → System Settings → Privacy & Security → Accessibility**. Enable the calling application shown by macOS. If it is absent, Apple's guide describes adding an application with the add button. This permits the native input and accessibility operations used by zuse. See [Allow accessibility apps to access your Mac](https://support.apple.com/guide/mac-help/allow-accessibility-apps-to-access-your-mac-mh43185/mac).
2. Open **Privacy & Security → Screen & System Audio Recording**. Enable access that includes screen recording for the calling application when screenshots or OCR are required. In the current Chinese guide, the section is **隐私与安全 → 录屏与系统录音**. See [Control access to screen and system audio recording](https://support.apple.com/guide/mac-help/control-access-screen-system-audio-recording-mchld6aa7d23/mac) and the [Chinese guide](https://support.apple.com/zh-cn/guide/mac-help/mchld6aa7d23/mac).
3. If macOS asks to quit and reopen an application, follow that instruction, restart zuse from the intended entry and run `zuse doctor` again.
4. For the System Events accessibility helper, review **Privacy & Security → Automation** when macOS presents an authorization request. Enable the actual caller's access to **System Events** when it is listed. Apple documents this separately in [Allow apps to automate and control other apps](https://support.apple.com/guide/mac-help/allow-apps-to-automate-and-control-other-apps-mchl108e1718/mac).

Automation is a separate permission. The native Accessibility and Screen Recording checks do not prove that Apple Events to System Events are authorized; the diagnostic therefore reports `automation: "not_checked"`. The first actual helper operation may require the human to answer an Automation prompt even though `zuse doctor` did not display one.

## Implemented desktop paths

The table describes the implementation. Live macOS acceptance is still outstanding for these operations.

| Operation | Implemented path | Permission and practical boundary |
| --- | --- | --- |
| Pointer position, movement, buttons and wheel | Quartz event APIs | Input effects require Accessibility. Position reads and input submission are separate operations. |
| Keyboard chords and Unicode text | Quartz keyboard events and UTF-16 strings | Requires Accessibility. Application handling must be observed after submission. |
| Input routed to a window's owning process | Window-to-PID lookup and `CGEventPostToPid` | Requires Accessibility; application dependent. Routing to a process does not select an arbitrary text widget. |
| Window enumeration | `CGWindowListCopyWindowInfo` | Returns the implemented visible-window metadata view; an available interface does not guarantee complete metadata for every window. |
| Window focus | ApplicationServices accessibility attributes and window raise | Requires Accessibility. Matching and behavior across applications remain unverified on a Mac. |
| Full-screen and window PNG capture | CoreGraphics image capture and ImageIO | Requires Screen Recording. Occluded and minimized-window behavior has not been established by live tests. |
| Structured accessibility and text | JXA through `osascript` and System Events | Depends on the helper's Accessibility/Automation access and the target application's exposed accessibility tree. |
| OCR text | A screenshot passed to the external Tesseract CLI | Requires Screen Recording and a usable `tesseract` executable in the launching process's PATH. |

Native keyboard results describe event submission with `delivery: "submitted"` and `application_confirmation_required: true`. An agent should inspect the resulting application state before treating text entry or a shortcut as completed. The [runtime](../src/runtime.rs) exposes this distinction; the [native backend](../src/platform/macos.rs) performs the OS calls.

## System Events and OCR helpers

Structured accessibility uses the system's `/usr/bin/osascript` with JavaScript for Automation (JXA). The helper talks to System Events and returns bounded snapshots containing roles, names, text, bounds and supported actions. It is an auxiliary system-program path, with its own authorization and application-coverage limits. The native permission diagnostic does not execute it. Implementation: [portable accessibility adapter](../src/accessibility_portable.rs).

After authorizing the intended caller, these commands inspect the window list and accessibility tree:

```sh
zuse eval 'window.list()'
zuse eval 'accessibility.tree(limit=128,depth=6)'
zuse eval 'screen.text()'
```

`screen.text()` reads text exposed through accessibility. Snapshot paths identify nodes in the current tree; obtain a fresh snapshot after the UI changes before reusing an action target.

OCR is selected explicitly:

```sh
command -v tesseract
tesseract --version
zuse eval 'screen.text(ocr=true)'
```

This path captures the full screen and runs `tesseract IMAGE stdout quiet`, with a bounded process timeout and output size. Check availability in the same environment that starts zuse: an executable available in an interactive shell may be absent from an application's launch PATH. Installing Tesseract does not grant the screen-recording permission.

The current OCR operation does not expose a language argument and does not pass `-l` to Tesseract. Tesseract's documented default is English; other languages require an appropriate language selection and data, so this interface does not establish multilingual OCR support. OCR yields text rather than accessibility roles or actionable node paths. See the [OCR integration](../src/integrations.rs) and [Tesseract's official command-line guide](https://tesseract-ocr.github.io/tessdoc/Command-Line-Usage.html).

## Verification recorded for this change

| Evidence | Result | What it establishes |
| --- | --- | --- |
| `cargo check --offline --locked --target aarch64-apple-darwin --all-targets` | Passed | The Apple Silicon target, including target-specific tests, type-checks. |
| `cargo check --offline --locked --target x86_64-apple-darwin --all-targets` | Passed | The Intel target, including target-specific tests, type-checks. |
| Missing-framework and missing-symbol permission tests | Compiled for both targets | Test code exists for mapping lookup failures to `unknown`; the tests were not executed on macOS. |
| Live macOS permission, input, capture, System Events and OCR acceptance | Not performed | No claim of successful Mac interaction, dialog behavior or screenshot correctness is based on these compile checks. |

The next runtime acceptance needs a Mac and an owned, disposable test application: record the capability states from the real launch context, have the human grant the required permissions, then verify delivered input and captured content in that fixture. This document records the current implementation and validation boundary; it does not turn cross-target type checks into runtime evidence.
