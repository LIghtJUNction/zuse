#[path = "wayland_evdev.rs"]
mod evdev;
#[path = "wayland_keyboard.rs"]
mod keyboard;
#[path = "wayland_keymap.rs"]
mod keymap;
#[path = "wayland_windows.rs"]
mod windows;
#[path = "wayland_xkb.rs"]
mod xkb;
use super::super::{Result, image_metadata, quote, split_chord, validate_text};
use crate::integrations::{desktop_output, executable};
use std::{
    io::{Read, Write},
    os::unix::{fs::FileTypeExt, net::UnixStream},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};
use zmcpio::json::{Document, Kind, Limits, Token};

pub(super) struct Wayland {
    socket: Option<PathBuf>,
    windows: Result<Option<windows::Windows>>,
    modern: bool,
}

fn run(command: Command) -> Result<()> {
    desktop_output(command, Duration::from_secs(15), 128 * 1024).map(|_| ())
}

impl Wayland {
    pub fn connect() -> Result<Self> {
        if !std::env::var("WAYLAND_DISPLAY").is_ok_and(|s| !s.is_empty()) {
            return Err("WAYLAND_DISPLAY is unset".into());
        }
        let mut socket = std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
            .ok()
            .and_then(|sig| {
                if sig.is_empty() || matches!(sig.as_str(), "." | "..") || sig.contains(['/', '\0'])
                {
                    return None;
                }
                let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)?;
                [
                    runtime.join("hypr").join(&sig).join(".socket.sock"),
                    PathBuf::from("/tmp/hypr").join(sig).join(".socket.sock"),
                ]
                .into_iter()
                .find(|p| p.is_absolute() && p.metadata().is_ok_and(|m| m.file_type().is_socket()))
            });
        let mut windows = windows::Windows::discover();
        if socket.is_some() && !matches!(&windows, Ok(None)) {
            socket = None;
            windows = Err(
                "ambiguous Wayland window backend: multiple compositor sockets in the environment"
                    .into(),
            );
        }
        let mut this = Self {
            socket,
            windows,
            modern: false,
        };
        if let Ok(version) = this.request("j/version") {
            let mut tokens = [Token::EMPTY; 128];
            if let Ok(d) = Document::parse(version.as_bytes(), &mut tokens, Limits::default()) {
                let tag = d
                    .root()
                    .get("tag")
                    .and_then(|n| n.string())
                    .and_then(|s| s.as_str())
                    .unwrap_or("");
                let mut parts = tag.trim_start_matches('v').split('.');
                let major = parts
                    .next()
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0);
                let minor = parts
                    .next()
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0);
                this.modern = major > 0 || minor >= 55;
            }
        }
        Ok(this)
    }
    pub fn capabilities(&self) -> String {
        let (compositor, windowing) = match &self.windows {
            _ if self.socket.is_some() => (
                Some("hyprland"),
                self.request("j/clients").and_then(|reply| {
                    crate::json::with_value(&reply, |root| {
                        if root.kind() == Kind::Array {
                            Ok(())
                        } else {
                            Err("invalid Hyprland window list".into())
                        }
                    })
                }),
            ),
            Ok(Some(windows)) => (Some(windows.name()), windows.probe()),
            Ok(None) => (
                None,
                Err("no Hyprland, Sway or niri IPC socket in this session".into()),
            ),
            Err(reason) => (None, Err(reason.clone())),
        };
        let supports_region = matches!(compositor, Some("hyprland" | "sway"));
        let compositor = compositor.map_or("null".into(), quote);
        let windows = windowing.is_ok();
        let window_reason = windowing
            .err()
            .map_or("null".into(), |reason| quote(&reason));
        let grim = executable("grim").is_some();
        let window_capture = grim && windows && supports_region;
        format!(
            concat!(
                "{{\"backend\":\"wayland\",\"compositor\":{compositor},\"pointer\":{},",
                "\"windows\":{windows},\"keyboard\":{},\"capture\":{grim},\"nativeIpc\":{},",
                "\"keyboard_state\":{},\"num_lock\":{},",
                "\"windowing\":{{\"available\":{windows},\"reason\":{window_reason}}},",
                "\"captureRoute\":{{\"helper\":\"grim\",\"executable\":{grim},\"readiness\":\"compositor screencopy required\",\"windowRegion\":{window_capture}}},",
                "\"operationMarker\":{{\"availability\":\"protocol-dependent\",\"mode\":\"last-operation-point\",\"input\":\"shared-user-pointer\",\"clickThrough\":true,\"independentPointer\":false,\"requires\":[\"wlr-layer-shell\",\"xdg-output\",\"ARGB8888\"]}},",
                "\"inputHelper\":\"native Wayland virtual keyboard; pointer buttons/wheel use authorized ydotool daemon\",\"windowCapture\":\"composited visible region\",",
                "\"backgroundInput\":\"Hyprland targeted shortcuts\",\"backgroundText\":\"current keymap plain/Shift levels; unavailable characters rejected before input\",",
                "\"keyboardState\":\"Hyprland 0.55+: current keyboard checked separately from other devices; no held keys, Caps off, group 0; focused chords preserve verified Num Lock; text and background chords require Num off; sticky/custom modifier state unsupported\"}}"
            ),
            self.socket.is_some() || executable("ydotool").is_some(),
            self.modern && keyboard::probe().is_ok(),
            windows,
            self.modern,
            self.modern,
            compositor = compositor,
            windows = windows,
            window_reason = window_reason,
            grim = grim,
            window_capture = window_capture,
        )
    }
    fn window_backend(&self) -> Result<&windows::Windows> {
        self.windows
            .as_ref()
            .map_err(Clone::clone)?
            .as_ref()
            .ok_or_else(|| "window operations require Hyprland, SWAYSOCK or NIRI_SOCKET".into())
    }
    fn request(&self, command: &str) -> Result<String> {
        self.request_until(command, Instant::now() + Duration::from_secs(5))
    }
    fn request_until(&self, command: &str, deadline: Instant) -> Result<String> {
        self.request_checked(command, crate::key_hold::CheckDeadline::transport(deadline))?
            .ok_or_else(|| "timeout: Hyprland IPC deadline reached".into())
    }
    fn request_checked(
        &self,
        command: &str,
        deadline: crate::key_hold::CheckDeadline,
    ) -> Result<Option<String>> {
        const TIMEOUT: &str = "timeout: Hyprland IPC deadline reached";
        let socket=self.socket.as_ref().ok_or("this operation requires Hyprland IPC; other Wayland compositors need their accessibility/portal backend")?;
        let mut stream = UnixStream::connect(socket).map_err(|e| format!("Hyprland IPC: {e}"))?;
        let Some(remaining) = deadline.remaining(TIMEOUT)? else {
            return Ok(None);
        };
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(remaining))
            .map_err(|e| e.to_string())?;
        if let Err(error) = stream.write_all(command.as_bytes()) {
            return deadline.io_failure(error, TIMEOUT);
        }
        let mut bytes = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            let Some(remaining) = deadline.remaining(TIMEOUT)? else {
                return Ok(None);
            };
            stream
                .set_read_timeout(Some(remaining))
                .map_err(|e| e.to_string())?;
            let count = match stream.read(&mut buffer) {
                Ok(count) => count,
                Err(error) => return deadline.io_failure(error, TIMEOUT),
            };
            if count == 0 {
                break;
            }
            if bytes.len() + count > 1_048_576 {
                return Err("Hyprland IPC response exceeds 1 MiB".into());
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|e| e.to_string())
    }
    fn dispatch(&self, legacy: &str, modern: &str) -> Result<()> {
        let response = self.request(&if self.modern {
            format!("/eval hl.dispatch({modern})")
        } else {
            format!("/dispatch {legacy}")
        })?;
        let lower = response.to_ascii_lowercase();
        if lower.trim() != "ok" {
            Err(format!("Hyprland: {}", response.trim()))
        } else {
            Ok(())
        }
    }
    fn quiescent_keyboard(&self) -> Result<()> {
        self.keyboard_state(false).map(|_| ())
    }

    fn require_keyboard_inspection(&self) -> Result<()> {
        if self.modern {
            Ok(())
        } else {
            Err(
                "unsupported_state: keyboard state inspection requires Hyprland 0.55 or newer"
                    .into(),
            )
        }
    }

    fn held_keys(&self) -> Result<bool> {
        self.require_keyboard_inspection()?;
        // This query includes extended Linux keyboard codes and never creates
        // a virtual device or changes the active keyboard.
        let held = self.request("/repl return (function() for code=8,775 do if hl.is_key_down(code) then return true end end return false end)()")?;
        parse_held_keys(&held)
    }

    pub fn keyboard_status(&self) -> Result<String> {
        self.require_keyboard_inspection()?;
        let devices = self.request("j/devices")?;
        keyboard_status_json(&devices, self.held_keys()?)
    }

    fn main_keyboard_sysname(&self, address: &str) -> Result<String> {
        let address = canonical_keyboard_address(address)?;
        // The compositor exposes the kernel sysname directly. Device display
        // names can be duplicated or normalized, so they are never guessed.
        let response = self.request(&format!(
            "/repl return (function() local count=0 local event=nil for _,d in ipairs(hl.get_devices()) do if d.type==\"keyboard\" and d.main and d.address=={} then count=count+1 event=tonumber(string.match(d.sysname or \"\",\"^event(%d+)$\")) end end if count==1 and event then return event end return -1 end)()",
            quote(&address)
        ))?;
        let event = response.trim().parse::<u32>().map_err(
            |_| "unsupported_state: the current keyboard has no unambiguous native evdev device",
        )?;
        Ok(format!("event{event}"))
    }

    pub fn num_lock(&self, action: &str) -> Result<String> {
        if action == "read" {
            return self.keyboard_status();
        }
        if !matches!(action, "on" | "off" | "toggle") {
            return Err("Num Lock action must be read, on, off, or toggle".into());
        }
        let before = self.keyboard_state(true)?;
        let wanted = match action {
            "on" => true,
            "off" => false,
            _ => !before.num_lock,
        };
        if before.num_lock == wanted {
            return self.keyboard_status();
        }
        let sysname = self.main_keyboard_sysname(&before.main_address)?;
        keyboard::toggle_num_lock(&sysname, || {
            if self.keyboard_state(true)? != before
                || self.main_keyboard_sysname(&before.main_address)? != sysname
            {
                return Err(
                    "unsupported_state: the current keyboard changed before Num Lock input".into(),
                );
            }
            Ok(())
        })?;

        // Only the reads are retried. Replaying a toggle after an uncertain
        // result would reverse an input that the compositor has already seen.
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            let devices = self.request("j/devices")?;
            let after = check_keyboard_state(&devices, true)?;
            if after.main_address != before.main_address {
                return Err("Num Lock input was sent but the active keyboard changed before verification; input was not replayed".into());
            }
            if after.num_lock == wanted {
                return keyboard_status_json(&devices, self.held_keys()?);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "Num Lock input was sent but readback stayed {}; input was not replayed",
                    if after.num_lock { "on" } else { "off" }
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn keyboard_state(&self, allow_num_lock: bool) -> Result<KeyboardState> {
        self.require_keyboard_inspection()?;
        let state = check_keyboard_state(&self.request("j/devices")?, allow_num_lock)?;
        if self.held_keys()? {
            Err("unsupported_state: a keyboard key is held; release it before input".into())
        } else {
            Ok(state)
        }
    }
    fn hold_keyboard_state(&self, allow_other_keys: bool) -> Result<KeyboardState> {
        if !allow_other_keys {
            return self.keyboard_state(true);
        }
        self.require_keyboard_inspection()?;
        // Key ownership and ordinary-key classification are checked against
        // the captured native map immediately before the actual press.
        check_keyboard_state(&self.request("j/devices")?, true)
    }
    pub fn position(&self) -> Result<(i32, i32)> {
        let json = self.request("j/cursorpos")?;
        let mut tokens = [Token::EMPTY; 32];
        let d = Document::parse(json.as_bytes(), &mut tokens, Limits::default())
            .map_err(|e| e.to_string())?;
        let get = |key| {
            d.root()
                .get(key)
                .ok_or_else(|| format!("missing cursor {key}"))?
                .integer()
                .map_err(|e| e.to_string())
                .and_then(|v| i32::try_from(v).map_err(|_| "cursor coordinate overflow".into()))
        };
        Ok((get("x")?, get("y")?))
    }
    pub fn move_to(&mut self, x: i32, y: i32) -> Result<()> {
        if self.socket.is_some() {
            self.dispatch(
                &format!("movecursor {x} {y}"),
                &format!("hl.dsp.cursor.move({{x={x},y={y}}})"),
            )
        } else {
            let mut c = Command::new("ydotool");
            c.args([
                "mousemove",
                "--absolute",
                "-x",
                &x.to_string(),
                "-y",
                &y.to_string(),
            ]);
            run(c)
        }
    }
    pub fn button(&mut self, button: &str, action: &str) -> Result<()> {
        let b = match button {
            "left" => 0,
            "right" => 1,
            "middle" => 2,
            "back" => 3,
            "forward" => 4,
            _ => return Err("button must be left, middle, right, back or forward".into()),
        };
        let state = match action {
            "down" => 0x40,
            "up" => 0x80,
            "click" => 0xc0,
            _ => return Err("button action must be down, up or click".into()),
        };
        let mut c = Command::new("ydotool");
        c.args(["click", &format!("0x{:x}", state + b)]);
        run(c)
    }
    pub fn scroll(&mut self, dx: i32, dy: i32) -> Result<()> {
        if dx.unsigned_abs() > 1000 || dy.unsigned_abs() > 1000 {
            return Err("scroll is limited to 1000 notches per axis".into());
        }
        let mut c = Command::new("ydotool");
        c.args([
            "mousemove",
            "--wheel",
            "-x",
            &dx.to_string(),
            "-y",
            &(-dy).to_string(),
        ]);
        run(c)
    }
    pub fn key(&mut self, chord: &str) -> Result<()> {
        self.key_target(None, chord)
    }
    pub fn key_into(&mut self, id: &str, chord: &str) -> Result<()> {
        self.key_target(Some(id), chord)
    }
    pub fn hold_key(
        &mut self,
        key: &str,
        duration_ms: u64,
        window: &str,
        allow_other_keys: bool,
        cancellation: &crate::observation::Cancellation,
    ) -> Result<String> {
        use crate::{
            key_hold::{self, NativeKey},
            target::TargetWindow,
        };
        let key = NativeKey::parse(key)?;
        crate::key_hold::validate_duration(duration_ms)?;
        let state = self.hold_keyboard_state(allow_other_keys)?;
        let native = if allow_other_keys && !state.main_is_last {
            let devices = self.request("j/devices")?;
            if check_keyboard_state(&devices, true)? != state {
                return Err(
                    "stale_target: keyboard changed before native key hold preparation".into(),
                );
            }
            Some(native_hold_identity(&devices, &state.main_address)?)
        } else {
            state.require_restorable_keyboard()?;
            None
        };
        let expected = TargetWindow::from_windows(&self.windows()?, window)?;
        if !expected.focused {
            return Err("stale_target: key holds require the currently focused window".into());
        }
        let before_input = || {
            if self.hold_keyboard_state(allow_other_keys)? != state {
                return Err("stale_target: keyboard changed while preparing the key hold".into());
            }
            Ok(())
        };
        let verify_target = |deadline, held_code, pressed_by_us, ordinary: &[u32]| {
            let Some(active) = self.request_checked("j/activewindow", deadline)? else {
                return Ok(());
            };
            let Some(monitors) = self.request_checked("j/monitors", deadline)? else {
                return Ok(());
            };
            let snapshot = window_rows(&format!("[{active}]"), &monitors, &active)?;
            let current = TargetWindow::from_windows(&snapshot, &expected.id)?;
            if current != expected || !current.focused {
                return Err("stale_target: window identity, geometry, scale or focus changed during the key hold".into());
            }
            if allow_other_keys {
                let Some(devices) = self.request_checked("j/devices", deadline)? else {
                    return Ok(());
                };
                let current = check_keyboard_state(&devices, true)?;
                if current.num_lock != state.num_lock {
                    return Err(
                        "stale_target: keyboard lock state changed during the key hold".into(),
                    );
                }
                if let Some((name, _)) = &native
                    && (current.main_address != state.main_address
                        || native_hold_identity(&devices, &state.main_address)?.0 != *name)
                {
                    return Err(
                        "stale_target: native keyboard identity changed during the key hold".into(),
                    );
                }
            }
            // Returning the first disallowed code keeps the IPC response
            // bounded, while allowing ordinary user keys to remain down.
            let allowed = ordinary
                .iter()
                .map(|code| format!("[{code}]=true"))
                .collect::<Vec<_>>()
                .join(",");
            let Some(other) = self.request_checked(&format!(
                    "/repl return (function() local allowed={{{allowed}}} for code=8,775 do if hl.is_key_down(code) then if code=={held_code} then if not {pressed_by_us} then return code end elseif not allowed[code] then return code end end end return 0 end)()"
                ), deadline)? else { return Ok(()); };
            let other = other
                .trim()
                .parse::<u32>()
                .map_err(|_| "unsupported_state: cannot inspect held keyboard keys")?;
            if other > 775 || (other != 0 && other < 8) {
                return Err("unsupported_state: invalid held keyboard key code".into());
            }
            key_hold::check_pressed_keys(
                allow_other_keys,
                pressed_by_us,
                held_code,
                (other != 0).then_some(other),
                |code| ordinary.binary_search(&code).is_ok(),
            )
        };
        if let Some((name, devices)) = &native {
            keyboard::hold_evdev(
                key,
                duration_ms,
                state.num_lock,
                (name, devices),
                cancellation,
                before_input,
                verify_target,
            )
        } else {
            keyboard::hold_key(
                key,
                duration_ms,
                state.num_lock,
                allow_other_keys,
                cancellation,
                before_input,
                verify_target,
            )
        }
    }
    fn selector(id: &str) -> Result<String> {
        if id == "active" {
            return Ok("active".into());
        }
        let hex = id
            .strip_prefix("0x")
            .ok_or("Hyprland window ID must be 0x hexadecimal or active")?;
        if hex.is_empty() || hex.len() > 16 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("invalid Hyprland window ID".into());
        }
        Ok(format!("address:0x{hex}"))
    }
    fn require_foreground_target(&self, window: Option<&str>) -> Result<()> {
        let Some(id) = window else {
            return Ok(());
        };
        Self::selector(id)?;
        if id == "active" {
            return Ok(());
        }
        let active = self.request("j/activewindow")?;
        let mut tokens = vec![Token::EMPTY; 8192];
        let document = Document::parse(active.as_bytes(), &mut tokens, Limits::default())
            .map_err(|_| "unsupported_state: cannot verify the focused target window")?;
        if document
            .root()
            .get("address")
            .and_then(|v| v.string())
            .is_some_and(|value| value.matches(id))
        {
            Ok(())
        } else {
            Err("unsupported_state: Num Lock chords require the focused window; background modifier restoration is unavailable".into())
        }
    }
    fn key_target(&self, window: Option<&str>, chord: &str) -> Result<()> {
        let parts = split_chord(chord)?;
        let last = *parts.last().ok_or("empty key chord")?;
        let key = match last.to_ascii_lowercase().as_str() {
            "enter" | "return" => "Return",
            "esc" | "escape" => "Escape",
            "backspace" => "BackSpace",
            "tab" => "Tab",
            "space" => "space",
            "left" => "Left",
            "right" => "Right",
            "up" => "Up",
            "down" => "Down",
            "delete" | "del" => "Delete",
            "pageup" => "Page_Up",
            "pagedown" => "Page_Down",
            "home" => "Home",
            "end" => "End",
            _ => last,
        };
        let raw_code = key.strip_prefix("code:");
        if let Some(code) = raw_code {
            if !code.bytes().all(|b| b.is_ascii_digit())
                || !code.parse::<u16>().is_ok_and(|n| (8..=255).contains(&n))
            {
                return Err("XKB keycode must be in 8..255".into());
            }
        } else if !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err("use XKB key names or code:8..255 in chords".into());
        }
        let mut modifiers = Vec::new();
        for p in &parts[..parts.len() - 1] {
            modifiers.push(match p.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => "CTRL",
                "alt" => "ALT",
                "shift" => "SHIFT",
                "super" | "meta" | "win" | "cmd" => "SUPER",
                _ => return Err(format!("unsupported modifier {p}")),
            });
        }
        if self.socket.is_some() {
            let state = self.keyboard_state(true)?;
            if state.num_lock {
                self.require_foreground_target(window)?;
                state.require_restorable_keyboard()?;
                return keyboard::key_with_num_lock(key, &modifiers, || {
                    self.require_foreground_target(window)?;
                    let current = self.keyboard_state(true)?;
                    current.require_restorable_keyboard()?;
                    if current != state {
                        return Err(
                            "unsupported_state: keyboard changed while preparing the chord".into(),
                        );
                    }
                    Ok(())
                });
            }
            let mods = modifiers.join(" ");
            let selector = window.map(Self::selector).transpose()?.unwrap_or_default();
            let modern = if selector.is_empty() {
                format!(
                    "hl.dsp.send_shortcut({{mods={},key={}}})",
                    quote(&mods),
                    quote(key)
                )
            } else {
                format!(
                    "hl.dsp.send_shortcut({{mods={},key={},window={}}})",
                    quote(&mods),
                    quote(key),
                    quote(&selector)
                )
            };
            self.dispatch(&format!("sendshortcut {mods},{key},{selector}"), &modern)
        } else {
            Err("unsupported_state: Wayland keys require Hyprland keyboard-state inspection".into())
        }
    }
    pub fn type_text(&mut self, text: &str) -> Result<()> {
        validate_text(text)?;
        if text.is_empty() {
            return Ok(());
        }
        let before = self.keyboard_state(false)?;
        if !before.main_is_last {
            // Destroying a virtual keyboard makes Hyprland select the last
            // native device. Use the existing keymap without creating a
            // device when that would change the user's active keyboard.
            return self.type_into("active", text);
        }
        before.require_restorable_keyboard()?;
        keyboard::type_text(text, || {
            let current = self.keyboard_state(false)?;
            current.require_restorable_keyboard()?;
            if current != before {
                return Err("unsupported_state: keyboard changed while preparing text".into());
            }
            Ok(())
        })
    }
    pub fn type_into(&mut self, id: &str, text: &str) -> Result<()> {
        validate_text(text)?;
        keymap::validate(text)?;
        Self::selector(id)?;
        if self.socket.is_none() {
            return Err("background text requires Hyprland targeted input".into());
        }
        if id != "active" {
            let windows = self.windows()?;
            let mut tokens = vec![Token::EMPTY; 32_768];
            let document = Document::parse(windows.as_bytes(), &mut tokens, Limits::default())
                .map_err(|e| e.to_string())?;
            if !document.root().elements().any(|w| {
                w.get("id")
                    .and_then(|v| v.string())
                    .is_some_and(|s| s.matches(id))
            }) {
                return Err("Hyprland target window is unavailable".into());
            }
        }
        let selector = Self::selector(id)?;
        // Resolve the whole string without changing the live keymap. This
        // remains valid when an input method restores the original map while
        // Hyprland temporarily routes a shortcut to an unfocused surface.
        let original = keyboard::original_keymap()?;
        let strokes = xkb::resolve(text, &original)?;
        self.quiescent_keyboard()?;
        let deadline = Instant::now() + Duration::from_secs(15);
        for stroke in strokes {
            if Instant::now() >= deadline {
                return Err("targeted text exceeded its deadline".into());
            }
            let code = stroke.code;
            let mods = stroke.modifiers;
            self.dispatch(
                &format!("sendshortcut {mods},code:{code},{selector}"),
                &format!(
                    "hl.dsp.send_shortcut({{mods={},key=\"code:{code}\",window={}}})",
                    quote(mods),
                    quote(&selector)
                ),
            )?;
        }
        Ok(())
    }
    pub fn windows(&self) -> Result<String> {
        if self.socket.is_none() {
            return self.window_backend()?.list();
        }
        window_rows(
            &self.request("j/clients")?,
            &self.request("j/monitors")?,
            &self.request("j/activewindow")?,
        )
    }
    pub fn focus(&mut self, id: &str) -> Result<()> {
        if self.socket.is_none() {
            return self.window_backend()?.focus(id);
        }
        let s = Self::selector(id)?;
        self.dispatch(
            &format!("focuswindow {s}"),
            &format!("hl.dsp.focus({{window={}}})", quote(&s)),
        )
    }
    pub fn screenshot(&self, path: &Path, window: Option<&str>) -> Result<String> {
        use crate::screenshot::{png_dimensions, validate_output_path, write_image};

        validate_output_path(path)?;
        let grim = executable("grim").ok_or("Wayland capture requires the grim executable")?;
        let target = window.map(|id| self.capture_region(id)).transpose()?;
        let mut command = Command::new(grim);
        if let Some((_, region)) = &target {
            command.args(["-g", &region.grim_argument()]);
        }
        command.arg("-");
        let bytes = desktop_output(command, Duration::from_secs(15), 32 * 1024 * 1024)?;
        let (width, height) = png_dimensions(&bytes, 16_384)?;
        write_image(path, &bytes, || {
            if let Some(id) = window
                && self.capture_region(id)?
                    != *target.as_ref().expect("capture target was resolved")
            {
                return Err(
                    "stale_target: window identity or geometry changed during capture".into(),
                );
            }
            Ok(())
        })?;
        let mut metadata = image_metadata(path, width, height, "wayland");
        metadata.pop();
        metadata.push_str(",\"captureMode\":\"composited-region\"");
        if let Some((id, region)) = target {
            use std::fmt::Write as _;
            write!(
                metadata,
                r#","window":{},"logicalRegion":{{"x":{},"y":{},"width":{},"height":{}}}"#,
                quote(&id),
                region.x,
                region.y,
                region.width,
                region.height
            )
            .expect("writing metadata to a string");
        }
        metadata.push('}');
        Ok(metadata)
    }

    fn capture_region(&self, id: &str) -> Result<(String, windows::Region)> {
        if self.socket.is_none() {
            return self.window_backend()?.capture_region(id);
        }
        Self::selector(id)?;
        let active = id == "active";
        let response = self.request(if active {
            "j/activewindow"
        } else {
            "j/clients"
        })?;
        crate::json::with_value(&response, |root| {
            let address = |node: zmcpio::json::Value<'_>| {
                node.get("address")
                    .and_then(|value| value.string())
                    .map(|s| s.chars().collect::<String>())
            };
            let node = if active {
                root
            } else {
                root.elements()
                    .find(|node| {
                        node.get("address")
                            .and_then(|value| value.string())
                            .is_some_and(|s| s.matches(id))
                    })
                    .ok_or("Hyprland window not found")?
            };
            let address = address(node)
                .filter(|address| address != "active")
                .ok_or("Hyprland active window not found")?;
            Self::selector(&address)?;
            let pair = |key| -> Result<[i64; 2]> {
                let mut values = node
                    .get(key)
                    .ok_or("missing Hyprland window geometry")?
                    .elements();
                let mut number = || {
                    values
                        .next()
                        .and_then(|value| value.integer().ok())
                        .ok_or("invalid Hyprland window geometry")
                };
                let pair = [number()?, number()?];
                if values.next().is_some() {
                    return Err("invalid Hyprland window geometry".into());
                }
                Ok(pair)
            };
            let [x, y] = pair("at")?;
            let [width, height] = pair("size")?;
            let invalid = |_| "invalid Hyprland window geometry";
            let region = windows::Region::new(
                i32::try_from(x).map_err(invalid)?,
                i32::try_from(y).map_err(invalid)?,
                u32::try_from(width).map_err(invalid)?,
                u32::try_from(height).map_err(invalid)?,
            )?;
            Ok((address, region))
        })
    }
}

