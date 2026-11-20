# zuse 0.2.1 validation record

Recorded on 2026-11-20 (Asia/Taipei). This record covers verified source behavior and local executables. The formal version bump and release publication had not occurred at this snapshot. Release identity and downloaded-artifact results belong in [the delivery record](delivery-0.2.1.zh-CN.md).

## Source and executable identity

| Input | Identity | Role |
| --- | --- | --- |
| Integrated source | [8c0a0f07fe53d36a92ed01f510a2414f13f63663](https://github.com/LIghtJUNction/zuse/commit/8c0a0f07fe53d36a92ed01f510a2414f13f63663) | Integration of the source changes |
| Test entry points and changelog | [52c4cdb0a90f8cccddad47090c3eb0e3b9a8ab29](https://github.com/LIghtJUNction/zuse/commit/52c4cdb0a90f8cccddad47090c3eb0e3b9a8ab29) | Recorded owner-side checks and executable acceptance |
| Refreshed X11 implementation | [8cb97129feeed2eea82b52aefaaf0743a29122b1](https://github.com/LIghtJUNction/zuse/commit/8cb97129feeed2eea82b52aefaaf0743a29122b1) | Reviewed focus, selection and held-key handling |
| Tested debug CLI SHA-256 | `ba171c507503ba48c60026a7648fc904fc0d27ac63b1851bde4843f7bab82562` | Actual CLI used for the 17-case GTK acceptance run and the recorded PTY/USB harness runs |
| Tested CLI version | `0.2.0` | Version reported before the formal 0.2.1 bump |
| Final source acceptance SHA | [a6dc1b0a47de4327aa6407a0881da547811c41eb](https://github.com/LIghtJUNction/zuse/commit/a6dc1b0a47de4327aa6407a0881da547811c41eb) | Fill from the final consolidated source receipt |

The source gate, native CLI details and downloaded release artifact have separate receipt destinations:

- [Source acceptance](evidence/acceptance-0.2.1.json): exact revision, commands and final gate results.
- [X11 CLI acceptance](evidence/x11-cli-0.2.1.json): executable identity and receiver observations.
- [Release acceptance](evidence/release-0.2.1.json): published artifact identity, download and execution results.

The source and CLI receipts are included with this acceptance snapshot. The release receipt is recorded after publishing and downloading the formal artifact.

## Executed checks

| Layer | Result | Scope |
| --- | --- | --- |
| Full Rust suite | **133 passed, 0 failed, 3 graphical tests ignored by default** | Host-side suite at the recorded integration |
| Portable regression additions | **21**, included in the 133 passes | Serial path validation: 2; Windows keyboard planning: 11; macOS focus control: 8 |
| Mobile-related Rust tests | **20 iOS and 15 Android-browser tests**, included in the 133 passes | Host-side adapter and protocol/fixture coverage |
| Formatting and lint | Passed | Formatting and Clippy checks |
| `no_std` builds | Passed | Native and WebAssembly targets |
| Cross-target all-target checks | Passed | Windows GNU and macOS arm64/x64 |
| GTK3/AT-SPI through the actual debug CLI | **17/17 expected outcomes** | 9 successful operations and 8 expected errors in an isolated native X11 session |
| Legacy native X11 and AT-SPI | Passed | Real private displays, input/PNG checks and the explicitly invoked accessibility test |
| MCP transports | **12/12, no skips** | stdio, HTTP, Unix sockets, errors, human handoff and a native isolated PNG |
| Linux serial PTY harness | **5/5 groups, exit 0** | Real kernel pseudoterminals, including 7 preflight cases |
| Isolated libusb C ABI harness | **11/11 groups, exit 0** | Native ABI calls against a controlled device fixture, including 11 preflight cases |

The 21 portable tests and the mobile-related Rust tests are subsets of the 133 Rust passes. The external native harness counts are recorded separately. The three default-ignored graphical tests remain visible in the suite result. AT-SPI was explicitly rerun, and the new GTK fixture was exercised through the actual CLI. The unchanged desktop Chromium path retains its preceding 0.2.0 receipt; it was not rerun in this iteration.

The portable tests call production validation, message-planning and focus-control code with supplied platform outcomes. They cover 8,194 accepted and 60 rejected COM path samples; Windows key order, scan-code flags and system-message state; and macOS query/action errors, missing targets and object lifetime. The `COM+1` regression failed against the old validation and passed after the fix. Windows/macOS coverage in this record consists of these host-side tests and cross-target checks.

## Native text input

The 17-case GTK run used the actual debug CLI identified above, with a real GTK3/AT-SPI receiver on an isolated X11 display and bus. Each case checked the receiver state after a single input operation.

| Scenario group | Cases | Observed outcome |
| --- | ---: | --- |
| Text commits, receiver pauses, selection/caret handling, held-key text, ASCII and existing shortcuts | 9 | Operation succeeded and receiver text matched the expected result |
| Background or noneditable target, mismatched process identity, remote X-client identity | 4 | Input was rejected as expected |
| Filtering, uncertain response, focus change after insertion, application rewrite of the old selection | 4 | The operation returned the expected error with the observed receiver changes preserved |

ASCII text while Shift is held now uses the AT-SPI text path. A focus change after insertion reports that input may already have been applied. When the application rewrites the old selection during insertion, zuse stops instead of deleting a range based on stale text and offsets.

Selection replacement is nontransactional: insertion, application callbacks, verification and any subsequent deletion are separately observable steps. A failed confirmation can therefore leave inserted or application-modified text. The caller should inspect the receiver before deciding what to do next; zuse does not automatically resend or blindly roll back uncertain edits.

The fixture's `CLIPBOARD` text, binary payload and owner were preserved. The `PRIMARY` owner changed during the tested operations. This record consequently describes preservation of the tested `CLIPBOARD` state.

The first legacy native-X11 attempt stopped because the new helper incorrectly required an optional `.XN-lock` file. The corrected helper uses an atomic display lease, a private Xauthority cookie and an authenticated readiness probe. The affected X11, AT-SPI and MCP checks then passed. The consolidated receipt retains the initial failure. A Git diff confirmed that production Rust, Cargo inputs, and the already-passed GTK/PTY/USB scripts were unchanged by this helper correction. Final explicit legacy X11/AT-SPI and MCP results: `native_x11.py` passed pointer input, foreground/background text and screen/window PNG checks; the explicit AT-SPI test passed observation, editing, actions and redaction; all 12 MCP transport tests passed with no skips..

## Serial and USB behavior

The serial run exercised real Linux pseudoterminals. It checked all 256 byte values in both directions, an 80 ms timeout scenario and restoration of termios state. Under backpressure, a 32,768-byte request stopped after an observed partial write of 15,360 bytes. The receipt records that partial result alongside the seven preflight cases.

The USB run exercised the libusb C ABI with isolated, controlled device responses. Its eleven groups covered reads, writes, partial results, timeout handling and resource cleanup, together with eleven whole-program preflight cases that produced no earlier side effects. Physical USB-device transfers are outside this fixture's execution scope.

## Consumer source integration

A real `ztype` executable was tested with the candidate dependency supplied through a private local source patch. Its held-Enter regression first exposed the old behavior; the updated source passed **11 checks**. This is verified consumer source integration. Registry dependency publication and the consumer's versioned dependency update remain separate propagation records in [the delivery record](delivery-0.2.1.zh-CN.md).

## Release boundary

The tested executable reports 0.2.0 because the formal bump had not yet run. Its SHA-256 above identifies the local debug binary. The future tag, release workflow, downloaded binary identity and registry versions must be populated from the completed release actions in `evidence/release-0.2.1.json` and the delivery record.

