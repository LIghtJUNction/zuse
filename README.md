# zuse

**An agent-native computer-use shell, Rust library, and MCP server.**

zuse turns small programs into desktop observations and actions. The same executor drives native windows, Chromium, accessibility trees, mobile devices, serial/USB devices and managed harness processes. Programs calculate coordinates and expand bounded control flow before input; persistent sessions keep runtime state available to external controllers.

[中文](README.zh-CN.md) · [Language](docs/language.md) · [Numeric programs](docs/scalar-programs.md) · [Sessions](docs/sessions.md) · [Protocol v1](docs/session-protocol.md) · [Platforms](docs/platforms.md) · [Releases](https://github.com/LIghtJUNction/zuse/releases)

## Small core, explicit effects

The flat-call parser and pointer-motion kernel compile with `no_std`, use no allocator, and have no dependencies. The optional host runtime, including the scoped-script compiler and scheduler, uses Rust's standard library and six Project Z crates across its dependency tree: zmcpio, zcall, zconv, zclap, z64, and zsys. WebSocket handshakes use the `sha1` crate with its default features disabled. Native dynamic-library loading reuses zsys 0.1.1. There is no embedded JavaScript engine or browser automation framework.

The runtime checks the entire program before its first effect. Unknown operations, wrong argument types, duplicate arguments, malformed strings, excessive steps, and excess fixed-duration wait budgets fail before execution. Speed-based target movement is charged to the remaining wait budget after its path is resolved, before sending its first pointer event. Runtime errors return completed steps and stop the remaining program. Execution records omit keyboard input contents.

## Install

Rust 1.100 or newer:

~~~sh
cargo install --git https://github.com/LIghtJUNction/zuse --locked
zuse --version
zuse doctor
~~~

The Cargo package is **zuse-computer**; the executable and Rust library are named **zuse**. The short package name was already registered by another project.

Native archives for Linux, Windows, and macOS are published on the [Releases page](https://github.com/LIghtJUNction/zuse/releases). Extract the matching archive and place its zuse executable on your PATH. Remove that executable to uninstall; Cargo installations use `cargo uninstall zuse-computer`.

### Update

~~~sh
zuse update --check       # Query the latest stable version; do not install
zuse update --dry-run     # Show the exact update command; do not install
zuse update              # Update a tracked user Cargo installation
~~~

The updater uses `curl` and Cargo, verifies installation ownership, and preserves its root, features, profile and target. It selects an exact stable registry version or official Git release tag and keeps newer development versions. System/AUR installs use their package manager; portable, custom and pinned installs receive manual instructions. JSON goes to stdout; progress and failures go to stderr. See [update behavior and verification](docs/update.md).

## Project Z dependencies

Project Z dependencies use their GitHub repositories directly. Each currently uses `main` as its default branch. Refresh the pinned Git commits with `cargo update -p zmcpio -p zclap -p z64 -p zsys -p zcall -p zconv`, review the lockfile diff, and run the relevant local checks before publishing a refresh. Reproducible builds use `--locked`. GitHub publishes the native binaries; registry packaging is separate and must not silently substitute a different dependency graph.

## First program

~~~sh
zuse eval 'window.list()'
zuse eval 'mouse.move(x=400,y=240,duration=350,curve="smooth",jitter=0.6,seed=42)' --dry-run
zuse script examples/scoped-mice.zuse --dry-run
zuse run examples/desktop.zuse
zuse shell
~~~

Flat programs use named calls separated by newlines or semicolons. This example explicitly selects the user's system pointer:

~~~text
# Observe before acting.
window.list()
mouse.use(kind="user", appearance="system")
mouse.position()

mouse.move(x=400, y=240, duration=350, curve="smooth", jitter=0.6, seed=42)
mouse.click(button="left")
keyboard.type(text="Hello from zuse")
wait(min=80, max=160, seed=42)
screen.capture(path="desktop.png")
mouse.release()
~~~

`zuse check FILE` validates without executing. `--read-only` permits observation operations, including screenshot artifacts, and rejects input, page JavaScript, process launches, and settings changes. `zuse schema` prints every supported operation and its arguments.

Point moves and `mouse.click_at` default to 350 ms, `curve="smooth"` and 0.6 px jitter. Jitter tapers to zero at the endpoints; timed samples avoid accumulated scheduling drift. `duration=0` remains an explicit instant-move option. Improve throughput by batching operations while keeping ordinary pointer movement smooth.

### Bounded programs and drawing

Use `zuse -c 'CODE'` for inline source or `zuse script -` for stdin. Immutable `let` values, arithmetic, comparisons, `if`/`else`, integer `for` ranges, pure math helpers and recursive functions expand before execution. A loop permits 4,096 iterations and calls nest at most 32 levels; preparation visits at most 16,384 statements, expressions and loop iterations. Invalid arithmetic fails before input.

~~~sh
zuse script - --dry-run <<'ZUSE'
let size = 80
for i in 0..3 {
    let x = 40 + i*100
    draw.triangle(x1=x,y1=100,x2=x+size,y2=100,x3=x+size/2,y3=20)
}
ZUSE
~~~

`draw.triangle` draws one closed stroke through three numeric vertices. Choose the application's brush and an observed window anchor before drawing. `mouse.click_at`, `mouse.stroke` and `draw.*` check the anchor's window identity, geometry, scale and focus. Tabs share a native window ID: stop input and observe again when the user changes pages. See [drawing](docs/drawing.md), [numeric programs](docs/scalar-programs.md) and the [recursive Sierpinski example](examples/fractal-sierpinski.zuse).

### Persistent sessions and external decisions

On Unix, a named session keeps functions, devices and anchors across client invocations:

~~~sh
zuse session start board --idle-timeout-ms 900000
zuse session observe board - <<'ZUSE'
window.list()
ZUSE
zuse session result board
~~~

Wait for the observation result, decide the next action externally, submit a short program through `zuse session script board -`, then query its `job_id` and observe again. `accepted` acknowledges submission; the result records completion or failure. One job runs at a time. `status`, `result`, `cancel` and `close` remain available; cancellation is cooperative and cleanup is confirmed separately.

The public [JSON-lines protocol v1](docs/session-protocol.md) uses the private Unix socket returned by `start`. Stable request IDs support bounded retries; `--expect-revision` checks Zuse session state. Confirm application content through fresh observations. Sessions execute flat Runtime programs, including expanded numeric code; scoped mouse programs support dry-run preflight only. Idle sessions close after 15 minutes by default, with running work excluded. See [sessions](docs/sessions.md). `zuse shell` remains available for one retained stdin stream. On builds with native signal handling, SIGINT or SIGTERM closes the shell through normal Runtime cleanup, reaps its managed child processes, and discards queued commands. This also covers idle input and incomplete lines; see [shell cancellation](docs/local-programs.md#timing-and-cancellation).

The bundled [Python controller example](examples/session_client.py) submits stdin source once and waits for its result on the same connection. It uses only the Python standard library and accepts the observed socket path, execution budgets and an optional expected revision.

### Packages and the standard library

Git packages declare entry modules, exports and dependencies; `zuse.lock.json` pins full commits. Imports load installed local declarations; explicit function calls execute them.

In a new project directory:

~~~sh
zuse pkg init my-drawing
zuse pkg add https://github.com/LIghtJUNction/zuse-std.git --rev v0.1.0
zuse -c 'import(path="pkg:std",as="std"); functions(namespace="std")'
~~~

Commit the manifest and lockfile; restore with `zuse pkg install --locked`. The [standard library](https://github.com/LIghtJUNction/zuse-std) provides desktop, drawing, text-input and timing functions. See [package management](docs/packages.md) for dependency checks and publication.

### Agent skills

The repository includes three small skills that can be distributed with Zuse:

- [Operate the computer](skills/zuse/SKILL.md): direct CLI programs, observed targets, batched input and session reuse.
- [Develop and publish packages](skills/zuse-publish-package/SKILL.md): source modules, validation, version tags and GitHub topics.
- [Find and use packages](skills/zuse-find-package/SKILL.md): repository discovery, source review, locked installation and import preflight.

## Reusable functions and local events

Import a `.zuse` module once in `zuse shell`, then reuse its functions across submissions:

~~~text
import(source="fn delay(ms){wait(ms=ms)}", as="local")
local.delay(ms=10)
eval(source="fn clock(){timer.status()}", as="tools")
tools.clock()
~~~

Add consecutive `///` descriptions immediately before declarations, then call
`functions(namespace="local")` to inspect names, ordered parameters, descriptions
and source locations without invoking functions. MCP exposes the same catalog
through `zuse.functions`; Rust exposes typed `function_metadata()` values. Run
`zuse run examples/function-catalog.zuse --read-only` for a complete example.

File imports use `import(path="module.zuse", as="name")`; loading only registers declarations. Explicit `eval` may also execute actions. The whole expanded program shares one preflight, read-only policy and budget. Failed preflight and dry runs do not replace retained definitions. `run`, `script` and `check` resolve file imports relative to the containing source file.

`observe`, `watch` and `wait_until` return structured text and use local DOM or AT-SPI events. Explicit `watch_until_click` binds a tab, origin, container and bounded appearance count. `timer.start/reset/stop/status` expose monotonic command and cumulative timing. Optional `--features vision` adds multi-scale image matching and explicitly rendered visual timelines. See [local programs](docs/local-programs.md), the [bundled Zuse skill](skills/zuse/SKILL.md), and [validation evidence](docs/validation-20261121.md).

`keyboard.state()` distinguishes the active Wayland keyboard from other devices; text input preserves lock states. `keyboard.num_lock(state="on")` is an explicit readback-checked operation, also accepting `off` or `toggle`. A Wayland glow marker is click-through and follows the shared user pointer; independent pointers still require X11 MPX. Unavailable native pinch input remains explicitly unsupported.

`keyboard.hold(key="Up", duration=400)` keeps a direction key down on the observed foreground Linux window, then releases it. `browser.hold(key="ArrowRight", duration=300)` provides the same bounded operation for the selected Chromium page. Both stop on cancellation or target changes. See [direction-key holds and receiver tests](docs/key-holds.md).

The importable [ChatGPT desktop module](docs/chatgpt-desktop.md) separates native observation, guarded draft preparation and an explicitly authorized send action. Draft preparation checks the process and an empty editor before writing. A real target must expose the required native accessibility interfaces.

## Independent, scoped mice

`zuse script` gives each mouse variable its own visible cursor, position, and button state. Independent mice are the default and require native Linux X11 with XInput2/MPX. Their glow, color, shape, size, and image are configurable. The following script moves two mice concurrently and closes both when its scope ends:

~~~text
let cyan = mouse(color="#2FE2FF", shape="arrow");
let violet = mouse(color="#B58CFF", shape="ring");
parallel {
    cyan.move(x=320, y=180, duration=600);
    violet.move(x=520, y=320, duration=600);
}
~~~

Run the full example with `zuse script examples/scoped-mice.zuse`. Blocks and functions own their local mice; aliases and function parameters borrow existing mice. `sequence` runs operations in order, while explicit blocks inside `parallel` group each branch's sequential work. The compiler rejects conflicting parallel access to one mouse before input. Normal exit, runtime errors, and cooperative cancellation close the script's mice and release synthetic held buttons.

Flat `mouse.*` actions also create an independent runtime mouse by default. Select the user pointer explicitly with `mouse.use(kind="user", appearance="system")`; a scoped script uses `user_mouse(appearance="system")`. This explicit system mode uses the existing Wayland, Windows, or macOS backend where supported. An unavailable independent backend returns an error. `mouse.position()` requires an already selected runtime handle; use `mouse.use()` first.

See [scoped mice, custom paths, Rust ownership, and cleanup limits](docs/scoped-mice.md) for the complete API, including the distinction between cooperative signal handling and termination that cannot run destructors.

## Move to text or a button

`mouse.move_to` resolves an accessible name or text, reads the current pointer position, and moves to the target's center plus an optional offset. One call controls the whole movement; its default duration is 350 ms and it only moves the pointer.

~~~sh
zuse eval 'mouse.move_to(text="Save",role="button",duration=650,curve="bezier",jitter=1.2,speed_variation=0.25,seed=42,offset_x=4,offset_y=-3)' --dry-run
zuse eval 'mouse.move_to(text="Continue",role="button",speed=600,via="320,180;480,220")' --dry-run
cargo run --example target_motion
~~~

`--dry-run` validates arguments without reading the desktop. The Rust example generates a complete path using supplied coordinates; it requires an explicit `--execute` argument to move a real pointer. Targets come from native accessibility, so image-only or canvas text must be exposed by the application before this operation can locate it. Matching is exact and case-sensitive by default; ambiguous matches require a scope or explicit index.

Use `MovePlan` to inspect a path or `Runtime::move_to_with` to supply a Rust callback receiving progress, the live start, and the offset-adjusted target. `motion::FunctionalMotion` provides the allocation-free `no_std` sampler. `mouse.circle` also schedules a closed circular trajectory in one call. See [target selection, motion parameters, and Rust examples](docs/target-motion.md).

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

`browser.connect(endpoint="http://127.0.0.1:9222")` connects to an explicitly selected loopback debugging endpoint and creates a fresh page. `browser.tabs()` lists targets; `browser.select(id="...")` attaches to an existing page. When connected to a browser started outside zuse, closing the connection preserves those selected existing pages. Re-selecting the current page preserves its session and content; another page is prepared before the previous selection is released.

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
| zuse.functions | Inspect retained function metadata, optionally filtered by module namespace |
| zuse.script | Run scoped mice, functions, and parallel tracks; close the run's mice before returning |
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
android.connect(serial="YOUR_DEVICE_SERIAL", mode="foreground")
android.status()
android.snapshot()
~~~

Background control selects an existing secondary display explicitly:

~~~text
android.connect(serial="YOUR_DEVICE_SERIAL", mode="background", display=4)
android.tap(x=240, y=160)
android.start(component="com.example.app/.MainActivity")
~~~

The native ADB client requires an authorized ADB server. Select a serial explicitly; omission requires exactly one ready device. Foreground mode uses display 0; background mode selects an existing nonzero display and binds input, launch and capture to it. Ambiguous capture mappings fail. Text accepts up to 4096 bytes of printable ASCII, excluding `%s`; unsupported input fails during preflight. See the platform guide for permissions and display setup.

Physical Android 16 testing covered foreground input, all 95 printable ASCII characters, exact screenshot pixels, and a fixture-owned secondary display whose input left the primary fixture unchanged. See [native acceptance testing](docs/native-testing.md) and the [device evidence](docs/evidence/android-physical-2026-11-20.json).

## Mobile browsers and iOS

~~~text
android.browser.sockets(serial="YOUR_DEVICE_SERIAL")
browser.connect_android(serial="YOUR_DEVICE_SERIAL")
browser.navigate(url="https://example.com")
browser.snapshot()
browser.capture(path="phone-browser.png")
browser.close()
~~~

Android Chrome uses a native ADB tunnel and one owned tab in the existing profile. It requires a responsive debugging socket; mobile tab selection stays within that owned tab. See [mobile browser control](docs/mobile-browser.md).

iOS uses an explicitly configured local WebDriverAgent endpoint for touch, Unicode input, observation, screenshots and application launch. Protocol fixtures pass; physical iPhone validation remains outstanding. The [iOS guide](docs/ios.md) covers preparation, ownership, redaction and coordinates. MCP screenshots support `desktop`, `browser`, `android` and `ios` sources from the connected runtime.

## Serial and USB devices

~~~text
serial.ports()
usb.devices()
~~~

Native serial adapters support explicit Linux/macOS TTY paths and Windows COM
ports, raw 8N1 configuration, bounded reads/writes and restoration on close.
The USB adapter loads the system libusb through zsys and requires explicit
bus/address, VID/PID and interface selection. It provides bulk, interrupt and
control transfers without automatically detaching kernel drivers.

Partial writes report progress and stop the remaining program; they are never
automatically replayed. Linux serial behavior was tested through real kernel
PTYs. USB transfer behavior was tested against an isolated C ABI fixture, not
an arbitrary physical device. See [device I/O](docs/device-io.md) for examples,
limits, timeouts and cleanup semantics.

## Native integrations

| Area | Interface |
| --- | --- |
| Linux X11 / Wine / XWayland | Xlib, XTest, targeted window events, window metadata and PNG capture |
| Linux Wayland / Omarchy | Hyprland windows and keyboard, Sway and niri window listing and exact focus, and grim capture |
| Linux accessibility | Native AT-SPI tree, text, focus, actions, editable text |
| Windows | Win32 input/window/GDI APIs; UI Automation through the system PowerShell runtime |
| macOS | Quartz/ApplicationServices; System Events accessibility through the system JXA runtime |
| Chromium | Native CDP client, visible or headless isolated browser |
| Android | Native ADB protocol, explicit displays and an owned Chrome CDP tab |
| iOS | Native HTTP client to an explicit local WebDriverAgent endpoint |
| Serial / USB | Native termios/Win32 COM; system libusb through zsys |
| IDE | VS Code open/goto/diff/extensions command interfaces |
| Harnesses | Installed harness discovery and bounded managed process input/output |

Backend dependencies and tested coverage are listed in [Platforms](docs/platforms.md). Wine windows use their host window protocol; this release does not embed Wine. Interactive harnesses need a supported pipe/protocol mode.

X11 text outside the keyboard layout requires a confirmed local AT-SPI editable target. The whole string is checked before input; unsupported targets fail without a keyboard-map or clipboard workaround. See [X11 text delivery](docs/x11-utf8-delivery.md) for focus binding, selection/caret semantics and the isolated native regression.

## Embed

~~~toml
[dependencies]
zuse = { package = "zuse-computer", git = "https://github.com/LIghtJUNction/zuse", tag = "v0.6.0", default-features = false }
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

Checks run locally; the CI workflow is available for pull requests and manual dispatch. The release workflow builds Linux x86_64/ARM64, macOS x86_64/ARM64, and Windows x86_64 archives. The native release workflow does not publish the Cargo package. Registry publication uses a separately validated source stage with exact matching registry dependencies. The Bump and release workflow performs the version commit, atomic branch/tag push, and reusable release call.

See [Development](docs/development.md) and [Validation](docs/validation-0.2.2.md) for exact gates and measured results.

## License

MIT.
