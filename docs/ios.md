# iOS through WebDriverAgent

zuse controls the native iOS interface through an explicitly selected local [WebDriverAgent (WDA) endpoint][wda-readme]. WDA runs the device-side XCTest automation; zuse sends bounded HTTP requests from its Rust client. A running Appium server is not required by the zuse adapter.

The current iOS acceptance consists of **20 passing isolated protocol tests**, including the actual MCP Engine screenshot path. **No physical Apple device was available for this acceptance.** Device provisioning, USB forwarding and on-device application behavior still need physical iOS validation. See [Verification](#verification-and-troubleshooting) for the exact scope.

## Prepare the device-side service

The operator must prepare a working WDA service before running the zuse examples. Keep the following information together: the selected device's UDID, the WDA runner build, its device-side HTTP port, and the local forwarding port.

1. Use a zuse build whose `zuse schema` output includes `ios.connect`. The adapter belongs to the default `std` build.
2. Prepare an Apple development environment that can build, sign, install and run `WebDriverAgentRunner` on the selected device. For a physical device, the runner needs a valid signing identity and provisioning profile. Follow the [WDA project instructions][wda-readme] and [upstream provisioning guide][provisioning] for that build.
3. Pair the device with the development host and complete its trust prompts. Apple documents [device pairing][apple-pairing] and [running a signed app on a device][apple-signing].
4. Enable Developer Mode where required and enable the device's UI Automation setting. See [Apple's Developer Mode instructions][developer-mode] and [upstream device preparation][device-preparation]. Keep the device available for the intended UI task.
5. Have the operator run the WDA test runner using the prepared development workflow. Record the port on which this particular runner serves HTTP; the examples below use device port `8100`.
6. Arrange a local endpoint that forwards to that exact device and runner. Check its `/status` response before creating an automation session.

Signing credentials, provisioning, trust dialogs, WDA installation and WDA startup remain part of the operator's device setup. The zuse connection command checks an existing endpoint. Upstream build preparation can have additional toolchain requirements; the Rust client does not perform those preparation steps.

### Forward an already running WDA over USB

When the selected device and WDA build are reachable through usbmux, an operator-managed `iproxy` process can expose the service locally. Set `IOS_UDID` to the selected device's actual UDID, then use the current [libusbmuxd `iproxy` syntax][iproxy]:

~~~sh
iproxy --udid "${IOS_UDID:?Set IOS_UDID to the selected device UDID}" \
  --local --source 127.0.0.1 8100:8100
~~~

The left port is the local listening port; the right port is WDA's device-side port. Leave the forwarding process running while zuse uses it. This command assumes WDA is already running; it does not build, sign or start the runner. Use a forwarding and runner-launch workflow compatible with the actual iOS version and host environment.

For multiple devices, assign each selected UDID its own local port and use that port in `ios.connect`. zuse does not discover an iPhone or select the first attached device.

## Select and check the local endpoint

The WDA endpoint must be an explicit `http://` URL with a nonzero port and a loopback host. Supported examples include `http://127.0.0.1:8100`, `http://localhost:8100` and `http://[::1]:8100`. `localhost` is mapped to IPv4 loopback; use `[::1]` explicitly for an IPv6 listener. A plain base-path prefix is supported when the local forwarding service requires one.

Remote hostnames, non-loopback addresses, embedded credentials, query strings and fragments are rejected for the WDA endpoint. Choose the device in the external forwarding setup and give zuse the resulting loopback URL.

Check the prepared service:

~~~sh
curl --fail --silent --show-error http://127.0.0.1:8100/status
~~~

A usable WDA status has `value.ready` set to `true`. The response may also identify an already active session. Treat that session as belonging to its existing owner until an explicit arrangement is made.

Run an observation-only batch:

~~~sh
zuse --read-only eval 'ios.connect(endpoint="http://127.0.0.1:8100"); ios.status(); ios.screen(); ios.snapshot()'
~~~

`ios.connect` checks the endpoint and stores it in the current Runtime. It does not create a session or silently adopt the session reported by WDA. Status, screen geometry, native source and screenshots can be requested without creating a session.

## Keep related operations in one Runtime

Use `zuse shell` for a persistent interactive workflow:

~~~sh
zuse shell
~~~

Enter each complete call on its own line:

~~~text
ios.connect(endpoint="http://127.0.0.1:8100")
ios.session_start()
ios.status()
ios.open_url(url="https://example.com")
ios.snapshot()
ios.capture(path="ios-screen.png")
ios.session_end()
:quit
~~~

This example creates an owned session only when the endpoint is available under the ownership rules below. It opens the URL through the device's default URL handler, observes the resulting native UI, captures a PNG and explicitly ends the owned session.

