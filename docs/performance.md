# Initial performance baseline

Measured on 2026-11-20 (Asia/Taipei) for zuse-computer 0.1.0, Linux x86_64, Rust 1.100.0, Intel Core i9-12900HX. The measured source tree was committed as [dd57e38](https://github.com/LIghtJUNction/zuse/commit/dd57e38a7b0eafb11f438a539c7dbbac98b349a0).

| Metric | Result |
| --- | ---: |
| Release executable | 1,146,072 bytes (1.093 MiB) |
| Idle MCP resident memory | 3,428 KiB (3.348 MiB) |
| Anonymous resident memory | 372 KiB |
| Idle MCP threads | 1 |
| Parsed calls | 300,000 in 68,690,615 ns |
| Mean parser time | 228.97 ns/call |
| Motion samples | 650,000 in 10,234,664 ns |
| Mean motion generation time | 15.75 ns/sample |
| Local Linux release archive | 566,274 bytes |

## Method

The optimized build used the release profile, a one-CPU scope, a 1 GiB memory limit, and one Cargo build job. The benchmark ran once. It parsed a three-call, 115-byte source and generated seeded Bezier motion samples without desktop input.

MCP resident memory was read from the owned process's /proc status after initialize, notifications/initialized, and tools/list. The server advertised eight tools. Browser and target-application memory are measured separately; this run did not launch a browser.

The working tree was committed while the measurement was running; committing did not change its source bytes. A clean source-tree check matched the final release commit after measurement.

## Reproduce

~~~sh
cargo build --locked --release --target x86_64-unknown-linux-gnu
cargo bench --locked --bench core
~~~

The benchmark emits JSON with exact work counts and elapsed nanoseconds. The integer nanoseconds-per-item fields are truncated; the table above divides the recorded totals before rounding.

These are single-run development-machine measurements. Cross-run variation and complete desktop/browser task latency have not been measured.
