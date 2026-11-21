# Function discovery and lexical module validation — 2026-11-21

This iteration makes imported functions discoverable through the Rust API,
the CLI language and MCP. Explicit descriptions stay attached to the parsed
declaration. Catalog queries read retained metadata without calling functions
or rereading module files.

## Changes and evidence

| Behavior | Verification |
| --- | --- |
| Names, parameter order, descriptions and source locations survive nested file imports | Import a documented UTF-8 fixture, inspect its typed metadata, delete the source files, and inspect the retained catalog again |
| Documentation comes from adjacent standalone `///` comments | Cover UTF-8, CRLF, a BOM, blank lines, ordinary comments, block comments, string contents and trailing comments |
| Namespace filtering respects module boundaries | Select `tools` and exclude `tools_extra`; preserve deterministic ordering |
| Replacement is transactional | Failed preflight, dry runs and already cancelled preparation preserve the previous metadata; explicit replacement updates it |
| Description storage is bounded | Accept 4096 UTF-8 bytes and reject 4098 bytes before the preceding timer action can execute |
| CLI discovery has no function-body effects | List a function that starts a timer, then verify that the timer remains stopped |
| MCP discovery uses the current session | Advertise and invoke `zuse.functions`; reject invalid argument types and unknown properties; keep another session empty |
| Local mouse bindings obey lexical scope | Exercise blocks, sequences, repeats, parallel siblings, declaration order and parameter shadowing |
| A later local binding cannot hide a recursive function call | Reject the recursion during module preparation |

Three selected lexical regression tests failed on the original `ac5713c`
baseline. The isolated fix then passed 57 module/compiler/runtime tests. Its
changes were integrated with the metadata implementation before the final gate.

The initial metadata run passed seven tests and exposed one incorrect test
expectation: invalid MCP arguments return JSON-RPC `-32602`, rather than a
successful tool response carrying `isError`. The expectation was corrected and
the final complete test run passed.

## Final gate

Environment: Rust 1.100.0, Linux x86_64. Builds used the unchanged Cargo lockfile,
offline dependency caches, a dedicated target directory, and the shared developer
machine's outer Cargo owner lock.

| Command or fixture | Result |
| --- | --- |
| `cargo test --all-targets --all-features --locked --offline` | 372 passed, 0 failed, 18 explicitly ignored native/subprocess tests |
| `cargo clippy --all-targets --all-features --locked --offline -- -D warnings` | Passed |
| `cargo check --no-default-features --locked --offline` | Passed |
| `cargo fmt --all -- --check` | Passed |
| `zuse run examples/function-catalog.zuse --read-only` | Returned the two documented functions and the ordered `ms` parameter |
| `python3 skills/zuse/scripts/check_examples.py --binary <tested-binary>` | All five bundled examples passed preflight |

Eight metadata tests and four lexical tests were added. The public syntax-tree
shape and existing Rust `functions()` name-list methods remain compatible.
No Cargo dependency or lockfile changed.

## Scope of the result

These checks establish parsing, preparation, session persistence, CLI dispatch and
MCP protocol behavior. This iteration did not repeat native desktop input tests
or install the candidate onto the running host. The ignored tests are not counted
as passes. Source publication, downstream source propagation and tagged binary
releases are tracked separately.