fn window_rows(clients: &str, monitors: &str, active: &str) -> Result<String> {
    let mut tokens = vec![Token::EMPTY; 32_768];
    let doc = Document::parse(clients.as_bytes(), &mut tokens, Limits::default())
        .map_err(|e| e.to_string())?;
    let mut monitor_tokens = vec![Token::EMPTY; 8192];
    let monitor_doc = Document::parse(monitors.as_bytes(), &mut monitor_tokens, Limits::default())
        .map_err(|e| e.to_string())?;
    let mut active_tokens = [Token::EMPTY; 512];
    let active_doc = Document::parse(active.as_bytes(), &mut active_tokens, Limits::default())
        .map_err(|e| e.to_string())?;
    let active_address = active_doc.root().get("address").and_then(|v| v.string());
    let mut rows = Vec::new();
    for window in doc.root().elements().take(4096) {
        let field = |key| window.get(key).map(|v| v.raw()).unwrap_or("null");
        let class = window
            .get("class")
            .and_then(|v| v.string())
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let initial = window
            .get("initialClass")
            .and_then(|v| v.string())
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let wine = [class, initial].iter().any(|s| {
            let lower = s.to_ascii_lowercase();
            lower.contains("wine") || lower.ends_with(".exe")
        });
        let at = window
            .get("at")
            .map(|v| v.elements().collect::<Vec<_>>())
            .unwrap_or_default();
        let size = window
            .get("size")
            .map(|v| v.elements().collect::<Vec<_>>())
            .unwrap_or_default();
        let coordinate = |list: &Vec<zmcpio::json::Value<'_>>, index: usize| {
            list.get(index)
                .and_then(|v| v.integer().ok())
                .map(|v| v.to_string())
                .unwrap_or_else(|| "null".into())
        };
        let monitor_id = window.get("monitor").and_then(|v| v.integer().ok());
        let scale = monitor_doc
            .root()
            .elements()
            .find(|monitor| {
                monitor_id.is_some()
                    && monitor.get("id").and_then(|v| v.integer().ok()) == monitor_id
            })
            .and_then(|monitor| monitor.get("scale"))
            .filter(|v| {
                v.raw()
                    .parse::<f64>()
                    .is_ok_and(|n| n.is_finite() && n > 0.0)
            })
            .map(|v| v.raw())
            .unwrap_or("null");
        let focused = active_address
            .as_ref()
            .zip(window.get("address").and_then(|v| v.string()))
            .map(|(active, address)| active.chars().eq(address.chars()).to_string())
            .unwrap_or_else(|| "null".into());
        let mapped = window
            .get("mapped")
            .and_then(|v| v.boolean())
            .map(|v| v.to_string())
            .unwrap_or_else(|| "null".into());
        rows.push(format!(
            "{{\"id\":{},\"title\":{},\"class\":{},\"pid\":{},\"x\":{},\"y\":{},\"width\":{},\"height\":{},\"wine\":{},\"workspace\":{},\"scale\":{scale},\"focused\":{focused},\"mapped\":{mapped}}}",
            field("address"),field("title"),field("class"),field("pid"),
            coordinate(&at,0),coordinate(&at,1),coordinate(&size,0),coordinate(&size,1),wine,field("workspace")
        ));
    }
    Ok(format!("[{}]", rows.join(",")))
}

