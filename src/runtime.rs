//! One executor shared by embedded callers, CLI and MCP.
use crate::{
    accessibility,
    android::Android,
    browser::Browser,
    cursor::{CursorColor, CursorShape, CursorStyle},
    device_io,
    drawing::{self, Anchor},
    human::{Queue, Request},
    integrations::{self, ProcessManager},
    ios::{self, Ios},
    json::{self, quote},
    language::{Call, Parser, Value},
    motion::{
        Curve, DEFAULT_MOVE_DURATION_MS, DEFAULT_MOVE_JITTER, FunctionalMotion, MAX_COORDINATE,
        Motion, PathOptions, Point, Rng,
    },
    mouse::{Mouse, MouseMode, MouseOptions},
    observation::{Cancellation, Condition, Observer, Query as ObservationQuery},
    platform::Desktop,
    pointer::{MoveOptions, MovePlan},
    script::{FunctionMetadata, ModuleSession, Prepared, ProgramMode},
    serial::{self, SerialPort},
    target::{self, Bounds, LocatedTarget, TargetQuery, TargetWindow, WindowTarget},
    touchpad::{self, Command as TouchpadCommand, Gesture, GestureOptions, GesturePlan},
    usb::{self, UsbDevice},
};
use std::{
    collections::BTreeMap,
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
mod plotter_control;
mod writing;

macro_rules! write_spec {
    ($name:literal, $read:literal, [$($arg:literal:$ty:ident:$required:literal),*]) => {
        spec!($name, $read, ["text":T:true,"font":T:false,"size":N:false,"advance":N:false,
            "line_height":N:false,"offset_x":N:false,"offset_y":N:false,"seed":I:false,
            "reference_units":N:false,"reference_distance":N:false,"x":N:false,"y":N:false,
            "area_width":N:false,"area_height":N:false,
            "margin_left":N:false,"margin_top":N:false,"margin_right":N:false,"margin_bottom":N:false,
            "offset_x_ratio":N:false,"offset_y_ratio":N:false,"tilt_degrees":N:false,"size_ratio":N:false,
            $($arg:$ty:$required),*])
    };
}

static SPECS: &[Spec] = &[
    write_spec!("write.preview", true, []),
    write_spec!("write.mouse", false, ["interval":I:false]),
    write_spec!("write.touchpad", false, ["width":I:true,"height":I:true,"interval":I:false]),
    write_spec!("write.touchscreen", false, ["width":I:true,"height":I:true,"interval":I:false]),
    write_spec!("write.gcode", true, ["pen_up":N:false,"pen_down":N:false,"feed":N:false,"travel_feed":N:false,"lift_feed":N:false,"swap_xy":B:false,"flip_x":B:false,"flip_y":B:false]),
    write_spec!("write.export", false, ["path":T:true,"pen_up":N:false,"pen_down":N:false,"feed":N:false,"travel_feed":N:false,"lift_feed":N:false,"swap_xy":B:false,"flip_x":B:false,"flip_y":B:false]),
    spec!("functions", true, ["namespace":T:false]),
    spec!("timer.start", true, []),
    spec!("timer.reset", true, []),
    spec!("timer.stop", true, []),
    spec!("timer.status", true, []),
    spec!("observe",true,["source":T:false,"selector":T:false,"scope":T:false,"watch":T:false]),
    spec!("watch",true,["source":T:false,"selector":T:false,"scope":T:false,"name":T:false]),
    spec!("watch.close",true,["name":T:false]),
    spec!("wait_until",true,["watch":T:false,"condition":T:false,"text":T:false,"role":T:false,"timeout":I:false]),
    spec!("watch_until_click",false,["source":T:false,"scope":T:false,"selector":T:true,"origin":T:false,"max_clicks":I:false,"timeout":I:false]),
    spec!("timeline.start",true,["name":T:false,"window":T:false,"columns":I:true,"rows":I:true,"enter":I:false,"exit":I:false]),
    spec!("timeline.sample",true,["name":T:false]),
    spec!("timeline.observe",true,["name":T:false]),
    spec!("timeline.wait_until",true,["name":T:false,"timeout":I:false,"interval":I:false]),
    spec!("timeline.render",true,["name":T:false,"path":T:true]),
    spec!("timeline.close",true,["name":T:false]),
    spec!("system.capabilities", true, []),
    spec!("touchpad.capabilities", true, []),
    spec!("touchpad.swipe",false,["dx":N:false,"dy":N:false,"fingers":I:false,"duration":I:false,"steps":I:false]),
    spec!("touchpad.pinch",false,["scale":N:true,"fingers":I:false,"duration":I:false,"steps":I:false]),
    spec!("touchpad.rotate",false,["degrees":N:true,"fingers":I:false,"duration":I:false,"steps":I:false]),
    spec!("touchpad.cancel", false, []),
    spec!("mouse.activate",false,["path":T:true,"pid":I:true,"name":T:true,"action":T:false]),
    spec!("mouse.use",false,["kind":T:false,"appearance":T:false,"color":T:false,"shape":T:false,"size":I:false,"glow":N:false,"name":T:false]),
    spec!("mouse.release", false, []),
    spec!("mouse.position", true, []),
    spec!("target.locate",true,["text":T:false,"template":T:false,"mode":T:false,"scales":T:false,"min_score":N:false,"min_margin":N:false,"role":T:false,"scope":T:false,"exact":B:false,"case_sensitive":B:false,"index":I:false,"window":T:false,"name":T:false]),
    spec!("anchor.set",true,["name":T:true,"window":T:true,"x":I:false,"y":I:false,"width":I:true,"height":I:true]),
    spec!("anchor.clear",true,["name":T:true]),
    spec!("mouse.click_at",false,["x":I:true,"y":I:true,"duration":I:false,"curve":T:false,"jitter":N:false,"seed":I:false,"button":T:false,"settle":I:false,"anchor":T:false]),
    spec!("mouse.stroke",false,["points":T:true,"duration":I:false,"settle":I:false,"button":T:false,"anchor":T:false]),
    spec!("draw.line",false,["x1":N:true,"y1":N:true,"x2":N:true,"y2":N:true,"duration":I:false,"settle":I:false,"button":T:false,"anchor":T:false]),
    spec!("draw.triangle",false,["x1":N:true,"y1":N:true,"x2":N:true,"y2":N:true,"x3":N:true,"y3":N:true,"duration":I:false,"settle":I:false,"button":T:false,"anchor":T:false]),
    spec!("draw.rect",false,["x":N:true,"y":N:true,"width":N:true,"height":N:true,"duration":I:false,"settle":I:false,"button":T:false,"anchor":T:false]),
    spec!("draw.circle",false,["x":N:true,"y":N:true,"radius":N:true,"steps":I:false,"duration":I:false,"settle":I:false,"button":T:false,"anchor":T:false]),
    spec!("draw.polygon",false,["points":T:true,"duration":I:false,"settle":I:false,"button":T:false,"anchor":T:false]),
    spec!("mouse.move",false,["x":I:true,"y":I:true,"duration":I:false,"steps":I:false,"curve":T:false,"jitter":N:false,"seed":I:false]),
    spec!("mouse.move_to",false,["text":T:true,"role":T:false,"scope":T:false,"exact":B:false,"case_sensitive":B:false,"index":I:false,"duration":I:false,"speed":N:false,"steps":I:false,"curve":T:false,"jitter":N:false,"speed_variation":N:false,"seed":I:false,"offset_x":N:false,"offset_y":N:false,"via":T:false]),
    spec!("mouse.circle",false,["x":I:true,"y":I:true,"radius":N:true,"duration":I:false,"steps":I:false,"curve":T:false,"jitter":N:false,"speed_variation":N:false,"seed":I:false]),
    spec!("mouse.click",false,["button":T:false,"count":I:false,"interval":I:false,"target":T:false]),
    spec!("mouse.down",false,["button":T:false]),
    spec!("mouse.up",false,["button":T:false]),
    spec!("mouse.scroll",false,["dx":I:false,"dy":I:false]),
    spec!("keyboard.type",false,["text":T:true,"window":T:false]),
    spec!("keyboard.key",false,["key":T:true,"window":T:false]),
    spec!("keyboard.hold",false,["key":T:true,"duration":I:true,"window":T:false,"allow_other_keys":B:false]),
    spec!("keyboard.state", true, []),
    spec!("keyboard.num_lock",false,["state":T:true]),
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
    spec!("browser.touch_gesture",false,["paths":T:true,"duration":I:false,"steps":I:false,"curve":T:false,"jitter":N:false,"seed":I:false,"speed_variation":N:false]),
    spec!("browser.click",false,["selector":T:true]),
    spec!("browser.fill",false,["selector":T:true,"text":T:true]),
    spec!("browser.key",false,["key":T:true]),
    spec!("browser.hold",false,["key":T:true,"duration":I:true]),
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
    spec!("accessibility.set_text",false,["path":T:true,"text":T:true,"expected_text":T:false,"expected_pid":I:false]),
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
    spec!("plotter.connect",false,["port":T:true,"baud":I:false,"timeout":I:false]),
    spec!("plotter.status",true,["timeout":I:false,"work":B:false,"offset":B:false]),
    spec!("plotter.bounds", true, []),
    spec!("plotter.clear_bounds", false, []),
    spec!("plotter.set_bounds",false,["x_min":N:true,"x_max":N:true,"y_min":N:true,"y_max":N:true,"z_min":N:true,"z_max":N:true,"wco_x":N:false,"wco_y":N:false,"wco_z":N:false,"expected_generation":I:false,"timeout":I:false]),
    spec!("plotter.disconnect", false, []),
    spec!("plotter.reset", false, ["timeout":I:false]),
    spec!("plotter.park", false, ["x":N:true,"y":N:true,"z":N:true,"feed":N:false,"timeout":I:false]),
    spec!("plotter.auto_bounds", false, ["timeout":I:false]),
    spec!("plotter.jog",false,["axis":T:true,"delta":N:true,"feed":N:false,"timeout":I:false]),
    spec!("plotter.pen",false,["z":N:true,"feed":N:false,"timeout":I:false]),
    spec!("plotter.send",false,["program":T:true,"require_bounds":B:false,"timeout":I:false]),
    spec!("plotter.check",false,["program":T:true,"timeout":I:false]),
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

fn observation_query(call: &Call<'_>) -> Result<ObservationQuery, String> {
    Ok(ObservationQuery {
        source: text(call, "source", "desktop")?,
        selector: text(call, "selector", "body")?,
        scope: text(call, "scope", "0")?,
    })
}
fn observation_condition(call: &Call<'_>) -> Result<Condition, String> {
    Ok(Condition {
        kind: text(call, "condition", "changed")?,
        text: call
            .get("text")
            .map(|_| text(call, "text", ""))
            .transpose()?,
        role: call
            .get("role")
            .map(|_| text(call, "role", ""))
            .transpose()?,
    })
}
fn click_watch_options(call: &Call<'_>) -> Result<crate::observation::ClickWatchOptions, String> {
    if text(call, "source", "browser")? != "browser" {
        return Err(
            "unsupported_state: watch_until_click currently requires the browser event backend"
                .into(),
        );
    }
    Ok(crate::observation::ClickWatchOptions {
        scope: text(call, "scope", "body")?,
        selector: text(call, "selector", "")?,
        expected_origin: call
            .get("origin")
            .map(|_| text(call, "origin", ""))
            .transpose()?,
        max_clicks: range(call, "max_clicks", 1, 1, 64)? as u32,
        timeout_ms: range(call, "timeout", 30_000, 1, 60_000)? as u64,
    })
}

#[cfg(feature = "vision")]
fn image_match_options(call: &Call<'_>) -> Result<crate::vision::MatchOptions, String> {
    let mode = match text(call, "mode", "edges")?.as_str() {
        "edges" => crate::vision::MatchMode::Edges,
        "gray" => crate::vision::MatchMode::Gray,
        _ => return Err("template mode must be edges or gray".into()),
    };
    let mut options = crate::vision::MatchOptions {
        mode,
        ..Default::default()
    };
    if call.get("scales").is_some() {
        options.scales = text(call, "scales", "")?
            .split(',')
            .map(|value| {
                value
                    .trim()
                    .parse::<f64>()
                    .map_err(|_| "scales must be comma-separated finite numbers".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
    }
    options.min_score = number(call, "min_score", 0.90);
    options.min_margin = number(call, "min_margin", 0.05);
    if options.scales.is_empty()
        || options.scales.len() > 16
        || options
            .scales
            .iter()
            .any(|s| !s.is_finite() || *s <= 0.0 || *s > 8.0)
        || !(0.0..=1.0).contains(&options.min_score)
        || !(0.0..=1.0).contains(&options.min_margin)
    {
        return Err("invalid template scales or confidence thresholds".into());
    }
    Ok(options)
}
fn touch_gesture(c: &Call<'_>) -> Result<crate::browser::touch::TouchGesture, String> {
    crate::browser::touch::TouchGesture::parse(
        &text(c, "paths", "")?,
        range(c, "duration", 200, 0, 10000)? as u64,
        range(c, "steps", 12, 1, 120)? as u32,
        Curve::parse(&text(c, "curve", "linear")?).ok_or("unknown motion curve")?,
        number(c, "jitter", 0.0),
        integer(c, "seed", 1) as u64,
        number(c, "speed_variation", 0.0),
    )
}

fn circle_options(c: &Call<'_>) -> Result<(f64, f64, f64, PathOptions), String> {
    let x = integer(c, "x", 0) as f64;
    let y = integer(c, "y", 0) as f64;
    let radius = number(c, "radius", 0.0);
    let jitter = number(c, "jitter", 0.0);
    let speed_variation = number(c, "speed_variation", 0.0);
    let duration = range(c, "duration", 1000, 1, 60_000)?;
    let steps = range(c, "steps", (duration / 8).clamp(1, 4096), 1, 4096)?;
    if steps > duration {
        return Err("mouse.circle: steps must fit duration in milliseconds".into());
    }
    if radius <= 0.0 || x.abs().max(y.abs()) + radius + jitter > MAX_COORDINATE {
        return Err("mouse.circle: radius and jitter must fit the coordinate range".into());
    }
    if !(0.0..=1000.0).contains(&jitter) || !(0.0..=1.0).contains(&speed_variation) {
        return Err("mouse.circle: jitter must be 0..1000 and speed_variation 0..1".into());
    }
    let easing = Curve::parse(&text(c, "curve", "smooth")?).ok_or("unknown motion curve")?;
    Ok((
        x,
        y,
        radius,
        PathOptions {
            duration_ms: duration as u32,
            steps: steps as u32,
            easing,
            jitter,
            speed_variation,
            seed: integer(c, "seed", 1) as u64,
        },
    ))
}

fn number(call: &Call<'_>, name: &str, default: f64) -> f64 {
    call.get(name).and_then(Value::as_f64).unwrap_or(default)
}
fn boolean(call: &Call<'_>, name: &str, default: bool) -> bool {
    call.get(name).and_then(Value::as_bool).unwrap_or(default)
}

fn touchpad_command(call: &Call<'_>) -> Result<TouchpadCommand, String> {
    let gesture = match call.name {
        "touchpad.capabilities" => return Ok(TouchpadCommand::Capabilities),
        "touchpad.cancel" => return Ok(TouchpadCommand::Cancel),
        "touchpad.swipe" => Gesture::Swipe {
            dx: number(call, "dx", 0.0),
            dy: number(call, "dy", 0.0),
        },
        "touchpad.pinch" => Gesture::Pinch {
            scale: number(call, "scale", 1.0),
        },
        "touchpad.rotate" => Gesture::Rotate {
            degrees: number(call, "degrees", 0.0),
        },
        _ => return Err("unknown touchpad operation".into()),
    };
    let options = GestureOptions {
        fingers: range(call, "fingers", 2, 2, 5)? as u8,
        duration_ms: range(call, "duration", 350, 0, 60_000)? as u32,
        steps: call
            .get("steps")
            .map(|_| range(call, "steps", 1, 1, 4096).map(|value| value as u32))
            .transpose()?,
    };
    GesturePlan::new(gesture, options).map(TouchpadCommand::Gesture)
}

fn require_touchpad_capabilities(calls: &[Call<'_>]) -> Result<(), String> {
    for call in calls {
        if call.name.starts_with("touchpad.") {
            touchpad_command(call)?
                .require_native()
                .map_err(|error| format!("{error} at {}:{}", call.span.line, call.span.column))?;
        }
    }
    Ok(())
}

fn mouse_options(call: &Call<'_>) -> Result<MouseOptions, String> {
    let mode = match text(call, "kind", "independent")?.as_str() {
        "independent" => MouseMode::Independent,
        "user" => MouseMode::User,
        "semantic" => MouseMode::Semantic,
        _ => return Err("mouse.use: kind must be independent, user or semantic".into()),
    };
    let appearance = match text(call, "appearance", "glow")?.as_str() {
        "glow" => Some(CursorStyle {
            color: CursorColor::parse(&text(call, "color", "#2FE2FF")?)?,
            shape: CursorShape::parse(&text(call, "shape", "arrow")?)?,
            size: range(call, "size", 32, 16, 96)? as u32,
            glow: number(call, "glow", 0.7),
        }),
        "system" if mode == MouseMode::User => {
            if ["color", "shape", "size", "glow"]
                .iter()
                .any(|name| call.get(name).is_some())
            {
                return Err("mouse.use: style options require appearance=\"glow\"".into());
            }
            None
        }
        "system" => return Err("mouse.use: independent mice require appearance=\"glow\"".into()),
        _ => return Err("mouse.use: appearance must be system or glow".into()),
    };
    let options = MouseOptions {
        mode,
        name: text(call, "name", "zuse-runtime")?,
        appearance,
        image: None,
        start: None,
    };
    options.validate()?;
    Ok(options)
}
fn mouse_element_action(call: &Call<'_>) -> Result<accessibility::ElementAction, String> {
    accessibility::ElementAction::new(
        text(call, "path", "")?,
        range(call, "pid", 0, 1, i64::from(u32::MAX))? as u32,
        text(call, "name", "")?,
        text(call, "action", "click")?,
    )
}

fn range(call: &Call<'_>, name: &str, default: i64, min: i64, max: i64) -> Result<i64, String> {
    let value = integer(call, name, default);
    if value < min || value > max {
        Err(format!("{}: {name} must be {min}..{max}", call.name))
    } else {
        Ok(value)
    }
}

fn drawing_button(call: &Call<'_>) -> Result<String, String> {
    let button = text(call, "button", "left")?;
    Ok(match button.as_str() {
        "1" | "left" => "left",
        "2" | "middle" => "middle",
        "3" | "right" => "right",
        "back" => "back",
        "forward" => "forward",
        _ => return Err("unknown mouse button".into()),
    }
    .into())
}

fn drawing_point(call: &Call<'_>) -> Result<Point, String> {
    Ok(Point {
        x: range(call, "x", 0, -MAX_COORDINATE as i64, MAX_COORDINATE as i64)? as f64,
        y: range(call, "y", 0, -MAX_COORDINATE as i64, MAX_COORDINATE as i64)? as f64,
    })
}

fn drawing_path(call: &Call<'_>) -> Result<Vec<Point>, String> {
    let point = |x, y| Point {
        x: number(call, x, 0.0),
        y: number(call, y, 0.0),
    };
    match call.name {
        "mouse.stroke" => drawing::parse_points(&text(call, "points", "")?),
        "draw.line" => drawing::line(point("x1", "y1"), point("x2", "y2")),
        "draw.triangle" => {
            drawing::triangle(point("x1", "y1"), point("x2", "y2"), point("x3", "y3"))
        }
        "draw.rect" => drawing::rect(
            point("x", "y"),
            number(call, "width", 0.0),
            number(call, "height", 0.0),
        ),
        "draw.circle" => drawing::circle(
            point("x", "y"),
            number(call, "radius", 0.0),
            range(call, "steps", 64, 3, (drawing::MAX_POINTS - 1) as i64)? as usize,
        ),
        "draw.polygon" => drawing::polygon(&text(call, "points", "")?),
        _ => Err("unknown drawing path".into()),
    }
}

fn anchor_region(call: &Call<'_>) -> Result<Bounds, String> {
    let region = Bounds {
        x: range(call, "x", 0, 0, MAX_COORDINATE as i64)? as f64,
        y: range(call, "y", 0, 0, MAX_COORDINATE as i64)? as f64,
        width: range(call, "width", 0, 1, MAX_COORDINATE as i64)? as f64,
        height: range(call, "height", 0, 1, MAX_COORDINATE as i64)? as f64,
    };
    Anchor::validate_region(region)?;
    Ok(region)
}

struct TargetArgs {
    text: String,
    role: Option<String>,
    scope: Option<String>,
    exact: bool,
    case_sensitive: bool,
    index: Option<usize>,
}

impl TargetArgs {
    fn query(&self) -> TargetQuery<'_> {
        TargetQuery {
            text: &self.text,
            role: self.role.as_deref(),
            scope: self.scope.as_deref(),
            exact: self.exact,
            case_sensitive: self.case_sensitive,
            index: self.index,
        }
    }
}

fn target_args(c: &Call<'_>) -> Result<TargetArgs, String> {
    let args = TargetArgs {
        text: text(c, "text", "")?,
        role: c.get("role").map(|_| text(c, "role", "")).transpose()?,
        scope: c.get("scope").map(|_| text(c, "scope", "")).transpose()?,
        exact: boolean(c, "exact", true),
        case_sensitive: boolean(c, "case_sensitive", true),
        index: c
            .get("index")
            .map(|_| range(c, "index", 0, 0, 4095).map(|i| i as usize))
            .transpose()?,
    };
    target::validate_query(&args.query())?;
    Ok(args)
}

fn move_options(c: &Call<'_>) -> Result<MoveOptions, String> {
    let via = text(c, "via", "")?;
    let via = if via.trim().is_empty() {
        Vec::new()
    } else {
        if via.split(';').count() > 64 {
            return Err("mouse.move_to: via accepts at most 64 points".into());
        }
        via.split(';')
            .map(|pair| {
                let (x, y) = pair
                    .split_once(',')
                    .ok_or("mouse.move_to: via must contain x,y pairs separated by semicolons")?;
                Ok(Point {
                    x: x.trim()
                        .parse::<f64>()
                        .map_err(|_| "invalid via x coordinate")?,
                    y: y.trim()
                        .parse::<f64>()
                        .map_err(|_| "invalid via y coordinate")?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?
    };
    let options = MoveOptions {
        duration_ms: c
            .get("duration")
            .map(|_| {
                range(
                    c,
                    "duration",
                    i64::from(DEFAULT_MOVE_DURATION_MS),
                    0,
                    60_000,
                )
                .map(|v| v as u32)
            })
            .transpose()?,
        speed: c.get("speed").map(|_| number(c, "speed", 0.0)),
        steps: c
            .get("steps")
            .map(|_| range(c, "steps", 1, 1, 4096).map(|v| v as u32))
            .transpose()?,
        curve: Curve::parse(&text(c, "curve", "smooth")?).ok_or("unknown motion curve")?,
        jitter: number(c, "jitter", DEFAULT_MOVE_JITTER),
        speed_variation: number(c, "speed_variation", 0.0),
        seed: integer(c, "seed", 1) as u64,
        offset: Point {
            x: number(c, "offset_x", 0.0),
            y: number(c, "offset_y", 0.0),
        },
        via,
    };
    options.validate()?;
    Ok(options)
}

/// Flat coordinate moves and click_at share one timing and path preflight.
fn point_move_options(call: &Call<'_>) -> Result<PathOptions, String> {
    let duration = range(
        call,
        "duration",
        i64::from(DEFAULT_MOVE_DURATION_MS),
        0,
        60_000,
    )? as u32;
    let steps = range(
        call,
        "steps",
        i64::from((duration / 8).clamp(1, 4096)),
        1,
        65_536,
    )? as u32;
    let easing = Curve::parse(&text(call, "curve", "smooth")?).ok_or("unknown motion curve")?;
    let jitter = number(call, "jitter", DEFAULT_MOVE_JITTER);
    let seed = integer(call, "seed", 1) as u64;
    Motion::new(
        Point::default(),
        Point::default(),
        duration,
        steps,
        easing,
        jitter,
        seed,
    )
    .map_err(|error| format!("{}: {error}", call.name))?;
    Ok(PathOptions {
        duration_ms: duration,
        steps,
        easing,
        jitter,
        speed_variation: 0.0,
        seed,
    })
}

fn coordinate_motion(
    mouse: &Mouse,
    target: Point,
    options: PathOptions,
) -> Result<std::iter::Skip<Motion>, String> {
    // An explicit immediate move needs neither a position read nor a duplicate
    // start event. Timed moves start at the handle's observed position.
    let immediate = options.duration_ms == 0;
    let from = if immediate { target } else { mouse.position()? };
    Motion::new(
        from,
        target,
        options.duration_ms,
        options.steps,
        options.easing,
        options.jitter,
        options.seed,
    )
    .map(|motion| motion.skip(usize::from(immediate)))
    .map_err(|error| error.to_string())
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

struct Validated<'a> {
    calls: Vec<Call<'a>>,
    wait_ms: u64,
    writing: BTreeMap<usize, writing::Writing>,
    machine_commands: BTreeMap<usize, plotter_control::Command>,
}

fn validate(source: &str, options: Options) -> Result<Validated<'_>, String> {
    let mut writing = BTreeMap::new();
    let mut machine_commands = BTreeMap::new();
    let mut calls = Vec::new();
    let mut wait_budget = 0u64;
    let mut gesture_samples = 0usize;
    let mut drawing_points = 0usize;
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
            name if name.starts_with("plotter.") => {
                let command = plotter_control::Command::prepare(&call)?;
                wait_budget = wait_budget.saturating_add(u64::from(command.timeout_ms));
                machine_commands.insert(calls.len(), command);
            }
            name if name.starts_with("write.") => {
                let plan = writing::Writing::prepare(&call)?;
                drawing_points = drawing_points.saturating_add(plan.points);
                if drawing_points > drawing::MAX_PROGRAM_POINTS {
                    return Err("program drawing points exceed 65536".into());
                }
                wait_budget = wait_budget.saturating_add(plan.wait_ms);
                writing.insert(calls.len(), plan);
            }
            "observe" | "watch" => {
                if call.name == "observe" && call.get("watch").is_some() {
                    if ["source", "selector", "scope"]
                        .iter()
                        .any(|key| call.get(key).is_some())
                    {
                        return Err(
                            "observe uses either a retained watch or a new source query".into()
                        );
                    }
                } else {
                    observation_query(&call)?.validate(call.name == "watch")?;
                }
                let name = text(
                    &call,
                    if call.name == "watch" {
                        "name"
                    } else {
                        "watch"
                    },
                    "last",
                )?;
                if name.is_empty() || name.len() > 128 {
                    return Err("watch name must contain 1..128 bytes".into());
                }
            }
            "wait_until" => {
                observation_condition(&call)?.validate()?;
                let timeout = range(&call, "timeout", 30_000, 0, 60_000)?;
                wait_budget = wait_budget.saturating_add(timeout as u64);
            }
            "watch_until_click" => {
                let options = click_watch_options(&call)?;
                options.validate()?;
                wait_budget = wait_budget.saturating_add(options.timeout_ms);
            }
            "timeline.start"
            | "timeline.sample"
            | "timeline.observe"
            | "timeline.wait_until"
            | "timeline.render"
            | "timeline.close" => {
                #[cfg(not(feature = "vision"))]
                return Err(
                    "unsupported_state: visual timelines require the optional vision feature"
                        .into(),
                );
                #[cfg(feature = "vision")]
                {
                    let name = text(&call, "name", "last")?;
                    if name.is_empty() || name.len() > 128 {
                        return Err("timeline name must contain 1..128 bytes".into());
                    }
                    if call.name == "timeline.start" {
                        let columns = range(&call, "columns", 1, 1, 4096)?;
                        let rows = range(&call, "rows", 1, 1, 4096)?;
                        if columns * rows > 4096 {
                            return Err("timeline grid exceeds 4096 cells".into());
                        }
                        let enter = range(&call, "enter", 24, 1, 255)?;
                        let exit = range(&call, "exit", 8, 0, 254)?;
                        if exit >= enter {
                            return Err(
                                "timeline exit threshold must be below enter threshold".into()
                            );
                        }
                    }
                    if call.name == "timeline.wait_until" {
                        range(&call, "interval", 32, 16, 1000)?;
                        wait_budget = wait_budget
                            .saturating_add(range(&call, "timeout", 30_000, 1, 60_000)? as u64);
                    }
                    if call.name == "timeline.render" && !text(&call, "path", "")?.ends_with(".svg")
                    {
                        return Err("timeline.render requires an explicit .svg destination".into());
                    }
                }
            }
            "touchpad.capabilities"
            | "touchpad.swipe"
            | "touchpad.pinch"
            | "touchpad.rotate"
            | "touchpad.cancel" => {
                let command = touchpad_command(&call)?;
                wait_budget =
                    wait_budget.saturating_add(u64::from(command.scheduled_duration_ms()));
                if let TouchpadCommand::Gesture(plan) = command {
                    gesture_samples = gesture_samples
                        .checked_add(plan.samples().len())
                        .ok_or("touchpad sample budget overflow")?;
                    if gesture_samples > touchpad::MAX_PROGRAM_SAMPLES {
                        return Err("program touchpad samples exceed 65536".into());
                    }
                }
            }
            "mouse.use" => {
                mouse_options(&call)?;
            }
            "mouse.activate" => {
                mouse_element_action(&call)?;
            }
            "anchor.set" | "anchor.clear" => {
                drawing::validate_name(&text(&call, "name", "")?)?;
                if call.name == "anchor.set" {
                    let window = text(&call, "window", "")?;
                    if window.trim().is_empty() || window == "active" || window.len() > 1024 {
                        return Err(
                            "anchor.set requires an explicit observed native window ID".into()
                        );
                    }
                    anchor_region(&call)?;
                }
            }
            "mouse.stroke" | "mouse.click_at" | "draw.line" | "draw.triangle" | "draw.rect"
            | "draw.circle" | "draw.polygon" => {
                drawing_button(&call)?;
                if call.get("anchor").is_some() {
                    drawing::validate_name(&text(&call, "anchor", "")?)?;
                }
                let settle = range(&call, "settle", 30, 0, 1000)? as u64;
                let (points, waits) = if call.name != "mouse.click_at" {
                    let points = drawing_path(&call)?;
                    let duration = range(&call, "duration", 300, 0, 60_000)? as u64;
                    if duration != 0 && duration < (points.len() - 1) as u64 {
                        return Err(format!(
                            "{}: duration must allow at least one millisecond per segment; use duration=0 for explicitly immediate input",
                            call.name
                        ));
                    }
                    (points.len(), duration + 2 * settle)
                } else {
                    drawing_point(&call)?;
                    (
                        1,
                        settle + u64::from(point_move_options(&call)?.duration_ms),
                    )
                };
                drawing_points = drawing_points.saturating_add(points);
                if drawing_points > drawing::MAX_PROGRAM_POINTS {
                    return Err("program drawing points exceed 65536".into());
                }
                wait_budget = wait_budget.saturating_add(waits);
            }
            "target.locate" => {
                if call.get("template").is_some() {
                    #[cfg(not(feature = "vision"))]
                    return Err(
                        "unsupported_state: template matching requires the optional vision feature"
                            .into(),
                    );
                    #[cfg(feature = "vision")]
                    {
                        if ["text", "role", "scope", "exact", "case_sensitive", "index"]
                            .iter()
                            .any(|key| call.get(key).is_some())
                        {
                            return Err("template matching and semantic target arguments are separate explicit locators".into());
                        }
                        if text(&call, "template", "")?.is_empty() {
                            return Err("template path is empty".into());
                        }
                        image_match_options(&call)?;
                    }
                } else {
                    target_args(&call)?;
                    if ["mode", "scales", "min_score", "min_margin"]
                        .iter()
                        .any(|key| call.get(key).is_some())
                    {
                        return Err("image matcher options require template".into());
                    }
                }
                let name = text(&call, "name", "last")?;
                if name.is_empty() || name.len() > 128 {
                    return Err("target name must contain 1..128 bytes".into());
                }
                if text(&call, "window", "active")?.is_empty() {
                    return Err(
                        "target window must be active or an explicit native window ID".into(),
                    );
                }
            }
            "keyboard.num_lock" => {
                if !matches!(text(&call, "state", "")?.as_str(), "on" | "off" | "toggle") {
                    return Err("keyboard.num_lock state must be on, off or toggle; use keyboard.state to read".into());
                }
            }
            "mouse.move_to" => {
                target_args(&call)?;
                wait_budget = wait_budget
                    .saturating_add(u64::from(move_options(&call)?.scheduled_duration_ms()));
            }
            "mouse.move" => {
                range(&call, "x", 0, -MAX_COORDINATE as i64, MAX_COORDINATE as i64)?;
                range(&call, "y", 0, -MAX_COORDINATE as i64, MAX_COORDINATE as i64)?;
                wait_budget =
                    wait_budget.saturating_add(u64::from(point_move_options(&call)?.duration_ms));
            }
            "mouse.circle" => {
                let (_, _, _, options) = circle_options(&call)?;
                wait_budget = wait_budget.saturating_add(u64::from(options.duration_ms));
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
            "android.capture" => {
                crate::screenshot::validate_output_path(Path::new(&text(&call, "path", "")?))?
            }
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
            "accessibility.set_text" => {
                let expected = call
                    .get("expected_text")
                    .map(|_| text(&call, "expected_text", ""))
                    .transpose()?;
                let pid = call
                    .get("expected_pid")
                    .map(|_| range(&call, "expected_pid", 1, 1, u32::MAX as i64))
                    .transpose()?
                    .map(|value| value as u32);
                accessibility::validate_text_edit(
                    &text(&call, "path", "")?,
                    &text(&call, "text", "")?,
                    expected.as_deref(),
                    pid,
                )?;
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
            "browser.touch_gesture" => {
                wait_budget = wait_budget.saturating_add(touch_gesture(&call)?.duration_ms);
            }
            "keyboard.hold" | "browser.hold" => {
                if call.name == "keyboard.hold" {
                    crate::key_hold::NativeKey::parse(&text(&call, "key", "")?)?;
                } else {
                    crate::key_hold::Direction::parse(&text(&call, "key", "")?)?;
                }
                let duration = range(&call, "duration", 0, 1, crate::key_hold::MAX_HOLD_MS as i64)?;
                wait_budget = wait_budget.saturating_add(duration as u64);
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
    Ok(Validated {
        calls,
        wait_ms: wait_budget,
        writing,
        machine_commands,
    })
}

pub fn check(source: &str, options: Options) -> Result<String, String> {
    let prepared = ModuleSession::default().prepare(source, options)?;
    check_prepared(&prepared, options)
}

/// Validate one already loaded program without reopening imported files.
pub fn check_prepared(prepared: &Prepared, options: Options) -> Result<String, String> {
    if prepared.mode == ProgramMode::Scoped {
        let plan = prepared.compile_scoped(options)?;
        return Ok(format!(
            "{{\"status\":\"planned\",\"language\":\"zuse script\",\"plan\":{}}}",
            plan.summary.to_json()
        ));
    }
    let calls = validate(&prepared.source, options)?.calls;
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

/// The resolved target and submitted pointer path for one completed movement.
#[derive(Clone, Debug)]
pub struct MoveReceipt {
    pub target: LocatedTarget,
    pub from: Point,
    pub to: Point,
    pub duration_ms: u32,
    pub elapsed_ms: u128,
    pub samples: usize,
    pub path_length: f64,
}

fn target_center(target: &LocatedTarget) -> Point {
    Point {
        x: target.bounds.x + target.bounds.width / 2.0,
        y: target.bounds.y + target.bounds.height / 2.0,
    }
}

impl MoveReceipt {
    pub fn to_json(&self) -> String {
        let b = self.target.bounds;
        format!(
            "{{\"target\":{{\"path\":{},\"role\":{},\"name\":{},\"bounds\":{{\"x\":{},\"y\":{},\"width\":{},\"height\":{}}}}},\"from\":{{\"x\":{},\"y\":{}}},\"to\":{{\"x\":{},\"y\":{}}},\"x\":{},\"y\":{},\"duration_ms\":{},\"elapsed_ms\":{},\"samples\":{},\"path_length\":{},\"delivery\":\"submitted\"}}",
            quote(&self.target.path),
            quote(&self.target.role),
            quote(&self.target.name),
            b.x,
            b.y,
            b.width,
            b.height,
            self.from.x,
            self.from.y,
            self.to.x,
            self.to.y,
            self.to.x.round() as i32,
            self.to.y.round() as i32,
            self.duration_ms,
            self.elapsed_ms,
            self.samples,
            self.path_length,
        )
    }
}

#[derive(Default)]
pub struct Runtime {
    modules: ModuleSession,
    observer: Observer,
    watches: BTreeMap<String, String>,
    cancellation: Cancellation,
    timer: crate::timing::Timer,
    targets: BTreeMap<String, WindowTarget>,
    anchors: BTreeMap<String, Anchor>,
    #[cfg(feature = "vision")]
    vision: crate::vision_runtime::VisionRuntime,
    mouse: Option<Mouse>,
    desktop: Option<Desktop>,
    browser: Option<Browser>,
    android: Option<Android>,
    ios: Option<Ios>,
    serial: Option<SerialPort>,
    plotter: Option<plotter_control::Selected>,
    usb: Option<UsbDevice>,
    processes: ProcessManager,
}
impl Runtime {
    pub fn new() -> Self {
        Self::default()
    }

    /// Function names retained by successful preflight in this session.
    pub fn functions(&self) -> Vec<String> {
        self.modules.functions()
    }

    /// Inspect retained callable metadata without executing or reloading modules.
    pub fn function_metadata(&self) -> Vec<FunctionMetadata> {
        self.modules.function_metadata()
    }

    /// The same read-only catalog exposed by functions() and zuse.functions.
    pub fn function_catalog(&self, namespace: &str) -> String {
        self.modules.functions_json(namespace)
    }

    pub fn cancellation(&self) -> Cancellation {
        self.cancellation.clone()
    }

    /// Begin a separately authorized execution with an explicit fresh token.
    pub fn set_cancellation(&mut self, cancellation: Cancellation) {
        self.cancellation = cancellation;
    }

    /// Resolve once inside one native window; the caller may retain the result.
    pub fn locate(
        &mut self,
        query: &TargetQuery<'_>,
        window: &str,
    ) -> Result<WindowTarget, String> {
        target::validate_query(query)?;
        let window = TargetWindow::from_windows(&self.desktop()?.windows()?, window)?;
        let snapshot = accessibility::snapshot(4096, 32)?;
        target::locate_in_window(&snapshot, query, &window)
    }

    /// Click a retained semantic target after a lightweight identity check.
    /// The check reads one node and window metadata; it performs no recognition.
    pub fn click_target(&mut self, target: &WindowTarget, button: &str) -> Result<(), String> {
        let cancellation = self.cancellation.clone();
        let center = target_center(&target.target);
        cancellation.dispatch(|| self.mouse()?.submit(center))?;
        let node = accessibility::node(&target.target.path)?;
        let windows = self.desktop()?.windows()?;
        target.verify(&windows, &node)?;
        if !accessibility::hit_test(
            &target.target.path,
            center.x.round() as i32,
            center.y.round() as i32,
        )? || !self.desktop()?.window_hit_test(
            &target.window.id,
            center.x.round() as i32,
            center.y.round() as i32,
        )? {
            return Err(
                "stale_target: another surface covers the retained target; no click was sent"
                    .into(),
            );
        }
        cancellation.dispatch(|| self.mouse()?.button(button, "click"))
    }

    /// Locate an accessible target and move from this runtime's live pointer.
    /// An independent mouse is created lazily unless `mouse.use(kind="user")`
    /// explicitly selected the user pointer. Coordinates use native screen units.
    pub fn move_to(
        &mut self,
        query: &TargetQuery<'_>,
        options: &MoveOptions,
    ) -> Result<MoveReceipt, String> {
        let (target, plan) = self.prepare_move_to(query, options)?;
        self.perform_move(target, plan)
    }

    /// Execute a caller-defined path from the current pointer to a located
    /// target. The callback receives progress, start and offset-adjusted end.
    /// All samples are validated before the first native input event.
    pub fn move_to_with<F: FnMut(f64, Point, Point) -> Point>(
        &mut self,
        query: &TargetQuery<'_>,
        options: &MoveOptions,
        path: F,
    ) -> Result<MoveReceipt, String> {
        options.validate()?;
        let target = target::locate(query)?;
        let from = self.mouse()?.position()?;
        let center = target_center(&target);
        let plan = MovePlan::with_path(from, center, options, path)?;
        self.perform_move(target, plan)
    }

    fn prepare_move_to(
        &mut self,
        query: &TargetQuery<'_>,
        options: &MoveOptions,
    ) -> Result<(LocatedTarget, MovePlan), String> {
        options.validate()?;
        let target = target::locate(query)?;
        let from = self.mouse()?.position()?;
        let plan = MovePlan::new(from, target_center(&target), options)?;
        Ok((target, plan))
    }

    fn perform_move(
        &mut self,
        target: LocatedTarget,
        plan: MovePlan,
    ) -> Result<MoveReceipt, String> {
        let cancellation = self.cancellation.clone();
        let mouse = self.mouse()?;
        let start = Instant::now();
        for (index, sample) in plan.samples().iter().enumerate() {
            let due = Duration::from_millis(u64::from(sample.at_ms));
            if let Some(delay) = due.checked_sub(start.elapsed()) {
                cancellation.sleep(delay)?;
            }
            cancellation
                .dispatch(|| mouse.submit(sample.point))
                .map_err(|error| {
                    format!("pointer movement stopped after {index} samples: {error}")
                })?;
        }
        Ok(MoveReceipt {
            target,
            from: plan.from(),
            to: plan.to(),
            duration_ms: plan.duration_ms(),
            elapsed_ms: start.elapsed().as_millis(),
            samples: plan.samples().len(),
            path_length: plan.length(),
        })
    }

    fn mouse(&mut self) -> Result<&mut Mouse, String> {
        if self.mouse.is_none() {
            self.mouse = Some(Mouse::independent()?);
        }
        self.mouse
            .as_mut()
            .ok_or_else(|| "runtime mouse is unavailable".into())
    }

    fn drawing_points(
        &mut self,
        call: &Call<'_>,
        points: Vec<Point>,
    ) -> Result<Vec<Point>, String> {
        if call.get("anchor").is_none() {
            return Ok(points);
        }
        let name = text(call, "anchor", "")?;
        let anchor = self
            .anchors
            .get(&name)
            .cloned()
            .ok_or("unknown anchor; observe the window and call anchor.set first")?;
        anchor.verify(&self.desktop()?.windows()?)?;
        anchor.resolve(&points)
    }

    fn release_mouse(&mut self) -> Result<(), String> {
        if let Some(mut mouse) = self.mouse.take() {
            mouse.close()?;
        }
        Ok(())
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
        let continuation = queue.continuation(id)?;
        let calls = validate(&continuation, options)?.calls;
        require_touchpad_capabilities(&calls)?;
        let source = queue.take(id)?;
        self.execute(&source, options)
    }

    pub fn execute(&mut self, source: &str, options: Options) -> Result<String, String> {
        let prepared = self.modules.prepare(source, options)?;
        self.execute_prepared(prepared, options)
    }

    /// Resolve entry-file imports against an explicit directory, never process cwd.
    pub fn execute_with_base(
        &mut self,
        source: &str,
        options: Options,
        base: &Path,
    ) -> Result<String, String> {
        let prepared = self.modules.prepare_with_base(source, options, base)?;
        self.execute_prepared(prepared, options)
    }

    /// Preflight the entire expansion before committing definitions or effects.
    /// A dry run leaves the retained module session unchanged.
    pub fn execute_prepared(
        &mut self,
        prepared: Prepared,
        options: Options,
    ) -> Result<String, String> {
        if options.dry_run {
            return check_prepared(&prepared, options);
        }
        if prepared.mode != ProgramMode::Flat {
            return Err(
                "scoped mouse handles require zuse script or script::execute_prepared_with_cancel"
                    .into(),
            );
        }
        let source = prepared.source;
        let Validated {
            calls,
            wait_ms: scheduled_wait_ms,
            mut writing,
            mut machine_commands,
        } = validate(&source, options)?;
        require_touchpad_capabilities(&calls)?;
        if !self.cancellation.is_cancelled() {
            self.modules = prepared.next_session;
        } else if calls.is_empty() {
            return Ok("{\"status\":\"failed\",\"stage\":\"prepare\",\"error\":\"cancelled: execution was cancelled\",\"error_code\":\"cancelled\",\"completed\":0,\"results\":[]}".into());
        }
        // Speeds need the live start/target/path before their duration is known.
        // Reserve every static wait first, then debit each resolved speed plan
        // before sending its first event.
        let mut dynamic_wait_ms = options.max_wait_ms - scheduled_wait_ms;
        let mut results = Vec::new();
        let mut bytes = 0;
        for (index, call) in calls.iter().enumerate() {
            if call.name == "human.request" && !self.cancellation.is_cancelled() {
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
            let was_timing = self.timer.running();
            let outcome = self
                .cancellation
                .check()
                .and_then(|_| match writing.remove(&index) {
                    Some(plan) => plan.execute(&self.cancellation),
                    None => match machine_commands.remove(&index) {
                        Some(command) => {
                            plotter_control::execute(&mut self.plotter, command, &self.cancellation)
                        }
                        None => self.step(call, &mut dynamic_wait_ms),
                    },
                });
            let elapsed = began.elapsed();
            let timing = if was_timing || self.timer.running() {
                format!(
                    ",\"timing\":{}",
                    self.timer.record(elapsed, !call.name.starts_with("timer."))
                )
            } else {
                String::new()
            };
            match outcome {
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
                        "{{\"step\":{index},\"operation\":{},\"elapsed_us\":{}{timing},\"value\":{value}}}",
                        quote(call.name),
                        elapsed.as_micros()
                    ));
                }
                Err(error) => {
                    if self.cancellation.is_cancelled() {
                        let _ = self.release_mouse();
                    }
                    let error_code = if error.starts_with("unsupported_state:") {
                        "unsupported_state"
                    } else if error.starts_with("unsupported_character:") {
                        "unsupported_character"
                    } else if error.starts_with("cancelled:") {
                        "cancelled"
                    } else if error.starts_with("timeout:") {
                        "timeout"
                    } else if error.starts_with("ambiguous_target:") {
                        "ambiguous_target"
                    } else if error.starts_with("stale_target:") {
                        "stale_target"
                    } else {
                        "execution_error"
                    };
                    return Ok(format!(
                        "{{\"status\":\"failed\",\"failed_step\":{index},\"operation\":{},\"error\":{},\"error_code\":{},\"elapsed_us\":{}{timing},\"completed\":{index},\"results\":[{}]}}",
                        quote(call.name),
                        quote(&error),
                        quote(error_code),
                        elapsed.as_micros(),
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

    #[cfg(feature = "vision")]
    fn vision_step(&mut self, call: &Call<'_>) -> Result<String, String> {
        let name = text(call, "name", "last")?;
        match call.name {
            "timeline.observe" => return self.vision.state(&name),
            "timeline.close" => return self.vision.close(&name),
            "timeline.render" => {
                return self
                    .vision
                    .render(&name, Path::new(&text(call, "path", "")?));
            }
            _ => {}
        }
        self.desktop()?;
        let desktop = self.desktop.as_ref().unwrap();
        match call.name {
            "timeline.start" => self.vision.start(
                desktop,
                &name,
                &text(call, "window", "active")?,
                (
                    integer(call, "columns", 1) as u16,
                    integer(call, "rows", 1) as u16,
                ),
                (
                    integer(call, "enter", 24) as u8,
                    integer(call, "exit", 8) as u8,
                ),
            ),
            "timeline.sample" => self.vision.sample(desktop, &name),
            "timeline.wait_until" => self.vision.wait_until(
                desktop,
                &name,
                integer(call, "timeout", 30_000) as u64,
                integer(call, "interval", 32) as u64,
                &self.cancellation,
            ),
            _ => Err("unknown visual timeline command".into()),
        }
    }

    fn step(&mut self, c: &Call<'_>, dynamic_wait_ms: &mut u64) -> Result<String, String> {
        let done = || Ok("{\"ok\":true}".into());
        let cancellation = self.cancellation.clone();
        match c.name {
            "functions" => Ok(self.function_catalog(&text(c, "namespace", "")?)),
            "timer.start" => {
                self.timer.start();
                Ok(self.timer.status())
            }
            "timer.reset" => {
                self.timer.reset();
                Ok(self.timer.status())
            }
            "timer.stop" => {
                self.timer.stop();
                Ok(self.timer.status())
            }
            "timer.status" => Ok(self.timer.status()),
            "observe" => {
                if c.get("watch").is_some() {
                    let name = text(c, "watch", "last")?;
                    let id = self
                        .watches
                        .get(&name)
                        .ok_or("unknown watch name; call watch first")?;
                    self.observer.snapshot(id, self.browser.as_mut())
                } else {
                    Observer::observe(&observation_query(c)?, self.browser.as_mut())
                }
            }
            "watch" => {
                let name = text(c, "name", "last")?;
                if let Some(previous) = self.watches.remove(&name) {
                    self.observer.close(&previous, self.browser.as_mut())?;
                }
                let result = self
                    .observer
                    .watch(observation_query(c)?, self.browser.as_mut())?;
                let id = json::with_value(&result, |root| {
                    root.get("watch")
                        .ok_or("watch receipt has no identity".to_owned())
                        .and_then(json::text)
                })?;
                self.watches.insert(name.clone(), id);
                Ok(format!(
                    "{{\"name\":{},\"receipt\":{result}}}",
                    quote(&name)
                ))
            }
            "watch.close" => {
                let name = text(c, "name", "last")?;
                let id = self.watches.remove(&name).ok_or("unknown watch name")?;
                self.observer.close(&id, self.browser.as_mut())
            }
            "wait_until" => {
                let name = text(c, "watch", "last")?;
                let id = self
                    .watches
                    .get(&name)
                    .ok_or("unknown watch name; call watch first")?;
                self.observer.wait_until(
                    id,
                    &observation_condition(c)?,
                    integer(c, "timeout", 30_000) as u64,
                    &self.cancellation,
                    self.browser.as_mut(),
                )
            }
            "watch_until_click" => Observer::watch_until_click(
                &click_watch_options(c)?,
                &cancellation,
                self.browser.as_mut(),
            ),
            #[cfg(feature = "vision")]
            "timeline.start"
            | "timeline.sample"
            | "timeline.observe"
            | "timeline.wait_until"
            | "timeline.render"
            | "timeline.close" => self.vision_step(c),
            "system.capabilities" => Ok(self.capabilities()),
            "touchpad.capabilities" => Ok(touchpad::capabilities_json()),
            "touchpad.swipe" | "touchpad.pinch" | "touchpad.rotate" => {
                let TouchpadCommand::Gesture(plan) = touchpad_command(c)? else {
                    return Err("invalid touchpad gesture command".into());
                };
                touchpad::execute(&plan)?;
                Ok(plan.to_json())
            }
            "touchpad.cancel" => {
                touchpad::cancel()?;
                done()
            }
            "mouse.use" => {
                let options = mouse_options(c)?;
                // Validate before releasing the old handle. Release it before
                // acquiring a replacement so shared cursor leases cannot
                // restore over the replacement's requested appearance.
                options.resolved_image()?;
                self.release_mouse()?;
                let mouse = Mouse::create(&options)?;
                let kind = mouse.mode().name();
                let result = format!(
                    "{{\"ok\":true,\"mouse_id\":{},\"kind\":{},\"device_id\":{}}}",
                    mouse.id(),
                    quote(kind),
                    mouse
                        .device_id()
                        .map_or_else(|| "null".into(), |id| id.to_string()),
                );
                self.mouse = Some(mouse);
                Ok(result)
            }
            "mouse.activate" => {
                let target = mouse_element_action(c)?;
                self.mouse
                    .as_ref()
                    .ok_or("mouse.activate requires mouse.use(kind=\"semantic\")")?
                    .activate_checked(&target, Some(&cancellation), || Ok(()))
            }
            "mouse.release" => {
                self.release_mouse()?;
                done()
            }
            "mouse.position" => {
                let mouse = self.mouse.as_ref().ok_or(
                    "runtime mouse is unavailable; use mouse.use() or a mouse action first",
                )?;
                let point = mouse.position()?;
                Ok(format!("{{\"x\":{},\"y\":{}}}", point.x, point.y))
            }
            "target.locate" => {
                let name = text(c, "name", "last")?;
                // A failed replacement must never reuse an older target.
                self.targets.remove(&name);
                #[cfg(feature = "vision")]
                {
                    self.vision.targets.remove(&name);
                    if c.get("template").is_some() {
                        let options = image_match_options(c)?;
                        self.desktop()?;
                        return self.vision.locate(
                            self.desktop.as_ref().unwrap(),
                            Path::new(&text(c, "template", "")?),
                            &text(c, "window", "active")?,
                            &name,
                            &options,
                        );
                    }
                }
                if self.targets.len() >= 64 {
                    return Err("at most 64 named targets may be retained in one runtime".into());
                }
                let args = target_args(c)?;
                let target = self.locate(&args.query(), &text(c, "window", "active")?)?;
                let result = format!(
                    "{{\"name\":{},\"location\":{}}}",
                    quote(&name),
                    target.to_json()
                );
                self.targets.insert(name, target);
                Ok(result)
            }
            "mouse.move_to" => {
                let args = target_args(c)?;
                let options = move_options(c)?;
                let (target, plan) = self.prepare_move_to(&args.query(), &options)?;
                if options.speed.is_some() {
                    let required = u64::from(plan.duration_ms());
                    if required > *dynamic_wait_ms {
                        return Err(format!(
                            "mouse.move_to: resolved movement needs {required}ms but only {}ms of wait budget remains",
                            *dynamic_wait_ms
                        ));
                    }
                    *dynamic_wait_ms -= required;
                }
                self.perform_move(target, plan)
                    .map(|receipt| receipt.to_json())
            }
            "anchor.set" => {
                let name = text(c, "name", "")?;
                // A failed observed replacement must not retain the old region.
                self.anchors.remove(&name);
                if self.anchors.len() >= 128 {
                    return Err("anchor limit exceeded (128); clear unused anchors".into());
                }
                let window = text(c, "window", "")?;
                let window = TargetWindow::from_windows(&self.desktop()?.windows()?, &window)?;
                let anchor = Anchor::new(window, anchor_region(c)?)?;
                let region = anchor.region;
                let result = format!(
                    "{{\"name\":{},\"window\":{},\"region\":{{\"x\":{},\"y\":{},\"width\":{},\"height\":{}}},\"coordinate_space\":\"window_local_native\",\"recognition\":\"caller_observed_region\"}}",
                    quote(&name),
                    anchor.window.to_json(),
                    region.x,
                    region.y,
                    region.width,
                    region.height
                );
                self.anchors.insert(name, anchor);
                Ok(result)
            }
            "anchor.clear" => {
                let name = text(c, "name", "")?;
                let removed = self.anchors.remove(&name).is_some();
                Ok(format!("{{\"removed\":{removed}}}"))
            }
            "mouse.stroke" | "draw.line" | "draw.triangle" | "draw.rect" | "draw.circle"
            | "draw.polygon" => {
                let points = drawing_path(c)?;
                let points = self.drawing_points(c, points)?;
                let button = drawing_button(c)?;
                let duration = integer(c, "duration", 300) as u32;
                let settle = integer(c, "settle", 30) as u32;
                let samples = drawing::stroke(
                    self.mouse()?,
                    &points,
                    duration,
                    settle,
                    &button,
                    &cancellation,
                )?;
                Ok(format!(
                    "{{\"points\":{samples},\"duration_ms\":{duration},\"button_released\":true,\"delivery\":\"submitted\",\"application_confirmation_required\":true}}"
                ))
            }
            "mouse.click_at" => {
                let points = self.drawing_points(c, vec![drawing_point(c)?])?;
                let point = points[0];
                let button = drawing_button(c)?;
                let settle = integer(c, "settle", 30) as u32;
                let options = point_move_options(c)?;
                let mouse = self.mouse()?;
                mouse.require_pointer_input()?;
                let path = coordinate_motion(mouse, point, options)?;
                let samples = drawing::click_at(mouse, path, settle, &button, &cancellation)?;
                Ok(format!(
                    "{{\"x\":{},\"y\":{},\"samples\":{samples},\"duration_ms\":{},\"button_released\":true,\"delivery\":\"submitted\",\"application_confirmation_required\":true}}",
                    point.x, point.y, options.duration_ms
                ))
            }
            "mouse.move" => {
                let options = point_move_options(c)?;
                let mouse = self.mouse()?;
                let target = Point {
                    x: integer(c, "x", 0) as f64,
                    y: integer(c, "y", 0) as f64,
                };
                let motion = coordinate_motion(mouse, target, options)?;
                let samples = drawing::move_path(mouse, motion, &cancellation)?;
                Ok(format!(
                    "{{\"x\":{},\"y\":{},\"samples\":{}}}",
                    target.x, target.y, samples
                ))
            }
            "mouse.circle" => {
                let (x, y, radius, options) = circle_options(c)?;
                let path = FunctionalMotion::new(
                    |u| {
                        if u == 0.0 || u == 1.0 {
                            return Point { x, y: y - radius };
                        }
                        let angle = std::f64::consts::TAU * u;
                        Point {
                            x: x + radius * angle.sin(),
                            y: y - radius * angle.cos(),
                        }
                    },
                    options,
                )
                .map_err(|e| e.to_string())?;
                let start = Instant::now();
                let mouse = self.mouse()?;
                let mut samples = 0;
                for sample in path {
                    let sample = sample.map_err(|e| e.to_string())?;
                    let due = Duration::from_millis(u64::from(sample.at_ms));
                    if let Some(delay) = due.checked_sub(start.elapsed()) {
                        cancellation.sleep(delay)?;
                    }
                    cancellation.dispatch(|| mouse.submit(sample.point))?;
                    samples += 1;
                }
                Ok(format!(
                    "{{\"x\":{x},\"y\":{},\"samples\":{samples}}}",
                    y - radius
                ))
            }
            "mouse.click" | "mouse.down" | "mouse.up" => {
                if let Some(mouse) = self.mouse.as_ref() {
                    mouse.require_pointer_input()?;
                }
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
                        cancellation
                            .sleep(Duration::from_millis(integer(c, "interval", 80) as u64))?;
                    }
                    if c.get("target").is_some() {
                        let name = text(c, "target", "")?;
                        if let Some(target) = self.targets.get(&name).cloned() {
                            self.click_target(&target, button)?;
                        } else {
                            #[cfg(feature = "vision")]
                            {
                                let target = self.vision.targets.get(&name).cloned()
                                    .ok_or("stale_target: no unique retained target has that name; use target.locate first")?;
                                cancellation.dispatch(|| {
                                    self.mouse()?.submit(Point {
                                        x: target.x,
                                        y: target.y,
                                    })
                                })?;
                                target.verify(self.desktop()?)?;
                                cancellation.dispatch(|| self.mouse()?.button(button, "click"))?;
                            }
                            #[cfg(not(feature = "vision"))]
                            return Err("stale_target: no retained target has that name; use target.locate first".into());
                        }
                    } else {
                        cancellation.dispatch(|| self.mouse()?.button(button, action))?;
                    }
                }
                Ok("{\"ok\":true,\"delivery\":\"submitted\",\"application_confirmation_required\":true}".into())
            }
            "mouse.scroll" => {
                cancellation.dispatch(|| {
                    self.mouse()?
                        .scroll(integer(c, "dx", 0) as i32, integer(c, "dy", 0) as i32)
                })?;
                Ok("{\"ok\":true,\"delivery\":\"submitted\",\"application_confirmation_required\":true}".into())
            }
            "keyboard.state" => self.desktop()?.keyboard_status(),
            "keyboard.num_lock" => {
                cancellation.dispatch(|| self.desktop()?.num_lock(&text(c, "state", "")?))
            }
            "keyboard.type" => {
                let value = text(c, "text", "")?;
                let window = text(c, "window", "")?;
                if window.is_empty() {
                    cancellation.dispatch(|| self.desktop()?.type_text(&value))?;
                } else {
                    cancellation.dispatch(|| self.desktop()?.type_into(&window, &value))?;
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
                    cancellation.dispatch(|| self.desktop()?.key(&key))?;
                } else {
                    cancellation.dispatch(|| self.desktop()?.key_into(&window, &key))?;
                }
                Ok("{\"ok\":true,\"delivery\":\"submitted\",\"application_confirmation_required\":true}".into())
            }
            "keyboard.hold" => {
                let key = text(c, "key", "")?;
                let window = text(c, "window", "active")?;
                self.desktop()?.hold_key_with_other_keys(
                    &key,
                    integer(c, "duration", 0) as u64,
                    &window,
                    boolean(c, "allow_other_keys", false),
                    &cancellation,
                )
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
                cancellation.sleep(Duration::from_millis(delay))?;
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
            "browser.touch_gesture" => self
                .browser()?
                .touch_gesture(touch_gesture(c)?, &cancellation),
            "browser.click" => self.browser()?.click(&text(c, "selector", "")?),
            "browser.fill" => self
                .browser()?
                .fill(&text(c, "selector", "")?, &text(c, "text", "")?),
            "browser.key" => self.browser()?.key(&text(c, "key", "")?),
            "browser.hold" => self.browser()?.hold_key(
                &text(c, "key", "")?,
                integer(c, "duration", 0) as u64,
                &cancellation,
            ),
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
                let expected = c
                    .get("expected_text")
                    .map(|_| text(c, "expected_text", ""))
                    .transpose()?;
                accessibility::set_text_checked_with_cancel(
                    &text(c, "path", "")?,
                    &text(c, "text", "")?,
                    expected.as_deref(),
                    c.get("expected_pid")
                        .map(|_| integer(c, "expected_pid", 0) as u32),
                    &self.cancellation,
                )
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
