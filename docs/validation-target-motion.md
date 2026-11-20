# Target movement validation

Recorded on 2026-11-21 (Asia/Taipei), with Rust 1.100.0 on Linux x86_64.

The production implementation is [96b6bda732b0e55521959c49255af7ef6b9b7225](https://github.com/LIghtJUNction/zuse/commit/96b6bda732b0e55521959c49255af7ef6b9b7225), including the functional trajectory layer from `592c26c`. The final acceptance change adds bounded observation of submitted X11 events to the native test; it does not change production input behavior. This is a source update at package version 0.2.2, not a new registry or binary release.

## Acceptance

| Check | Result | Scope |
| --- | --- | --- |
| Complete Rust suite | 189 passed, 0 failed, 4 ignored by default | Unit, integration, parsing, planning and protocol regressions |
| Format and Clippy | Passed | All targets, warnings denied |
| no_std core and functional motion tests | 20 passed | 17 core and 3 functional trajectory tests; also represented in the full suite |
| WebAssembly no_std library | Passed | wasm32-unknown-unknown compile check |
| Windows GNU all targets | Passed | x86_64-pc-windows-gnu compile check |
| macOS Intel all targets | Passed | x86_64-apple-darwin compile check |
| macOS Apple Silicon all targets | Passed | aarch64-apple-darwin compile check |
| Linux native accessibility and movement | 1 passed explicitly | Private Xvfb, D-Bus and GTK fixture; one of the four ignored tests |
| Rust target_motion example | Passed | Pure planning mode; no desktop input |

The default suite was run before a test-only Clippy correction and a macOS visibility guard that retains unknown visibility instead of assuming hidden. Clippy and both macOS target checks passed on the corrected source. The final native observation helper passed Clippy and the explicit Linux fixture. Unchanged browser, mobile and keyboard native fixtures were not rerun.

## Observed behavior

The private desktop fixture verified that movement starts at the current pointer position and reaches the exact rounded target center plus offset. It exercised a timed Bezier path with a waypoint, seeded displacement and speed variation, a Rust callback path, an external CLI call selecting a duplicate button explicitly, and an MCP call selecting visible text. The timed request did not return before the planned interval.

Missing, explicitly hidden and ambiguous targets failed without pointer movement. Insufficient dynamic wait budgets failed before the affected movement; a two-movement program preserved the first completed endpoint when the second exceeded the budget. An invalid callback was rejected before submitting input. Moving over a button did not invoke its click action. Existing accessibility editing, native button invocation and password redaction continued to pass.

X11 submits input asynchronously. The test observes successful endpoints for at most one second and compares exact integer coordinates, with no coordinate tolerance. Failure-path assertions remain immediate exact comparisons. Runtime receipts retain `delivery: "submitted"`; they do not claim application acknowledgement.

## Reproduce

Run the following commands from the repository. On the shared Project Z development host, hold `/var/tmp/project-z-cargo-target/.project-z-cargo-owner.lock` around the entire batch, without nested acquisition.

~~~sh
cargo fmt --all --check
cargo test --locked --offline
cargo clippy --locked --offline --all-targets -- -D warnings
cargo test --locked --offline --no-default-features --test core --test functional_motion
cargo check --locked --offline --no-default-features --lib --target wasm32-unknown-unknown
cargo check --locked --offline --all-targets --target x86_64-pc-windows-gnu
cargo check --locked --offline --all-targets --target x86_64-apple-darwin
cargo check --locked --offline --all-targets --target aarch64-apple-darwin
python3 tests/run_accessibility_native.py --manifest-path Cargo.toml
cargo run --locked --offline --example target_motion
~~~

Windows and macOS results are compile checks, not live desktop acceptance. The native fixture uses X11, not Wayland. The operation depends on the application's accessible text/name and screen bounds; it does not perform OCR or track a moving target after lookup. See [target movement](target-motion.md) for the argument contract and coordinate assumptions.