The shell retains the connection and ownership between lines. Each separate `zuse eval` or `zuse run` process starts a fresh Runtime. To run a saved batch, save the `ios.*` calls as `ios-task.zuse` and use:

~~~sh
zuse check ios-task.zuse
zuse run ios-task.zuse
~~~

`check` validates the complete source without contacting WDA. It checks argument types and bounds, endpoint syntax, source/raw options, supported keys and HTTP/HTTPS URL authority syntax. Reachability, the current active session and device behavior are checked during execution.

### Native input and Safari

After creating or explicitly attaching to a session, the input operations are:

| Operation | Call shape |
| --- | --- |
| Tap | `ios.tap(x=120, y=240)` |
| Swipe | `ios.swipe(x1=120, y1=500, x2=120, y2=250, duration=350)` |
| Type text | `ios.type(text="Hello 世界")` |
| Key or device button | `ios.key(key="Enter")` |
| Launch an app | `ios.launch(bundle="com.apple.mobilesafari")` |
| Open a web URL | `ios.open_url(url="https://example.com")` |

Choose coordinates from a current observation of the intended control. The values in the table illustrate syntax. Text is passed as Unicode to WDA's native typing route; the application's focus and keyboard state determine where it is received. Supported key names are `Home`, `VolumeUp`, `VolumeDown`, `Enter`/`Return` and `Backspace`. Swipe duration is movement time in milliseconds, defaults to `300`, and accepts `1..10000`.

