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
#[derive(Clone, Copy)]
struct Deadline(Instant);
impl Deadline {
    fn after(duration: Duration) -> Self {
        Self(Instant::now() + duration)
    }
    fn operation() -> Self {
        Self::after(TIMEOUT)
    }
    fn remaining(self) -> Result<Duration> {
        self.0
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| "Android operation deadline exceeded".into())
    }
    fn check(self) -> Result<()> {
        self.remaining().map(|_| ())
    }
}
fn io_error(context: &str, error: std::io::Error) -> String {
    if matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ) {
        format!("{context}: Android operation deadline exceeded")
    } else {
        format!("{context}: {error}")
    }
}
fn read_exact_until(
    stream: &mut TcpStream,
    mut bytes: &mut [u8],
    deadline: Deadline,
    context: &str,
) -> Result<()> {
    while !bytes.is_empty() {
        stream
            .set_read_timeout(Some(deadline.remaining()?))
            .map_err(|e| e.to_string())?;
        match stream.read(bytes) {
            Ok(0) => return Err(format!("{context}: unexpected end of ADB stream")),
            Ok(count) => bytes = &mut bytes[count..],
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(io_error(context, error)),
        }
    }
    deadline.check()
}
fn write_all_until(
    stream: &mut TcpStream,
    mut bytes: &[u8],
    deadline: Deadline,
    context: &str,
) -> Result<()> {
    while !bytes.is_empty() {
        stream
            .set_write_timeout(Some(deadline.remaining()?))
            .map_err(|e| e.to_string())?;
        match stream.write(bytes) {
            Ok(0) => return Err(format!("{context}: ADB write made no progress")),
            Ok(count) => bytes = &bytes[count..],
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(io_error(context, error)),
        }
    }
    deadline.check()
}
fn connect_socket(address: SocketAddr, deadline: Deadline) -> Result<TcpStream> {
    if !address.ip().is_loopback() {
        return Err(
            "ADB server must be loopback; use an authorized local tunnel for a remote ADB server"
                .into(),
        );
    }
    let socket =
        TcpStream::connect_timeout(&address, deadline.remaining()?.min(Duration::from_secs(3)))
            .map_err(|e| format!("connect ADB server {address}: {e}"))?;
    deadline.check()?;
    socket.set_nodelay(true).map_err(|e| e.to_string())?;
    Ok(socket)
}
fn read_length(stream: &mut TcpStream, deadline: Deadline) -> Result<usize> {
    let mut raw = [0; 4];
    read_exact_until(stream, &mut raw, deadline, "ADB length")?;
    let text = std::str::from_utf8(&raw).map_err(|_| "ADB length is not ASCII")?;
    usize::from_str_radix(text, 16).map_err(|_| "ADB length is not hexadecimal".into())
}
fn request(stream: &mut TcpStream, service: &str, deadline: Deadline) -> Result<()> {
    if service.len() > 65_535 {
        return Err("ADB service request exceeds 65535 bytes".into());
    }
    write_all_until(
        stream,
        format!("{:04x}", service.len()).as_bytes(),
        deadline,
        "ADB request length",
    )?;
    write_all_until(stream, service.as_bytes(), deadline, "ADB request")?;
    let mut status = [0; 4];
    read_exact_until(stream, &mut status, deadline, "ADB status")?;
    match &status {
        b"OKAY" => Ok(()),
        b"FAIL" => {
            let len = read_length(stream, deadline)?;
            let mut data = vec![0; len];
            read_exact_until(stream, &mut data, deadline, "ADB rejection")?;
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
fn inventory(address: SocketAddr, deadline: Deadline) -> Result<Vec<Device>> {
    let mut s = connect_socket(address, deadline)?;
    request(&mut s, "host:devices-l", deadline)?;
    let len = read_length(&mut s, deadline)?;
    let mut bytes = vec![0; len];
    read_exact_until(&mut s, &mut bytes, deadline, "ADB device listing")?;
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
fn shell(address: SocketAddr, serial: &str, command: &str, deadline: Deadline) -> Result<Output> {
    let mut stream = connect_socket(address, deadline)?;
    request(&mut stream, &format!("host:transport:{serial}"), deadline)?;
    request(&mut stream, &format!("shell,v2,raw:{command}"), deadline)?;
    // Shell-v2 packet 4 closes stdin without terminating stdout/stderr.
    write_all_until(&mut stream, &[4, 0, 0, 0, 0], deadline, "ADB close stdin")?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    loop {
        let mut header = [0; 5];
        read_exact_until(&mut stream, &mut header, deadline, "ADB shell packet")?;
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
            read_exact_until(&mut stream, &mut exit, deadline, "ADB exit packet")?;
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
        read_exact_until(
            &mut stream,
            &mut target[offset..],
            deadline,
            "ADB packet payload",
        )?;
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
    fn checked_effect(self) -> Result<()> {
        // Some Android command versions report errors on stderr but exit zero.
        // A nonzero exit is handled by success(), including both output streams.
        if self.exit == 0 {
            let stderr = String::from_utf8_lossy(&self.stderr);
            if reports_failure(&stderr) {
                return Err(format!("Android rejected operation: {}", stderr.trim()));
            }
        }
        let stdout = String::from_utf8(self.success()?)
            .map_err(|_| "Android text response is not UTF-8".to_string())?;
        if reports_failure(&stdout) {
            return Err(format!("Android rejected operation: {}", stdout.trim()));
        }
        Ok(())
    }
}
fn reports_failure(output: &str) -> bool {
    output.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("Error:")
            || line.starts_with("Exception")
            || line.contains("SecurityException")
            || line.starts_with("Unknown command:")
    })
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
    /// List observed Chrome DevTools socket names for one explicitly chosen device.
    /// Does not start Chrome, inspect tabs, create a forward, or select a display.
    pub fn browser_sockets(serial: &str) -> Result<String> {
        Self::browser_sockets_at(endpoint()?, serial)
    }

    pub fn browser_sockets_at(address: SocketAddr, serial: &str) -> Result<String> {
        let sockets = browser_sockets_until(address, serial, Deadline::operation())?;
        Ok(format!(
            r#"{{"backend":"adb-native-tunnel","sockets":[{}],"scope":"chrome","existingTabsRead":false}}"#,
            sockets
                .iter()
                .map(|socket| quote(socket))
                .collect::<Vec<_>>()
                .join(",")
        ))
    }

    pub fn devices() -> Result<String> {
        Self::devices_at(endpoint()?)
    }
    pub fn devices_at(address: SocketAddr) -> Result<String> {
        let deadline = Deadline::operation();
        let devices = inventory(address, deadline)?
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
        let result = format!("[{}]", devices.join(","));
        deadline.check()?;
        Ok(result)
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
        Self::connect_at_until(address, serial, mode, display, Deadline::operation())
    }
    pub(crate) fn connect_with_status(
        serial: Option<&str>,
        mode: &str,
        display: u32,
    ) -> Result<(Self, String)> {
        let deadline = Deadline::operation();
        let android = Self::connect_at_until(endpoint()?, serial, mode, display, deadline)?;
        let status = android.status_until(deadline)?;
        Ok((android, status))
    }
    fn connect_at_until(
        address: SocketAddr,
        serial: Option<&str>,
        mode: &str,
        display: u32,
        deadline: Deadline,
    ) -> Result<Self> {
        let mode = Mode::parse(mode, display)?;
        let devices = inventory(address, deadline)?;
        let selected = if let Some(serial) = serial {
            devices
                .into_iter()
                .find(|d| d.serial == serial)
                .ok_or("requested Android serial is not connected")?
        } else {
            // An unavailable phone must not make an unrelated emulator the
            // implicit target. Count every discovered transport, not only ready ones.
            if devices.len() > 1 {
                return Err("multiple Android devices connected; select serial explicitly".into());
            }
            devices.into_iter().next().ok_or(
                "no authorized Android device; approve the device's own USB debugging prompt",
            )?
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
        this.ensure_display_until(deadline)?;
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
    fn text_until(&self, command: &str, deadline: Deadline) -> Result<String> {
        let output =
            String::from_utf8(shell(self.address, &self.serial, command, deadline)?.success()?)
                .map_err(|_| "Android text response is not UTF-8".to_string())?;
        deadline.check()?;
        Ok(output)
    }
    fn ensure_display_until(&self, deadline: Deadline) -> Result<()> {
        let ids = self.text_until("cmd display get-displays --ids-only", deadline)?;
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
        let deadline = Deadline::operation();
        self.ensure_display_until(deadline)?;
        let result = shell(self.address, &self.serial, command, deadline)?.checked_effect();
        deadline.check()?;
        result
    }
    pub fn status(&self) -> Result<String> {
        self.status_until(Deadline::operation())
    }
    fn status_until(&self, deadline: Deadline) -> Result<String> {
        self.ensure_display_until(deadline)?;
        let displays = self.text_until("cmd display get-displays", deadline)?;
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
        let deadline = Deadline::operation();
        self.ensure_display_until(deadline)?;
        let dump = self.text_until("dumpsys window windows", deadline)?;
        let focus_dump = self.text_until("dumpsys window displays", deadline).ok();
        deadline.check()?;
        let focus = focus_dump
            .as_deref()
            .map(|dump| display_focus(dump, self.display))
            .unwrap_or(Focus::Unknown);
        let windows = scoped_windows(&dump, self.display, focus);
        let (xml, note) = if self.mode == Mode::Foreground {
            match self.text_until("uiautomator dump --compressed /dev/tty", deadline) {
                Ok(s) if s.contains("<hierarchy") => {
                    let start = s
                        .find("<?xml")
                        .or_else(|| s.find("<hierarchy"))
                        .unwrap_or(0);
                    let end = s.rfind("</hierarchy>").map(|i| i + 12).unwrap_or(s.len());
                    match s.get(start..end) {
                        Some(xml) => (Some(xml.to_owned()), "uiautomator"),
                        None => (
                            None,
                            "uiautomator XML malformed; scoped native window metadata returned",
                        ),
                    }
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
        deadline.check()?;
        let result = format!(
            "{{\"display\":{},\"mode\":{},\"windows\":[{}],\"focusKnown\":{},\"focusSource\":{},\"accessibilityXml\":{},\"observation\":{}}}",
            self.display,
            quote(self.mode.name()),
            windows.join(","),
            !matches!(focus, Focus::Unknown),
            if matches!(focus, Focus::Unknown) {
                "null"
            } else {
                "\"dumpsys-window-displays\""
            },
            xml.map(|s| quote(&s)).unwrap_or("null".into()),
            quote(note)
        );
        deadline.check()?;
        Ok(result)
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
        let code = key_code(key)?;
        self.effect(&format!("input -d {} keyevent {code}", self.display))
    }
    pub fn type_text(&self, text: &str) -> Result<()> {
        validate_text(text)?;
        let encoded = text.replace(' ', "%s");
        self.effect(&format!(
            "input -d {} text {}",
            self.display,
            shell_quote(&encoded)?
        ))
    }
    pub fn start_app(&self, component: &str) -> Result<()> {
        validate_component(component)?;
        self.effect(&format!(
            "am start --display {} -n {}",
            self.display,
            shell_quote(component)?
        ))
    }
    /// Captures the matching SurfaceFlinger display, whose ID is distinct from
    /// the logical ID used by input and activity launch.
    pub fn screenshot(&self, path: &Path) -> Result<String> {
        let deadline = Deadline::operation();
        self.ensure_display_until(deadline)?;
        let displays = self.text_until("cmd display get-displays", deadline)?;
        let line = display_line(&displays, self.display)?;
        let query = if virtual_display(line) {
            "dumpsys SurfaceFlinger --displays"
        } else {
            "dumpsys SurfaceFlinger --display-id"
        };
        let surfaces = self.text_until(query, deadline)?;
        let id = surface_for(line, &displays, &surfaces)?;
        let png = shell(
            self.address,
            &self.serial,
            &format!("screencap -p -d {id}"),
            deadline,
        )?
        .success()?;
        deadline.check()?;
        if png.len() < 24 || png[..8] != *b"\x89PNG\r\n\x1a\n" {
            return Err("Android capture did not return PNG for the selected display".into());
        }
        let width = u32::from_be_bytes(png[16..20].try_into().map_err(|_| "PNG width")?);
        let height = u32::from_be_bytes(png[20..24].try_into().map_err(|_| "PNG height")?);
        if width == 0 || height == 0 || width > 16_384 || height > 16_384 {
            return Err("Android PNG dimensions exceed limits".into());
        }
        deadline.check()?;
        std::fs::write(path, &png).map_err(|e| format!("write Android screenshot: {e}"))?;
        // A late filesystem completion is reported without deleting the caller's file.
        deadline.check()?;
        Ok(format!(
            "{{\"path\":{},\"mimeType\":\"image/png\",\"width\":{width},\"height\":{height},\"display\":{},\"surfaceDisplay\":{},\"mode\":{}}}",
            quote(&path.to_string_lossy()),
            self.display,
            quote(&id.to_string()),
            quote(self.mode.name())
        ))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus<'a> {
    Unknown,
    None,
    Window(&'a str),
}
fn window_token(text: &str) -> Option<&str> {
    let (_, tail) = text.split_once("Window{")?;
    let token = tail.split_whitespace().next()?;
    if !token.is_empty() && token.bytes().all(|b| b.is_ascii_hexdigit()) && tail.contains('}') {
        Some(token)
    } else {
        None
    }
}
fn display_focus(dump: &str, display: u32) -> Focus<'_> {
    let mut current = None;
    let mut sections = 0;
    let mut focus = None;
    for raw in dump.lines() {
        let line = raw.trim();
        if line.starts_with("Display:") {
            current = line
                .strip_prefix("Display: mDisplayId=")
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|id| id.parse::<u32>().ok());
            if current == Some(display) {
                sections += 1;
                if sections > 1 {
                    return Focus::Unknown;
                }
            }
        } else if current == Some(display)
            && let Some(value) = line.strip_prefix("mCurrentFocus=")
        {
            if focus.is_some() {
                return Focus::Unknown;
            }
            let value = value.trim();
            focus = Some(if value == "null" {
                Focus::None
            } else if let Some(token) = window_token(value) {
                Focus::Window(token)
            } else {
                Focus::Unknown
            });
        }
    }
    focus.unwrap_or(Focus::Unknown)
}
fn scoped_windows(dump: &str, display: u32, focus: Focus<'_>) -> Vec<String> {
    let marker = format!("mDisplayId={display}");
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
        let focused = match focus {
            Focus::Unknown => "null",
            Focus::None => "false",
            Focus::Window(expected) => match window_token(first) {
                Some(actual) if actual == expected => "true",
                Some(_) => "false",
                None => "null",
            },
        };
        windows.push(format!(
            "{{\"window\":{},\"visible\":{visible},\"focused\":{focused},\"display\":{display}}}",
            quote(first)
        ));
        if windows.len() >= 4096 {
            break;
        }
    }
    windows
}
/// Validates shell text without connecting to an Android device.
pub(crate) fn validate_text(text: &str) -> Result<()> {
    // Shell input synthesizes hardware key events; it is not a Unicode IME.
    if text.len() > 4096
        || !text.bytes().all(|b| b == b' ' || b.is_ascii_graphic())
        || text.contains("%s")
    {
        return Err("Android shell text supports up to 4096 printable ASCII bytes without literal %s; use an authorized IME for Unicode".into());
    }
    Ok(())
}
fn key_code(key: &str) -> Result<&str> {
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
        return Err("Android key must be a known alias, numeric keycode or KEYCODE_NAME".into());
    }
    Ok(code)
}
/// Validates a key argument without injecting an event.
pub(crate) fn validate_key(key: &str) -> Result<()> {
    key_code(key).map(|_| ())
}
/// Validates the flattened package/activity form without launching an app.
pub(crate) fn validate_component(component: &str) -> Result<()> {
    let valid_parts = component
        .split_once('/')
        .is_some_and(|(package, activity)| {
            !package.is_empty() && !activity.is_empty() && !activity.contains('/')
        });
    if component.len() > 512
        || !valid_parts
        || !component
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'/' | b'$'))
    {
        return Err("Android app must be a package/activity component".into());
    }
    Ok(())
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
fn virtual_display(line: &str) -> bool {
    line.contains(", type VIRTUAL,") && line.contains("uniqueId \"virtual:")
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
        return Err(
            "selected physical display has no matching capture ID; no fallback selected".into(),
        );
    }
    if !virtual_display(line) {
        return Err(
            "logical display has no supported explicit physical or virtual identity".into(),
        );
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
    let mut current: Option<Option<u64>> = None;
    let mut fields = 0;
    let mut matched = None;
    for raw in surfaces.lines() {
        let value = raw.trim();
        if let Some(id) = value.strip_prefix("Virtual Display ") {
            current = Some(id.parse::<u64>().ok());
            fields = 0;
        } else if value.starts_with("Display ") || (!value.is_empty() && raw == raw.trim_start()) {
            current = None;
            fields = 0;
        } else if let Some(id) = current
            && let Some(value) = value.strip_prefix("name=")
        {
            fields += 1;
            if fields > 1 {
                return Err("SurfaceFlinger virtual display name metadata is ambiguous".into());
            }
            let candidate = value
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'));
            if candidate == Some(name) {
                let id = id.ok_or("invalid SurfaceFlinger virtual capture ID")?;
                if matched.replace(id).is_some() {
                    return Err("SurfaceFlinger display name is ambiguous".into());
                }
            }
        }
    }
    matched.ok_or("selected virtual display has no unambiguous SurfaceFlinger capture ID".into())
}
/// Validate explicit mobile-browser arguments without connecting to a device.
pub(crate) fn validate_browser_options(serial: &str, socket: Option<&str>) -> Result<()> {
    if serial.is_empty()
        || serial.len() > 256
        || !serial.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(
            "Android browser requires an explicit nonempty serial of at most 256 ASCII bytes"
                .into(),
        );
    }
    if socket.is_some_and(|name| !chrome_socket_name(name)) {
        return Err("Android browser socket must be a Chrome DevTools abstract socket name".into());
    }
    Ok(())
}

fn chrome_socket_name(name: &str) -> bool {
    if name == "chrome_devtools_remote" {
        return true;
    }
    name.len() <= 107
        && name
            .strip_prefix("chrome_devtools_remote_")
            .is_some_and(|suffix| {
                !suffix.is_empty()
                    && suffix.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
                    })
            })
}

/// Observe listening Chrome sockets only; no page target is enumerated.
/// ADB's documented transport/localabstract services carry the later tunnel:
/// https://android.googlesource.com/platform/packages/modules/adb/+/refs/heads/main/docs/dev/services.md
fn browser_sockets_until(
    address: SocketAddr,
    serial: &str,
    deadline: Deadline,
) -> Result<Vec<String>> {
    validate_browser_options(serial, None)?;
    let selected = inventory(address, deadline)?
        .into_iter()
        .find(|device| device.serial == serial)
        .ok_or("requested Android serial is not connected")?;
    if selected.state != "device" {
        return Err(format!("Android device state is {}", selected.state));
    }
    let bytes = shell(address, serial, "cat /proc/net/unix", deadline)?.success()?;
    if bytes.len() > 1024 * 1024 {
        return Err("Android socket inventory exceeds 1 MiB".into());
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| "Android socket inventory is not UTF-8")?;
    let mut sockets = Vec::new();
    for line in text.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        // /proc/net/unix: Num RefCount Protocol Flags Type St Inode Path.
        // Only listening SOCK_STREAM abstract sockets are eligible.
        if fields.len() != 8
            || u32::from_str_radix(fields[3], 16)
                .ok()
                .is_none_or(|flags| flags & 0x10000 == 0)
            || fields[4] != "0001"
            || fields[5] != "01"
        {
            continue;
        }
        let Some(name) = fields[7].strip_prefix('@') else {
            continue;
        };
        if chrome_socket_name(name) && !sockets.iter().any(|other| other == name) {
            if sockets.len() == 64 {
                return Err("Android Chrome socket inventory exceeds 64 candidates".into());
            }
            sockets.push(name.to_owned());
        }
    }
    sockets.sort();
    deadline.check()?;
    Ok(sockets)
}

/// A selected device/socket, without an ADB forward or a listening host port.
/// Every opened stream belongs to the caller and closes with that caller.
pub(crate) struct DevToolsTunnel {
    address: SocketAddr,
    serial: String,
    socket: String,
}

impl DevToolsTunnel {
    pub(crate) fn connect(serial: &str, socket: Option<&str>) -> Result<Self> {
        Self::connect_at(endpoint()?, serial, socket)
    }

    pub(crate) fn connect_at(
        address: SocketAddr,
        serial: &str,
        socket: Option<&str>,
    ) -> Result<Self> {
        validate_browser_options(serial, socket)?;
        let sockets = browser_sockets_until(address, serial, Deadline::operation())?;
        let selected = if let Some(socket) = socket {
            sockets
                .into_iter()
                .find(|candidate| candidate == socket)
                .ok_or("requested Chrome DevTools socket was not observed on the selected device")?
        } else {
            if sockets.is_empty() {
                return Err("no Chrome DevTools socket on the selected device; Chrome must already be running; WebView-only debugging is unsupported".into());
            }
            if sockets.len() != 1 {
                return Err("multiple Chrome DevTools sockets; select socket explicitly".into());
            }
            sockets
                .into_iter()
                .next()
                .ok_or("missing Chrome DevTools socket")?
        };
        Ok(Self {
            address,
            serial: serial.to_owned(),
            socket: selected,
        })
    }

    pub(crate) fn open(&self) -> Result<TcpStream> {
        let deadline = Deadline::operation();
        let mut stream = connect_socket(self.address, deadline)?;
        request(
            &mut stream,
            &format!("host:transport:{}", self.serial),
            deadline,
        )?;
        request(
            &mut stream,
            &format!("localabstract:{}", self.socket),
            deadline,
        )?;
        Ok(stream)
    }
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
        // Field layout and Virtual Display section follow the API 36 device fixture.
        let displays = "Display id 0: DisplayInfo{\"built-in\", type INTERNAL, uniqueId \"local:123\"}\nDisplay id 8: DisplayInfo{\"zuse-fixture\", type VIRTUAL, uniqueId \"virtual:best.lmm.zuse.fixture,10410,zuse-fixture,5\", layerStack 8}\n";
        let physical = "Display 123 (HWC display 0): displayName=\"\"\n";
        let virtuals = "Display 123\n    name=\"zuse-fixture\"\nVirtual Display 11529215047770986250\n    name=\"zuse-fixture\"\n    powerMode=On\n    optimizationPolicy=optimizeForPerformance\n";
        assert_eq!(
            surface_for(display_line(displays, 0).unwrap(), displays, physical).unwrap(),
            123
        );
        assert_eq!(
            surface_for(display_line(displays, 8).unwrap(), displays, virtuals).unwrap(),
            11529215047770986250
        );
        assert!(surface_for(display_line(displays, 8).unwrap(), displays, physical).is_err());
        assert!(
            surface_for(
                display_line(displays, 0).unwrap(),
                displays,
                "Display 1230 (HWC display 0): displayName=\"built-in\"\n"
            )
            .is_err()
        );
        assert!(display_line(displays, 3).is_err());
    }
    #[test]
    fn virtual_capture_rejects_ambiguous_and_prefix_names() {
        let line =
            "Display id 8: DisplayInfo{\"zuse\", type VIRTUAL, uniqueId \"virtual:fixture\"}";
        assert!(surface_for(line, line, "Virtual Display 100\n    name=\"zuse-other\"\n").is_err());
        assert!(surface_for(line, line, "Display 100\n    name=\"zuse\"\n").is_err());
        assert!(surface_for(line, line, "Virtual Display invalid\n    name=\"zuse\"\n").is_err());
        assert!(
            surface_for(
                line,
                line,
                "Virtual Display 100\n    name=\"zuse\"\nVirtual Display 101\n    name=\"zuse\"\n"
            )
            .is_err()
        );
        assert!(
            surface_for(
                line,
                line,
                "Virtual Display 100\n    name=\"zuse\"\n    name=\"zuse-other\"\n"
            )
            .is_err()
        );
        let repeated = format!(
            "{line}\nDisplay id 9: DisplayInfo{{\"zuse\", type VIRTUAL, uniqueId \"virtual:other\"}}"
        );
        assert!(surface_for(line, &repeated, "Virtual Display 100\n    name=\"zuse\"\n").is_err());
        assert_eq!(surface_for(line, line, "Virtual Display 100\n    name=\"zuse-other\"\nVirtual Display 101\n    name=\"zuse\"\n").unwrap(), 101);
    }
    #[test]
    fn focus_is_scoped_by_display_and_window_token() {
        let windows = "WINDOW MANAGER WINDOWS\n  Window #0 Window{aaaa u0 com.other/.Activity}:\n    mDisplayId=0 taskId=1\n    isOnScreen=true\n  Window #1 Window{bbbb u0 best.lmm.zuse.fixture/.MainActivity}:\n    mDisplayId=8 taskId=2\n    isVisible=true\n  Window #2 Window{cccc u0 best.lmm.zuse.fixture/.OtherActivity}:\n    mDisplayId=8 taskId=3\n    isVisible=true\n";
        let displays = "WINDOW MANAGER DISPLAY CONTENTS\n  Display: mDisplayId=0 (organized)\n    mCurrentFocus=Window{aaaa u0 com.other/.Activity}\n  Display: mDisplayId=8\n    mCurrentFocus=Window{bbbb u0 best.lmm.zuse.fixture/.MainActivity}\n";
        let selected = scoped_windows(windows, 8, display_focus(displays, 8));
        assert_eq!(selected.len(), 2);
        assert!(selected[0].contains("\"focused\":true"));
        assert!(selected[1].contains("\"focused\":false"));
        assert!(!selected.iter().any(|window| window.contains("com.other")));
        // Real fixture had no display-8 focus and a focused display-0 Activity.
        let actual = "  Display: mDisplayId=8\n  mCurrentFocus=null\n  Display: mDisplayId=0 (organized)\n  mCurrentFocus=Window{5c5af9 u0 best.lmm.zuse.fixture/best.lmm.zuse.fixture.MainActivity}\n";
        assert_eq!(display_focus(actual, 8), Focus::None);
        assert_eq!(display_focus(actual, 0), Focus::Window("5c5af9"));
    }
    #[test]
    fn missing_or_ambiguous_focus_is_unknown() {
        let window =
            "  Window #0 Window{abc u0 fixture/.Activity}:\n    mDisplayId=8\n    isVisible=true\n";
        for dump in [
            "",
            "mCurrentFocus=Window{abc u0 fixture/.Activity}\n",
            "Display: mDisplayId=0\nmCurrentFocus=Window{abc u0 fixture/.Activity}\n",
            "Display: mDisplayId=8\nmCurrentFocus=unrecognized\n",
            "Display: mDisplayId=8\nmCurrentFocus=null\nmCurrentFocus=Window{abc u0 fixture/.Activity}\n",
            "Display: mDisplayId=8\nmCurrentFocus=null\nDisplay: mDisplayId=8\nmCurrentFocus=null\n",
        ] {
            let focus = display_focus(dump, 8);
            assert_eq!(focus, Focus::Unknown, "{dump}");
            assert!(scoped_windows(window, 8, focus)[0].contains("\"focused\":null"));
        }
        let focus = display_focus("Display: mDisplayId=8\nmCurrentFocus=null\n", 8);
        assert!(scoped_windows(window, 8, focus)[0].contains("\"focused\":false"));
    }
    #[test]
    fn deadline_includes_handshake_and_partial_payload_reads() {
        use std::{net::TcpListener, thread};
        fn receive(stream: &mut TcpStream) {
            let size = read_length(stream, Deadline::after(Duration::from_secs(2))).unwrap();
            let mut bytes = vec![0; size];
            stream.read_exact(&mut bytes).unwrap();
        }
        for slow_handshake in [true, false] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                receive(&mut stream);
                if slow_handshake {
                    for byte in b"OKAY" {
                        if stream.write_all(&[*byte]).is_err() {
                            return;
                        }
                        thread::sleep(Duration::from_millis(70));
                    }
                    return;
                }
                stream.write_all(b"OKAY").unwrap();
                receive(&mut stream);
                stream.write_all(b"OKAY").unwrap();
                let mut closed = [0; 5];
                stream.read_exact(&mut closed).unwrap();
                stream.write_all(&[1, 8, 0, 0, 0]).unwrap();
                for byte in b"abcdefgh" {
                    if stream.write_all(&[*byte]).is_err() {
                        return;
                    }
                    thread::sleep(Duration::from_millis(40));
                }
                let _ = stream.write_all(&[3, 1, 0, 0, 0, 0]);
            });
            let error = shell(
                address,
                "fixture",
                "fixture-read-only",
                Deadline::after(Duration::from_millis(160)),
            )
            .err()
            .expect("slow fragments must not renew the operation deadline");
            server.join().unwrap();
            assert!(error.contains("deadline"), "{error}");
        }
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
            let len = read_length(s, Deadline::after(Duration::from_secs(2))).unwrap();
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
                    let list = b"fixture\tdevice model:Zuse\nother\toffline\n";
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
    fn zero_exit_does_not_hide_command_errors() {
        for message in [
            "Error: input injection failed\n",
            "java.lang.SecurityException: permission denied\n",
            "Unknown command: input\n",
        ] {
            for on_stderr in [true, false] {
                let output = Output {
                    stdout: if on_stderr {
                        Vec::new()
                    } else {
                        message.as_bytes().to_vec()
                    },
                    stderr: if on_stderr {
                        message.as_bytes().to_vec()
                    } else {
                        Vec::new()
                    },
                    exit: 0,
                };
                assert!(output.checked_effect().is_err(), "{message}");
            }
        }
        assert!(
            Output {
                stdout: b"Starting: Intent {}\n".to_vec(),
                stderr: b"Warning: Activity already running\n".to_vec(),
                exit: 0,
            }
            .checked_effect()
            .is_ok()
        );
        let error = Output {
            stdout: b"device detail".to_vec(),
            stderr: b"command failed".to_vec(),
            exit: 2,
        }
        .checked_effect()
        .unwrap_err();
        assert!(error.contains("exited 2"));
        assert!(error.contains("command failed"));
    }

    #[test]
    fn unavailable_peer_still_requires_explicit_serial() {
        use std::{net::TcpListener, thread};
        for state in ["offline", "unauthorized"] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = thread::spawn(move || {
                let (mut s, _) = listener.accept().unwrap();
                s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                s.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
                let len = read_length(&mut s, Deadline::after(Duration::from_secs(2))).unwrap();
                let mut service = vec![0; len];
                s.read_exact(&mut service).unwrap();
                assert_eq!(service, b"host:devices-l");
                let list = format!("phone\t{state}\nemulator-5554\tdevice model:OtherProject\n");
                s.write_all(b"OKAY").unwrap();
                s.write_all(format!("{:04x}", list.len()).as_bytes())
                    .unwrap();
                s.write_all(list.as_bytes()).unwrap();
            });
            let error = Android::connect_at(address, None, "foreground", 0)
                .err()
                .expect("ambiguous inventory must fail");
            server.join().unwrap();
            assert!(error.contains("select serial explicitly"), "{error}");
        }
    }
    #[test]
    fn rejects_remote_adb_server() {
        assert!(connect_socket("192.0.2.1:5037".parse().unwrap(), Deadline::operation()).is_err());
    }
}
