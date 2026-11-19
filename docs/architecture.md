# Architecture

The central abstraction is a validated call: an operation name, typed named arguments, and a source span. The parser borrows the program text. It does not allocate or perform I/O.

The motion kernel turns a start point, end point, duration, curve, jitter amplitude, and seed into timestamped samples. It returns exact endpoints. The host runtime applies those samples using the selected desktop backend.

## Shared executor

The CLI, embedded Rust interface, and MCP tools all call the same runtime. The runtime validates the whole batch, applies policy and budgets, then executes sequentially. A backend error stops later actions. Results identify the operation, elapsed time, completed steps, and structured output.

~~~mermaid
flowchart TD
    CLI["CLI or persistent shell"] --> Runtime["Validated runtime"]
    MCP["MCP transports"] --> Runtime
    Rust["Embedded Rust caller"] --> Runtime
    Runtime --> Desktop["OS desktop and accessibility"]
    Runtime --> Browser["Native CDP browser"]
    Runtime --> Android["Native ADB device"]
    Runtime --> Human["Human continuation"]
    Runtime --> Process["IDE and process adapters"]
~~~

## Dependency boundary

The no_std build contains the parser and motion kernel only. Host I/O is behind the std feature. MCP framing, protocol negotiation, JSON parsing, and HTTP validation reuse zmcpio. zclap parses CLI arguments and z64 encodes MCP image content. The transitive Project Z call contracts remain no_std libraries.

This release reuses the existing Project Z primitive libraries and is maintained in one repository.

## Sessions

Persistent shells and MCP servers own browser connections, managed child processes, and selected Android targets. A program can return multiple structured observations while retaining that session. A one-shot command ends its owned runtime when the command exits.

Human requests store their continuation with private local permissions. A local operator can approve, deny, or perform the assisted action. Modern MCP form elicitation provides a client-native completion dialog. Password entry stays in the local operator flow.

## Effects and permissions

The program runs with its caller's OS identity. Read-only mode is an operation policy; it prevents input and executable actions while allowing observation artifacts. Native application permissions, display authorization, browser endpoint authorization, and ADB device authorization still apply.

The HTTP listener uses plaintext HTTP; terminate TLS at a reverse proxy or use an encrypted tunnel for remote access. Network MCP binds to loopback by default. Remote binds require a token. HTTP checks Host and Origin before reading or executing the program, then uses zmcpio to validate protocol metadata and bounded framing.

## Protocol sources

- [MCP 2026-07-28 Streamable HTTP](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/streamable-http)
- [MCP 2025-11-25 lifecycle](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle)
- [Chrome DevTools Protocol](https://chromedevtools.github.io/devtools-protocol/)
- [Android ADB service protocol](https://android.googlesource.com/platform/packages/modules/adb/+/refs/heads/main/SERVICES.TXT)

Implementation tests and host-specific validation are recorded separately in validation.md.
