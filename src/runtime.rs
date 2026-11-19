//! One executor shared by embedded callers, CLI and MCP.
use crate::{
    accessibility,
    android::Android,
    browser::Browser,
    device_io,
    human::{Queue, Request},
    integrations::{self, ProcessManager},
    ios::{self, Ios},
    json::{self, quote},
    language::{Call, Parser, Value},
    motion::{Curve, Motion, Point, Rng},
    platform::Desktop,
    serial::{self, SerialPort},
    usb::{self, UsbDevice},
};
use std::{
    path::Path,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub dry_run: bool,
    pub read_only: bool,
    pub max_steps: usize,
    pub max_wait_ms: u64,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            dry_run: false,
            read_only: false,
            max_steps: 256,
            max_wait_ms: 60_000,
        }
    }
}

#[derive(Clone, Copy)]
enum Type {
    Text,
    Integer,
    Number,
    Bool,
}
use Type::{Bool as B, Integer as I, Number as N, Text as T};
type Param = (&'static str, Type, bool);
struct Spec {
    name: &'static str,
    args: &'static [Param],
    read_only: bool,
}
macro_rules! spec { ($name:literal, $read:literal, [$($arg:literal:$ty:ident:$required:literal),* $(,)?]) => { Spec{name:$name,args:&[$(($arg,$ty,$required)),*],read_only:$read} }; }
static SPECS: &[Spec] = &[
    spec!("system.capabilities", true, []),
    spec!("mouse.position", true, []),
    spec!("mouse.move",false,["x":I:true,"y":I:true,"duration":I:false,"steps":I:false,"curve":T:false,"jitter":N:false,"seed":I:false]),
    spec!("mouse.click",false,["button":T:false,"count":I:false,"interval":I:false]),
    spec!("mouse.down",false,["button":T:false]),
    spec!("mouse.up",false,["button":T:false]),
    spec!("mouse.scroll",false,["dx":I:false,"dy":I:false]),
    spec!("keyboard.type",false,["text":T:true,"window":T:false]),
    spec!("keyboard.key",false,["key":T:true,"window":T:false]),
    spec!("window.list", true, []),
    spec!("wine.windows", true, []),
    spec!("window.focus",false,["id":T:true]),
    spec!("screen.capture",true,["path":T:true,"window":T:false]),
    spec!("window.capture",true,["id":T:true,"path":T:true]),
    spec!("accessibility.tree",true,["limit":I:false,"depth":I:false]),
    spec!("screen.text",true,["ocr":B:false,"limit":I:false,"depth":I:false]),
    spec!("wait",true,["ms":I:false,"min":I:false,"max":I:false,"seed":I:false]),
    spec!("browser.launch",false,["headless":B:false]),
    spec!("browser.connect",false,["endpoint":T:true]),
    spec!("browser.tabs", true, []),
    spec!("browser.viewport",false,["width":I:true,"height":I:true]),
    spec!("browser.select",false,["id":T:true]),
    spec!("browser.navigate",false,["url":T:true]),
    spec!("browser.snapshot", true, []),
    spec!("browser.text", true, []),
    spec!("browser.evaluate",false,["expression":T:true]),
    spec!("browser.click",false,["selector":T:true]),
    spec!("browser.fill",false,["selector":T:true,"text":T:true]),
    spec!("browser.key",false,["key":T:true]),
    spec!("browser.capture",true,["path":T:true]),
    spec!("browser.close", false, []),
    spec!("human.request",false,["kind":T:true,"message":T:true,"url":T:false,"window":T:false,"timeout":I:false]),
    spec!("human.pending", true, []),
    spec!("notify",false,["title":T:false,"message":T:true]),
    spec!("harness.list", true, []),
    spec!("harness.start",false,["name":T:true,"args":T:false]),
    spec!("process.start",false,["program":T:true,"args":T:false]),
    spec!("process.list", true, []),
    spec!("process.read",true,["id":I:true]),
    spec!("harness.read",true,["id":I:true]),
    spec!("process.write",false,["id":I:true,"text":T:true]),
    spec!("harness.send",false,["id":I:true,"text":T:true]),
    spec!("process.stop",false,["id":I:true]),
    spec!("harness.stop",false,["id":I:true]),
    spec!("ide.open",false,["path":T:true,"line":I:false,"column":I:false]),
    spec!("ide.diff",false,["left":T:true,"right":T:true]),
    spec!("ide.extensions", true, []),
    spec!("omarchy.status", true, []),
    spec!("omarchy.workspaces", true, []),
    spec!("omarchy.workspace",false,["id":T:true]),
    spec!("omarchy.themes", true, []),
    spec!("omarchy.theme",false,["name":T:true]),
    spec!("accessibility.action",false,["path":T:true,"index":I:false]),
    spec!("accessibility.focus",false,["path":T:true]),
    spec!("accessibility.set_text",false,["path":T:true,"text":T:true]),
    spec!("android.devices", true, []),
    spec!("android.connect",true,["serial":T:false,"mode":T:false,"display":I:false]),
    spec!("android.status", true, []),
    spec!("android.snapshot", true, []),
    spec!("android.capture",true,["path":T:true]),
    spec!("android.tap",false,["x":I:true,"y":I:true]),
    spec!("android.swipe",false,["x1":I:true,"y1":I:true,"x2":I:true,"y2":I:true,"duration":I:false]),
    spec!("android.key",false,["key":T:true]),
    spec!("android.type",false,["text":T:true]),
    spec!("android.start",false,["component":T:true]),
    spec!("android.browser.sockets",true,["serial":T:true]),
    spec!("browser.connect_android",false,["serial":T:true,"socket":T:false]),
    spec!("ios.connect",true,["endpoint":T:true,"session":T:false]),
    spec!("ios.status", true, []),
    spec!("ios.screen", true, []),
    spec!("ios.snapshot", true, []),
    spec!("ios.source",true,["format":T:false,"raw":B:false]),
    spec!("ios.capture",true,["path":T:true]),
    spec!("ios.session_start",false,["bundle":T:false,"replace":B:false]),
    spec!("ios.session_end", false, []),
    spec!("ios.tap",false,["x":N:true,"y":N:true]),
    spec!("ios.swipe",false,["x1":N:true,"y1":N:true,"x2":N:true,"y2":N:true,"duration":I:false]),
    spec!("ios.type",false,["text":T:true]),
    spec!("ios.key",false,["key":T:true]),
    spec!("ios.launch",false,["bundle":T:true]),
    spec!("ios.open_url",false,["url":T:true]),
    spec!("serial.ports", true, []),
    spec!("serial.open",false,["path":T:true,"baud":I:false]),
    spec!("serial.status", true, []),
    spec!("serial.read",true,["length":I:false,"timeout":I:false]),
    spec!("serial.write",false,["text":T:false,"base64":T:false,"timeout":I:false]),
    spec!("serial.close", false, []),
    spec!("usb.devices", true, []),
    spec!("usb.open",false,["bus":I:true,"address":I:true,"vendor_id":I:true,"product_id":I:true,"interface":I:false]),
    spec!("usb.status", true, []),
    spec!("usb.bulk_read",false,["endpoint":I:true,"length":I:false,"timeout":I:false]),
    spec!("usb.bulk_write",false,["endpoint":I:true,"base64":T:true,"timeout":I:false]),
    spec!("usb.interrupt_read",false,["endpoint":I:true,"length":I:false,"timeout":I:false]),
    spec!("usb.interrupt_write",false,["endpoint":I:true,"base64":T:true,"timeout":I:false]),
    spec!("usb.control_read",false,["request_type":I:true,"request":I:true,"value":I:false,"index":I:false,"length":I:false,"timeout":I:false]),
    spec!("usb.control_write",false,["request_type":I:true,"request":I:true,"value":I:false,"index":I:false,"base64":T:true,"timeout":I:false]),
    spec!("usb.close", false, []),
];

pub fn schema() -> String {
    let commands: Vec<_> = SPECS
        .iter()
        .map(|spec| {
            let parameters: Vec<_> = spec
                .args
                .iter()
                .map(|(name, kind, required)| {
                    format!(
                        "{{\"name\":{},\"type\":{},\"required\":{required}}}",
                        quote(name),
                        quote(match kind {
                            T => "text",
                            I => "integer",
                            N => "number",
                            B => "boolean",
                        })
                    )
                })
                .collect();
            format!(
                "{{\"name\":{},\"read_only\":{},\"parameters\":[{}]}}",
                quote(spec.name),
                spec.read_only,
                parameters.join(",")
            )
        })
        .collect();
    format!(
        "{{\"version\":{},\"syntax\":\"name(key=value); newline or semicolon separates calls\",\"commands\":[{}]}}",
        quote(crate::VERSION),
        commands.join(",")
    )
}

fn text(call: &Call<'_>, name: &str, default: &str) -> Result<String, String> {
    match call.get(name) {
        None => Ok(default.into()),
        Some(value) => {
            let mut buffer = vec![0; value.decoded_len().map_err(|e| format!("{e:?}"))?];
            Ok(value
                .decode_text(&mut buffer)
                .map_err(|e| format!("{e:?}"))?
                .to_owned())
        }
    }
}
fn integer(call: &Call<'_>, name: &str, default: i64) -> i64 {
    call.get(name).and_then(Value::as_i64).unwrap_or(default)
}
fn number(call: &Call<'_>, name: &str, default: f64) -> f64 {
    call.get(name).and_then(Value::as_f64).unwrap_or(default)
}
fn boolean(call: &Call<'_>, name: &str, default: bool) -> bool {
    call.get(name).and_then(Value::as_bool).unwrap_or(default)
}
fn range(call: &Call<'_>, name: &str, default: i64, min: i64, max: i64) -> Result<i64, String> {
    let value = integer(call, name, default);
    if value < min || value > max {
        Err(format!("{}: {name} must be {min}..{max}", call.name))
    } else {
        Ok(value)
    }
}

fn device_payload(call: &Call<'_>, allow_text: bool) -> Result<Vec<u8>, String> {
    let has_text = call.get("text").is_some();
    let has_base64 = call.get("base64").is_some();
    if has_text == has_base64 || (has_text && !allow_text) {
        return Err(
            "device write requires exactly one of text or base64 (USB requires base64)".into(),
        );
    }
    let bytes = if has_text {
        text(call, "text", "")?.into_bytes()
    } else {
        device_io::decode_base64(&text(call, "base64", "")?)?
    };
    if bytes.len() > device_io::MAX_BYTES {
        return Err("device payload exceeds 65536 bytes".into());
    }
    Ok(bytes)
}

fn validate<'a>(source: &'a str, options: Options) -> Result<Vec<Call<'a>>, String> {
    let mut calls = Vec::new();
    let mut wait_budget = 0u64;
    for parsed in Parser::new(source) {
        let call = parsed.map_err(|e| e.to_string())?;
        if calls.len() >= options.max_steps {
            return Err(format!("step limit exceeded ({})", options.max_steps));
        }
        let spec = SPECS
            .iter()
            .find(|s| s.name == call.name)
            .ok_or_else(|| format!("line {}: unknown operation {}", call.span.line, call.name))?;
        if options.read_only && !spec.read_only {
            return Err(format!("{} is blocked in read-only mode", call.name));
        }
        for arg in call.args() {
            let (_, kind, _) = spec
                .args
                .iter()
                .find(|a| a.0 == arg.name)
                .ok_or_else(|| format!("{}: unknown argument {}", call.name, arg.name))?;
            let valid = match kind {
                T => arg.value.as_text().is_some(),
                I => arg.value.as_i64().is_some(),
                N => arg.value.as_f64().is_some_and(f64::is_finite),
                B => arg.value.as_bool().is_some(),
            };
            if !valid {
                return Err(format!(
                    "{}: invalid value type for {}",
                    call.name, arg.name
                ));
            }
            if matches!(kind, T) {
                let decoded = text(&call, arg.name, "")?;
                if decoded.contains('\0') {
                    return Err(format!("{}: {} contains a NUL byte", call.name, arg.name));
                }
            }
        }
        for (name, _, required) in spec.args {
            if *required && call.get(name).is_none() {
                return Err(format!("{} requires {name}", call.name));
            }
        }
        match call.name {
            "mouse.move" => {
                range(&call, "x", 0, i32::MIN as i64, i32::MAX as i64)?;
                range(&call, "y", 0, i32::MIN as i64, i32::MAX as i64)?;
                let duration = range(&call, "duration", 0, 0, 60_000)?;
                let steps = range(&call, "steps", (duration / 8).clamp(1, 4096), 1, 65536)?;
                if (duration == 0 && steps != 1) || (duration > 0 && steps > duration) {
                    return Err("mouse.move: steps must fit duration in milliseconds".into());
                }
                if Curve::parse(&text(&call, "curve", "smooth")?).is_none() {
                    return Err("unknown motion curve".into());
                }
                if !(0.0..=1000.0).contains(&number(&call, "jitter", 0.0)) {
                    return Err("jitter must be 0..1000 pixels".into());
                }
                wait_budget = wait_budget.saturating_add(duration as u64);
            }
            "mouse.click" | "mouse.down" | "mouse.up" => {
                if !["left", "right", "middle", "back", "forward", "1", "2", "3"]
                    .contains(&text(&call, "button", "left")?.as_str())
                {
                    return Err("unknown mouse button".into());
                }
                let count = range(&call, "count", 1, 1, 10)?;
                let interval = range(&call, "interval", 80, 0, 1000)?;
                wait_budget = wait_budget.saturating_add(((count - 1) * interval) as u64);
            }
            "mouse.scroll" => {
                range(&call, "dx", 0, -1000, 1000)?;
                range(&call, "dy", 0, -1000, 1000)?;
            }
            "wait" => {
                if call.get("ms").is_some()
                    && (call.get("min").is_some() || call.get("max").is_some())
                {
                    return Err("wait uses either ms or min/max".into());
                }
                let low = range(&call, "min", integer(&call, "ms", 0), 0, 60_000)?;
                let high = range(&call, "max", integer(&call, "ms", low), 0, 60_000)?;
                range(&call, "ms", 0, 0, 60_000)?;
                if low > high {
                    return Err("wait min exceeds max".into());
                }
                wait_budget = wait_budget.saturating_add(high as u64);
            }

            "android.browser.sockets" | "browser.connect_android" => {
                let socket = text(&call, "socket", "")?;
                crate::android::validate_browser_options(
                    &text(&call, "serial", "")?,
                    call.get("socket").map(|_| socket.as_str()),
                )?;
            }
            "ios.connect" => {
                ios::validate_endpoint(&text(&call, "endpoint", "")?)?;
                if call.get("session").is_some() {
                    ios::validate_session(&text(&call, "session", "")?)?;
                }
            }
            "ios.session_start" => {
                if call.get("bundle").is_some() {
                    ios::validate_bundle(&text(&call, "bundle", "")?)?;
                }
            }
            "ios.launch" => ios::validate_bundle(&text(&call, "bundle", "")?)?,
            "ios.open_url" => ios::validate_url(&text(&call, "url", "")?)?,
            "ios.source" => ios::validate_source(
                &text(&call, "format", "json")?,
                boolean(&call, "raw", false),
            )?,
            "ios.capture" => ios::validate_output_path(Path::new(&text(&call, "path", "")?))?,
            "ios.type" => ios::validate_text(&text(&call, "text", "")?)?,
            "ios.key" => ios::validate_key(&text(&call, "key", "")?)?,
            "ios.tap" => ios::validate_point(number(&call, "x", 0.0), number(&call, "y", 0.0))?,
            "ios.swipe" => {
                let duration = range(&call, "duration", 300, 1, 10_000)? as u64;
                ios::validate_swipe(
                    number(&call, "x1", 0.0),
                    number(&call, "y1", 0.0),
                    number(&call, "x2", 0.0),
                    number(&call, "y2", 0.0),
                    duration,
                )?;
                wait_budget = wait_budget.saturating_add(duration);
            }
            "serial.open" => {
                serial::validate_path(&text(&call, "path", "")?)?;
                serial::validate_baud(range(&call, "baud", 115200, 1, u32::MAX as i64)? as u32)?;
            }
            "serial.read" | "serial.write" => {
                let timeout = range(&call, "timeout", 1000, 1, 30_000)?;
                wait_budget = wait_budget.saturating_add(timeout as u64);
                if call.name == "serial.read" {
                    range(&call, "length", 4096, 1, device_io::MAX_BYTES as i64)?;
                } else {
                    device_payload(&call, true)?;
                }
            }
            "usb.open" => {
                range(&call, "bus", 0, 0, 255)?;
                range(&call, "address", 0, 1, 127)?;
                range(&call, "vendor_id", 0, 0, 65535)?;
                range(&call, "product_id", 0, 0, 65535)?;
                range(&call, "interface", 0, 0, 255)?;
            }
            "usb.bulk_read" | "usb.bulk_write" | "usb.interrupt_read" | "usb.interrupt_write" => {
                let input = call.name.ends_with("_read");
                let endpoint = range(&call, "endpoint", 0, 0, 255)? as u8;
                usb::validate_endpoint(endpoint, input)?;
                let timeout = range(&call, "timeout", 1000, 1, 30_000)?;
                wait_budget = wait_budget.saturating_add(timeout as u64);
                if input {
                    range(&call, "length", 4096, 1, device_io::MAX_BYTES as i64)?;
                } else {
                    device_payload(&call, false)?;
                }
            }
            "usb.control_read" | "usb.control_write" => {
                let input = call.name.ends_with("_read");
                let request_type = range(&call, "request_type", 0, 0, 255)? as u8;
                if (request_type & 0x80 != 0) != input {
                    return Err("USB control direction does not match the operation".into());
                }
                range(&call, "request", 0, 0, 255)?;
                range(&call, "value", 0, 0, 65535)?;
                range(&call, "index", 0, 0, 65535)?;
                let timeout = range(&call, "timeout", 1000, 1, 30_000)?;
                wait_budget = wait_budget.saturating_add(timeout as u64);
                if input {
                    range(&call, "length", 64, 0, 65535)?;
                } else {
                    usb::validate_control_length(device_payload(&call, false)?.len())?;
                }
            }
            "android.connect" => {
                let mode = text(&call, "mode", "foreground")?;
                let display = range(&call, "display", 0, 0, u32::MAX as i64)?;
                if !["foreground", "preempt", "background"].contains(&mode.as_str()) {
                    return Err("android mode must be foreground, preempt or background".into());
                }
                if mode == "background" && display == 0 {
                    return Err("background Android requires an explicit nonzero display".into());
                }
                if mode != "background" && display != 0 {
                    return Err("foreground Android uses display 0".into());
                }
            }
            "android.type" => crate::android::validate_text(&text(&call, "text", "")?)?,
            "android.key" => crate::android::validate_key(&text(&call, "key", "")?)?,
            "android.start" => crate::android::validate_component(&text(&call, "component", "")?)?,
            "android.tap" | "android.swipe" => {
                for key in ["x", "y", "x1", "y1", "x2", "y2"] {
                    if call.get(key).is_some() {
                        range(&call, key, 0, 0, 65535)?;
                    }
                }
                let duration = range(&call, "duration", 300, 1, 10000)?;
                if call.name == "android.swipe" {
                    wait_budget = wait_budget.saturating_add(duration as u64);
                }
            }
            "accessibility.action" => {
                range(&call, "index", 0, 0, 128)?;
            }
            "human.request" => {
                let kind = text(&call, "kind", "")?;
                if !crate::human::KINDS.contains(&kind.as_str()) {
                    return Err("unknown human request kind".into());
                }
                if text(&call, "message", "")?.is_empty()
                    || text(&call, "message", "")?.len() > 4096
                {
                    return Err("human request message must contain 1..4096 bytes".into());
                }
                if kind == "password" {
                    let target = text(&call, "window", "")?;
                    if target.is_empty() || target == "active" {
                        return Err("password requires a stable explicit target window ID".into());
                    }
                }
                let url = text(&call, "url", "")?;
                if !url.is_empty() && !url.starts_with("http://") && !url.starts_with("https://") {
                    return Err("human login URL must use http or https".into());
                }
                range(&call, "timeout", 300, 1, 86400)?;
            }
            "accessibility.tree" | "screen.text" => {
                range(&call, "limit", 256, 1, 4096)?;
                range(&call, "depth", 8, 1, 32)?;
            }
            "browser.viewport" => {
                range(&call, "width", 0, 1, 8192)?;
                range(&call, "height", 0, 1, 8192)?;
            }
            "ide.open" => {
                range(&call, "line", 1, 1, i32::MAX as i64)?;
                range(&call, "column", 1, 1, i32::MAX as i64)?;
            }
            "keyboard.key" => {
                crate::platform::split_chord(&text(&call, "key", "")?)?;
            }
            "process.start" | "harness.start" => {
                let program = text(
                    &call,
                    if call.name == "process.start" {
                        "program"
                    } else {
                        "name"
                    },
                    "",
                )?;
                if call.name == "harness.start"
                    && !["codex", "claude", "pi", "opencode", "ssmcp", "aider"]
                        .contains(&program.as_str())
                {
                    return Err("unknown harness; use process.start for other executables".into());
                }
                integrations::validate_start(&program, &text(&call, "args", "")?)?;
            }
            name if (name.starts_with("process.") || name.starts_with("harness."))
                && call.get("id").is_some() =>
            {
                range(&call, "id", 0, 1, i64::MAX)?;
            }
            _ => {}
        }
        if wait_budget > options.max_wait_ms {
            return Err(format!(
                "program wait budget exceeds {}ms",
                options.max_wait_ms
            ));
        }
        calls.push(call);
    }
    Ok(calls)
}

