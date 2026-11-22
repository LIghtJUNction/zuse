# Persistent shell signal cleanup

Date: 2026-11-22 (Asia/Taipei).

The CLI now retains its SIGINT/SIGTERM handler for the full `zuse shell`
lifetime. An interrupt cancels active work, discards queued input, and exits
through normal Runtime destruction. This also covers an idle stdin stream or a
partially entered line. Managed child processes are terminated and reaped before
the shell exits.

## Reproduction and fix

The development machine's tracked Git installation was Zuse 0.6.0 at
`6e88dd885eb73545a1ab6a9cd50097ef63fba423`. A private shell launched
`/bin/sleep 60` through `process.start`, returned its completion receipt, and
then waited with stdin still open. Sending a signal only to that shell produced:

| Signal | Original shell exit | Managed child after shell exit | Fixed subprocess regression |
| --- | ---: | --- | --- |
| SIGINT | Terminated by signal 2 | Still running | Exit status 1; child reaped |
| SIGTERM | Terminated by signal 15 | Still running | Exit status 1; child reaped |

The reproduction explicitly cleaned up its own surviving process groups.
Neither the reproduction nor the regressions used desktop input.

The guard is created before the retained Runtime, so Rust drops the Runtime
before restoring the previous signal handlers. A shared internal execution
helper borrows the existing guard and gives each command a fresh cancellation
token. Idle waits check the flag in 25 ms slices while measuring the original
total idle deadline; running commands remain excluded from idle time.

## Local verification

| Check | Result |
| --- | --- |
| `cargo fmt --all` | Passed |
| `cargo test --locked --offline --test shell_signals --test shell_idle --test cli_code --test runtime_cancellation --test script_entrypoints -j 2` | 19 passed, 0 failed, 0 ignored |
| `cargo clippy --locked --offline --all-targets -j 2 -- -D warnings` | Passed |
| `cargo build --locked --offline --release --bin zuse -j 2` | Passed |
| Installed Git CLI signal acceptance | 6 cases passed; all managed children reaped |
| zuse-skills persistent shell function reuse | 1 passed, 0 skipped |
| SSMCP standalone CLI discovery | Passed; executable path and digest match the installed acceptance target |

The two new Rust tests execute six signal cases:

- SIGINT and SIGTERM while idle after starting a managed process.
- Both signals while stdin contains an incomplete line.
- Both signals during an executing program, with another program already queued.

The executing-program fixture asks its managed child to signal its parent after
receiving `process.write`. It verifies a cancellation receipt, absence of the
queued command's receipt, exit status 1, and that the child has been reaped.
This synchronization does not depend on a fixed sleep to guess when a command
has started. Failure cleanup is limited to processes created by the fixture.

Existing tests also cover inline code, read-only and dry-run behavior, step and
wait budgets, file/script execution, idle timeout refresh, explicit shell exit,
and CLI cancellation. Native subprocess verification was performed on Linux
x86_64. Other operating systems were not executed in this validation.

## Installed CLI acceptance

The development machine's existing Git installation was updated with
`cargo install --git https://github.com/LIghtJUNction/zuse.git --locked --force -j 2`.
Cargo records source commit
[`290d8a571f2a94003851e71dedb98cdddf9a440a`](https://github.com/LIghtJUNction/zuse/commit/290d8a571f2a94003851e71dedb98cdddf9a440a);
the executable reports `zuse 0.6.0`. Its SHA-256 is
`1fc732c9f771155bd75eb9bea779862fc0fb79b69d34f42d80ff7e06a5cf3c10`.

An independent subprocess acceptance run exercised SIGINT and SIGTERM against
that installed file in each of three states: idle, incomplete stdin line, and
active work with a queued next command. All six cases exited with status 1,
reaped their managed child, and left the queued command unexecuted.

The zuse-skills test
`NativeModules.test_persistent_native_shell_retains_module` passed against the
same installed CLI: imported functions remained available in the next shell
submission. SSMCP's existing `computer.version` probe then returned
`available: true`, version `0.6.0`, and the same executable digest. These checks
used owned subprocesses and read-only module discovery.

## Consumer scope

This change affects the standalone CLI's `src/main.rs`. Library targets,
public APIs, package manifests, dependency locks, and published release assets
are unchanged. The reverse-dependency review used Project Z's
`47bd4a54e3e6a48df8361cde4f61cc8974320795` catalog and the source entries below.

| Consumer | Observed source and actual relationship | Required change |
| --- | --- | --- |
| ztype | `c89dd36dc7dea1fcbded743c03500a7544d3201e`; calls the Zuse desktop library through its optional desktop feature | No library or lock change |
| SSMCP | `2c37cf61562ff1cd521f8aa4344aac7ea9e06c03`; discovers and manages an independent Zuse executable, with no Zuse Cargo dependency | Existing CLI discovery passed against the installed executable |
| zrc | `31f37e9bb8f5c5adcf195f68692c47ec7ea26705`; indirect SSMCP integration, no Zuse executable or library pin | No source or lock change |
| oh-my-ztty | `dbb909b78d152f21a0c270724a1b958b843d0388`; bundles ztty, ssmcp, zdot, zfetch and zmem, with no Zuse binary entry | No bundle or component-lock change |
| zuse-std | `d79f43f8f3c2a1bff05962420f014b6c16403c5c`; declaration-only source package requiring Zuse 0.6.0 or later | No package change |
| zuse-skills | `201a7508d3ed2928295ebaf0853ea4528212b980`; invokes CLI programs and checks persistent-shell function reuse, without a Zuse Git-source lock | Persistent-shell reuse contract passed against the installed executable |
| Project Z | Existing snapshot records Zuse at `6e88dd885eb73545a1ab6a9cd50097ef63fba423` | Retain the independent aggregate snapshot; this CLI patch does not claim to update it |

An interrupt now closes the entire shell. Controllers that need to cancel one
job and preserve their Runtime should use `session cancel NAME`, whose named
session protocol is unchanged.
