# Platform support and verification

Capabilities are discovered at runtime. Missing permissions, system libraries, utilities, display protocols, or device commands produce explicit errors. A successful compile is recorded separately from a live interaction test.

| Backend | Implemented interface | Validation in this release |
| --- | --- | --- |
| Linux X11 | Xlib/XTest pointer, keyboard, windows, background window events, PNG capture | Live private Xvfb; exact input and PNG pixel/CRC checks |
| Linux Hyprland | Native compositor socket IPC, windows/workspaces/focus, pointer operations | Current development compositor read-only discovery and harmless IPC check; no broad desktop settings changes |
| Wayland input/capture | Compositor-supported native utilities | Capability detection; actual utilities and permissions remain host-specific |
| Linux AT-SPI | Tree, text, bounds, focus, action, editable text, password redaction | Live private D-Bus/Xvfb GTK application |
| Chromium CDP | Launch/connect, pages, navigation, text/AX snapshot, evaluation, input, viewport, PNG | Live isolated sandboxed Chromium fixture |
| Windows | Win32 input, window management, GDI capture, UI Automation | Cross compile/type checks; live Windows session not available |
| macOS | Quartz/ApplicationServices, window capture, System Events AX | Cross compile/type checks and JXA syntax check; live macOS session not available |
| Android | Native ADB, shell-v2, display-aware input/capture/app launch | Wire tests with a fake ADB server and read-only connection to a real authorized device |
| Wine | Host X11/XWayland window control and Wine-marked window discovery | Shared X11 path verified; no Wine application available for live testing |
| IDE / Omarchy / harness adapters | Direct argument-based system command interfaces and managed processes | Process lifecycle tests; installed capability discovery; no model sessions launched by tests |

## Linux requirements

X11 needs the system X11 and XTest libraries. zuse loads them at runtime, so the core and non-X11 backends do not acquire a link-time X11 dependency. Xvfb is needed only for isolated development tests.

On Wayland, global input and screenshots depend on the compositor's authorization model. Hyprland metadata and dispatch use its native socket. Capture and input adapters may require grim, wtype, or ydotool; inspect `zuse doctor` for the selected backend. `ZUSE_BACKEND=x11` selects an X11/XWayland target explicitly.

AT-SPI requires libatspi and an accessible D-Bus session. Applications must expose their accessibility tree. OCR requires Tesseract. Desktop notifications use notify-send. The local graphical password prompt uses Zenity.

Omarchy operations call the installed Omarchy command interfaces and Hyprland. The package does not install or replace desktop themes, system settings, or Omarchy itself.

## Windows and macOS requirements

Windows input uses Win32 APIs. UI Automation uses the system PowerShell/.NET UIA interfaces. OS-protected desktops and application permission boundaries remain in effect. The password assistance flow uses the operator's secure terminal prompt.

macOS requires the appropriate Accessibility and Screen Recording permissions. Native input uses Quartz; tree operations use the system System Events/JXA interface. The local password dialog uses the OS scripting interface.

These backends have been type-checked, but runtime behavior still needs acceptance on their respective systems.

## Browser scope

The native browser backend supports Chromium-family DevTools Protocol. A launched instance owns an isolated temporary profile. Explicit close and orderly session shutdown clean up its owned browser and profile. Connecting requires an explicitly selected loopback `http://` or `ws://` endpoint; HTTP discovery uses `/json/version`. Browser endpoints create a fresh page, while a page WebSocket selects that page directly. Firefox and Safari remote automation protocols are outside this version.

## Android scope

An authorized local ADB server must be listening on loopback. The Rust client selects a device transport and sends ADB/shell-v2 frames directly. Multiple devices require a serial.

Background mode selects an existing secondary logical display and verifies its presence before effects. The adapter resolves SurfaceFlinger capture IDs separately and rejects missing or ambiguous mappings. It does not fall back to the primary display. Virtual-display provisioning is external to this release.

Android command availability varies by device. The tested device exposes window/display metadata but its uiautomator command has no dump operation; zuse reports the available observation source. Unicode text support follows the device input service; unsupported text fails explicitly.