pub fn check(source: &str, options: Options) -> Result<String, String> {
    let calls = validate(source, options)?;
    let list: Vec<_> = calls
        .iter()
        .map(|c| {
            format!(
                "{{\"operation\":{},\"line\":{}}}",
                quote(c.name),
                c.span.line
            )
        })
        .collect();
    Ok(format!(
        "{{\"status\":\"valid\",\"steps\":{},\"plan\":[{}]}}",
        calls.len(),
        list.join(",")
    ))
}

#[derive(Default)]
pub struct Runtime {
    desktop: Option<Desktop>,
    browser: Option<Browser>,
    android: Option<Android>,
    ios: Option<Ios>,
    serial: Option<SerialPort>,
    usb: Option<UsbDevice>,
    processes: ProcessManager,
}
impl Runtime {
    pub fn new() -> Self {
        Self::default()
    }
    fn desktop(&mut self) -> Result<&mut Desktop, String> {
        if self.desktop.is_none() {
            self.desktop = Some(Desktop::connect()?);
        }
        self.desktop
            .as_mut()
            .ok_or_else(|| "desktop is unavailable".into())
    }
    fn browser(&mut self) -> Result<&mut Browser, String> {
        self.browser.as_mut().ok_or_else(|| {
            "use browser.launch, browser.connect or browser.connect_android first".into()
        })
    }

