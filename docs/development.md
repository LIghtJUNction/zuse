# Development and release

Use Rust 1.100 or newer. The version and release scripts require Python 3.11 or newer for the standard-library TOML parser. The default host build has only Project Z Rust dependencies. Build-time packages for graphical tests are optional and are not linked into zuse.

## Local gates

~~~sh
python3 scripts/check.py
cargo check --locked --no-default-features --lib
cargo check --locked --target x86_64-pc-windows-gnu
cargo check --locked --target aarch64-apple-darwin
cargo bench --locked --bench core
~~~

The check script checks formatting, runs Clippy and tests, and checks the no_std library, including WebAssembly when that target is installed. Package validation is also performed before publication. Inspect the script for the precise current sequence.

Native integration checks use independent fixtures. The X11 and AT-SPI fixtures require Linux with the system dependencies listed in [Platforms](platforms.md); the browser fixture requires an installed Chromium executable:

~~~sh
python3 tests/native_x11.py
cargo test --locked --test browser_native -- --ignored
python3 tests/run_accessibility_native.py
python3 tests/transports.py
~~~

The tests do not need the operator's browser profile. Android wire tests use a fake ADB service. Read-only device checks are kept separate from input tests.

## Bump

~~~sh
python3 scripts/bump.py patch --dry-run
python3 scripts/bump.py patch --commit --tag --push
~~~

The bump script requires a clean working tree, updates the package version and lockfile, creates the release commit/tag, and atomically pushes main and the tag. Minor, major, and explicit stable versions are also accepted.

GitHub's **Bump and release** workflow exposes the same version input. It calls the reusable release workflow after pushing; this avoids relying on a GITHUB_TOKEN-generated tag to trigger another workflow.

## Published targets

| OS | Targets |
| --- | --- |
| Linux | x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu |
| macOS | x86_64-apple-darwin, aarch64-apple-darwin |
| Windows | x86_64-pc-windows-msvc |

Release identity is checked against Cargo.toml and the exact tagged commit. Each native build is packaged with its license, README, documentation, and runnable examples. GitHub releases publish all required archives. CARGO_REGISTRY_TOKEN enables Cargo publication.

The CI workflow runs for pull requests or manual dispatch. Local verification is the development gate; tagged native builds and publication remain the release gate.

## Reproducibility and resource use

The core benchmark reports work counts and elapsed time as JSON. Run it with a release profile when making performance comparisons. Measure the executable's RSS separately from a browser or application it controls; those external processes have their own memory use.

Release archives normalize metadata and timestamps. Package validation must include benches, examples, tests, and both README languages so a registry download can reproduce the build.

## Dependency propagation

Library updates include all direct and transitive consumers. Follow the required [maintenance procedure](maintenance.md), coordinate existing owners, and retain consumer test/commit receipts or a concrete blocker and takeover owner.
