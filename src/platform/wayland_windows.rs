//! Native window discovery and exact focus for Sway and niri.
use crate::json::{quote, with_value};
use std::{
    io::{Read, Write},
    os::unix::{fs::FileTypeExt, net::UnixStream},
    path::PathBuf,
    time::{Duration, Instant},
};
use zmcpio::json::{Kind, Value};

use super::Result;

const MAX_REPLY: usize = 2 * 1024 * 1024;
const SWAY_MAGIC: &[u8; 6] = b"i3-ipc";
const SWAY_COMMAND: u32 = 0;
const SWAY_TREE: u32 = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Region {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Region {
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Result<Self> {
        if !(1..=16_384).contains(&width)
            || !(1..=16_384).contains(&height)
            || x.checked_add_unsigned(width).is_none()
            || y.checked_add_unsigned(height).is_none()
        {
            return Err("invalid capture geometry".into());
        }
        Ok(Self {
            x,
            y,
            width,
            height,
        })
    }

    pub fn grim_argument(self) -> String {
        format!("{},{} {}x{}", self.x, self.y, self.width, self.height)
    }
}

pub(super) enum Windows {
    Sway(PathBuf),
    Niri(PathBuf),
}

struct Window {
    id: u64,
    title: String,
    class: String,
    pid: Option<u32>,
    focused: Option<bool>,
    visible: Option<bool>,
    region: Option<Region>,
}

impl Window {
    fn json(&self, backend: &str) -> String {
        let id = format!("{backend}:{}", self.id);
        let class = self.class.to_ascii_lowercase();
        let wine = class.ends_with(".exe") || class.contains("wine");
        let field = |value: Option<i64>| value.map_or("null".into(), |n| n.to_string());
        let focused = self
            .focused
            .map_or("null", |v| if v { "true" } else { "false" });
        format!(
            concat!(
                r#"{{"id":{},"title":{},"class":{},"pid":{},"x":{},"y":{},"width":{},"height":{},"#,
                r#""wine":{wine},"scale":null,"focused":{focused},"mapped":null}}"#
            ),
            quote(&id),
            quote(&self.title),
            quote(&self.class),
            field(self.pid.map(i64::from)),
            field(self.region.map(|r| i64::from(r.x))),
            field(self.region.map(|r| i64::from(r.y))),
            field(self.region.map(|r| i64::from(r.width))),
            field(self.region.map(|r| i64::from(r.height))),
            wine = wine,
            focused = focused,
        )
    }
}

impl Windows {
    pub fn discover() -> Result<Option<Self>> {
        let socket = |name| {
            std::env::var_os(name).map(PathBuf::from).filter(|path| {
                path.is_absolute() && path.metadata().is_ok_and(|m| m.file_type().is_socket())
            })
        };
        match (socket("SWAYSOCK"), socket("NIRI_SOCKET")) {
            (Some(path), None) => Ok(Some(Self::Sway(path))),
            (None, Some(path)) => Ok(Some(Self::Niri(path))),
            (None, None) => Ok(None),
            (Some(_), Some(_)) => Err(
                "ambiguous Wayland window backend: both SWAYSOCK and NIRI_SOCKET are set".into(),
            ),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Sway(_) => "sway",
            Self::Niri(_) => "niri",
        }
    }

    pub fn list(&self) -> Result<String> {
        let rows: Vec<_> = self
            .windows()?
            .into_iter()
            .map(|window| window.json(self.name()))
            .collect();
        Ok(format!("[{}]", rows.join(",")))
    }

    pub fn probe(&self) -> Result<()> {
        self.windows().map(|_| ())
    }

    pub fn focus(&self, id: &str) -> Result<()> {
        let target = self.target(id)?;
        if target.focused == Some(true) {
            return Ok(());
        }
        match self {
            Self::Sway(_) => {
                let reply = self.sway(SWAY_COMMAND, &format!("[con_id={}] focus", target.id))?;
                with_value(&reply, |root| {
                    let mut entries = root.elements();
                    if entries
                        .next()
                        .and_then(|v| v.get("success"))
                        .and_then(Value::boolean)
                        == Some(true)
                        && entries.next().is_none()
                    {
                        Ok(())
                    } else {
                        Err("Sway refused to focus the selected window".into())
                    }
                })?;
            }
            Self::Niri(_) => {
                let request = format!(r#"{{"Action":{{"FocusWindow":{{"id":{}}}}}}}"#, target.id);
                self.niri(&request, |response| {
                    if response
                        .string()
                        .is_some_and(|value| value.matches("Handled"))
                    {
                        Ok(())
                    } else {
                        Err("niri did not acknowledge window focus".into())
                    }
                })?;
            }
        }
        if self.target(id)?.focused != Some(true) {
            return Err("stale_target: compositor did not focus the selected window".into());
        }
        Ok(())
    }

    pub fn capture_region(&self, id: &str) -> Result<(String, Region)> {
        if matches!(self, Self::Niri(_)) {
            return Err(
                "unsupported_state: niri window screen geometry is unavailable; use screen.capture"
                    .into(),
            );
        }
        let window = self.target(id)?;
        if window.visible != Some(true) {
            return Err("unsupported_state: Sway target is not visible on the desktop".into());
        }
        let region = window
            .region
            .ok_or("Sway window has no valid capture geometry")?;
        Ok((format!("{}:{}", self.name(), window.id), region))
    }

    fn target(&self, id: &str) -> Result<Window> {
        let selected = if id == "active" {
            None
        } else {
            let digits = id
                .strip_prefix(self.name())
                .and_then(|suffix| suffix.strip_prefix(':'))
                .ok_or_else(|| format!("window ID must be {}:<number> or active", self.name()))?;
            Some(parse_id(digits).ok_or("invalid compositor window ID")?)
        };
        let mut matches = self
            .windows()?
            .into_iter()
            .filter(|window| selected.map_or(window.focused == Some(true), |id| id == window.id));
        let result = matches
            .next()
            .ok_or("stale_target: compositor window not found")?;
        if matches.next().is_some() {
            return Err("ambiguous compositor window identity".into());
        }
        Ok(result)
    }

    fn windows(&self) -> Result<Vec<Window>> {
        match self {
            Self::Sway(_) => with_value(&self.sway(SWAY_TREE, "")?, |root| {
                if !root
                    .get("type")
                    .and_then(Value::string)
                    .is_some_and(|s| s.matches("root"))
                {
                    return Err("invalid Sway window tree".into());
                }
                let mut windows = Vec::new();
                collect_sway_windows(root, &mut windows)?;
                Ok(windows)
            }),
            Self::Niri(_) => self.niri(r#""Windows""#, |root| {
                let windows = root
                    .get("Windows")
                    .filter(|value| value.kind() == Kind::Array)
                    .ok_or("invalid niri window list")?;
                windows
                    .elements()
                    .map(|node| {
                        // niri's per-column layout positions do not identify a screen rectangle.
                        window_fields(node, "title", node.get("app_id"), "is_focused")
                    })
                    .collect()
            }),
        }
    }

    fn sway(&self, kind: u32, payload: &str) -> Result<String> {
        let Self::Sway(path) = self else {
            unreachable!()
        };
        let mut ipc = Ipc::connect(path, "Sway")?;
        let mut request = Vec::from(*SWAY_MAGIC);
        request.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
        request.extend_from_slice(&kind.to_ne_bytes());
        request.extend_from_slice(payload.as_bytes());
        ipc.write(&request)?;
        let mut header = [0; 14];
        ipc.read_exact(&mut header)?;
        let size = u32::from_ne_bytes(header[6..10].try_into().unwrap()) as usize;
        if &header[..6] != SWAY_MAGIC || header[10..] != kind.to_ne_bytes() || size > MAX_REPLY {
            return Err("invalid or oversized Sway IPC header".into());
        }
        let mut bytes = vec![0; size];
        ipc.read_exact(&mut bytes)?;
        String::from_utf8(bytes).map_err(|e| format!("Sway IPC: {e}"))
    }

    fn niri<T>(&self, request: &str, parse: impl FnOnce(Value<'_>) -> Result<T>) -> Result<T> {
        let Self::Niri(path) = self else {
            unreachable!()
        };
        let mut ipc = Ipc::connect(path, "niri")?;
        ipc.write(format!("{request}\n").as_bytes())?;
        let mut bytes = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            let count = ipc.read(&mut buffer)?;
            if count == 0 {
                return Err("niri IPC response ended before newline".into());
            }
            let newline = buffer[..count].iter().position(|&byte| byte == b'\n');
            let end = newline.unwrap_or(count);
            if bytes.len() + end > MAX_REPLY {
                return Err("niri IPC response exceeds 2 MiB".into());
            }
            bytes.extend_from_slice(&buffer[..end]);
            if newline.is_some() {
                break;
            }
        }
        let reply = std::str::from_utf8(&bytes).map_err(|e| format!("niri IPC: {e}"))?;
        with_value(reply, |root| {
            let response = root.get("Ok").ok_or("niri rejected the IPC request")?;
            parse(response)
        })
    }
}

fn parse_id(text: &str) -> Option<u64> {
    if text.bytes().all(|b| b.is_ascii_digit()) {
        text.parse().ok().filter(|&id| id > 0)
    } else {
        None
    }
}

fn window_fields(
    node: Value<'_>,
    title: &str,
    class: Option<Value<'_>>,
    focused: &str,
) -> Result<Window> {
    let string = |value: Option<Value<'_>>| {
        value
            .and_then(Value::string)
            .map(|string| string.chars().collect())
            .unwrap_or_default()
    };
    Ok(Window {
        id: node
            .get("id")
            .and_then(|v| parse_id(v.raw()))
            .ok_or("invalid compositor window ID")?,
        title: string(node.get(title)),
        class: string(class),
        pid: node
            .get("pid")
            .and_then(|v| v.integer().ok())
            .and_then(|pid| u32::try_from(pid).ok())
            .filter(|&pid| pid > 0),
        focused: node.get(focused).and_then(Value::boolean),
        visible: None,
        region: None,
    })
}