    fn android(&self) -> Result<&Android, String> {
        self.android
            .as_ref()
            .ok_or_else(|| "use android.connect first".into())
    }

    fn ios(&mut self) -> Result<&mut Ios, String> {
        self.ios
            .as_mut()
            .ok_or_else(|| "use ios.connect first".into())
    }
    fn serial(&mut self) -> Result<&mut SerialPort, String> {
        self.serial
            .as_mut()
            .ok_or_else(|| "use serial.open first".into())
    }
    fn usb(&self) -> Result<&UsbDevice, String> {
        self.usb.as_ref().ok_or_else(|| "use usb.open first".into())
    }

    pub fn capabilities(&mut self) -> String {
        let desktop = match self.desktop() {
            Ok(d) => d.capabilities(),
            Err(e) => format!("{{\"available\":false,\"reason\":{}}}", quote(&e)),
        };
        format!(
            "{{\"version\":{},\"platform\":{},\"architecture\":{},\"desktop\":{},\"browser_connected\":{},\"ios_connected\":{},\"serial_connected\":{},\"usb_connected\":{},\"mobile\":{{\"android_browser\":\"native ADB tunnel, owned Chrome tab in existing profile\",\"ios\":\"explicit local WebDriverAgent endpoint; real device not validated\"}},\"device_io\":{{\"serial\":\"native 8N1, bounded I/O\",\"usb\":\"system libusb; explicit VID/PID/interface; no kernel-driver detach\"}},\"harnesses\":{},\"transports\":[\"stdio\",\"http\",\"unix-socket\"],\"core\":{{\"no_std\":true,\"allocations\":0}},\"limits\":{{\"steps\":256,\"script_bytes\":1048576,\"wait_ms\":60000}}}}",
            quote(crate::VERSION),
            quote(std::env::consts::OS),
            quote(std::env::consts::ARCH),
            desktop,
            self.browser.is_some(),
            self.ios.is_some(),
            self.serial.is_some(),
            self.usb.is_some(),
            integrations::installed_harnesses()
        )
    }

