# Android Chrome through native ADB

zuse connects directly to an authorized ADB server, opens the selected device's
observed Chrome abstract socket, and speaks HTTP/WebSocket/CDP through that
stream. Product operations do not spawn adb, create a forwarded TCP listener,
or require a browser automation framework.

## Prepare and connect

Enable and authorize Android USB debugging, start the local ADB server, and
open Chrome on the selected phone. A running process or a socket listed in
/proc/net/unix is not sufficient: Chrome must respond to its debugging endpoint.

Use one script, a persistent shell, or MCP:

~~~sh
zuse shell
~~~

~~~text
android.browser.sockets(serial="YOUR_DEVICE_SERIAL")
browser.connect_android(serial="YOUR_DEVICE_SERIAL")
browser.navigate(url="https://example.com")
browser.snapshot()
browser.text()
browser.capture(path="phone-browser.png")
browser.close()
~~~

The serial is required even with one connected device. If multiple supported
Chrome sockets are observed, pass the exact socket returned by discovery:

~~~text
browser.connect_android(serial="YOUR_DEVICE_SERIAL", socket="chrome_devtools_remote")
~~~

Socket names are validated against the observed listening Chrome sockets.
This adapter does not attach to arbitrary WebViews. It does not start Chrome,
change USB debugging authorization, or silently move an application to the
foreground. A readiness failure can be handled with the existing
`human.request` operation and retried after the operator opens Chrome.

## Ownership and background behavior

Each connection creates one about:blank target with CDP background=true.
The existing Chrome profile is reused; cookies and account state are not
isolated. Desktop `browser.launch` has a separate temporary profile.

For Android sessions, `browser.tabs` returns only the owned target.
`browser.select` rejects other target IDs before sending a selection request.
Normal close, Drop, and failed attachment clean up only a target whose creation
returned a known ID. Existing targets are not enumerated or deliberately
selected. A close response with success=false is an error, including on repeated
close; it is not rewritten into a successful cleanup result.

Creating a target with background=true is a request to Chrome, not a universal
foreground guarantee. The Android handler's implementation and the device's
application lifecycle determine behavior. Observe the actual foreground state
when that matters. Do not assume a disconnected or malformed response proves
that an effect did not occur.

The physical acceptance used Android 16/API 36 and Chrome 157.0.8103.50. Its
optional setup briefly brought Chrome to the foreground and waited for launch,
then restored the original activity **before** navigation and input. All page
operations subsequently passed while that original activity remained in front.
This establishes that tested configuration; it does not establish behavior for
every Android vendor, Chrome version, screen lock or power-management state.

## Operations and coordinates

The connected target uses the existing browser command family:

| Purpose | Operations |
| --- | --- |
| Observation | browser.tabs, browser.snapshot, browser.text |
| Page interaction | browser.navigate, browser.fill, browser.click, browser.key |
| Explicit page code | browser.evaluate |
| Viewport and image | browser.viewport, browser.capture |
| Cleanup | browser.close |

Selectors refer to the owned document. Input is sent with CDP input commands;
the physical fixture verified Unicode text and a trusted click event rather
than treating a successful command return as delivered input. CDP coordinates
use the page viewport. They are distinct from Android display pixels used by
android.tap. The tested 640×480 viewport produced a matching PNG.

Snapshots omit input values. Password fields are rejected by browser.fill;
use human assistance for credentials. Explicit browser.evaluate has access to
page JavaScript and can read page data. A screenshot contains what the page
actually renders.

MCP can return the PNG directly after connection:

~~~json
{"name":"zuse.screenshot","arguments":{"source":"browser"}}
~~~

Use source=android for a separately connected Android display screenshot.
That source is different from the browser viewport.

## A real-device framing regression

The initial implementation waited for TCP EOF after GET /json/version.
The phone returned HTTP 200, Content-Length:447 and the complete version JSON,
but kept the ADB stream open despite Connection: close. Waiting for EOF therefore
reported a timeout after a successful response.

Discovery now finishes at the declared Content-Length. It rejects duplicate,
conflicting or malformed lengths, oversized responses, truncated bodies and
unsupported Transfer-Encoding. Responses without a length retain the bounded
EOF path. Header and body share one absolute deadline.

The regression suite includes an ADB fixture that deliberately keeps the HTTP
connection open until the later CDP session ends. The physical test then passed
all 13 checks, including Unicode, Enter, a trusted selector click, AX/text,
exact PNG dimensions/pixels, selection rejection, foreground preservation and
owned-target cleanup.

See [sanitized evidence](evidence/android-chrome-2026-11-20.json) and the
[fixture image](assets/android-chrome-2026-11-20.png).

## Reproduce

~~~sh
cargo build --locked
python3 tests/native_android_browser.py \
  --binary target/debug/zuse \
  --serial YOUR_DEVICE_SERIAL \
  --output target/native-android-browser
~~~

When Chrome needs explicit readiness activation, add --wake-chrome. This
opt-in fixture setup temporarily opens Chrome and restores the previous
activity; it is not an automatic product fallback. The fixture uses an offline
data URL, never lists pre-existing targets, and writes a report without device
serials or other applications' identities. It requires Pillow for PNG decoding.

## Protocol references

- [Chrome's Android debugging guide](https://developer.chrome.com/docs/devtools/remote-debugging)
- [ADB service protocol](https://android.googlesource.com/platform/packages/modules/adb/+/refs/heads/main/docs/dev/services.md)
- [Android Chrome target handler](https://chromium.googlesource.com/chromium/src/+/main/chrome/browser/devtools/protocol/target_handler_android.cc)
