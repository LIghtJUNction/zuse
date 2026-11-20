# zuse 0.2.2 validation

The checked source is [b64d940202bbf2aefc9c76dbb9d9856368c0a897](https://github.com/LIghtJUNction/zuse/commit/b64d940202bbf2aefc9c76dbb9d9856368c0a897). This patch fixes browser target selection, macOS window selection, and mobile screenshot replacement. The source-stage build reports version 0.2.1. The official [0.2.2 release commit](https://github.com/LIghtJUNction/zuse/commit/94c837551a7a2fd8ec9f577a3e2f57309bd5ce5a) changes only the package version in Cargo.toml and Cargo.lock; the checked production source is unchanged.

## Behavior and evidence

| Area | Resulting behavior | Evidence |
| --- | --- | --- |
| Browser selection | Re-selecting the current page preserves its session and owned target. A candidate page is attached and initialized before releasing the current page. Explicit preparation or cleanup errors return without silently selecting the candidate. | Seven stateful loopback CDP regressions; the fixture tracks live targets and sessions and executes observations after failed selections. |
| Isolated Chromium | DOM content and the entered text survive selecting the current target. A nonexistent target returns an error while the original page remains usable. | One explicitly run headless Chromium regression with a private profile and successful browser cleanup. |
| macOS focus | The complete AX window list must contain one matching title before the application is brought forward. Duplicate titles, missing targets, and unreadable titles fail before focus effects. | Twelve portable production-helper tests, including four new regression tests; both macOS architectures compile. |
| Android capture | Truncated PNG output, missing image data, invalid chunk bounds, invalid termination, and trailing bytes fail before replacing a previous screenshot. Empty output paths fail whole-program validation. | Five ADB shell-v2 integration tests, including a table of malformed PNG responses and a valid byte-for-byte capture. |
| Mobile screenshot output | Android and iOS share staged file replacement. A write, flush, final deadline-check, or replacement failure preserves the earlier destination and attempts to remove the staged file. The tested failure cases leave no staged file. | One deterministic deadline-failure test and the Android replacement-failure test; the existing twenty iOS protocol tests pass. |

The Android regression reproduces a complete, successful ADB shell-v2 transaction whose output contains only the first 24 PNG bytes. The previous implementation accepted it as a screenshot. The fixed implementation rejects it and retains the previous destination.

PNG validation checks framing, required chunks and dimension bounds. It does not decode compressed pixels or validate chunk CRCs. The common writer checks the operation deadline immediately before replacement; synchronous filesystem calls are not interrupted by this check.

macOS selection still uses the window title to match the Quartz target to its AX representation. A duplicate title now produces an explicit ambiguity error. This avoids choosing the wrong window; applications with duplicate titles need a distinguishable target. Lookup and activation are separate native operations.

## Local acceptance

| Check | Result | Scope |
| --- | --- | --- |
| Complete Rust suite | 150 passed, 0 failed, 4 ignored by default | Unit, integration and protocol tests |
| Format and Clippy | Passed | All targets; warnings treated as errors |
| Native no_std library | Passed | Allocator-free core |
| WebAssembly no_std library | Passed | wasm32-unknown-unknown |
| Windows GNU all-target check | Passed | Compile check |
| macOS arm64 all-target check | Passed | Compile check |
| macOS x64 all-target check | Passed | Compile check |
| New native browser selection test | 1 passed explicitly | Real isolated headless Chromium |

The seven CDP, twelve macOS helper, five Android and twenty iOS tests are subsets of the 150-test suite. The additional explicitly executed native browser test is one of the four tests ignored by default. The unchanged X11, accessibility and original browser native tests were not rerun for this patch.

Commands:

~~~sh
python3 scripts/check.py
cargo check --locked --all-targets --target x86_64-pc-windows-gnu
cargo check --locked --all-targets --target aarch64-apple-darwin
cargo check --locked --all-targets --target x86_64-apple-darwin
cargo test --locked --test browser_selection_native -- --ignored
~~~

Windows and macOS checks are compilation and portable control-flow validation. Android and iOS checks use controlled host-side protocol peers. This patch does not claim new physical-device or live macOS/Windows desktop acceptance.

## Source changes

- [macOS selection](https://github.com/LIghtJUNction/zuse/commit/2195fce8bb3da8b73ad47834d43d6a2f1d68316f)
- [Browser selection](https://github.com/LIghtJUNction/zuse/commit/fe5d6bc4f47a3464a46d7ea358b99b2766ec56c2)
- [Mobile screenshot replacement](https://github.com/LIghtJUNction/zuse/commit/b64d940202bbf2aefc9c76dbb9d9856368c0a897)

The release identity, downloaded artifact checks and reverse-dependency receipts are recorded in the [0.2.2 delivery report](delivery-0.2.2.zh-CN.md) and [release evidence](evidence/release-0.2.2.json). The relevant consumers are ztype, ssmcp, zrc, oh-my-ztty and the Project Z aggregate.
