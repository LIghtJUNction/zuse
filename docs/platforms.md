# Platform support and verification

Capabilities are discovered at runtime. Missing permissions, system libraries, display protocols, or device commands produce explicit errors. Compilation and actual application interaction are recorded separately.

| Backend | Implemented interface | Verified scope through 0.2.0 |
| --- | --- | --- |
| Linux X11 | Xlib/XTest pointer and keyboard, window metadata, targeted synthetic events, PNG | Private Xvfb fixture; delivered text and events, image dimensions/pixels/CRCs |
| Linux Hyprland | Native compositor IPC, Wayland virtual keyboard, actual XKB keymap lookup | Hyprland 0.57.2 private graphical sessions, including Fcitx; foreground Unicode, background ASCII and state rejection |
| Shared Linux desktop | Window discovery and pointer operations | Actual development session; keyboard stopped before effects because Num Lock was enabled; focus/pointer restored |
| Wayland capture | grim using the compositor's capture protocol | Fixture PNG color and dimensions verified; window capture is its composited visible region |
| Linux AT-SPI | Tree, text, bounds, focus, actions, editable text and password redaction | Private D-Bus/Xvfb GTK fixture |
| Chromium CDP | Isolated browser, navigation, text/AX, input, viewport and PNG | Isolated sandboxed Chromium fixture rerun after the 0.2.0 mobile fix |
| Windows | Win32 input/window/GDI and system UI Automation | Full x86_64-pc-windows-gnu type check; no live Windows desktop in this acceptance |
| macOS | Quartz/ApplicationServices and System Events accessibility | Clippy/all-targets on aarch64 and x86_64 apple-darwin; permission preflight implemented; no live Mac |
| Android | Native ADB/shell-v2, display-aware input/capture/launch | Physical Android 16/API 36, foreground and fixture-owned secondary display |
| Android Chrome | Native ADB tunnel and owned CDP target | Physical Chrome 157: 13 checks, restored foreground during background page operations |
| iOS | Native HTTP to explicit local WDA | 20 protocol/MCP tests; no iPhone available |
| Serial | Linux/macOS termios, Windows COM | 5 real Linux PTY test groups; no physical serial peripheral |
| USB | System libusb through zsys | 11 isolated C ABI groups and real descriptor enumeration; no hardware transfer tests |
| Wine | Host X11/XWayland control and Wine-marked discovery | Shared X11 implementation verified; no separate Wine application acceptance |
| IDE / Omarchy / harness | Installed command interfaces and bounded managed processes | Process tests and capability discovery; tests do not launch model sessions |

Detailed results: [0.2.0 validation](validation-0.2.0.md), [0.1.1 validation](validation-0.1.1.md), [Android evidence](evidence/android-physical-2026-11-20.json), [Linux evidence](evidence/linux-native-2026-11-20.json).

## Linux libraries and helpers

Native dynamic-library loading uses the exact Project Z dependency zsys 0.1.1. X11 needs libX11 and libXtst. Wayland keyboard support needs libwayland-client and libxkbcommon. AT-SPI needs libatspi and a usable accessibility D-Bus session. These libraries are loaded at runtime, with no link-time X11 dependency. Static-musl native dynamic loading is unsupported by this zsys version.

Hyprland metadata, focus and targeted input use its native socket. Screen capture uses grim. Pointer buttons/wheel require an authorized ydotool daemon; pointer positioning can use Hyprland IPC. Text input no longer depends on wtype.

OCR uses Tesseract, notifications use notify-send, and the graphical password prompt uses Zenity. Omarchy operations call installed Omarchy/Hyprland interfaces; zuse does not install a theme or change system settings during capability discovery.

## Wayland keyboard conditions

The current keyboard path requires a Hyprland session that exposes the state queries used by the adapter, no held keys, Caps Lock and Num Lock off, and layout group 0. Known nonzero or unobservable required state returns error_code=unsupported_state before keyboard effects. Sticky modifiers and custom latched/locked state are not supported: the compositor's public interface does not expose a complete modifier snapshot to an unfocused observer.

Foreground typing installs a bounded Unicode keymap and restores the captured original keymap before disconnecting. Background typing leaves the live keymap intact. It resolves the whole string against the real plain/Shift levels before sending targeted shortcuts. A missing character returns error_code=unsupported_character before any character in that call is sent. This does not provide arbitrary background Unicode on every layout; a US map, for example, cannot represent Chinese or emoji through these levels.

The Fcitx regression demonstrates the tested session, not every input method, preedit state, compositor or application. zuse does not toggle the operator's locks or reclaim a changed focus to make an operation pass.

## Submission and application acceptance

A successful native key request means the event was submitted to the OS/compositor. It does not prove that a widget consumed it. In particular, X11 applications can ignore XSendEvent events even when the X server accepts them.

Keyboard results include delivery=submitted and application_confirmation_required=true. The compatibility field typed_bytes counts submitted UTF-8 bytes, as does submitted_bytes; neither measures the widget's final text. Read accessibility state or another application observation to verify the intended result. Applications that reject background events require an explicit foreground or accessibility operation.

## Windows and macOS requirements

Windows uses Win32 for native input/window/capture and the system PowerShell/.NET UI Automation interface for accessibility. OS-protected desktops and application permission boundaries remain in effect.

macOS exposes non-prompting Accessibility and Screen Recording preflight with granted/denied/unknown states. See [macOS setup](macos.md). Actual input/capture requires the corresponding permission. Native input uses Quartz; tree operations use System Events/JXA. The local password dialog uses the OS scripting interface. These desktop paths need live acceptance on their respective systems; cross-target compilation does not establish permission behavior or widget delivery.

## Browser scope

The browser adapter supports Chromium-family DevTools Protocol. A launched browser owns an isolated temporary profile. Connecting uses an explicit loopback endpoint and creates a fresh page. Existing pages require explicit selection; closing an external connection preserves pages it did not create. Android Chrome uses an owned target in its existing profile; see [mobile browser scope](mobile-browser.md). Firefox remote automation and Safari DOM/CDP are outside 0.2.0. [iOS](ios.md) controls native application UI through WDA and opens URLs through the default handler.

## Android scope

A previously authorized ADB server must listen on loopback. zuse sends smart-socket and shell-v2 frames directly from Rust. It does not spawn adb for product operations. An explicit serial is recommended. Without a serial, the complete inventory must contain exactly one ready device, including consideration of offline or unauthorized entries.

Foreground/preempt mode selects display 0. Background mode selects an explicitly supplied existing nonzero logical display, and never falls back to the primary display. The test APK can create its own temporary secondary display; that fixture ability is not a product virtual-display provisioning feature.

Capture maps logical displays to SurfaceFlinger IDs. Physical IDs and virtual display IDs are resolved separately; absent, nonunique or ambiguous virtual names are rejected. Focus is correlated with per-display window tokens; unknown focus is reported as null rather than invented as false.

Text input accepts printable ASCII, excluding the ADB-reserved literal sequence %s, up to 4096 bytes. Unsupported text, keys and components fail whole-program preflight. Android I/O uses a shared operation deadline, including handshake and partial reads; this is not a promise that arbitrary filesystem work or a multi-step program finishes within 15 seconds.

The tested Android 16 device exposes window metadata, but its uiautomator command does not offer XML dumping. The result reports this observation source explicitly. Other devices and permission configurations can expose different accessibility capabilities.

## Peripheral interfaces

See [device I/O](device-io.md) for serial configuration/restoration, exact USB selection, transfer bounds, partial completion and cleanup. Device operations use the same runtime and MCP command schema.