fn collect_sway_windows(node: Value<'_>, windows: &mut Vec<Window>) -> Result<()> {
    // Only actual views have a shell, app_id, X11 window or pid.
    let has_string = |key| node.get(key).and_then(Value::string).is_some();
    let has_integer = |key| node.get(key).is_some_and(|v| v.integer().is_ok());
    if has_string("shell") || has_string("app_id") || has_integer("window") || has_integer("pid") {
        let class = node
            .get("app_id")
            .filter(|v| v.kind() == Kind::String)
            .or_else(|| node.get("window_properties").and_then(|v| v.get("class")));
        let mut window = window_fields(node, "name", class, "focused")?;
        window.visible = node.get("visible").and_then(Value::boolean);
        window.region = node.get("rect").and_then(|rect| {
            let number = |key| rect.get(key)?.integer().ok();
            Region::new(
                i32::try_from(number("x")?).ok()?,
                i32::try_from(number("y")?).ok()?,
                u32::try_from(number("width")?).ok()?,
                u32::try_from(number("height")?).ok()?,
            )
            .ok()
        });
        windows.push(window);
    }
    for key in ["nodes", "floating_nodes"] {
        if let Some(children) = node.get(key) {
            for child in children.elements() {
                collect_sway_windows(child, windows)?;
            }
        }
    }
    Ok(())
}