    pub fn resume(&mut self, id: &str, options: Options) -> Result<String, String> {
        if options.dry_run {
            return Err("resume consumes a human request; use human.pending to inspect it".into());
        }
        let queue = Queue::new()?;
        let saved = queue.options(id)?;
        let options = Options {
            dry_run: false,
            read_only: options.read_only || saved.read_only,
            max_steps: options.max_steps.min(saved.max_steps),
            max_wait_ms: options.max_wait_ms.min(saved.max_wait_ms),
        };
        // Validate the saved plan before claiming its single-use approval.
        validate(&queue.continuation(id)?, options)?;
        let source = queue.take(id)?;
        self.execute(&source, options)
    }

    pub fn execute(&mut self, source: &str, options: Options) -> Result<String, String> {
        if options.dry_run {
            return check(source, options);
        }
        let calls = validate(source, options)?;
        let mut results = Vec::new();
        let mut bytes = 0;
        for (index, call) in calls.iter().enumerate() {
            if call.name == "human.request" {
                let next = calls.get(index + 1).map_or(source.len(), |c| c.span.start);
                let kind = text(call, "kind", "")?;
                let message = text(call, "message", "")?;
                let url = text(call, "url", "")?;
                let window = text(call, "window", "")?;
                let pending = Queue::new()?.request_with_options(
                    Request {
                        kind: &kind,
                        message: &message,
                        url: &url,
                        window: &window,
                        timeout_secs: integer(call, "timeout", 300) as u64,
                    },
                    &source[next..],
                    true,
                    options,
                )?;
                return Ok(format!(
                    "{{\"status\":\"waiting_for_human\",\"completed\":{index},\"handoff\":{pending},\"results\":[{}]}}",
                    results.join(",")
                ));
            }
            let began = Instant::now();
            match self.step(call) {
                Ok(value) => {
                    bytes += value.len();
                    if bytes > 1024 * 1024 {
                        return Ok(format!(
                            "{{\"status\":\"failed\",\"failed_step\":{index},\"error\":\"result budget exhausted\",\"completed\":{},\"results\":[{}]}}",
                            index + 1,
                            results.join(",")
                        ));
                    }
                    results.push(format!(
                        "{{\"step\":{index},\"operation\":{},\"elapsed_us\":{},\"value\":{value}}}",
                        quote(call.name),
                        began.elapsed().as_micros()
                    ));
                }
                Err(error) => {
                    let error_code = if error.starts_with("unsupported_state:") {
                        "unsupported_state"
                    } else if error.starts_with("unsupported_character:") {
                        "unsupported_character"
                    } else {
                        "execution_error"
                    };
                    return Ok(format!(
                        "{{\"status\":\"failed\",\"failed_step\":{index},\"operation\":{},\"error\":{},\"error_code\":{},\"completed\":{index},\"results\":[{}]}}",
                        quote(call.name),
                        quote(&error),
                        quote(error_code),
                        results.join(",")
                    ));
                }
            }
        }
        Ok(format!(
            "{{\"status\":\"completed\",\"completed\":{},\"results\":[{}]}}",
            calls.len(),
            results.join(",")
        ))
    }

