use super::super::{Result, image_metadata, quote, split_chord, validate_text};
use std::{
    fs::File,
    io::{Read, Write},
    os::unix::net::UnixStream,
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
            "{{\"backend\":\"wayland\",\"compositor\":{},\"pointer\":{},\"windows\":{},\"keyboard\":{},\"capture\":{},\"nativeIpc\":{},\"inputHelper\":\"wtype or ydotool with user-authorized daemon\",\"windowCapture\":\"composited visible region\",\"backgroundInput\":\"Hyprland targeted shortcuts\"}}",
            if self.socket.is_some() {
                "\"hyprland\""
            } else {
                "null"
            },
            self.socket.is_some() || installed("ydotool"),
            self.socket.is_some(),
            self.socket.is_some() || installed("wtype"),
            installed("grim"),
            self.socket.is_some()
        )
    }
    fn request(&self, command: &str) -> Result<String> {
        let socket=self.socket.as_ref().ok_or("this operation requires Hyprland IPC; other Wayland compositors need their accessibility/portal backend")?;
        let mut stream = UnixStream::connect(socket).map_err(|e| format!("Hyprland IPC: {e}"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| e.to_string())?;
        stream
            .write_all(command.as_bytes())
            .map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        stream
            .take(1_048_577)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 1_048_576 {
            return Err("Hyprland IPC response exceeds 1 MiB".into());
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
        if !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err("use XKB key names in chords, for example plus or slash".into());
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
        } else if window.is_some() {
            Err("background keys require Hyprland targeted input".into())
        } else {
            let mut c = Command::new("wtype");
            for m in &modifiers {
                c.args(["-M", &m.to_ascii_lowercase()]);
            }
            c.args(["-k", key]);
            for m in modifiers.iter().rev() {
                c.args(["-m", &m.to_ascii_lowercase()]);
            }
            run(c)
        }
    }
    pub fn type_text(&mut self, text: &str) -> Result<()> {
        validate_text(text)?;
        let mut c = Command::new("wtype");
        c.arg("--").arg(text);
        run(c)
    }
    pub fn type_into(&mut self, id: &str, text: &str) -> Result<()> {
        validate_text(text)?;
        if !text.is_ascii() {
            return Err("Hyprland background text currently accepts ASCII; use a foreground Unicode input method".into());
        }
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
        for ch in text.chars() {
            let (name, shift) = match ch {
                'a'..='z' | '0'..='9' => (ch.to_string(), false),
                'A'..='Z' => (ch.to_ascii_lowercase().to_string(), true),
                ' ' => (String::from("space"), false),
                '\n' | '\r' => (String::from("Return"), false),
                '\t' => (String::from("Tab"), false),
                c => {
                    let name=match c{
                    '!'=>"exclam",'"'=>"quotedbl",'#'=>"numbersign",'$'=>"dollar",'%'=>"percent",'&'=>"ampersand",
                    '\''=>"apostrophe",'('=>"parenleft",')'=>"parenright",'*'=>"asterisk",'+'=>"plus",','=>"comma",
                    '-'=>"minus",'.'=>"period",'/'=>"slash",':'=>"colon",';'=>"semicolon",'<'=>"less",'='=>"equal",
                    '>'=>"greater",'?'=>"question",'@'=>"at",'['=>"bracketleft",'\\'=>"backslash",']'=>"bracketright",
                    '^'=>"asciicircum",'_'=>"underscore",'\u{60}'=>"grave",'{'=>"braceleft",'|'=>"bar",'}'=>"braceright",'~'=>"asciitilde",
                    _=>return Err("Hyprland background text currently accepts ASCII; foreground text uses wtype Unicode support".into())};
                    (name.to_owned(), false)
                }
            };
            self.key_target(
                Some(id),
                &if shift { format!("Shift+{name}") } else { name },
            )?;
        }
        Ok(())
    }
    pub fn windows(&self) -> Result<String> {
        let response = self.request("j/clients")?;
        let mut tokens = vec![Token::EMPTY; 32_768];
        let doc = Document::parse(response.as_bytes(), &mut tokens, Limits::default())
            .map_err(|e| e.to_string())?;
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
                list.get(index).and_then(|v| v.integer().ok()).unwrap_or(0)
            };
            rows.push(format!(
                "{{\"id\":{},\"title\":{},\"class\":{},\"pid\":{},\"x\":{},\"y\":{},\"width\":{},\"height\":{},\"wine\":{},\"workspace\":{}}}",
                field("address"),field("title"),field("class"),field("pid"),
                coordinate(&at,0),coordinate(&at,1),coordinate(&size,0),coordinate(&size,1),wine,field("workspace")
            ));
        }
        Ok(format!("[{}]", rows.join(",")))
    }
    pub fn focus(&mut self, id: &str) -> Result<()> {
        let s = Self::selector(id)?;
        self.dispatch(
            &format!("focuswindow {s}"),
            &format!("hl.dsp.focus({{window={}}})", quote(&s)),
        )
    }
    pub fn screenshot(&self, path: &Path, window: Option<&str>) -> Result<String> {
        let mut command = Command::new("grim");
        if let Some(id) = window {
            Self::selector(id)?;
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
