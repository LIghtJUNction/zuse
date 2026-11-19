# zuse

**An agent-native computer-use shell, Rust library, and MCP server.**

zuse turns small programs into desktop observations and actions. The same executor drives native windows, an isolated Chromium browser, accessibility trees, Android displays, IDE entry points, and managed harness processes. Human assistance pauses a program and gives it an explicit continuation.

[中文](README.zh-CN.md) · [Language](docs/language.md) · [Platforms](docs/platforms.md) · [Architecture](docs/architecture.md) · [Releases](https://github.com/LIghtJUNction/zuse/releases)

## Small core, explicit effects

The parser and pointer-motion kernel compile with `no_std`, use no allocator, and have no dependencies. The optional host runtime uses Rust's standard library and five Project Z crates across its dependency tree: zmcpio, zcall, zconv, zclap, and z64. There is no embedded JavaScript engine or browser automation framework.

The runtime checks the entire program before its first effect. Unknown operations, wrong argument types, duplicate arguments, malformed strings, excessive steps, and excess wait budgets fail before execution. Runtime errors return completed steps and stop the remaining program. Execution records omit keyboard input contents.

## Install

Rust 1.100 or newer:

~~~sh
cargo install zuse-computer --locked
zuse --version
zuse doctor
~~~

The Cargo package is **zuse-computer**; the executable and Rust library are named **zuse**. The short package name was already registered by another project.

To install from this repository:

~~~sh
cargo install --git https://github.com/LIghtJUNction/zuse --locked
~~~

Native archives for Linux, Windows, and macOS are published on the [Releases page](https://github.com/LIghtJUNction/zuse/releases). Extract the matching archive and place its zuse executable on your PATH. Remove that executable to uninstall; Cargo installations use `cargo uninstall zuse-computer`.

## First program

~~~sh
zuse eval 'mouse.position(); window.list()'
zuse eval 'mouse.move(x=400,y=240,duration=300,curve="bezier",jitter=1,seed=42)' --dry-run
zuse run examples/desktop.zuse
zuse shell
~~~

Programs use named calls separated by newlines or semicolons:

~~~text
# Observe before acting.
window.list()
mouse.position()

mouse.move(x=400, y=240, duration=350, curve="smooth", jitter=0.8, seed=42)
mouse.click(button="left")
keyboard.type(text="Hello from zuse")
wait(min=80, max=160, seed=42)
screen.capture(path="desktop.png")
~~~

`zuse check FILE` validates without executing. `--read-only` permits observation operations, including screenshot artifacts, and rejects input, evaluation, process launches, and settings changes. `zuse schema` prints every supported operation and its arguments.

Motion supports linear, smoothstep, cubic ease-in/out, and curved Bezier paths. Seeded jitter tapers to zero at the endpoints. Samples use absolute time offsets so scheduling delays do not accumulate across the path.

## Background browser

~~~text
browser.launch(headless=true)
browser.viewport(width=1280, height=800)
browser.navigate(url="data:text/html,<title>zuse</title><input id=message><button id=save>Save</button>")
browser.fill(selector="#message", text="Hello")
browser.click(selector="#save")
browser.snapshot()
browser.capture(path="browser.png")
browser.close()
~~~

The browser adapter speaks Chrome DevTools Protocol over native sockets. Launch creates an isolated temporary profile. Browser snapshots expose accessibility roles, names, relationships, and DOM identifiers, while omitting input values. `browser.capture` writes a PNG of the current viewport; `browser.viewport` sets its size in CSS pixels.

`browser.fill` rejects password inputs. Use a visible browser and `human.request` for a local operator to complete those steps. Explicit `browser.evaluate` executes JavaScript with access to the page.

`browser.connect(endpoint="http://127.0.0.1:9222")` connects to an explicitly selected loopback debugging endpoint and creates a fresh page. `browser.tabs()` lists targets; `browser.select(id="...")` attaches to an existing page. When connected to a browser started outside zuse, closing the connection preserves those selected existing pages.

## MCP

~~~sh
zuse mcp --stdio
zuse mcp --host 127.0.0.1 --port 8377
zuse mcp --socket /run/user/1000/zuse.sock
~~~

Use your own runtime directory for the socket path. Unix socket files are created with mode 0600. TCP serves Streamable HTTP at `/mcp`. A non-loopback bind requires a `ZUSE_TOKEN` of at least 32 bytes; clients send it as a Bearer token. HTTP validates Host, Origin, request metadata, framing, and body bounds.

Stdio and socket connections negotiate legacy MCP and support the current 2026-07-28 protocol. HTTP uses the 2026-07-28 stateless transport. Diagnostics go to stderr; stdout remains a protocol stream.

Typical MCP client configuration:

~~~json
{
  "mcpServers": {
    "zuse": {
      "command": "zuse",
      "args": ["mcp", "--stdio"]
    }
  }
}
~~~

| Tool | Purpose |
| --- | --- |
| zuse.execute | Validate and run a program |
| zuse.check | Return a plan without effects |
| zuse.capabilities | Inspect available host capabilities |
| zuse.schema | Get the command schema |
| zuse.observe | Read windows, pointer, accessibility, or text |
| zuse.screenshot | Return a real PNG image to the agent |
| zuse.human.pending | Inspect human handoffs |
| zuse.resume | Continue an approved request once |

## Human assistance

~~~text
human.request(
  kind="yubikey",
  message="Touch the security key, then confirm that the action is complete.",
  timeout=300
)
window.list()
~~~

The request produces a system notification and a durable request ID. A local operator uses `zuse human assist ID`, or records completion with `zuse human approve ID`. Denial, expiry, and consumed requests stop resumption. A modern MCP client with form elicitation can display a completion dialog and retry using the supplied request state.

Supported kinds are approval, password, yubikey, login, touch, and permission. Password requests require an explicit window target. The local helper obtains the password, sends it to that window, and omits it from MCP results and execution logs. Login requests can open their HTTP(S) URL for the operator.

Keep stateful workflows in `zuse shell` or an MCP server. Those processes retain browser sessions and harnesses while a request is pending. A one-shot CLI invocation saves the remaining source, while its owned browser/process objects end with the invocation.

## Android: foreground and background

~~~text
android.devices()
android.connect(mode="foreground")
android.status()
android.snapshot()
~~~

Background control selects an existing secondary display explicitly:

~~~text
android.connect(mode="background", display=4)
android.tap(x=240, y=160)
android.start(component="com.example.app/.MainActivity")
~~~

The Android adapter implements the ADB smart-socket and shell-v2 wire protocols directly in Rust. It does not spawn adb for commands. Start an authorized ADB server first; select a serial when several devices are connected.

Foreground/preempt mode uses display 0. Background mode requires an existing nonzero logical display. Input and app launch stay bound to that display. Capture resolves the separate SurfaceFlinger display ID and fails if the mapping is absent or ambiguous. Automatic virtual-display creation is outside this release. Device permissions and available platform commands determine accessibility and capture support.

## Native integrations

| Area | Interface |
| --- | --- |
| Linux X11 / Wine / XWayland | Xlib, XTest, targeted window events, window metadata and PNG capture |
| Linux Wayland / Omarchy | Hyprland IPC, compositor metadata, supported desktop utilities |
| Linux accessibility | Native AT-SPI tree, text, focus, actions, editable text |
| Windows | Win32 input/window/GDI APIs; UI Automation through the system PowerShell runtime |
| macOS | Quartz/ApplicationServices; System Events accessibility through the system JXA runtime |
| Chromium | Native CDP client, visible or headless isolated browser |
| Android | Native ADB protocol, explicit foreground or existing secondary display |
| IDE | VS Code open/goto/diff/extensions command interfaces |
| Harnesses | Installed harness discovery and bounded managed process input/output |

Backend dependencies and tested coverage are listed in [Platforms](docs/platforms.md). Wine windows use their host window protocol; this release does not embed Wine. Interactive harnesses need a supported pipe/protocol mode.

## Embed

~~~toml
[dependencies]
zuse = { package = "zuse-computer", version = "0.1.0", default-features = false }
~~~

~~~rust
use zuse::language::Parser;

for call in Parser::new("mouse.move(x=10,y=20)") {
    let call = call?;
    assert_eq!(call.name, "mouse.move");
}
# Ok::<(), zuse::language::ParseError>(())
~~~

Enable the default `std` feature for `zuse::runtime::Runtime`, native backends, and MCP transports.

## Development and releases

~~~sh
python3 scripts/check.py
cargo check --no-default-features --lib
cargo bench --bench core
python3 scripts/bump.py patch --dry-run
python3 scripts/bump.py patch --commit --tag --push
~~~

Checks run locally; the CI workflow is available for pull requests and manual dispatch. The release workflow builds Linux x86_64/ARM64, macOS x86_64/ARM64, and Windows x86_64 archives. It also publishes the Cargo package when `CARGO_REGISTRY_TOKEN` is configured. The Bump and release workflow performs the version commit, atomic branch/tag push, and reusable release call.

See [Development](docs/development.md) and [Validation](docs/validation.md) for exact gates and measured results.

## License

MIT.