struct Ipc {
    socket: UnixStream,
    backend: &'static str,
    deadline: Instant,
}

impl Ipc {
    fn connect(path: &std::path::Path, backend: &'static str) -> Result<Self> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let socket = UnixStream::connect(path).map_err(|e| format!("{backend} IPC: {e}"))?;
        Ok(Self {
            socket,
            backend,
            deadline,
        })
    }

    fn remaining(&self) -> Result<Duration> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("{} IPC deadline reached", self.backend));
        }
        Ok(remaining)
    }

    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.socket
            .set_write_timeout(Some(self.remaining()?))
            .and_then(|()| self.socket.write_all(bytes))
            .map_err(|e| format!("{} IPC write: {e}", self.backend))
    }

    fn read(&mut self, bytes: &mut [u8]) -> Result<usize> {
        self.socket
            .set_read_timeout(Some(self.remaining()?))
            .and_then(|()| self.socket.read(bytes))
            .map_err(|e| format!("{} IPC read: {e}", self.backend))
    }

    fn read_exact(&mut self, mut bytes: &mut [u8]) -> Result<()> {
        while !bytes.is_empty() {
            let count = self.read(bytes)?;
            if count == 0 {
                return Err(format!("{} IPC response is truncated", self.backend));
            }
            bytes = &mut bytes[count..];
        }
        Ok(())
    }
}
