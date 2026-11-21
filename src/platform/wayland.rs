#[path = "wayland_hold.rs"]
mod hold;
#[path = "wayland_keyboard.rs"]
mod keyboard;
#[path = "wayland_keymap.rs"]
mod keymap;
#[path = "wayland_xkb.rs"]
mod xkb;
use super::super::{Result, hypr_keys, image_metadata, quote, validate_text};
use std::{
    fs::File,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, net::UnixStream},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use zmcpio::json::{Document, Limits, Token};

pub(super) struct Wayland {
    socket: Option<PathBuf>,
    modern: bool,
}
fn installed(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
}
fn run(mut command: Command) -> Result<()> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| format!("start native utility: {e}"))?;
    let begin = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if begin.elapsed() < Duration::from_secs(15) => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("desktop utility exceeded 15 seconds".into());
            }
            Err(e) => return Err(format!("wait for desktop utility: {e}")),
        }
    }
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "desktop utility failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}
impl Wayland {
    pub fn connect() -> Result<Self> {
        if !std::env::var("WAYLAND_DISPLAY").is_ok_and(|s| !s.is_empty()) {
            return Err("WAYLAND_DISPLAY is unset".into());
        }
        let socket = std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
            .ok()
            .and_then(|sig| {
                if sig.contains('/') || sig.contains('\0') {
                    return None;
                }
                let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)?;
                [
                    runtime.join("hypr").join(&sig).join(".socket.sock"),
                    PathBuf::from("/tmp/hypr").join(sig).join(".socket.sock"),
                ]
                .into_iter()
                .find(|p| p.exists())
            });
        let mut this = Self {
            socket,
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
        format!(
            "{{\"backend\":\"wayland\",\"compositor\":{},\"pointer\":{},\"windows\":{},\"keyboard\":{},\"capture\":{},\"nativeIpc\":{},\"keyboard_state\":{},\"num_lock\":{},\"operationMarker\":{{\"availability\":\"protocol-dependent\",\"mode\":\"last-operation-point\",\"input\":\"shared-user-pointer\",\"clickThrough\":true,\"independentPointer\":false,\"requires\":[\"wlr-layer-shell\",\"xdg-output\",\"ARGB8888\"]}},\"inputHelper\":\"native Wayland virtual keyboard; pointer buttons/wheel use authorized ydotool daemon\",\"windowCapture\":\"composited visible region\",\"backgroundInput\":\"Hyprland targeted shortcuts\",\"backgroundText\":\"current keymap plain/Shift levels; unavailable characters rejected before input\",\"keyboardState\":\"Hyprland 0.55+: current keyboard checked separately from other devices; no held keys, Caps off, group 0; focused chords preserve verified Num Lock; text and background chords require Num off; sticky/custom modifier state unsupported\"}}",
            if self.socket.is_some() {
                "\"hyprland\""
            } else {
                "null"
            },
            self.socket.is_some() || installed("ydotool"),
            self.socket.is_some(),
            self.modern && keyboard::probe().is_ok(),
            installed("grim"),
            self.socket.is_some(),
            self.modern,
            self.modern
        )
    }
    fn request(&self, command: &str) -> Result<String> {
        let mut stream = self.ipc_stream(Duration::from_secs(5))?;
        stream
            .write_all(command.as_bytes())
            .map_err(|e| e.to_string())?;
        Self::read_reply(stream, Duration::from_secs(5))
    }
    fn ipc_stream(&self, timeout: Duration) -> Result<UnixStream> {
        let socket = self.socket.as_ref().ok_or("this operation requires Hyprland IPC; other Wayland compositors need their accessibility/portal backend")?;
        // SO_SNDTIMEO applies to connect as well as write on Linux. Install it
        // before connecting: a full compositor listen queue can otherwise
        // block the target check or release indefinitely after a key-down.
        let path = socket.as_os_str().as_bytes();
        #[repr(C)]
        struct UnixAddress {
            family: u16,
            path: [u8; 108],
        }
        let mut address = UnixAddress {
            family: 1,
            path: [0; 108],
        };
        if path.is_empty() || path.len() >= address.path.len() || path.contains(&0) {
            return Err("invalid Hyprland IPC socket path".into());
        }
        address.path[..path.len()].copy_from_slice(path);
        unsafe extern "C" {
            #[link_name = "socket"]
            fn unix_socket(domain: i32, kind: i32, protocol: i32) -> i32;
            fn connect(fd: i32, address: *const UnixAddress, len: u32) -> i32;
        }
        let stream_type = if cfg!(any(
            target_arch = "mips",
            target_arch = "mips32r6",
            target_arch = "mips64",
            target_arch = "mips64r6"
        )) {
            2
        } else {
            1
        };
        let cloexec = if cfg!(any(target_arch = "sparc", target_arch = "sparc64")) {
            1 << 22
        } else {
            1 << 19
        };
        // SAFETY: Linux AF_UNIX stream socket. The new descriptor is owned by
        // this UnixStream on every subsequent return path.
        let fd = unsafe { unix_socket(1, stream_type | cloexec, 0) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        let stream = unsafe { UnixStream::from_raw_fd(fd) };
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|e| e.to_string())?;
        // SAFETY: initialized sockaddr_un including the terminating NUL.
        let len = size_of::<u16>() + path.len() + 1;
        if unsafe { connect(stream.as_raw_fd(), &address, len as u32) } < 0 {
            return Err(format!("Hyprland IPC: {}", std::io::Error::last_os_error()));
        }
        Ok(stream)
    }
    fn read_reply(mut stream: UnixStream, timeout: Duration) -> Result<String> {
        let mut bytes = Vec::new();
        let deadline = Instant::now() + timeout;
        let mut buffer = [0; 8192];
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or("Hyprland IPC exceeded its deadline")?;
            stream
                .set_read_timeout(Some(remaining))
                .map_err(|e| e.to_string())?;
            let count = match stream.read(&mut buffer) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                result => result.map_err(|e| e.to_string())?,
            };
            if count == 0 {
                break;
            }
            if bytes.len() + count > 1_048_576 {
                return Err("Hyprland IPC response exceeds 1 MiB".into());
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        String::from_utf8(bytes).map_err(|e| e.to_string())
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
    fn require_foreground_target(&self, window: Option<&str>) -> Result<()> {
        let Some(id) = window else {
            return Ok(());
        };
        hypr_keys::selector(id)?;
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
        let (key, modifiers) = hypr_keys::chord(chord)?;
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
            let selector = window
                .map(hypr_keys::selector)
                .transpose()?
                .unwrap_or_default();
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
        hypr_keys::selector(id)?;
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
        let selector = hypr_keys::selector(id)?;
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
        window_rows(
            &self.request("j/clients")?,
            &self.request("j/monitors")?,
            &self.request("j/activewindow")?,
        )
    }
    pub fn focus(&mut self, id: &str) -> Result<()> {
        let s = hypr_keys::selector(id)?;
        self.dispatch(
            &format!("focuswindow {s}"),
            &format!("hl.dsp.focus({{window={}}})", quote(&s)),
        )
    }
    pub fn screenshot(&self, path: &Path, window: Option<&str>) -> Result<String> {
        let mut command = Command::new("grim");
        if let Some(id) = window {
            hypr_keys::selector(id)?;
            let response = if id == "active" {
                self.request("j/activewindow")?
            } else {
                self.request("j/clients")?
            };
            let mut tokens = vec![Token::EMPTY; 32_768];
            let doc = Document::parse(response.as_bytes(), &mut tokens, Limits::default())
                .map_err(|e| e.to_string())?;
            let node = if id == "active" {
                doc.root()
            } else {
                doc.root()
                    .elements()
                    .find(|n| {
                        n.get("address")
                            .and_then(|v| v.string())
                            .is_some_and(|s| s.matches(id))
                    })
                    .ok_or("Hyprland window not found")?
            };
            let at = node
                .get("at")
                .ok_or("missing window coordinates")?
                .elements()
                .collect::<Vec<_>>();
            let size = node
                .get("size")
                .ok_or("missing window dimensions")?
                .elements()
                .collect::<Vec<_>>();
            if at.len() != 2 || size.len() != 2 {
                return Err("invalid Hyprland window geometry".into());
            }
            let x = at[0].integer().map_err(|e| e.to_string())?;
            let y = at[1].integer().map_err(|e| e.to_string())?;
            let w = size[0].integer().map_err(|e| e.to_string())?;
            let h = size[1].integer().map_err(|e| e.to_string())?;
            if w <= 0 || h <= 0 || w > 16_384 || h > 16_384 {
                return Err("invalid capture geometry".into());
            }
            command.args(["-g", &format!("{x},{y} {w}x{h}")]);
        }
        command.arg("--").arg(path);
        run(command)?;
        let mut file = File::open(path).map_err(|e| e.to_string())?;
        let mut header = [0; 24];
        file.read_exact(&mut header).map_err(|e| e.to_string())?;
        if header[..8] != *b"\x89PNG\r\n\x1a\n" {
            return Err("capture utility did not return PNG".into());
        }
        let width = u32::from_be_bytes(header[16..20].try_into().map_err(|_| "PNG width")?);
        let height = u32::from_be_bytes(header[20..24].try_into().map_err(|_| "PNG height")?);
        let mut metadata = image_metadata(path, width, height, "wayland");
        metadata.pop();
        metadata.push_str(",\"captureMode\":\"composited-region\"}");
        Ok(metadata)
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
        canonical_keyboard_address, check_keyboard_state, keyboard_status_json, parse_held_keys,
        window_rows,
    };
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