    fn step(&mut self, c: &Call<'_>) -> Result<String, String> {
        let done = || Ok("{\"ok\":true}".into());
        match c.name {
            "system.capabilities" => Ok(self.capabilities()),
            "mouse.position" => {
                let (x, y) = self.desktop()?.position()?;
                Ok(format!("{{\"x\":{x},\"y\":{y}}}"))
            }
            "mouse.move" => {
                let duration = integer(c, "duration", 0) as u32;
                let steps = integer(c, "steps", ((duration / 8) as i64).clamp(1, 4096)) as u32;
                let curve = Curve::parse(&text(c, "curve", "smooth")?).ok_or("unknown curve")?;
                let (x, y) = self.desktop()?.position()?;
                let target = Point {
                    x: integer(c, "x", 0) as f64,
                    y: integer(c, "y", 0) as f64,
                };
                let motion = Motion::new(
                    Point {
                        x: x as f64,
                        y: y as f64,
                    },
                    target,
                    duration,
                    steps,
                    curve,
                    number(c, "jitter", 0.0),
                    integer(c, "seed", 1) as u64,
                )
                .map_err(|e| format!("{e:?}"))?;
                let start = Instant::now();
                for sample in motion {
                    let desired = Duration::from_millis(sample.at_ms as u64);
                    if let Some(delay) = desired.checked_sub(start.elapsed()) {
                        std::thread::sleep(delay);
                    }
                    self.desktop()?
                        .move_to(sample.point.x.round() as i32, sample.point.y.round() as i32)?;
                }
                Ok(format!(
                    "{{\"x\":{},\"y\":{},\"samples\":{}}}",
                    target.x,
                    target.y,
                    steps + 1
                ))
            }
            "mouse.click" | "mouse.down" | "mouse.up" => {
                let button = text(c, "button", "left")?;
                let button = match button.as_str() {
                    "1" => "left",
                    "2" => "middle",
                    "3" => "right",
                    other => other,
                };
                let action = c.name.rsplit('.').next().unwrap_or("click");
                for i in 0..integer(c, "count", 1) {
                    if i > 0 {
                        std::thread::sleep(
                            Duration::from_millis(integer(c, "interval", 80) as u64),
                        );
                    }
                    self.desktop()?.button(button, action)?;
                }
                Ok("{\"ok\":true,\"delivery\":\"submitted\",\"application_confirmation_required\":true}".into())
            }
            "mouse.scroll" => {
                self.desktop()?
                    .scroll(integer(c, "dx", 0) as i32, integer(c, "dy", 0) as i32)?;
                Ok("{\"ok\":true,\"delivery\":\"submitted\",\"application_confirmation_required\":true}".into())
            }
            "keyboard.type" => {
                let value = text(c, "text", "")?;
                let window = text(c, "window", "")?;
                if window.is_empty() {
                    self.desktop()?.type_text(&value)?;
                } else {
                    self.desktop()?.type_into(&window, &value)?;
                }
                Ok(format!(
                    "{{\"typed_bytes\":{},\"submitted_bytes\":{},\"delivery\":\"submitted\",\"application_confirmation_required\":true}}",
                    value.len(),
                    value.len()
                ))
            }
            "keyboard.key" => {
                let key = text(c, "key", "")?;
                let window = text(c, "window", "")?;
                if window.is_empty() {
                    self.desktop()?.key(&key)?;
                } else {
                    self.desktop()?.key_into(&window, &key)?;
                }
                Ok("{\"ok\":true,\"delivery\":\"submitted\",\"application_confirmation_required\":true}".into())
            }
            "window.list" => self.desktop()?.windows(),
            "wine.windows" => {
                let windows = self.desktop()?.windows()?;
                json::with_value(&windows, |v| {
                    let list: Vec<_> = v
                        .elements()
                        .filter(|w| w.get("wine").and_then(|x| x.boolean()) == Some(true))
                        .map(|w| w.raw().to_owned())
                        .collect();
                    Ok(format!("[{}]", list.join(",")))
                })
            }
            "window.focus" => {
                self.desktop()?.focus(&text(c, "id", "")?)?;
                done()
            }
            "screen.capture" | "window.capture" => {
                let window = text(
                    c,
                    if c.name == "window.capture" {
                        "id"
                    } else {
                        "window"
                    },
                    "",
                )?;
                self.desktop()?.screenshot(
                    Path::new(&text(c, "path", "")?),
                    if window.is_empty() {
                        None
                    } else {
                        Some(&window)
                    },
                )
            }
            "accessibility.tree" => accessibility::snapshot(
                integer(c, "limit", 256) as usize,
                integer(c, "depth", 8) as usize,
            ),
            "screen.text" => {
                if !boolean(c, "ocr", false) {
                    let value = accessibility::text(
                        integer(c, "limit", 256) as usize,
                        integer(c, "depth", 8) as usize,
                    )?;
                    return Ok(format!(
                        "{{\"source\":\"accessibility\",\"text\":{}}}",
                        quote(&value)
                    ));
                }
                let capture = TemporaryScreenshot::new()?;
                self.desktop()?.screenshot(&capture.path, None)?;
                let value = integrations::ocr(&capture.path)?;
                Ok(format!("{{\"source\":\"ocr\",\"text\":{}}}", quote(&value)))
            }
            "wait" => {
                let min = integer(c, "min", integer(c, "ms", 0)) as u64;
                let max = integer(c, "max", integer(c, "ms", min as i64)) as u64;
                let delay = Rng::new(integer(c, "seed", 1) as u64)
                    .range_u64(min, max)
                    .map_err(|e| format!("{e:?}"))?;
                std::thread::sleep(Duration::from_millis(delay));
                Ok(format!("{{\"waited_ms\":{delay}}}"))
            }
            "browser.launch" => {
                if self.browser.is_some() {
                    return Err(
                        "browser already connected; close it before launching another".into(),
                    );
                }
                self.browser = Some(Browser::launch(boolean(c, "headless", true))?);
                self.browser()?.tabs()
            }
            "browser.connect" => {
                if self.browser.is_some() {
                    return Err("browser already connected".into());
                }
                self.browser = Some(Browser::connect(&text(c, "endpoint", "")?)?);
                self.browser()?.tabs()
            }
            "browser.select" => self.browser()?.select(&text(c, "id", "")?),
            "browser.tabs" => self.browser()?.tabs(),
            "browser.viewport" => self.browser()?.viewport(
                integer(c, "width", 0) as u32,
                integer(c, "height", 0) as u32,
            ),
            "browser.navigate" => self.browser()?.navigate(&text(c, "url", "")?),
            "browser.snapshot" => self.browser()?.snapshot(),
            "browser.text" => self.browser()?.text(),
            "browser.evaluate" => self.browser()?.evaluate(&text(c, "expression", "")?),
            "browser.click" => self.browser()?.click(&text(c, "selector", "")?),
            "browser.fill" => self
                .browser()?
                .fill(&text(c, "selector", "")?, &text(c, "text", "")?),
            "browser.key" => self.browser()?.key(&text(c, "key", "")?),
            "browser.capture" => self.browser()?.screenshot(&text(c, "path", "")?),
            "browser.close" => {
                if let Some(mut browser) = self.browser.take() {
                    browser.close()?;
                }
                done()
            }
            "human.pending" => Queue::new()?.list(),
            "notify" => {
                crate::human::notify_user(&text(c, "title", "zuse")?, &text(c, "message", "")?)?;
                done()
            }
            "harness.list" => Ok(integrations::installed_harnesses()),
            "process.start" => self
                .processes
                .start(&text(c, "program", "")?, &text(c, "args", "")?),
            "harness.start" => {
                let name = text(c, "name", "")?;
                if !["codex", "claude", "pi", "opencode", "ssmcp", "aider"].contains(&name.as_str())
                {
                    return Err("unknown harness; use process.start for other executables".into());
                }
                self.processes.start(&name, &text(c, "args", "")?)
            }
            "process.list" => Ok(self.processes.list()),
            "process.read" | "harness.read" => self.processes.read(integer(c, "id", 0) as u64),
            "process.write" | "harness.send" => self
                .processes
                .write(integer(c, "id", 0) as u64, &text(c, "text", "")?),
            "process.stop" | "harness.stop" => self.processes.stop(integer(c, "id", 0) as u64),
            "ide.open" => integrations::ide_open(
                &text(c, "path", "")?,
                integer(c, "line", 1) as u32,
                integer(c, "column", 1) as u32,
            ),
            "ide.diff" => integrations::ide_diff(&text(c, "left", "")?, &text(c, "right", "")?),
            "ide.extensions" => integrations::ide_extensions(),
            "omarchy.status" => integrations::omarchy_status(),
            "omarchy.workspaces" => integrations::omarchy_workspaces(),
            "omarchy.workspace" => integrations::omarchy_workspace(&text(c, "id", "")?),
            "omarchy.themes" => integrations::omarchy_themes(),
            "omarchy.theme" => integrations::omarchy_theme(&text(c, "name", "")?),

            "accessibility.action" => {
                accessibility::action(&text(c, "path", "")?, integer(c, "index", 0) as usize)
            }
            "accessibility.focus" => accessibility::focus(&text(c, "path", "")?),
            "accessibility.set_text" => {
                accessibility::set_text(&text(c, "path", "")?, &text(c, "text", "")?)
            }

            "android.browser.sockets" => Android::browser_sockets(&text(c, "serial", "")?),
            "browser.connect_android" => {
                self.browser = None;
                let socket = text(c, "socket", "")?;
                self.browser = Some(Browser::connect_android(
                    &text(c, "serial", "")?,
                    c.get("socket").map(|_| socket.as_str()),
                )?);
                Ok("{\"connected\":true,\"backend\":\"adb-native-tunnel\",\"scope\":\"owned_chrome_tab\",\"isolated_profile\":false}".into())
            }
            "ios.connect" => {
                self.ios = None;
                let session = text(c, "session", "")?;
                let (ios, status) = Ios::connect_with_status(
                    &text(c, "endpoint", "")?,
                    c.get("session").map(|_| session.as_str()),
                )?;
                self.ios = Some(ios);
                Ok(status)
            }
            "ios.status" => self.ios()?.status(),
            "ios.screen" => self.ios()?.screen(),
            "ios.snapshot" => self.ios()?.snapshot(),
            "ios.source" => self
                .ios()?
                .source_with_options(&text(c, "format", "json")?, boolean(c, "raw", false)),
            "ios.capture" => self.ios()?.screenshot(Path::new(&text(c, "path", "")?)),
            "ios.session_start" => {
                let bundle = text(c, "bundle", "")?;
                self.ios()?.session_start(
                    c.get("bundle").map(|_| bundle.as_str()),
                    boolean(c, "replace", false),
                )
            }
            "ios.session_end" => self.ios()?.session_end(),
            "ios.tap" => self.ios()?.tap(number(c, "x", 0.0), number(c, "y", 0.0)),
            "ios.swipe" => self.ios()?.swipe(
                number(c, "x1", 0.0),
                number(c, "y1", 0.0),
                number(c, "x2", 0.0),
                number(c, "y2", 0.0),
                integer(c, "duration", 300) as u64,
            ),
            "ios.type" => self.ios()?.type_text(&text(c, "text", "")?),
            "ios.key" => self.ios()?.key(&text(c, "key", "")?),
            "ios.launch" => self.ios()?.launch(&text(c, "bundle", "")?),
            "ios.open_url" => self.ios()?.open_url(&text(c, "url", "")?),
            "serial.ports" => serial::ports(),
            "serial.open" => {
                if let Some(mut previous) = self.serial.take() {
                    previous.close()?;
                }
                let port =
                    SerialPort::open(&text(c, "path", "")?, integer(c, "baud", 115200) as u32)?;
                let status = port.status();
                self.serial = Some(port);
                Ok(status)
            }
            "serial.status" => Ok(self.serial()?.status()),
            "serial.read" => Ok(self
                .serial()?
                .read(
                    integer(c, "length", 4096) as usize,
                    integer(c, "timeout", 1000) as u32,
                )?
                .json()),
            "serial.write" => {
                let bytes = device_payload(c, true)?;
                let count = self
                    .serial()?
                    .write(&bytes, integer(c, "timeout", 1000) as u32)?;
                Ok(format!("{{\"bytes_written\":{count},\"complete\":true}}"))
            }
            "serial.close" => {
                if let Some(mut port) = self.serial.take() {
                    port.close()?;
                }
                done()
            }
            "usb.devices" => usb::devices(),
            "usb.open" => {
                if let Some(previous) = self.usb.take() {
                    previous.close()?;
                }
                let device = UsbDevice::open(
                    integer(c, "bus", 0) as u8,
                    integer(c, "address", 0) as u8,
                    integer(c, "vendor_id", 0) as u16,
                    integer(c, "product_id", 0) as u16,
                    integer(c, "interface", 0) as u8,
                )?;
                let status = device.status();
                self.usb = Some(device);
                Ok(status)
            }
            "usb.status" => Ok(self.usb()?.status()),
            "usb.bulk_read" | "usb.interrupt_read" => Ok(self
                .usb()?
                .read(
                    integer(c, "endpoint", 0) as u8,
                    integer(c, "length", 4096) as usize,
                    integer(c, "timeout", 1000) as u32,
                    c.name == "usb.interrupt_read",
                )?
                .json()),
            "usb.bulk_write" | "usb.interrupt_write" => {
                let bytes = device_payload(c, false)?;
                let count = self.usb()?.write(
                    integer(c, "endpoint", 0) as u8,
                    &bytes,
                    integer(c, "timeout", 1000) as u32,
                    c.name == "usb.interrupt_write",
                )?;
                Ok(format!("{{\"bytes_written\":{count},\"complete\":true}}"))
            }
            "usb.control_read" => Ok(self
                .usb()?
                .control_read(
                    integer(c, "request_type", 0) as u8,
                    integer(c, "request", 0) as u8,
                    integer(c, "value", 0) as u16,
                    integer(c, "index", 0) as u16,
                    integer(c, "length", 64) as usize,
                    integer(c, "timeout", 1000) as u32,
                )?
                .json()),
            "usb.control_write" => {
                let bytes = device_payload(c, false)?;
                let count = self.usb()?.control_write(
                    integer(c, "request_type", 0) as u8,
                    integer(c, "request", 0) as u8,
                    integer(c, "value", 0) as u16,
                    integer(c, "index", 0) as u16,
                    &bytes,
                    integer(c, "timeout", 1000) as u32,
                )?;
                Ok(format!("{{\"bytes_written\":{count},\"complete\":true}}"))
            }
            "usb.close" => {
                if let Some(device) = self.usb.take() {
                    device.close()?;
                }
                done()
            }

            "android.devices" => Android::devices(),
            "android.connect" => {
                // A failed retarget must not leave an earlier device selected.
                self.android = None;
                let serial = text(c, "serial", "")?;
                let (android, status) = Android::connect_with_status(
                    if serial.is_empty() {
                        None
                    } else {
                        Some(&serial)
                    },
                    &text(c, "mode", "foreground")?,
                    integer(c, "display", 0) as u32,
                )?;
                self.android = Some(android);
                Ok(status)
            }
            "android.status" => self.android()?.status(),
            "android.snapshot" => self.android()?.snapshot(),
            "android.capture" => self.android()?.screenshot(Path::new(&text(c, "path", "")?)),
            "android.tap" => {
                self.android()?
                    .tap(integer(c, "x", 0) as i32, integer(c, "y", 0) as i32)?;
                done()
            }
            "android.swipe" => {
                self.android()?.swipe(
                    integer(c, "x1", 0) as i32,
                    integer(c, "y1", 0) as i32,
                    integer(c, "x2", 0) as i32,
                    integer(c, "y2", 0) as i32,
                    integer(c, "duration", 300) as u32,
                )?;
                done()
            }
            "android.key" => {
                self.android()?.key(&text(c, "key", "")?)?;
                done()
            }
            "android.type" => {
                self.android()?.type_text(&text(c, "text", "")?)?;
                done()
            }
            "android.start" => {
                self.android()?.start_app(&text(c, "component", "")?)?;
                done()
            }
            _ => Err(format!("unsupported operation {}", c.name)),
        }
    }
}
struct TemporaryScreenshot {
    directory: std::path::PathBuf,
    path: std::path::PathBuf,
}
impl TemporaryScreenshot {
    fn new() -> Result<Self, String> {
        let directory = std::env::temp_dir().join(format!(
            "zuse-ocr-{}-{}",
            std::process::id(),
            SystemTimeId::next()
        ));
        std::fs::create_dir(&directory).map_err(|e| e.to_string())?;
        let capture = Self {
            path: directory.join("screen.png"),
            directory,
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&capture.directory, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        Ok(capture)
    }
}
impl Drop for TemporaryScreenshot {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir(&self.directory);
    }
}
struct SystemTimeId;
impl SystemTimeId {
    fn next() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    }
}
pub fn failed(output: &str) -> bool {
    json::with_value(output, |v| {
        Ok(v.get("status")
            .and_then(|s| s.string())
            .is_some_and(|s| s.matches("failed")))
    })
    .unwrap_or(true)
}
