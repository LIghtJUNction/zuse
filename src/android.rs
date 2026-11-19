//! Native client for the local Android Debug Bridge smart-socket protocol.
//!
//! Uses an existing, authorized ADB server. Each operation names one serial and
//! one logical display. Background mode requires an existing secondary display
//! and never falls back to display zero. No adb executable is spawned.
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::Path,
    time::{Duration, Instant},
};
pub type Result<T> = std::result::Result<T, String>;
const LIMIT: usize = 32 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(15);

fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < ' ' => {
                use std::fmt::Write;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
fn shell_quote(s: &str) -> Result<String> {
    if s.contains('\0') || s.len() > 16_384 {
        return Err("Android argument must be at most 16384 non-NUL bytes".into());
    }
    Ok(format!("'{}'", s.replace('\'', "'\\''")))
}
fn endpoint() -> Result<SocketAddr> {
    std::env::var("ZUSE_ADB_SERVER")
        .unwrap_or_else(|_| "127.0.0.1:5037".into())
        .parse()
        .map_err(|_| "ZUSE_ADB_SERVER must be an IP:port address".into())
}
fn connect_socket(address: SocketAddr) -> Result<TcpStream> {
    if !address.ip().is_loopback() {
        return Err(
            "ADB server must be loopback; use an authorized local tunnel for a remote ADB server"
                .into(),
        );
    }
    let s = TcpStream::connect_timeout(&address, Duration::from_secs(3))
        .map_err(|e| format!("connect ADB server {address}: {e}"))?;
    s.set_read_timeout(Some(TIMEOUT))
        .map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(TIMEOUT))
        .map_err(|e| e.to_string())?;
    s.set_nodelay(true).map_err(|e| e.to_string())?;
    Ok(s)
}
fn read_length(s: &mut TcpStream) -> Result<usize> {
    let mut raw = [0; 4];
    s.read_exact(&mut raw)
        .map_err(|e| format!("ADB length: {e}"))?;
    let text = std::str::from_utf8(&raw).map_err(|_| "ADB length is not ASCII")?;
    usize::from_str_radix(text, 16).map_err(|_| "ADB length is not hexadecimal".into())
}
fn request(s: &mut TcpStream, service: &str) -> Result<()> {
    if service.len() > 65_535 {
        return Err("ADB service request exceeds 65535 bytes".into());
    }
    s.write_all(format!("{:04x}", service.len()).as_bytes())
        .map_err(|e| e.to_string())?;
    s.write_all(service.as_bytes()).map_err(|e| e.to_string())?;
    let mut status = [0; 4];
    s.read_exact(&mut status)
        .map_err(|e| format!("ADB status: {e}"))?;
    match &status {
        b"OKAY" => Ok(()),
        b"FAIL" => {
            let len = read_length(s)?;
            let mut data = vec![0; len];
            s.read_exact(&mut data).map_err(|e| e.to_string())?;
            Err(format!(
                "ADB rejected request: {}",
                String::from_utf8_lossy(&data)
            ))
        }
        _ => Err("ADB returned an invalid status".into()),
    }
}
#[derive(Debug, Clone)]
struct Device {
    serial: String,
    state: String,
    detail: String,
}
fn inventory(address: SocketAddr) -> Result<Vec<Device>> {
    let mut s = connect_socket(address)?;
    request(&mut s, "host:devices-l")?;
    let len = read_length(&mut s)?;
    let mut bytes = vec![0; len];
    s.read_exact(&mut bytes).map_err(|e| e.to_string())?;
    let response = String::from_utf8(bytes).map_err(|_| "ADB device listing is not UTF-8")?;
    let mut devices = Vec::new();
    for line in response.lines() {
        let mut fields = line.split_whitespace();
        let Some(serial) = fields.next() else {
            continue;
        };
        let Some(state) = fields.next() else { continue };
        devices.push(Device {
            serial: serial.into(),
            state: state.into(),
            detail: fields.collect::<Vec<_>>().join(" "),
        });
    }
    Ok(devices)
}
struct Output {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit: u8,
}
fn shell(address: SocketAddr, serial: &str, command: &str) -> Result<Output> {
    let mut s = connect_socket(address)?;
    request(&mut s, &format!("host:transport:{serial}"))?;
    request(&mut s, &format!("shell,v2,raw:{command}"))?;
    // ADB shell-v2 packet 4 closes stdin without terminating stdout/stderr.
    s.write_all(&[4, 0, 0, 0, 0]).map_err(|e| e.to_string())?;
    let begin = Instant::now();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    loop {
        let remaining = TIMEOUT
            .checked_sub(begin.elapsed())
            .ok_or("Android operation exceeded 15 seconds")?;
        s.set_read_timeout(Some(remaining))
            .map_err(|e| e.to_string())?;
        let mut header = [0; 5];
        s.read_exact(&mut header)
            .map_err(|e| format!("ADB shell packet: {e}"))?;
        let len =
            u32::from_le_bytes(header[1..].try_into().map_err(|_| "ADB packet length")?) as usize;
        if len > LIMIT
            || stdout
                .len()
                .saturating_add(stderr.len())
                .saturating_add(len)
                > LIMIT
        {
            return Err("ADB output exceeds 32 MiB".into());
        }
        if header[0] == 3 {
            if len != 1 {
                return Err("ADB exit packet must contain one byte".into());
            }
            let mut exit = [0];
            s.read_exact(&mut exit).map_err(|e| e.to_string())?;
            return Ok(Output {
                stdout,
                stderr,
                exit: exit[0],
            });
        }
        if !matches!(header[0], 1 | 2) {
            return Err(format!("unsupported ADB shell packet {}", header[0]));
        }
        let target = if header[0] == 1 {
            &mut stdout
        } else {
            &mut stderr
        };
        let offset = target.len();
        target.resize(offset + len, 0);
        s.read_exact(&mut target[offset..])
            .map_err(|e| format!("ADB packet payload: {e}"))?;
    }
}
impl Output {
    fn success(self) -> Result<Vec<u8>> {
        if self.exit != 0 {
            return Err(format!(
                "Android command exited {}: {}{}",
                self.exit,
                String::from_utf8_lossy(&self.stderr),
                String::from_utf8_lossy(&self.stdout)
            ));
        }
        Ok(self.stdout)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Foreground,
    Background,
}
impl Mode {
    fn parse(value: &str, display: u32) -> Result<Self> {
        match value {
            "foreground" | "preempt" if display == 0 => Ok(Self::Foreground),
            "background" if display > 0 => Ok(Self::Background),
            "foreground" | "preempt" => Err("foreground/preempt mode requires display 0".into()),
            "background" => Err(
                "background mode requires an explicit nonzero display; display 0 is never used"
                    .into(),
            ),
            _ => Err("Android mode must be foreground, preempt or background".into()),
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Foreground => "foreground",
            Self::Background => "background",
        }
    }
}
/// A selected Android device and display. Existing ADB device authorization is
/// required; this client never approves device trust or starts an ADB server.
pub struct Android {
    address: SocketAddr,
    serial: String,
    mode: Mode,
    display: u32,
}
impl Android {
    pub fn devices() -> Result<String> {
        Self::devices_at(endpoint()?)
    }
    pub fn devices_at(address: SocketAddr) -> Result<String> {
        let devices = inventory(address)?
            .into_iter()
            .map(|d| {
                format!(
                    "{{\"serial\":{},\"state\":{},\"description\":{}}}",
                    quote(&d.serial),
                    quote(&d.state),
                    quote(&d.detail)
                )
            })
            .collect::<Vec<_>>();
        Ok(format!("[{}]", devices.join(",")))
    }
    pub fn connect(serial: Option<&str>, mode: &str, display: u32) -> Result<Self> {
        Self::connect_at(endpoint()?, serial, mode, display)
    }
    pub fn connect_at(
        address: SocketAddr,
        serial: Option<&str>,
        mode: &str,
        display: u32,
    ) -> Result<Self> {
        let mode = Mode::parse(mode, display)?;
        let devices = inventory(address)?;
        let selected = if let Some(serial) = serial {
            devices
                .into_iter()
                .find(|d| d.serial == serial)
                .ok_or("requested Android serial is not connected")?
        } else {
            let mut ready = devices.into_iter().filter(|d| d.state == "device");
            let d = ready.next().ok_or(
                "no authorized Android device; approve the device's own USB debugging prompt",
            )?;
            if ready.next().is_some() {
                return Err("multiple Android devices connected; select serial explicitly".into());
            }
            d
        };
        if selected.state != "device" {
            return Err(format!("Android device state is {}", selected.state));
        }
        if selected.serial.len() > 256 || !selected.serial.bytes().all(|b| b.is_ascii_graphic()) {
            return Err("invalid Android serial".into());
        }
        let this = Self {
            address,
            serial: selected.serial,
            mode,
            display,
        };
        this.ensure_display()?;
        Ok(this)
    }
    pub fn serial(&self) -> &str {
        &self.serial
    }
    pub fn display(&self) -> u32 {
        self.display
    }
    pub fn mode(&self) -> Mode {
        self.mode
    }
    fn text(&self, command: &str) -> Result<String> {
        String::from_utf8(shell(self.address, &self.serial, command)?.success()?)
            .map_err(|_| "Android text response is not UTF-8".into())
    }
    fn ensure_display(&self) -> Result<()> {
        let ids = self.text("cmd display get-displays --ids-only")?;
        if ids
            .lines()
            .filter_map(|s| s.trim().parse::<u32>().ok())
            .any(|id| id == self.display)
        {
            Ok(())
        } else {
            Err(format!(
                "Android display {} is unavailable; no fallback display was selected",
                self.display
            ))
        }
    }
    fn effect(&self, command: &str) -> Result<()> {
        self.ensure_display()?;
        let output = self.text(command)?;
        if output.lines().any(|line| {
            let s = line.trim_start();
            s.starts_with("Error:")
                || s.starts_with("Exception")
                || s.contains("SecurityException")
                || s.starts_with("Unknown command:")
        }) {
            return Err(format!("Android rejected operation: {}", output.trim()));
        }
        Ok(())
    }
    pub fn status(&self) -> Result<String> {
        self.ensure_display()?;
        let displays = self.text("cmd display get-displays")?;
        let detail = display_line(&displays, self.display)?;
        Ok(format!(
            "{{\"serial\":{},\"mode\":{},\"display\":{},\"displayInfo\":{},\"backend\":\"adb-smart-socket\",\"backgroundCreation\":\"requires an existing virtual or secondary display\"}}",
            quote(&self.serial),
            quote(self.mode.name()),
            self.display,
            quote(detail)
        ))
    }
    /// Lists windows scoped to this display. XML is included only when the
    /// device's uiautomator dump tool supplies it for the foreground display.
    pub fn snapshot(&self) -> Result<String> {
        self.ensure_display()?;
        let dump = self.text("dumpsys window windows")?;
        let marker = format!("mDisplayId={}", self.display);
        let mut windows = Vec::new();
        for block in dump.split("  Window #").skip(1) {
            let scoped = block.lines().any(|line| {
                line.split_whitespace()
                    .any(|word| word.trim_end_matches(',') == marker)
            });
            if !scoped {
                continue;
            }
            let first = block.lines().next().unwrap_or("");
            let visible = block.contains("isOnScreen=true") || block.contains("isVisible=true");
            let focus = block.contains("mIsFocused=true");
            windows.push(format!(
                "{{\"window\":{},\"visible\":{visible},\"focused\":{focus},\"display\":{}}}",
                quote(first),
                self.display
            ));
            if windows.len() >= 4096 {
                break;
            }
        }
        let (xml, note) = if self.mode == Mode::Foreground {
            match self.text("uiautomator dump --compressed /dev/tty") {
                Ok(s) if s.contains("<hierarchy") => {
                    let start = s
                        .find("<?xml")
                        .or_else(|| s.find("<hierarchy"))
                        .unwrap_or(0);
                    let end = s.rfind("</hierarchy>").map(|i| i + 12).unwrap_or(s.len());
                    (Some(s[start..end].to_owned()), "uiautomator")
                }
                _ => (
                    None,
                    "uiautomator XML unavailable; scoped native window metadata returned",
                ),
            }
        } else {
            (
                None,
                "secondary displays use scoped window metadata; shell uiautomator targets display 0",
            )
        };
        Ok(format!(
            "{{\"display\":{},\"mode\":{},\"windows\":[{}],\"accessibilityXml\":{},\"observation\":{}}}",
            self.display,
            quote(self.mode.name()),
            windows.join(","),
            xml.map(|s| quote(&s)).unwrap_or("null".into()),
            quote(note)
        ))
    }
    pub fn tap(&self, x: i32, y: i32) -> Result<()> {
        coords(x, y)?;
        self.effect(&format!("input -d {} tap {x} {y}", self.display))
    }
    pub fn swipe(&self, x1: i32, y1: i32, x2: i32, y2: i32, duration_ms: u32) -> Result<()> {
        coords(x1, y1)?;
        coords(x2, y2)?;
        if duration_ms == 0 || duration_ms > 10_000 {
            return Err("Android swipe duration must be 1..10000 ms".into());
        }
        self.effect(&format!(
            "input -d {} swipe {x1} {y1} {x2} {y2} {duration_ms}",
            self.display
        ))
    }
    pub fn key(&self, key: &str) -> Result<()> {
        let code = match key.to_ascii_lowercase().as_str() {
            "back" => "KEYCODE_BACK",
            "home" => "KEYCODE_HOME",
            "enter" | "return" => "KEYCODE_ENTER",
            "backspace" => "KEYCODE_DEL",
            "delete" => "KEYCODE_FORWARD_DEL",
            "tab" => "KEYCODE_TAB",
            "space" => "KEYCODE_SPACE",
            "esc" | "escape" => "KEYCODE_ESCAPE",
            "up" => "KEYCODE_DPAD_UP",
            "down" => "KEYCODE_DPAD_DOWN",
            "left" => "KEYCODE_DPAD_LEFT",
            "right" => "KEYCODE_DPAD_RIGHT",
            "recents" => "KEYCODE_APP_SWITCH",
            "power" => "KEYCODE_POWER",
            _ => key,
        };
        if code.is_empty()
            || code.len() > 64
            || !code
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        {
            return Err(
                "Android key must be a known alias, numeric keycode or KEYCODE_NAME".into(),
            );
        }
        self.effect(&format!("input -d {} keyevent {code}", self.display))
    }
    pub fn type_text(&self, text: &str) -> Result<()> {
        // Android's shell input text maps hardware key events; it does not
        // provide an IME Unicode protocol. Reject unsupported text explicitly.
        if text.len() > 4096
            || !text.bytes().all(|b| b == b' ' || b.is_ascii_graphic())
            || text.contains("%s")
        {
            return Err("Android shell text supports up to 4096 printable ASCII bytes without literal %s; use an authorized IME for Unicode".into());
        }
        let encoded = text.replace(' ', "%s");
        self.effect(&format!(
            "input -d {} text {}",
            self.display,
            shell_quote(&encoded)?
        ))
    }
    pub fn start_app(&self, component: &str) -> Result<()> {
        if component.len() > 512
            || !component.contains('/')
            || !component
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'/' | b'$'))
        {
            return Err("Android app must be a package/activity component".into());
        }
        self.effect(&format!(
            "am start --display {} -n {}",
            self.display,
            shell_quote(component)?
        ))
    }
    /// Captures the matching SurfaceFlinger display, whose ID is distinct from
    /// the logical ID used by input and activity launch.
    pub fn screenshot(&self, path: &Path) -> Result<String> {
        self.ensure_display()?;
        let displays = self.text("cmd display get-displays")?;
        let line = display_line(&displays, self.display)?;
        let surfaces = self.text("dumpsys SurfaceFlinger --display-id")?;
        let id = surface_for(line, &displays, &surfaces)?;
        let png = shell(self.address, &self.serial, &format!("screencap -p -d {id}"))?.success()?;
        if png.len() < 24 || png[..8] != *b"\x89PNG\r\n\x1a\n" {
            return Err("Android capture did not return PNG for the selected display".into());
        }
        let width = u32::from_be_bytes(png[16..20].try_into().map_err(|_| "PNG width")?);
        let height = u32::from_be_bytes(png[20..24].try_into().map_err(|_| "PNG height")?);
        if width == 0 || height == 0 || width > 16_384 || height > 16_384 {
            return Err("Android PNG dimensions exceed limits".into());
        }
        std::fs::write(path, &png).map_err(|e| format!("write Android screenshot: {e}"))?;
        Ok(format!(
            "{{\"path\":{},\"mimeType\":\"image/png\",\"width\":{width},\"height\":{height},\"display\":{},\"surfaceDisplay\":{},\"mode\":{}}}",
            quote(&path.to_string_lossy()),
            self.display,
            quote(&id.to_string()),
            quote(self.mode.name())
        ))
    }
}
fn coords(x: i32, y: i32) -> Result<()> {
    if (0..=1_000_000).contains(&x) && (0..=1_000_000).contains(&y) {
        Ok(())
    } else {
        Err("Android coordinates must be 0..1000000".into())
    }
}
fn display_line(displays: &str, id: u32) -> Result<&str> {
    let prefix = format!("Display id {id}:");
    displays
        .lines()
        .find(|line| line.starts_with(&prefix))
        .ok_or_else(|| format!("display {id} is absent from Android display metadata"))
}
fn display_name(line: &str) -> Option<&str> {
    line.split_once("DisplayInfo{\"")
        .and_then(|(_, s)| s.split_once('"').map(|(name, _)| name))
}
fn surface_for(line: &str, displays: &str, surfaces: &str) -> Result<u64> {
    if let Some((_, rest)) = line.split_once("uniqueId \"local:") {
        let value = rest
            .split('"')
            .next()
            .ok_or("missing physical display ID")?;
        let id = value
            .parse::<u64>()
            .map_err(|_| "invalid physical display ID")?;
        if surfaces
            .lines()
            .any(|s| s.starts_with(&format!("Display {id} ")))
        {
            return Ok(id);
        }
    }
    let name = display_name(line).ok_or("logical display has no usable display name")?;
    if displays
        .lines()
        .filter(|s| display_name(s) == Some(name))
        .count()
        != 1
    {
        return Err("display name is ambiguous; cannot safely map capture display".into());
    }
    let mut matches = surfaces
        .lines()
        .filter(|s| s.contains(&format!("displayName=\"{name}\"")));
    let matched = matches
        .next()
        .ok_or("selected display has no SurfaceFlinger capture ID")?;
    if matches.next().is_some() {
        return Err("SurfaceFlinger display name is ambiguous".into());
    }
    matched
        .strip_prefix("Display ")
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse().ok())
        .ok_or("invalid SurfaceFlinger capture ID".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn modes_do_not_fall_back() {
        assert!(Mode::parse("background", 0).is_err());
        assert!(Mode::parse("foreground", 3).is_err());
        assert_eq!(Mode::parse("background", 3).unwrap(), Mode::Background);
    }
    #[test]
    fn quote_shell_arguments() {
        assert_eq!(shell_quote("a'b").unwrap(), "'a'\\''b'");
        assert!(shell_quote("x\0y").is_err());
    }
    #[test]
    fn capture_mapping_is_display_specific() {
        let displays = "Display id 0: DisplayInfo{\"built-in\", uniqueId \"local:123\"\nDisplay id 4: DisplayInfo{\"zuse\", uniqueId \"virtual:zuse\"\n";
        let surfaces = "Display 123 (HWC display 0): displayName=\"\"\nDisplay 999 (Virtual display): displayName=\"zuse\"\n";
        assert_eq!(
            surface_for(display_line(displays, 0).unwrap(), displays, surfaces).unwrap(),
            123
        );
        assert_eq!(
            surface_for(display_line(displays, 4).unwrap(), displays, surfaces).unwrap(),
            999
        );
        assert!(
            surface_for(
                display_line(displays, 4).unwrap(),
                displays,
                "Display 123 (HWC display 0): displayName=\"\""
            )
            .is_err()
        );
        assert!(display_line(displays, 3).is_err());
    }
    #[test]
    fn adb_shell_protocol_and_targeting() {
        use std::{
            net::TcpListener,
            sync::{Arc, Mutex},
            thread,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let commands = Arc::new(Mutex::new(Vec::<String>::new()));
        let observed = commands.clone();
        fn receive(s: &mut TcpStream) -> String {
            let len = read_length(s).unwrap();
            let mut b = vec![0; len];
            s.read_exact(&mut b).unwrap();
            String::from_utf8(b).unwrap()
        }
        fn packet(s: &mut TcpStream, id: u8, data: &[u8]) {
            s.write_all(&[id]).unwrap();
            s.write_all(&(data.len() as u32).to_le_bytes()).unwrap();
            s.write_all(data).unwrap();
        }
        let server = thread::spawn(move || {
            // inventory, connect validation, per-operation validation, and tap.
            for _ in 0..4 {
                let (mut s, _) = listener.accept().unwrap();
                let service = receive(&mut s);
                s.write_all(b"OKAY").unwrap();
                if service == "host:devices-l" {
                    let list = b"fixture\tdevice model:Zuse\n";
                    s.write_all(format!("{:04x}", list.len()).as_bytes())
                        .unwrap();
                    s.write_all(list).unwrap();
                    continue;
                }
                assert_eq!(service, "host:transport:fixture");
                let cmd = receive(&mut s);
                assert!(cmd.starts_with("shell,v2,raw:"));
                s.write_all(b"OKAY").unwrap();
                let mut close = [0; 5];
                s.read_exact(&mut close).unwrap();
                assert_eq!(close, [4, 0, 0, 0, 0]);
                observed.lock().unwrap().push(cmd.clone());
                if cmd.ends_with("get-displays --ids-only") {
                    packet(&mut s, 1, b"0\n4\n");
                }
                packet(&mut s, 3, &[0]);
            }
        });
        let android = Android::connect_at(address, Some("fixture"), "background", 4).unwrap();
        android.tap(12, 34).unwrap();
        server.join().unwrap();
        let commands = commands.lock().unwrap();
        assert_eq!(
            commands.last().unwrap(),
            "shell,v2,raw:input -d 4 tap 12 34"
        );
        assert!(!commands.iter().any(|s| s.contains("input -d 0")));
    }
    #[test]
    fn rejects_remote_adb_server() {
        assert!(connect_socket("192.0.2.1:5037".parse().unwrap()).is_err());
    }
}
