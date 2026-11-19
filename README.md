# zuse

An agent-native computer use shell and Rust library in Project Z.

zuse runs small, inspectable programs that observe a desktop, move a pointer, type text, inspect accessibility trees, drive an isolated browser, and pause when a person needs to act. The same executor serves the CLI and MCP.

## Architecture

- A dependency-free `no_std` parser and deterministic motion core.
- Optional host runtime with native platform backends and explicit capability discovery.
- MCP integration through Project Z's `zmcpio` and CLI parsing through `zclap`.
- Human assistance represented as a pending request with an explicit resume step.

Implementation and local integration validation are in progress. The capability matrix and reproducible commands will be published with the initial release.

## License

MIT.