Safari can be launched and inspected as a native application. `ios.open_url` currently sends only the URL to WDA, whose [URL route][wda-session] invokes the [device's default URL handler][wda-device] when no application bundle is supplied. It therefore follows the phone's configured default browser. For a Safari-specific workflow, launch Safari and operate its native address field, or prepare Safari as the device's default browser.

This adapter provides **native Safari UI control**. It exposes no Safari DOM selectors, JavaScript evaluation, Web Inspector connection or browser-context API. The `browser.*` CDP commands apply to their separately connected browser backend.

## Session ownership and replacement

WDA maintains a global active session for an endpoint. Its [session-creation handler][wda-session] clears the previous session before validating and creating the new one. zuse checks the current status before sending that request and tracks attached and owned identities separately.

| Request or state | Behavior |
| --- | --- |
| `ios.connect(endpoint="…")` | Connects for observation without adopting the current WDA session. |
| `ios.connect(endpoint="…", session="…")` | Verifies and attaches to the exact supplied session ID. Input is permitted through that ID; ownership stays external. |
| `ios.session_start()` | Creates a session when no active session exists, or replaces a session already owned by this same instance. |
| Existing external session or unknown active-session identity | `session_start` refuses unless the caller explicitly supplies `replace=true`. |
| `ios.session_start(replace=true)` | Explicitly permits replacement of the endpoint's current global session. |
| `ios.session_end()` | Deletes only a session created by this instance, after checking that its exact ID is still active. |
| Reconnect or process exit | Drops local connection/ownership state. It does not delete the remote session. |

A successful session-start result contains the WDA identity at `session.sessionId` inside the operation's `value`. `ios.status()` reports `attachedSessionId`, `owned`, `activeSessionKnown` and `activeSessionId`. The active identity can change when another client uses the same WDA endpoint.

To attach to an existing session, obtain its exact ID from its owner or a status response and provide it explicitly. Attaching does not grant deletion rights: `ios.session_end()` rejects cleanup of an externally owned session. End an owned session before reconnecting or exiting when remote cleanup is intended.

`replace=true` authorizes the global replacement behavior. A failed creation may already have ended the previous WDA session; zuse clears the old attached input target before sending the creation request. A failed actual reconnect also leaves no old device target for subsequent input.

WDA provides no atomic compare-and-create session operation. Another client can change the active session between the status check and creation. Assign one coordinator to each WDA endpoint or otherwise serialize session changes with its owner. The local ownership checks do not supply a cross-client lock.

## Observe coordinates and sensitive fields

### UIKit points and PNG pixels

Input coordinates use **UIKit points**. `ios.screen()` returns WDA's screen geometry with `coordinateSpace="points"`. `ios.snapshot()` includes that geometry alongside the native accessibility tree.

Screenshot dimensions use **PNG pixels**. `ios.capture` reports `width`, `height`, `coordinateSpace="pixels"` and `inputCoordinateSpace="points"`. WDA's [screen route][wda-screen] and [screenshot route][wda-screenshot] are separate observations.

For example, if a same-orientation full-screen image is `1170 × 2532` pixels and the corresponding screen is `390 × 844` points, an image position of `(300, 600)` maps to `(100, 200)` points. This is an illustrative conversion, not a measured device result. Match the current image, geometry and orientation before converting coordinates; do not pass an image's pixel coordinates directly to `ios.tap`.

Capture a file in an observation-only batch:

~~~sh
zuse --read-only eval 'ios.connect(endpoint="http://127.0.0.1:8100"); ios.capture(path="ios-screen.png")'
~~~

Successful capture replaces the requested output file. A failed capture preserves an existing destination. On Unix, the adapter creates its temporary image with mode `0600`, then replaces the destination after receiving and checking the PNG structure.

### Default secure-field redaction

Use these defaults for structured observation:

~~~text
ios.snapshot()
ios.source()
ios.source(format="json")
~~~

WDA's [JSON tree generator][wda-tree] directly includes native values, labels and identifiers. zuse therefore filters structured results before returning them. For a recognized secure/password node and all its descendants, the result keeps only the element type, numeric rectangle, supported boolean state, child structure and `redacted=true`. Values, labels, names, identifiers, placeholders and unknown content attributes are omitted.

The filter relies on the application's native secure role or secure flags. A custom widget that presents a secret as ordinary text needs its application to expose the correct accessibility semantics. Screenshot pixels retain the visible screen content.

XML and description source require an explicit raw option because their sensitive content is not filtered:

~~~text
ios.source(format="xml", raw=true)
ios.source(format="description", raw=true)
~~~

`ios.source(format="json", raw=true)` also returns the unfiltered WDA tree. Raw output can include sensitive application text and becomes part of the normal CLI/MCP result. The default remains `format="json", raw=false`.

## Use the persistent MCP server

Start a zuse MCP process and keep related calls on that running server:

~~~sh
zuse mcp --stdio
~~~

The server retains the selected iOS connection and owned session across tool calls. Through the MCP client's normal tool-call interface, call `zuse.execute` with these arguments:

~~~json
{
  "code": "ios.connect(endpoint=\"http://127.0.0.1:8100\"); ios.session_start(); ios.open_url(url=\"https://example.com\"); ios.snapshot()"
}
~~~

Then call `zuse.screenshot`:

~~~json
{
  "source": "ios"
}
~~~

The screenshot tool returns both structured capture metadata and a real `image/png` MCP image block. With no `path`, it uses and removes its temporary file. Supply `path` when a retained output file is wanted. The screenshot source enum also includes `desktop`, `browser` and `android`.

End the owned session through the same running server:

~~~json
{
  "code": "ios.session_end()"
}
~~~

These are tool argument objects; the MCP client supplies the protocol envelopes. A separately started zuse server has fresh local ownership state.

For an observation-only server, use:

~~~sh
zuse mcp --stdio --read-only
~~~

This permits connection, status, source and screenshots, including the requested screenshot output file. It rejects device input, app/URL opening and session creation/deletion. A tool request cannot disable the server's read-only setting.

HTTP and Unix-socket transports retain the same runtime behavior:

~~~sh
zuse mcp --host 127.0.0.1 --port 8377
zuse mcp --socket /tmp/zuse-ios-mcp.sock
~~~

Run the transport appropriate for the client. Port `8377` in this example serves zuse's MCP protocol; port `8100` is the separately prepared WDA endpoint.

## Verification and troubleshooting

The [iOS protocol tests](../tests/ios/protocol.rs) exercise the native client against isolated loopback WDA fixtures. All **20 tests passed** in the integrated local test run. They cover session ownership, explicit replacement, invalidation after failed creation, Unicode/request payloads, HTTP framing and bounds, slow partial responses, secure-field filtering, URL preflight, PNG output preservation, and MCP image delivery/cleanup through the public Engine.

| Claim | Evidence and current scope |
| --- | --- |
| Native WDA request routes and payloads | Fixed upstream source review plus isolated protocol tests. |
| Owned-session cleanup and replacement rules | Mocked external sessions, replacement, failed creation and changed active identity. |
| Whole-program URL rejection before connection | Runtime regression checks that the earlier connect sends no request. |
| MCP iOS image content and temporary cleanup | Real Engine path with a WDA fixture, exact PNG base64 and file-removal assertions. |
| Physical iPhone/iPad operation, signing and USB forwarding | Physical Apple-device acceptance is pending. |

Each public iOS operation uses one absolute **15-second network deadline**, including all requests and partial reads within that operation. This is a per-operation limit; a script with several operations has several such budgets. Inputs and response sizes are also bounded. The client never automatically replays a failed action.

| Result | Next step |
| --- | --- |
| Endpoint connection fails or WDA is not ready | Check the selected device, running runner, forwarding process, local port and `/status` response. |
| Another active session exists | Coordinate with its owner, explicitly attach to its ID, or intentionally authorize replacement. |
| Input requires an attached or owned session | Create a session or attach to the exact agreed session ID in the same Runtime. |
| XML/description requires `raw=true` | Use default JSON for filtered observation, or explicitly choose raw output. |
| Action outcome is unknown or could not be confirmed | Observe the device state before deciding whether another action is appropriate. |
| Native input succeeds but the desired UI state is absent | Re-observe focus, geometry and application state before issuing the next step. |

## Fixed protocol references

The implemented WDA contract was reviewed against `appium/appium-ios` commit **`9a9092aa35e913a893f6dafddc2bfc13893d1daf`**, under `packages/webdriveragent`. The relevant upstream entry points are:

- [Project and XCTest runner overview][wda-readme].
- [Session lifecycle, app launch and URL opening][wda-session].
- [Session-ID route resolution][wda-route].
- [JSON/XML/description source routes][wda-source] and [native JSON tree generation][wda-tree].
- [Screen geometry and device buttons][wda-screen].
- [PNG screenshot route][wda-screenshot].
- [Tap/text routes][wda-element], [W3C touch actions][wda-touch] and [native text synthesis][wda-typing].
- [Device URL dispatch and default application selection][wda-device].

The zuse implementation is [src/ios.rs](../src/ios.rs); MCP image integration is in [src/mcp.rs](../src/mcp.rs). Use `zuse schema` for the command arguments supported by the actual installed build.

[wda-readme]: https://github.com/appium/appium-ios/blob/9a9092aa35e913a893f6dafddc2bfc13893d1daf/packages/webdriveragent/README.md
[wda-session]: https://github.com/appium/appium-ios/blob/9a9092aa35e913a893f6dafddc2bfc13893d1daf/packages/webdriveragent/WebDriverAgentLib/Commands/FBSessionCommands.m
[wda-route]: https://github.com/appium/appium-ios/blob/9a9092aa35e913a893f6dafddc2bfc13893d1daf/packages/webdriveragent/WebDriverAgentLib/Routing/FBRoute.m
[wda-source]: https://github.com/appium/appium-ios/blob/9a9092aa35e913a893f6dafddc2bfc13893d1daf/packages/webdriveragent/WebDriverAgentLib/Commands/FBDebugCommands.m
[wda-tree]: https://github.com/appium/appium-ios/blob/9a9092aa35e913a893f6dafddc2bfc13893d1daf/packages/webdriveragent/WebDriverAgentLib/Categories/XCUIApplication%2BFBHelpers.m
[wda-screen]: https://github.com/appium/appium-ios/blob/9a9092aa35e913a893f6dafddc2bfc13893d1daf/packages/webdriveragent/WebDriverAgentLib/Commands/FBCustomCommands.m
[wda-screenshot]: https://github.com/appium/appium-ios/blob/9a9092aa35e913a893f6dafddc2bfc13893d1daf/packages/webdriveragent/WebDriverAgentLib/Commands/FBScreenshotCommands.m
[wda-element]: https://github.com/appium/appium-ios/blob/9a9092aa35e913a893f6dafddc2bfc13893d1daf/packages/webdriveragent/WebDriverAgentLib/Commands/FBElementCommands.m
[wda-touch]: https://github.com/appium/appium-ios/blob/9a9092aa35e913a893f6dafddc2bfc13893d1daf/packages/webdriveragent/WebDriverAgentLib/Commands/FBTouchActionCommands.m
[wda-typing]: https://github.com/appium/appium-ios/blob/9a9092aa35e913a893f6dafddc2bfc13893d1daf/packages/webdriveragent/WebDriverAgentLib/Categories/XCUIElement%2BFBTyping.m
[wda-device]: https://github.com/appium/appium-ios/blob/9a9092aa35e913a893f6dafddc2bfc13893d1daf/packages/webdriveragent/WebDriverAgentLib/Categories/XCUIDevice%2BFBHelpers.m
[iproxy]: https://github.com/libimobiledevice/libusbmuxd/blob/cf4b806dff1003e56b8219367bbb29cb01c3f619/tools/iproxy.c
[developer-mode]: https://developer.apple.com/documentation/xcode/enabling-developer-mode-on-a-device
[apple-pairing]: https://developer.apple.com/documentation/xcode/managing-your-simulated-and-physical-devices-in-device-hub
[apple-signing]: https://help.apple.com/xcode/mac/current/en.lproj/dev5a825a1ca.html
[device-preparation]: https://appium.github.io/appium-xcuitest-driver/12.15/getting-started/device-setup/
[provisioning]: https://github.com/appium/appium-xcuitest-driver/blob/master/docs/getting-started/provisioning-profile/index.md