// Hyprland de-duplicates device names across every HID category, not just
// keyboards. Fail closed if an unnamed tablet pad/tool hides that inventory.
fn native_hold_identity(snapshot: &str, address: &str) -> Result<(String, Vec<String>)> {
    if address.is_empty() {
        return Err(
            "unsupported_state: native key holds require the current keyboard identity".into(),
        );
    }
    crate::json::with_value(snapshot, |root| {
        let mut names = Vec::new();
        let mut selected = None;
        for category in ["mice", "keyboards", "tablets", "touch"] {
            let devices = root
                .get(category)
                .filter(|value| value.kind() == Kind::Array)
                .ok_or("unsupported_state: complete compositor device names are unavailable")?;
            for device in devices.elements() {
                if names.len() >= 128 {
                    return Err(
                        "unsupported_state: too many compositor input devices to inspect".into(),
                    );
                }
                let name = device
                    .get("name")
                    .and_then(|value| value.string())
                    .map(|value| value.chars().collect::<String>())
                    .filter(|value| !value.is_empty() && value.len() <= 255 && value.is_ascii())
                    .ok_or(
                        "unsupported_state: compositor device identity is missing or unsupported",
                    )?;
                if category == "keyboards"
                    && device.get("main").and_then(|value| value.boolean()) == Some(true)
                {
                    if selected.is_some()
                        || !device
                            .get("address")
                            .and_then(|value| value.string())
                            .is_some_and(|value| value.matches(address))
                    {
                        return Err("stale_target: native keyboard selection changed".into());
                    }
                    selected = Some(name.clone());
                }
                names.push(name);
            }
        }
        let name = selected.ok_or("unsupported_state: no unique current native keyboard")?;
        evdev::check_compositor_names(&name, &names)?;
        Ok((name, names))
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct KeyboardState {
    num_lock: bool,
    main_address: String,
    main_is_last: bool,
}

impl KeyboardState {
    fn require_restorable_keyboard(&self) -> Result<()> {
        // Hyprland destroys an owned virtual keyboard by selecting the last
        // remaining native device. Restrict the Num Lock path to cases where
        // that selection is exactly the keyboard we observed before input.
        if !self.main_is_last || self.main_address.is_empty() {
            return Err(
                "unsupported_state: cannot restore the same active keyboard after virtual input"
                    .into(),
            );
        }
        Ok(())
    }
}

fn check_keyboard_state(json: &str, allow_num_lock: bool) -> Result<KeyboardState> {
    let mut tokens = vec![Token::EMPTY; 8192];
    let doc = Document::parse(json.as_bytes(), &mut tokens, Limits::default())
        .map_err(|_| "unsupported_state: cannot parse compositor keyboard state")?;
    let keyboards = doc
        .root()
        .get("keyboards")
        .ok_or("unsupported_state: compositor keyboard state is unavailable")?;
    let mut count = 0;
    let mut main = 0;
    let mut main_index = 0;
    let mut main_address = String::new();
    let mut main_num_lock = false;
    for keyboard in keyboards.elements().take(65) {
        count += 1;
        if count > 64 {
            return Err("unsupported_state: too many keyboards to inspect".into());
        }
        let is_main = keyboard.get("main").and_then(|v| v.boolean()) == Some(true);
        if is_main {
            main += 1;
            main_index = count;
            if let Some(address) = keyboard.get("address").and_then(|v| v.string()) {
                main_address = address.chars().collect();
            }
        }
        if !is_main {
            // A device's LED state is not the active seat's modifier state.
            // Secondary keyboards are diagnostic data, not input guards.
            continue;
        }
        for (field, name) in [("capsLock", "Caps Lock"), ("numLock", "Num Lock")] {
            match keyboard.get(field).and_then(|v| v.boolean()) {
                Some(false) => {}
                Some(true) if field == "numLock" && allow_num_lock => {
                    main_num_lock = true;
                }
                Some(true) => {
                    return Err(format!(
                        "unsupported_state: {name} is enabled; disable it before input"
                    ));
                }
                None => return Err(format!("unsupported_state: {name} state is unavailable")),
            }
        }
        match keyboard
            .get("active_layout_index")
            .and_then(|v| v.integer().ok())
        {
            Some(0) => {}
            Some(_) => {
                return Err(
                    "unsupported_state: keyboard layout group must be 0 before input".into(),
                );
            }
            None => return Err("unsupported_state: keyboard layout group is unavailable".into()),
        }
    }
    if count == 0 || main != 1 {
        return Err("unsupported_state: a unique main keyboard is required".into());
    }
    Ok(KeyboardState {
        num_lock: main_num_lock,
        main_address,
        main_is_last: main_index == count,
    })
}

fn parse_held_keys(response: &str) -> Result<bool> {
    match response.trim() {
        "false" => Ok(false),
        "true" => Ok(true),
        _ => Err("unsupported_state: compositor cannot report held keyboard keys".into()),
    }
}

fn canonical_keyboard_address(address: &str) -> Result<String> {
    // `hyprctl -j devices` omits the 0x prefix used by Lua device.address.
    let digits = address.strip_prefix("0x").unwrap_or(address);
    let value = u64::from_str_radix(digits, 16)
        .ok()
        .filter(|address| *address != 0)
        .ok_or("unsupported_state: the current keyboard has no stable identity")?;
    Ok(format!("0x{value:x}"))
}

fn keyboard_status_json(json: &str, held: bool) -> Result<String> {
    let mut tokens = vec![Token::EMPTY; 8192];
    let doc = Document::parse(json.as_bytes(), &mut tokens, Limits::default())
        .map_err(|_| "unsupported_state: cannot parse compositor keyboard state")?;
    let keyboards = doc
        .root()
        .get("keyboards")
        .ok_or("unsupported_state: compositor keyboard state is unavailable")?;
    let mut current = None;
    let mut other_devices = Vec::new();
    for (index, device) in keyboards.elements().take(65).enumerate() {
        if index == 64 {
            return Err("unsupported_state: too many keyboards to inspect".into());
        }
        if device.get("main").and_then(|v| v.boolean()) == Some(true) {
            if current.replace(device.raw()).is_some() {
                return Err("unsupported_state: a unique main keyboard is required".into());
            }
        } else {
            other_devices.push(device.raw());
        }
    }
    let current = current.ok_or("unsupported_state: a unique main keyboard is required")?;
    Ok(format!(
        "{{\"backend\":\"wayland\",\"compositor\":\"hyprland\",\"current\":{current},\"other_devices\":[{}],\"keys_held\":{held}}}",
        other_devices.join(",")
    ))
}
#[cfg(test)]
mod state_tests {
    use super::{
        canonical_keyboard_address, check_keyboard_state, keyboard_status_json,
        native_hold_identity, parse_held_keys, window_rows,
    };
    #[test]
    fn native_identity_requires_complete_globally_unique_device_inventory() {
        let devices = r#"{"mice":[{"name":"pointer"}],"keyboards":[{"name":"at-translated-set-2-keyboard","address":"0x1","main":true},{"name":"hl-virtual-keyboard-fcitx5","address":"0x2","main":false}],"tablets":[],"touch":[]}"#;
        assert_eq!(
            native_hold_identity(devices, "0x1").unwrap().0,
            "at-translated-set-2-keyboard"
        );
        assert!(native_hold_identity(devices, "0x2").is_err());
        assert!(
            native_hold_identity(
                &devices.replace("pointer", "at-translated-set-2-keyboard-1"),
                "0x1"
            )
            .is_err()
        );
        assert!(
            native_hold_identity(
                &devices.replace("\"tablets\":[]", "\"tablets\":[{\"type\":\"tabletTool\"}]"),
                "0x1"
            )
            .is_err()
        );
        assert!(
            native_hold_identity(&devices.replace("\"touch\":[]", "\"touch\":null"), "0x1")
                .is_err()
        );
    }

    #[test]
    #[ignore = "opt-in read-only native identity/capability preflight; opens the existing device without submitting input"]
    fn native_evdev_preflight_current_keyboard_without_input() -> super::Result<()> {
        let wayland = super::Wayland::connect()?;
        let snapshot = wayland.request("j/devices")?;
        let state = check_keyboard_state(&snapshot, true)?;
        let name = crate::json::with_value(&snapshot, |root| {
            let keyboards = root.get("keyboards").ok_or("missing keyboards")?;
            let current = keyboards
                .elements()
                .find(|device| device.get("main").and_then(|v| v.boolean()) == Some(true))
                .ok_or("missing main keyboard")?;
            crate::json::text(current.get("name").ok_or("missing keyboard name")?)
        })?;
        if name.starts_with("hl-virtual-") || name == "wl_keyboard" {
            eprintln!("SKIP: current keyboard is virtual; no device selection or input performed");
            return Ok(());
        }
        let (name, inventory) = native_hold_identity(&snapshot, &state.main_address)?;
        let keymap = super::keyboard::original_keymap()?;
        let chord = super::xkb::chord_in_state("Right", &[], &keymap, state.num_lock)?;
        let device = super::evdev::Keyboard::open_unique(&name, &inventory, chord.code)?;
        device.verify()?;
        let _ = device.held_codes()?; // Read the bitmap, never log the user's keys.
        let after = wayland.request("j/devices")?;
        if check_keyboard_state(&after, true)? != state
            || native_hold_identity(&after, &state.main_address)?.0 != name
        {
            return Err("stale_target: current keyboard changed during read-only preflight".into());
        }
        assert!(!device.pressed_by_us());
        eprintln!(
            "PASS: exact native identity, capability and same-FD verification; input_events=0"
        );
        Ok(())
    }

    #[test]
    fn rejects_known_nonzero_or_unknown_states() {
        let neutral = r#"{"keyboards":[{"main":true,"capsLock":false,"numLock":false,"active_layout_index":0}]}"#;
        check_keyboard_state(neutral, false).unwrap();
        for invalid in [
            neutral.replace("capsLock\":false", "capsLock\":true"),
            neutral.replace("numLock\":false", "numLock\":true"),
            neutral.replace("active_layout_index\":0", "active_layout_index\":1"),
            neutral.replace("main\":true", "main\":false"),
            r#"{"keyboards":[]}"#.into(),
            r#"{"keyboards":[{"main":true}]}"#.into(),
        ] {
            assert!(
                check_keyboard_state(&invalid, false)
                    .unwrap_err()
                    .starts_with("unsupported_state:")
            );
        }
    }

    #[test]
    fn num_lock_is_allowed_only_for_a_restorable_foreground_chord() {
        let num = r#"{"keyboards":[{"address":"0x123","main":true,"capsLock":false,"numLock":true,"active_layout_index":0}]}"#;
        assert!(
            check_keyboard_state(num, false).is_err(),
            "text remains guarded"
        );
        let state = check_keyboard_state(num, true).unwrap();
        assert!(state.num_lock);
        state.require_restorable_keyboard().unwrap();
        for invalid in [
            num.replace("capsLock\":false", "capsLock\":true"),
            num.replace("active_layout_index\":0", "active_layout_index\":1"),
            num.replace("numLock\":true", "numLock\":null"),
        ] {
            assert!(check_keyboard_state(&invalid, true).is_err());
        }
        let missing_identity = num.replace("\"address\":\"0x123\",", "");
        assert!(
            check_keyboard_state(&missing_identity, true)
                .unwrap()
                .require_restorable_keyboard()
                .is_err()
        );
    }

    #[test]
    fn num_lock_does_not_silently_switch_to_another_native_keyboard() {
        let main = r#"{"address":"0x1","main":true,"capsLock":false,"numLock":true,"active_layout_index":0}"#;
        let other = r#"{"address":"0x2","main":false,"capsLock":false,"numLock":false,"active_layout_index":0}"#;
        let safe = format!("{{\"keyboards\":[{other},{main}]}}");
        check_keyboard_state(&safe, true)
            .unwrap()
            .require_restorable_keyboard()
            .unwrap();
        let unsafe_order = format!("{{\"keyboards\":[{main},{other}]}}");
        assert!(
            check_keyboard_state(&unsafe_order, true)
                .unwrap()
                .require_restorable_keyboard()
                .is_err()
        );
        let mixed = safe
            .replace("\"numLock\":true", "\"numLock\":false")
            .replace(
                "\"main\":false,\"capsLock\":false,\"numLock\":false",
                "\"main\":false,\"capsLock\":false,\"numLock\":true",
            );
        assert!(!check_keyboard_state(&mixed, true).unwrap().num_lock);
    }

    #[test]
    fn eight_other_num_locks_do_not_block_the_current_keyboard() {
        let main = r#"{"address":"0x1","main":true,"capsLock":false,"numLock":false,"active_layout_index":0}"#;
        let other = r#"{"main":false,"capsLock":true,"numLock":true,"active_layout_index":3}"#;
        let devices = format!("{{\"keyboards\":[{main},{}]}}", [other; 8].join(","));
        let state = check_keyboard_state(&devices, false).unwrap();
        assert!(!state.num_lock);
        assert!(
            !state.main_is_last,
            "text must avoid creating a virtual keyboard"
        );
        let report = keyboard_status_json(&devices, false).unwrap();
        let mut tokens = [super::Token::EMPTY; 256];
        let doc = super::Document::parse(report.as_bytes(), &mut tokens, super::Limits::default())
            .unwrap();
        assert_eq!(
            doc.root()
                .get("current")
                .unwrap()
                .get("numLock")
                .unwrap()
                .boolean(),
            Some(false)
        );
        assert_eq!(
            doc.root().get("other_devices").unwrap().elements().count(),
            8
        );
        assert!(
            doc.root()
                .get("other_devices")
                .unwrap()
                .elements()
                .all(|device| device.get("numLock").unwrap().boolean() == Some(true))
        );
    }

    #[test]
    fn unknown_secondary_state_is_diagnostic_but_current_state_must_be_known() {
        let devices = r#"{"keyboards":[{"main":true,"capsLock":false,"numLock":false,"active_layout_index":0},{"main":false}]}"#;
        check_keyboard_state(devices, false).unwrap();
        for field in ["capsLock", "numLock", "active_layout_index"] {
            assert!(check_keyboard_state(&devices.replace(field, "missing"), false).is_err());
        }
        let ambiguous = devices.replace("{\"main\":false}", "{\"main\":true}");
        assert!(check_keyboard_state(&ambiguous, false).is_err());
        assert!(keyboard_status_json(&ambiguous, false).is_err());
    }

    #[test]
    fn held_keys_and_unknown_held_state_remain_distinct_from_quiescence() {
        assert!(!parse_held_keys("false\n").unwrap());
        assert!(parse_held_keys("true\n").unwrap());
        for unknown in ["", "nil", "error", "0"] {
            assert!(parse_held_keys(unknown).is_err());
        }
    }

    #[test]
    fn keyboard_inspection_rejects_held_or_unreported_keys_before_input() {
        use std::{
            io::{Read, Write},
            os::unix::net::UnixListener,
        };
        for (index, held) in ["true", "nil"].into_iter().enumerate() {
            let path = std::env::temp_dir().join(format!(
                "zuse-keyboard-state-{}-{index}.sock",
                std::process::id()
            ));
            let listener = UnixListener::bind(&path).unwrap();
            listener.set_nonblocking(true).unwrap();
            let server = std::thread::spawn(move || {
                for (expected, response) in [
                    (
                        "j/devices",
                        r#"{"keyboards":[{"main":true,"capsLock":false,"numLock":false,"active_layout_index":0}]}"#,
                    ),
                    ("/repl return", held),
                ] {
                    let deadline = super::Instant::now() + super::Duration::from_secs(2);
                    let mut stream = loop {
                        match listener.accept() {
                            Ok((stream, _)) => break stream,
                            Err(error)
                                if error.kind() == std::io::ErrorKind::WouldBlock
                                    && super::Instant::now() < deadline =>
                            {
                                std::thread::sleep(super::Duration::from_millis(1))
                            }
                            Err(error) => panic!("keyboard inspection request missing: {error}"),
                        }
                    };
                    stream
                        .set_read_timeout(Some(super::Duration::from_secs(2)))
                        .unwrap();
                    let mut bytes = [0; 1024];
                    let length = stream.read(&mut bytes).unwrap();
                    assert!(
                        std::str::from_utf8(&bytes[..length])
                            .unwrap()
                            .starts_with(expected)
                    );
                    stream.write_all(response.as_bytes()).unwrap();
                }
            });
            let backend = super::Wayland {
                socket: Some(path.clone()),
                windows: Ok(None),
                modern: true,
            };
            let result = backend.keyboard_state(false);
            server.join().unwrap();
            std::fs::remove_file(path).unwrap();
            assert!(result.unwrap_err().starts_with("unsupported_state:"));
        }
    }

    #[test]
    fn native_keyboard_lookup_normalizes_the_json_and_lua_addresses() {
        assert_eq!(canonical_keyboard_address("12ab").unwrap(), "0x12ab");
        assert_eq!(canonical_keyboard_address("0x12AB").unwrap(), "0x12ab");
        for invalid in ["", "0", "0x", "../input/event0"] {
            assert!(canonical_keyboard_address(invalid).is_err());
        }
    }

    #[test]
    fn window_scale_focus_and_mapping_are_observed_without_defaults() {
        let clients = r#"[{"address":"0x1","at":[40,50],"size":[800,600],"monitor":2,"mapped":true},{"address":"0x2","monitor":9}]"#;
        let monitors = r#"[{"id":1,"scale":1},{"id":2,"scale":1.5}]"#;
        let rows = window_rows(clients, monitors, r#"{"address":"0x1"}"#).unwrap();
        let mut tokens = [super::Token::EMPTY; 128];
        let doc =
            super::Document::parse(rows.as_bytes(), &mut tokens, super::Limits::default()).unwrap();
        let rows = doc.root().elements().collect::<Vec<_>>();
        assert_eq!(rows[0].get("scale").unwrap().raw(), "1.5");
        assert_eq!(rows[0].get("focused").unwrap().boolean(), Some(true));
        assert_eq!(rows[0].get("mapped").unwrap().boolean(), Some(true));
        for missing in ["scale", "mapped", "x", "y", "width", "height"] {
            assert_eq!(rows[1].get(missing).unwrap().raw(), "null");
        }
        assert_eq!(rows[1].get("focused").unwrap().boolean(), Some(false));
        assert!(
            window_rows(clients, monitors, "{}")
                .unwrap()
                .contains("\"focused\":null")
        );
    }
}
