//! Writer UI adapter. Motion and controller ownership live in the named Zuse session.
use std::{
    env, fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use zuse::json::{self, quote};

type Result<T> = std::result::Result<T, String>;
type Strokes = Vec<Vec<(f64, f64)>>;

#[derive(Clone, Debug, PartialEq)]
pub struct Form {
    pub text: String,
    pub font: String,
    pub port: String,
    pub baud: u32,
    pub size: f64,
    pub advance: f64,
    pub line_height: f64,
    pub x: f64,
    pub y: f64,
    pub area_width: f64,
    pub area_height: f64,
    pub margins: [f64; 4],
    pub offset_x: f64,
    pub offset_y: f64,
    pub tilt_degrees: f64,
    pub size_percent: f64,
    pub reference_units: f64,
    pub reference_distance: f64,
    pub swap_xy: bool,
    pub flip_x: bool,
    pub flip_y: bool,
    pub pen_up: f64,
    pub pen_down: f64,
    pub feed: f64,
    pub jog_step: f64,
    pub seed: u64,
}
impl Default for Form {
    fn default() -> Self {
        Self {
            text: "Zuse".into(),
            font: "builtin".into(),
            port: String::new(),
            baud: 115200,
            size: 10.,
            advance: 12.,
            line_height: 15.,
            x: 0.,
            y: 0.,
            area_width: 150.,
            area_height: 200.,
            margins: [5.; 4],
            offset_x: 3.5,
            offset_y: 3.5,
            tilt_degrees: 3.,
            size_percent: 4.,
            reference_units: 1.,
            reference_distance: 1.,
            swap_xy: false,
            flip_x: false,
            flip_y: false,
            pen_up: 3.,
            pen_down: 0.,
            feed: 300.,
            jog_step: 1.,
            seed: 1,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ScreenRequest {
    source: String,
    operation: &'static str,
}
impl ScreenRequest {
    fn new(source: String, preview: bool) -> Result<Self> {
        let mut calls = zuse::language::Parser::new(&source);
        let call = calls
            .next()
            .ok_or("缺少屏幕输出调用")?
            .map_err(|e| e.to_string())?;
        let operation = match (preview, call.name) {
            (true, "write.preview") => "write.preview",
            (false, "write.mouse") => "write.mouse",
            (false, "write.touchscreen") => "write.touchscreen",
            (false, "write.touchpad") => "write.touchpad",
            _ => return Err("屏幕操作与所选输出模式不符".into()),
        };
        if calls.next().is_some() {
            return Err("每次只接受一个屏幕输出调用".into());
        }
        Ok(Self { source, operation })
    }
}

#[derive(Clone, Debug)]
pub enum Action {
    Screen(ScreenRequest),
    Preview,
    Connect,
    Refresh,
    CalibrationStatus,
    Mount(MountAction, Option<[f64; 3]>),
    CalibrationCapture(CalibrationStep),
    Disconnect,
    Reset,
    Park { x: f64, y: f64, up: f64 },
    Jog { axis: char, delta: f64 },
    SetPenUp,
    SetPenDown,
    PenUp,
    PenDown,
    Export(PathBuf),
    Check,
    Send,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalibrationStep {
    Status,
    PenUp,
    PenDown,
}

#[derive(Clone, Copy, Debug)]
pub struct CalibrationReceipt {
    pub epoch: u64,
    pub probe_active: Option<bool>,
    pub request_id: u64,
    pub step: CalibrationStep,
    pub work: [f64; 3],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MachineBounds {
    pub min: [f64; 3],
    pub max: [f64; 3],
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MountAction {
    Status,
    MoveZ(f64),
    Jog { axis: char, delta: f64, feed: f64 },
    ApplyBounds(MachineBounds),
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MountReceipt {
    pub request_id: u64,
    pub epoch: u64,
    pub action: MountAction,
    pub work: [f64; 3],
    pub work_offset: Option<[f64; 3]>,
}
const MOUNT_STATUS: &str = "plotter.status(work=true,offset=true,timeout=6000)";

impl MountAction {
    fn program(self, reference: Option<[f64; 3]>, generation: Option<u64>) -> Result<String> {
        match self {
            Self::Status => Ok(MOUNT_STATUS.into()),
            Self::MoveZ(z) if (-10000.0..=10000.0).contains(&z) => {
                Ok(format!("plotter.pen(z={z},feed=40,timeout=300000)"))
            }
            Self::MoveZ(_) => Err("安装高度必须是有效的毫米工作坐标".into()),
            Self::Jog { axis, delta, feed } => {
                let axis = axis.to_ascii_lowercase();
                let limit = match axis {
                    'x' | 'y' => 10.,
                    'z' => 0.5,
                    _ => return Err("示教点动只支持 X、Y、Z".into()),
                };
                if !delta.is_finite() || !(0.001..=limit).contains(&delta.abs()) {
                    return Err(format!("该轴每次点动限 0.001–{limit} mm"));
                }
                if !(40. ..=500.).contains(&feed) || (axis == 'z' && feed != 40.) {
                    return Err("点动速度需为 40–500 mm/min".into());
                }
                Ok(format!(
                    "plotter.jog(axis=\"{axis}\",delta={delta},feed={feed},timeout=20000)"
                ))
            }
            Self::ApplyBounds(bounds) => {
                let wco = reference.ok_or("应用边界前请读取并核对当前 WCO")?;
                if !wco.iter().all(|x| x.is_finite())
                    || !(0..3).all(|i| {
                        bounds.min[i].is_finite()
                            && bounds.max[i].is_finite()
                            && bounds.min[i] < bounds.max[i]
                    })
                {
                    return Err("边界须为三组有序且有限的坐标".into());
                }
                let generation = generation
                    .map(|n| format!(",expected_generation={n}"))
                    .unwrap_or_default();
                Ok(format!(
                    "plotter.set_bounds(x_min={},x_max={},y_min={},y_max={},z_min={},z_max={},wco_x={},wco_y={},wco_z={}{generation})",
                    bounds.min[0],
                    bounds.max[0],
                    bounds.min[1],
                    bounds.max[1],
                    bounds.min[2],
                    bounds.max[2],
                    wco[0],
                    wco[1],
                    wco[2]
                ))
            }
        }
    }
}

impl Action {
    fn calibration_step(&self) -> Option<CalibrationStep> {
        match self {
            Self::CalibrationCapture(step) => Some(*step),
            Self::CalibrationStatus => Some(CalibrationStep::Status),
            Self::SetPenUp => Some(CalibrationStep::PenUp),
            Self::SetPenDown => Some(CalibrationStep::PenDown),
            _ => None,
        }
    }
}

const WORK_STATUS: &str = "plotter.status(work=true,timeout=6000)";
const LEGACY_STATUS: &str = "plotter.status(timeout=1200)";

fn source(action: &Action, f: &Form, program: &str) -> Result<String> {
    let text = match action {
        Action::Screen(screen) => screen.source.clone(),
        Action::Preview => format!(
            "write.gcode(text={},font={},size={},advance={},line_height={},area_width={},area_height={},margin_left={},margin_top={},margin_right={},margin_bottom={},x={},y={},offset_x_ratio={},offset_y_ratio={},tilt_degrees={},size_ratio={},reference_units={},reference_distance={},swap_xy={},flip_x={},flip_y={},seed={},pen_up={},pen_down={},feed={feed},travel_feed={feed},lift_feed={feed})",
            quote(&f.text),
            quote(&f.font),
            f.size,
            f.advance,
            f.line_height,
            f.area_width,
            f.area_height,
            f.margins[0],
            f.margins[1],
            f.margins[2],
            f.margins[3],
            f.x,
            f.y,
            f.offset_x / 100.,
            f.offset_y / 100.,
            f.tilt_degrees,
            f.size_percent / 100.,
            f.reference_units,
            f.reference_distance,
            f.swap_xy,
            f.flip_x,
            f.flip_y,
            f.seed,
            f.pen_up,
            f.pen_down,
            feed = f.feed
        ),
        Action::Connect => format!("plotter.connect(port={},baud={})", quote(&f.port), f.baud),
        Action::Refresh => "plotter.status()".into(),
        Action::Mount(kind, reference) => kind.program(*reference, None)?,
        Action::CalibrationCapture(_)
        | Action::CalibrationStatus
        | Action::SetPenUp
        | Action::SetPenDown => WORK_STATUS.into(),
        Action::Disconnect => "plotter.disconnect()".into(),
        Action::Reset => "plotter.reset()".into(),
        Action::Park { x, y, up } => format!("plotter.park(x={x},y={y},z={up},feed=300)"),
        Action::Jog { axis, delta } => {
            let a = axis.to_ascii_lowercase();
            if !matches!(a, 'x' | 'y' | 'z') || !delta.is_finite() || *delta == 0. {
                return Err("请输入非零的单轴点动距离".into());
            }
            let feed = if a == 'z' { 40. } else { 300. };
            format!("plotter.jog(axis=\"{a}\",delta={delta},feed={feed})")
        }
        Action::PenUp | Action::PenDown => {
            let z = if matches!(action, Action::PenUp) {
                f.pen_up
            } else {
                f.pen_down
            };
            if !(-10000.0..=10000.0).contains(&z) {
                return Err("无效的 Z 坐标".into());
            }
            format!("plotter.pen(z={z},feed=40)")
        }
        Action::Send => format!(
            "plotter.send(program={},require_bounds=true,timeout=300000)",
            quote(program)
        ),
        Action::Check => format!("plotter.check(program={},timeout=300000)", quote(program)),
        _ => return Err("操作没有会话脚本".into()),
    };
    if text.len() > 256 * 1024 {
        return Err("作业超过 Zuse 会话 256 KiB 限额".into());
    }
    Ok(text)
}

// Consume only output from write.gcode. Render the exact rounded machine vertices
// from the immutable export, even if the source font is subsequently edited.
fn preview_paths(program: &str, pen_up: f64) -> Result<Strokes> {
    let pen_up = format!("{pen_up:.3}")
        .parse::<f64>()
        .map_err(|_| "抬笔高度无效")?;
    let mut strokes = Vec::new();
    let mut next = None;
    let mut drawing = false;
    for line in program.lines() {
        if let Some(z) = line
            .strip_prefix("G0 Z")
            .or_else(|| line.strip_prefix("G1 Z"))
        {
            let target = z
                .split_ascii_whitespace()
                .next()
                .and_then(|v| v.parse::<f64>().ok())
                .ok_or("笔高无效")?;
            drawing = target != pen_up;
            if drawing {
                let start = next.take().ok_or("落笔前缺少 XY 起点")?;
                strokes.push(vec![start]);
            }
            continue;
        }
        if matches!(line, "G21 G17 G90 G94" | "M2") {
            continue;
        }
        let words: Vec<_> = line.split_ascii_whitespace().collect();
        if let [motion, x, y, ..] = words.as_slice() {
            let xy = |s: &str, axis| {
                s.strip_prefix(axis)
                    .and_then(|v| v.parse::<f64>().ok())
                    .filter(|v| v.is_finite())
                    .ok_or("笔迹坐标无效")
            };
            let pt = (xy(x, 'X')?, xy(y, 'Y')?);
            match *motion {
                "G0" | "G1" if !drawing => {
                    next = Some(pt);
                    continue;
                }
                "G1" if drawing => {
                    strokes.last_mut().unwrap().push(pt);
                    continue;
                }
                _ => {}
            }
        }
        return Err(format!("无法预览生成的程序块 {line}"));
    }
    if strokes.is_empty() {
        return Err("笔迹为空".into());
    }
    Ok(strokes)
}

#[derive(Clone)]
struct Session {
    bin: std::ffi::OsString,
    name: String,
}
impl Default for Session {
    fn default() -> Self {
        Self {
            bin: env::var_os("ZUSE_BIN")
                .or_else(|| {
                    let peer = env::current_exe()
                        .ok()?
                        .with_file_name(format!("zuse{}", env::consts::EXE_SUFFIX));
                    peer.is_file().then(|| peer.into_os_string())
                })
                .unwrap_or_else(|| "zuse".into()),
            name: env::var("ZUSE_WRITER_SESSION").unwrap_or_else(|_| "writer".into()),
        }
    }
}
fn request_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!(
        "writer-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}
fn raw(s: &str, key: &str) -> Option<String> {
    json::with_value(s, |v| Ok(v.get(key).map(|v| v.raw().to_owned())))
        .ok()
        .flatten()
}
fn num(s: &str, key: &str) -> Option<u64> {
    raw(s, key)?.parse().ok()
}
fn field(s: &str, key: &str) -> Option<String> {
    json::field(s, key).ok()
}
impl Session {
    fn cli(&self, op: &str, args: &[&str], input: Option<&str>) -> Result<String> {
        let mut child = Command::new(&self.bin)
            .args(["session", op, &self.name])
            .args(args)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("启动 zuse: {e}"))?;
        if let Some(data) = input {
            let result = child.stdin.take().unwrap().write_all(data.as_bytes());
            if let Err(e) = result {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("脚本传输回执未知: {e}"));
            }
        }
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        let body = String::from_utf8(out.stdout).map_err(|_| "Zuse 返回无效 UTF-8")?;
        if !out.status.success() || raw(&body, "ok").as_deref() != Some("true") {
            let detail = raw(&body, "error")
                .and_then(|e| field(&e, "message"))
                .unwrap_or_else(|| {
                    format!(
                        "{} {}",
                        body.trim(),
                        String::from_utf8_lossy(&out.stderr).trim()
                    )
                });
            return Err(format!("session {op}: {detail}"));
        }
        if field(&body, "session").as_deref() != Some(&self.name) {
            return Err("会话名不匹配".into());
        }
        Ok(body)
    }
    fn status(&self) -> Result<Snapshot> {
        Snapshot::parse(&self.cli("status", &[], None)?)
    }
    fn result(&self, id: u64) -> Result<String> {
        let out = self.cli("result", &[&id.to_string()], None)?;
        if num(&out, "job_id") != Some(id) {
            return Err("作业 ID 不匹配".into());
        }
        Ok(out)
    }
    fn cancel(&self, target: &Snapshot) -> Result<()> {
        let fresh = self.status()?;
        if target.active.is_none() || fresh != *target {
            return Err("作业或修订号已变化，取消未发送".into());
        }
        self.cli(
            "cancel",
            &[
                "--expect-revision",
                &target.rev.to_string(),
                "--request-id",
                &request_id(),
            ],
            None,
        )?;
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Snapshot {
    pid: u64,
    rev: u64,
    active: Option<u64>,
    last: Option<u64>,
}
impl Snapshot {
    fn parse(s: &str) -> Result<Self> {
        Ok(Self {
            pid: num(s, "pid").ok_or("缺少会话 PID")?,
            rev: num(s, "state_revision").ok_or("缺少会话修订号")?,
            active: num(s, "active_job_id"),
            last: num(s, "last_job_id"),
        })
    }
}

fn value(receipt: &str) -> Result<String> {
    let status = field(receipt, "status").ok_or("作业回执缺少状态")?;
    let result = raw(receipt, "result").ok_or("作业缺少结果")?;
    if status != "completed" || field(&result, "status").as_deref() != Some("completed") {
        return Err(format!(
            "{status}: {}",
            field(&result, "error").unwrap_or(result.chars().take(600).collect())
        ));
    }
    json::with_value(&result, |v| {
        v.get("results")
            .and_then(|v| v.elements().last())
            .and_then(|v| v.get("value"))
            .map(|v| v.raw().to_owned())
            .ok_or("缺少操作回执".into())
    })
}

enum Event {
    Observed(Result<(Snapshot, Option<String>)>, u64),
    Accepted(Snapshot),
    ScreenSettled(Snapshot),
    Done(Action, Box<Form>, Result<(String, Option<Snapshot>)>),
    Cancelled(Result<()>),
}

fn submit(
    session: &Session,
    action: &Action,
    script: &str,
    expected: Option<&Snapshot>,
    stop: &AtomicBool,
    events: &mpsc::Sender<Event>,
) -> Result<(String, Option<Snapshot>)> {
    let before = match session.status() {
        Ok(s) => s,
        Err(_)
            if expected.is_none()
                && matches!(
                    action,
                    Action::Connect | Action::Preview | Action::Screen(_)
                ) =>
        {
            // Another client may win the start race. Query the existing named session.
            let start = session.cli("start", &[], None);
            session
                .status()
                .map_err(|e| format!("启动会话: {:?}; {e}", start.err()))?
        }
        Err(e) => return Err(e),
    };
    if before.active.is_some() || expected.is_some_and(|s| s != &before) {
        return Err("共享会话已变化，请查看最新状态后重试".into());
    }
    if stop.load(Ordering::Acquire) {
        return Err("提交前已取消".into());
    }
    let id = request_id();
    let command = if matches!(action,Action::Screen(request) if request.operation=="write.preview")
        || matches!(
            action,
            Action::Refresh
                | Action::CalibrationCapture(_)
                | Action::CalibrationStatus
                | Action::SetPenUp
                | Action::SetPenDown
        ) {
        "observe"
    } else {
        "script"
    };
    let wait_ms = if matches!(action, Action::Screen(_)) {
        600_000_u64
    } else {
        300_000
    };
    let accepted = session.cli(
        command,
        &[
            "-",
            "--request-id",
            &id,
            "--expect-revision",
            &before.rev.to_string(),
            "--max-wait-ms",
            &wait_ms.to_string(),
        ],
        Some(script),
    )?;
    let job = num(&accepted, "job_id").ok_or("提交回执未知，禁止自动重发")?;
    if field(&accepted, "request_id") != Some(id)
        || field(&accepted, "status").as_deref() != Some("accepted")
    {
        return Err("无法关联作业提交回执".into());
    }
    let target = Snapshot {
        pid: before.pid,
        rev: num(&accepted, "state_revision").ok_or("缺少作业修订号")?,
        active: Some(job),
        last: before.last,
    };
    events
        .send(Event::Accepted(target.clone()))
        .map_err(|_| "窗口已关闭")?;
    let until = Instant::now() + Duration::from_millis(wait_ms + 20_000);
    let mut cancel_sent = false;
    loop {
        let now = session.status()?;
        if now.pid != before.pid {
            return Err("服务已更换，原作业结果未知".into());
        }
        if stop.load(Ordering::Acquire) && !cancel_sent && now.active == Some(job) {
            // Bind to the accepted job and its original revision.
            if now == target {
                session.cancel(&target)?;
            }
            cancel_sent = true;
        }
        let out = session.result(job)?;
        if field(&out, "status").as_deref() != Some("running") {
            let after = session.status()?;
            if after.pid != before.pid {
                return Err("服务已更换，结果无法关联".into());
            }
            let current =
                after.active.is_none() && after.last == Some(job) && after.rev == target.rev + 1;
            // Only this precise, correlated zero-effect preflight rejection permits an
            // older daemon to service the request. Never retry an uncertain observation.
            if current
                && matches!(script, WORK_STATUS | MOUNT_STATUS)
                && action.calibration_step().is_some()
                && legacy_work_rejection(&out)
            {
                return legacy_work_status(
                    session,
                    action,
                    after,
                    stop,
                    events,
                    script == MOUNT_STATUS,
                );
            }
            if let Action::Screen(request) = action {
                if current {
                    events
                        .send(Event::ScreenSettled(after.clone()))
                        .map_err(|_| "窗口已关闭")?;
                }
                let payload = value(&out)?;
                if sole_operation(&out).as_deref() != Some(request.operation) {
                    return Err("屏幕作业回执与请求的操作不符".into());
                }
                return Ok((payload, current.then_some(after)));
            }
            return Ok((value(&out)?, current.then_some(after)));
        }
        if Instant::now() > until {
            return Err("等待作业超时，结果未知；未重发".into());
        }
        thread::sleep(Duration::from_millis(
            if script.starts_with("plotter.jog_held(") {
                40
            } else {
                160
            },
        ));
    }
}

fn legacy_work_rejection(receipt: &str) -> bool {
    let Some(result) = raw(receipt, "result") else {
        return false;
    };
    field(receipt, "status").as_deref() == Some("failed")
        && field(&result, "stage").as_deref() == Some("prepare")
        && field(&result, "error_code").as_deref() == Some("preflight_error")
        && num(&result, "completed") == Some(0)
        && matches!(
            field(&result, "error").as_deref(),
            Some(
                "plotter.status: unknown argument work" | "plotter.status: unknown argument offset"
            )
        )
}

fn legacy_work_status(
    session: &Session,
    action: &Action,
    mut last: Snapshot,
    stop: &AtomicBool,
    events: &mpsc::Sender<Event>,
    offset_required: bool,
) -> Result<(String, Option<Snapshot>)> {
    let deadline = Instant::now() + Duration::from_secs(6);
    for sample in 1..=8 {
        let (payload, snapshot) =
            submit(session, action, LEGACY_STATUS, Some(&last), stop, events)?;
        let Some(next) = snapshot else {
            return Ok((payload, None));
        };
        // Use only positions from one receipt, and never continue sampling an unready machine.
        if raw(&payload, "connected").as_deref() != Some("true")
            || field(&payload, "state").as_deref() != Some("Idle")
            || {
                let sample = Coordinates::parse(&payload)?;
                sample.work.is_some() && (!offset_required || sample.offset.is_some())
            }
            || sample == 8
            || Instant::now() >= deadline
        {
            return Ok((payload, Some(next)));
        }
        last = next;
        thread::sleep(Duration::from_millis(100));
    }
    unreachable!()
}

fn coords(s: &str, key: &str) -> Result<Option<[f64; 3]>> {
    let Some(data) = raw(s, key) else {
        return Ok(None);
    };
    json::with_value(&data, |v| {
        let mut parts = v.elements();
        let mut xyz = [0.; 3];
        for n in &mut xyz {
            *n = parts
                .next()
                .and_then(|v| v.raw().parse::<f64>().ok())
                .filter(|n| n.is_finite())
                .ok_or("无效的 GRBL 坐标")?;
        }
        Ok(Some(xyz))
    })
}

#[derive(Clone, Copy)]
struct Coordinates {
    machine: Option<[f64; 3]>,
    work: Option<[f64; 3]>,
    offset: Option<[f64; 3]>,
}
impl Coordinates {
    // All three terms come exclusively from this one millimetre report.
    fn parse(payload: &str) -> Result<Self> {
        let m = coords(payload, "machine_position")?;
        let w = coords(payload, "work_position")?;
        let o = coords(payload, "work_offset")?;
        let machine = m.or_else(|| w.zip(o).map(|(w, o)| std::array::from_fn(|i| w[i] + o[i])));
        let work = w.or_else(|| m.zip(o).map(|(m, o)| std::array::from_fn(|i| m[i] - o[i])));
        let offset = o.or_else(|| m.zip(w).map(|(m, w)| std::array::from_fn(|i| m[i] - w[i])));
        if machine
            .iter()
            .chain(&work)
            .chain(&offset)
            .flatten()
            .any(|n| !n.is_finite())
        {
            return Err("GRBL 坐标超出范围".into());
        }
        Ok(Self {
            machine,
            work,
            offset,
        })
    }
}

fn export(path: &std::path::Path, program: &str) -> Result<()> {
    let temp = path.with_file_name(format!(".{}.tmp", request_id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|e| e.to_string())?;
    let result = file
        .write_all(program.as_bytes())
        .and_then(|_| file.sync_all());
    drop(file);
    let result = result.and_then(|_| fs::hard_link(&temp, path));
    let _ = fs::remove_file(temp);
    result.map_err(|e| format!("导出 {}: {e}", path.display()))
}

// Only a completed plotter operation can attest to the session's selected machine.
fn device_receipt(receipt: &str) -> Option<String> {
    let payload = value(receipt).ok()?;
    let result = raw(receipt, "result")?;
    let operation = json::with_value(&result, |v| {
        let step = v
            .get("results")
            .and_then(|v| v.elements().last())
            .ok_or("missing step")?;
        json::text(step.get("operation").ok_or("missing operation")?)
    })
    .ok()?;
    matches!(
        operation.as_str(),
        "plotter.status" | "plotter.connect" | "plotter.disconnect"
    )
    .then_some(payload)
}

// A neighbouring revision may contain a single other window's read. A skipped
// revision or a program that merely ends in plotter.status can hide real motion.
fn contiguous_completion(previous: &Snapshot, current: &Snapshot, job: u64) -> bool {
    if previous.pid != current.pid || current.active.is_some() || current.last != Some(job) {
        return false;
    }
    if let Some(active) = previous.active {
        active == job && previous.rev.checked_add(1) == Some(current.rev)
    } else {
        previous.last.unwrap_or(0).checked_add(1) == Some(job)
            && previous.rev.checked_add(2) == Some(current.rev)
    }
}

fn sole_operation(receipt: &str) -> Option<String> {
    if field(receipt, "status").as_deref() != Some("completed") {
        return None;
    }
    let result = raw(receipt, "result")?;
    if field(&result, "status").as_deref() != Some("completed") {
        return None;
    }
    json::with_value(&result, |v| {
        let mut steps = v.get("results").ok_or("missing steps")?.elements();
        let op = steps
            .next()
            .and_then(|s| s.get("operation"))
            .ok_or("missing operation")?;
        if steps.next().is_some() {
            return Err("multiple operations".into());
        }
        json::text(op)
    })
    .ok()
}
fn observed_operation(previous: &Snapshot, current: &Snapshot, receipt: &str) -> Option<String> {
    let job = num(receipt, "job_id")?;
    contiguous_completion(previous, current, job)
        .then(|| sole_operation(receipt))
        .flatten()
}

#[derive(Clone, Debug)]
struct Device {
    reference_generation: Option<u64>,
    offset: Option<[f64; 3]>,
    id: Option<String>,
    model: Option<String>,
    firmware: Option<String>,
    port: String,
    baud: u32,
    label: Option<String>,
    #[cfg(unix)]
    tty: Option<(u64, u64, u64)>,
}

#[cfg(unix)]
fn tty_identity(path: &str) -> Option<(u64, u64, u64)> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let m = fs::metadata(path).ok()?;
    m.file_type()
        .is_char_device()
        .then(|| (m.dev(), m.ino(), m.rdev()))
}

impl Device {
    fn matches(&self, port: &str, baud: u32) -> bool {
        if self.baud != baud {
            return false;
        }
        #[cfg(unix)]
        if let Some(tty) = self.tty {
            return tty_identity(port) == Some(tty);
        }
        self.port == port
    }
}

fn same_xyz(a: [f64; 3], b: [f64; 3], tolerance: f64) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() <= tolerance)
}
fn check_bound_receipt(data: &str, b: MachineBounds, reference: [f64; 3]) -> Result<()> {
    let box_data = raw(data, "bounds").ok_or("控制器未返回已应用的边界")?;
    for (i, axis) in ["x", "y", "z"].iter().enumerate() {
        for (side, target) in [("min", b.min[i]), ("max", b.max[i])] {
            if raw(&box_data, &format!("{axis}_{side}")).and_then(|v| v.parse::<f64>().ok())
                != Some(target)
            {
                return Err("控制器回执与指定边界不一致".into());
            }
        }
    }
    if !coords(data, "work_offset")?.is_some_and(|offset| same_xyz(offset, reference, 0.000_001)) {
        return Err("控制器未确认边界绑定的工作偏移".into());
    }
    Ok(())
}

fn device_envelope(data: &str, selected: &Device) -> bool {
    raw(data, "connected").as_deref() == Some("true")
        && field(data, "port")
            .zip(num(data, "baud").and_then(|n| u32::try_from(n).ok()))
            .is_some_and(|(path, baud)| selected.matches(&path, baud))
}

// Absent on older daemons. A malformed or vanished generation from a known
// generation-aware link must never silently downgrade a guard.
fn reported_generation(payload: &str) -> Result<Option<u64>> {
    raw(payload, "reference_generation")
        .map(|n| {
            n.parse::<u64>()
                .ok()
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or_else(|| "设备回执的坐标参考代号无效".to_string())
        })
        .transpose()
}

struct MountObservation {
    reference_generation: Option<u64>,
    payload: String,
    snapshot: Snapshot,
    work: [f64; 3],
    offset: [f64; 3],
}

impl MountObservation {
    fn from_result(
        (payload, snapshot): (String, Option<Snapshot>),
        device: &Device,
    ) -> Result<Self> {
        let snapshot = snapshot.ok_or("工作坐标回执已被其他会话操作取代")?;
        if !device_envelope(&payload, device)
            || field(&payload, "state").as_deref() != Some("Idle")
            || field(&payload, "units").as_deref() != Some("mm")
            || [
                ("device_id", &device.id),
                ("controller", &device.model),
                ("firmware", &device.firmware),
            ]
            .into_iter()
            .any(|(key, known)| {
                known
                    .as_ref()
                    .is_some_and(|v| field(&payload, key).as_ref() != Some(v))
            })
        {
            return Err("示教所选写字机身份、空闲状态或毫米单位未确认".into());
        }
        let xyz = Coordinates::parse(&payload)?;
        let reference_generation = reported_generation(&payload)?;
        if device.reference_generation.is_some() && reference_generation.is_none() {
            return Err("设备回执缺少之前已报告的坐标参考代号".into());
        }
        Ok(Self {
            reference_generation,
            payload,
            snapshot,
            work: xyz.work.ok_or("本次回执缺少 WPos")?,
            offset: xyz.offset.ok_or("本次回执缺少可核实的 WCO")?,
        })
    }

    fn completed(self) -> (String, Option<Snapshot>) {
        (self.payload, Some(self.snapshot))
    }
}

fn held_jog_program(axis: char, delta: f64, feed: f64, device: &Device) -> Result<String> {
    if !matches!(axis, 'X' | 'Y')
        || !(0.001..=2.).contains(&delta.abs())
        || !(40. ..=500.).contains(&feed)
    {
        return Err("长按只支持 XY 轴，每段最多 2 mm，速度 40–500 mm/min".into());
    }
    let generation = device
        .reference_generation
        .ok_or("连续点动需要支持参考代号的控制器服务")?;
    let firmware = device
        .firmware
        .as_deref()
        .ok_or("连续点动前请刷新固件身份")?;
    let id = device
        .id
        .as_ref()
        .map(|id| format!(",device_id={}", quote(id)))
        .unwrap_or_default();
    let offset = device
        .offset
        .map(|[x, y, z]| format!(",wco_x={x},wco_y={y},wco_z={z}"))
        .unwrap_or_default();
    Ok(format!(
        "plotter.jog_held(axis=\"{}\",delta={delta},feed={feed},expected_generation={generation},firmware={}{}{},timeout=12000)",
        axis.to_ascii_lowercase(),
        quote(firmware),
        id,
        offset
    ))
}

fn confirmed_held_jog(
    kind: MountAction,
    device: &Device,
    result: (String, Option<Snapshot>),
) -> Result<(String, Option<Snapshot>)> {
    let MountAction::Jog { axis, delta, feed } = kind else {
        return Err("无效的持续点动请求".into());
    };
    let observed = MountObservation::from_result(result, device)?;
    let metric = |key| {
        raw(&observed.payload, key)
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite())
    };
    let start = metric("from_mm").ok_or("点动回执缺少实际起点")?;
    let target = metric("target_mm").ok_or("点动回执缺少目标")?;
    let tolerance = metric("axis_tolerance_mm")
        .filter(|mm| (0. ..=delta.abs() * 0.5 + 0.001_271).contains(mm))
        .ok_or("点动回执缺少可核实的轴分辨率")?;
    let i = if axis == 'X' { 0 } else { 1 };
    if observed.reference_generation != device.reference_generation
        || device
            .offset
            .is_some_and(|offset| !same_xyz(offset, observed.offset, 0.000_001))
        || field(&observed.payload, "axis").as_deref() != Some(if axis == 'X' { "X" } else { "Y" })
        || metric("feed_mm_min") != Some(feed)
        || (start + delta - target).abs() > 0.000_001
        || (observed.work[i] - target).abs() > tolerance
        || observed.work[i] == start
    {
        return Err("点动回执的设备参考、方向、速度或终点不符，请核对设备".into());
    }
    Ok(observed.completed())
}

fn mount_job(
    action: &Action,
    snapshot: Option<&Snapshot>,
    device: Option<&Device>,
    mut run: impl FnMut(&Action, &str, &Snapshot) -> Result<(String, Option<Snapshot>)>,
) -> Result<(String, Option<Snapshot>)> {
    let Action::Mount(kind, reference) = action else {
        unreachable!()
    };
    let device = device.ok_or("连接写字机后再开始示教")?;
    let initial = snapshot.ok_or("示教需要已核实的会话快照")?;
    let before = MountObservation::from_result(
        run(&Action::CalibrationStatus, MOUNT_STATUS, initial)?,
        device,
    )?;
    if *kind == MountAction::Status {
        return Ok(before.completed());
    }
    if before.reference_generation != device.reference_generation {
        return Err("控制器坐标参考已变化，命令未发送，请重新示教".into());
    }
    if reference.is_some_and(|saved| !same_xyz(saved, before.offset, 0.000_001)) {
        return Err("示教参考 WCO 已变化，命令未发送".into());
    }
    let mut target = before.work;
    match *kind {
        MountAction::MoveZ(z) => {
            if (z - target[2]).abs() * 1500.0 > 295_000.0 {
                return Err("安装位距离超出本次低速移动的 300 秒上限".into());
            }
            target[2] = z;
        }
        MountAction::Jog { axis, delta, .. } => {
            let i = match axis.to_ascii_uppercase() {
                'X' => 0,
                'Y' => 1,
                'Z' => 2,
                _ => return Err("无效的点动轴".into()),
            };
            target[i] += delta;
        }
        _ => {}
    }
    let program = kind.program(*reference, before.reference_generation)?;
    // Only submit()'s read-only observation can fall back on a legacy daemon.
    let (receipt, after) = run(action, &program, &before.snapshot)?;
    let after = after.ok_or("命令已结束，结果被其他操作取代，请核对写字机")?;
    if !device_envelope(&receipt, device) {
        return Err("命令已结束，回执未确认所选设备，请勿重复提交".into());
    }
    if reported_generation(&receipt)? != before.reference_generation {
        return Err("命令已收到回执，坐标参考已变化或未确认，请勿重复提交".into());
    }
    if let MountAction::ApplyBounds(bounds) = kind {
        check_bound_receipt(&receipt, *bounds, before.offset)?;
    }
    let confirmed = run(&Action::CalibrationStatus, MOUNT_STATUS, &after)
        .and_then(|result| MountObservation::from_result(result, device))
        .map_err(|e| format!("命令已收到回执，最终工作位置未确认，请勿重复提交：{e}"))?;
    // GRBL reports three decimals in millimetres and four in inches.
    let tolerance = if field(&confirmed.payload, "report_units").as_deref() == Some("inch") {
        0.001_271
    } else {
        0.000_501
    };
    if ["device_id", "controller", "firmware", "report_units"]
        .iter()
        .any(|key| field(&before.payload, key) != field(&confirmed.payload, key))
        || before.reference_generation != confirmed.reference_generation
        || !same_xyz(before.offset, confirmed.offset, 0.000_001)
        || !same_xyz(target, confirmed.work, tolerance)
        || (0..3).any(|i| target[i] != before.work[i] && confirmed.work[i] == before.work[i])
    {
        return Err("命令已收到回执，最终工作位置或偏移与预期不符，请核对写字机".into());
    }
    Ok(confirmed.completed())
}

pub struct Controller {
    screen: Option<(u64, String)>,
    screen_running: bool,
    screen_job: Option<Snapshot>,
    pub form: Form,
    pub paper: Option<crate::workspace_profile::WorkspaceProfile>,
    pub strokes: Strokes,
    pub gcode: String,
    pub status: String,
    pub mpos: Option<[f64; 3]>,
    pub wpos: Option<[f64; 3]>,
    pressure_sample: Option<(f64, String)>,
    probe_pin: Option<bool>,
    calibration: Option<CalibrationReceipt>,
    calibration_epoch: u64,
    mount: Option<MountReceipt>,
    work_offset: Option<[f64; 3]>,
    session: Session,
    snapshot: Option<Snapshot>,
    events: mpsc::Receiver<Event>,
    tx: mpsc::Sender<Event>,
    stop: Option<Arc<AtomicBool>>,
    cancel_pending: bool,
    observing: bool,
    last_poll: Instant,
    generation: u64,
    link: Option<Device>,
    needs_device_status: bool,
    auto_refresh: bool,
    tried_revision: Option<(u64, u64)>,
    plan: Option<(Form, String)>,
    sent: bool,
}
impl Default for Controller {
    fn default() -> Self {
        let (tx, events) = mpsc::channel();
        Self {
            screen: None,
            screen_running: false,
            screen_job: None,
            form: Form::default(),
            paper: None,
            strokes: vec![],
            gcode: String::new(),
            status: "就绪".into(),
            mpos: None,
            wpos: None,
            pressure_sample: None,
            probe_pin: None,
            calibration: None,
            calibration_epoch: 0,
            mount: None,
            work_offset: None,
            session: Session::default(),
            snapshot: None,
            events,
            tx,
            stop: None,
            cancel_pending: false,
            observing: false,
            last_poll: Instant::now() - Duration::from_secs(2),
            generation: 0,
            link: None,
            needs_device_status: true,
            auto_refresh: false,
            tried_revision: None,
            plan: None,
            sent: false,
        }
    }
}

impl Controller {
    pub fn session_name(&self) -> &str {
        &self.session.name
    }
    pub fn active_job(&self) -> Option<u64> {
        self.snapshot.as_ref().and_then(|s| s.active)
    }
    pub fn cancel_target(&self) -> Option<(u64, u64, u64)> {
        let s = self.snapshot.as_ref()?;
        Some((s.pid, s.active?, s.rev))
    }
    pub fn owns_job(&self) -> bool {
        self.stop.is_some()
    }
    pub fn busy(&self) -> bool {
        self.owns_job() || self.cancel_pending || self.active_job().is_some()
    }
    /// Last device verified by this named session, independently of the editable selector.
    pub fn connected_device(&self) -> Option<(&str, u32, Option<&str>)> {
        self.link
            .as_ref()
            .map(|d| (d.port.as_str(), d.baud, d.label.as_deref()))
    }
    pub fn connected(&self) -> bool {
        self.link
            .as_ref()
            .is_some_and(|d| d.matches(&self.form.port, self.form.baud))
    }
    /// Screen previews and output use the same named session as the other writer jobs.
    pub fn dispatch_screen(&mut self, source: String, preview: bool) -> Option<u64> {
        let request = match ScreenRequest::new(source, preview) {
            Ok(request) => request,
            Err(error) => {
                self.status = error;
                return None;
            }
        };
        let before = self.generation;
        self.dispatch(Action::Screen(request));
        (self.generation != before).then_some(self.generation)
    }
    pub fn screen_receipt(&self) -> Option<(u64, &str)> {
        self.screen
            .as_ref()
            .map(|(token, value)| (*token, value.as_str()))
    }
    pub fn hardware_id(&self) -> Option<&str> {
        if self.needs_device_status || !self.connected() {
            return None;
        }
        self.link.as_ref()?.id.as_deref()
    }
    pub(crate) fn apply_paper_profile(
        &mut self,
        profile: crate::workspace_profile::WorkspaceProfile,
    ) -> Result<()> {
        let [x, y] = profile.corner_a;
        let [right, bottom] = profile.corner_b;
        let swap = self.form.swap_xy;
        let (flip_x, flip_y) = if swap {
            (right > x, bottom < y)
        } else {
            (right < x, bottom > y)
        };
        self.set_paper_axes([swap, flip_x, flip_y], false)?;
        let dx = (right - x).abs();
        let dy = (bottom - y).abs();
        [self.form.x, self.form.y] = [x, y];
        self.form.reference_units = 1.;
        self.form.reference_distance = 1.;
        [self.form.area_width, self.form.area_height] = if swap { [dy, dx] } else { [dx, dy] };
        self.form.pen_up = profile.pen_up_z;
        self.form.pen_down = profile.pen_down_z;
        self.paper = Some(profile);
        Ok(())
    }

    pub(crate) fn set_paper_direction(&mut self, reverse: bool, persist: bool) -> Result<()> {
        let (x, y) = if self.form.swap_xy {
            (reverse, self.form.flip_y)
        } else {
            (self.form.flip_x, reverse)
        };
        self.set_paper_axes([self.form.swap_xy, x, y], persist)
    }

    pub(crate) fn set_paper_axes(&mut self, axes: [bool; 3], persist: bool) -> Result<()> {
        if persist && let Some(id) = self.hardware_id() {
            crate::workspace_profile::save_axes(id, axes)?;
        }
        [self.form.swap_xy, self.form.flip_x, self.form.flip_y] = axes;
        self.invalidate_plan();
        Ok(())
    }

    pub fn controller_identity(&self) -> Option<(&str, &str)> {
        if self.needs_device_status || !self.connected() {
            return None;
        }
        let d = self.link.as_ref()?;
        Some((d.model.as_deref()?, d.firmware.as_deref()?))
    }
    pub fn work_offset(&self) -> Option<[f64; 3]> {
        self.work_offset
    }
    pub fn mount_receipt(&self) -> Option<&MountReceipt> {
        if self.busy() || self.needs_device_status {
            None
        } else {
            self.mount.as_ref()
        }
    }
    fn dispatch_mount(&mut self, what: MountAction, reference: Option<[f64; 3]>) -> Option<u64> {
        let before = self.generation;
        self.dispatch(Action::Mount(what, reference));
        (self.generation != before).then_some(self.generation)
    }
    pub fn refresh_mount_reference(&mut self) -> Option<u64> {
        self.dispatch_mount(MountAction::Status, None)
    }
    pub fn move_mount_z(&mut self, z: f64) -> Option<u64> {
        self.dispatch_mount(
            MountAction::MoveZ(z),
            self.link.as_ref().and_then(|d| d.offset),
        )
    }
    pub fn jog_calibration(&mut self, axis: char, delta: f64, feed: f64) -> Option<u64> {
        self.dispatch_mount(
            MountAction::Jog {
                axis: axis.to_ascii_uppercase(),
                delta,
                feed,
            },
            self.link.as_ref().and_then(|d| d.offset),
        )
    }
    pub(crate) fn jog_held(&mut self, axis: char, delta: f64, feed: f64) -> Option<u64> {
        let device = self.link.as_ref()?;
        let script = match held_jog_program(axis, delta, feed, device) {
            Ok(script) => script,
            Err(e) => {
                self.status = e;
                return None;
            }
        };
        let before = self.generation;
        self.dispatch_inner(
            Action::Mount(MountAction::Jog { axis, delta, feed }, device.offset),
            Some(script),
        );
        (self.generation != before).then_some(self.generation)
    }

    pub fn apply_calibration_bounds_at(&mut self, b: MachineBounds, wco: [f64; 3]) -> Option<u64> {
        self.dispatch_mount(MountAction::ApplyBounds(b), Some(wco))
    }

    pub fn pressure(&self) -> Option<f64> {
        self.pressure_sample.as_ref().map(|p| p.0)
    }
    pub fn pressure_unit(&self) -> Option<&str> {
        self.pressure_sample.as_ref().map(|p| p.1.as_str())
    }
    pub fn probe_active(&self) -> Option<bool> {
        self.probe_pin
    }
    /// Changes when earlier captures can no longer be combined with new ones.
    pub fn calibration_epoch(&self) -> u64 {
        self.calibration_epoch
    }
    fn invalidate_calibration(&mut self) {
        self.calibration_epoch = self.calibration_epoch.wrapping_add(1);
        self.calibration = None;
        self.mount = None;
        self.paper = None;
    }
    pub fn calibration_receipt(&self) -> Option<&CalibrationReceipt> {
        if self.needs_device_status || self.busy() {
            return None;
        }
        self.calibration.as_ref()
    }

    /// Return the accepted window action's token. Busy/refused dispatches have no token.
    pub fn dispatch_calibration(&mut self, step: CalibrationStep) -> Option<u64> {
        let before = self.generation;
        self.dispatch(Action::CalibrationCapture(step));
        (self.generation != before).then_some(self.generation)
    }

    fn clear_samples(&mut self) {
        self.mpos = None;
        self.wpos = None;
        self.pressure_sample = None;
        self.probe_pin = None;
        self.work_offset = None;
    }

    fn mark_device_stale(&mut self) {
        self.invalidate_calibration();
        self.needs_device_status = true;
        self.clear_samples();
    }
    fn forget_device(&mut self) {
        if self.link.take().is_some() {
            self.invalidate_calibration();
        }
        self.clear_samples();
    }
    fn invalidate_plan(&mut self) {
        self.plan = None;
        self.gcode.clear();
        self.strokes.clear();
    }
    pub fn dispatch(&mut self, action: Action) {
        self.dispatch_inner(action, None);
    }

    fn dispatch_inner(&mut self, action: Action, held_program: Option<String>) {
        if matches!(action, Action::Cancel) {
            if self.cancel_pending {
                return;
            }
            if let Some(flag) = &self.stop {
                if !flag.swap(true, Ordering::AcqRel) && !self.screen_running {
                    self.invalidate_calibration();
                }
                self.status = "已请求取消，等待作业回执".into();
            } else if let Some(target) = self.snapshot.clone().filter(|s| s.active.is_some()) {
                self.cancel_pending = true;
                self.invalidate_calibration();
                let session = self.session.clone();
                let tx = self.tx.clone();
                thread::spawn(move || {
                    let _ = tx.send(Event::Cancelled(session.cancel(&target)));
                });
            }
            return;
        }
        if self.busy() {
            self.status = "共享会话正忙".into();
            return;
        }
        // A display alias may refer to the port already owned by the session.
        let action = if matches!(action, Action::Connect) && self.connected() {
            Action::Refresh
        } else {
            action
        };
        if matches!(action, Action::Disconnect) && (self.link.is_none() || self.needs_device_status)
        {
            self.status = "请先刷新当前会话的设备状态".into();
            return;
        }
        if matches!(
            action,
            Action::Jog { .. }
                | Action::PenUp
                | Action::PenDown
                | Action::Reset
                | Action::Park { .. }
                | Action::Send
                | Action::Check
                | Action::CalibrationCapture(_)
                | Action::CalibrationStatus
                | Action::Mount(..)
                | Action::SetPenUp
                | Action::SetPenDown
        ) && (!self.connected() || self.needs_device_status)
        {
            self.status = "请先连接或刷新所选控制器".into();
            return;
        }
        let mut program = String::new();
        if matches!(action, Action::Export(_) | Action::Send | Action::Check) {
            let Some((form, plan)) = &self.plan else {
                self.status = "请先生成预览".into();
                return;
            };
            if form != &self.form || plan != &self.gcode || self.strokes.is_empty() {
                self.status = "参数或预览已变更，请重新生成".into();
                return;
            }
            if matches!(action, Action::Send) && self.sent {
                self.status = "该预览已提交过，请核对设备再生成新预览".into();
                return;
            }
            program = plan.clone();
        }
        if matches!(action, Action::Preview) {
            self.invalidate_plan();
        }
        let script = if matches!(action, Action::Export(_)) {
            String::new()
        } else {
            match source(&action, &self.form, &program) {
                Ok(s) => s,
                Err(e) => {
                    self.status = e;
                    return;
                }
            }
        };
        if matches!(action, Action::Send) {
            if !self.paper.as_ref().is_some_and(|p| {
                p.pen_up_z == self.form.pen_up && p.pen_down_z == self.form.pen_down
            }) {
                self.status = "请先保存或载入纸面与装笔校准，并使用已校准的笔高。".into();
                return;
            }
            self.sent = true
        }
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = Some(stop.clone());
        self.generation += 1;
        self.screen_running = matches!(action, Action::Screen(_));
        if self.screen_running {
            self.screen = None;
        }
        if action.calibration_step().is_some() {
            self.calibration = None;
        }
        if matches!(action, Action::Mount(..)) {
            self.mount = None;
        }
        let device = self.link.clone();
        let (session, form, tx, expected) = (
            self.session.clone(),
            self.form.clone(),
            self.tx.clone(),
            self.snapshot.clone(),
        );
        self.status = "处理中…".into();
        thread::spawn(move || {
            let result = match &action {
                Action::Export(path) => {
                    export(path, &program).map(|()| (path.display().to_string(), expected))
                }
                Action::Mount(kind, _) if held_program.is_some() => (|| {
                    let last = expected
                        .as_ref()
                        .ok_or("连续点动需要已核实的共享会话快照")?;
                    let selected = device.as_ref().ok_or("连续点动需要已连接的写字机")?;
                    let result = submit(
                        &session,
                        &action,
                        held_program.as_deref().unwrap(),
                        Some(last),
                        &stop,
                        &tx,
                    )?;
                    confirmed_held_jog(*kind, selected, result)
                })(),
                Action::Mount(..) => mount_job(
                    &action,
                    expected.as_ref(),
                    device.as_ref(),
                    |request, program, last| {
                        submit(&session, request, program, Some(last), &stop, &tx)
                    },
                ),
                _ => submit(&session, &action, &script, expected.as_ref(), &stop, &tx),
            };
            let _ = tx.send(Event::Done(action, Box::new(form), result));
        });
    }

    fn position(&mut self, payload: &str) -> Result<()> {
        if raw(payload, "connected").as_deref() == Some("false") {
            self.forget_device();
            self.needs_device_status = false;
            return Ok(());
        }
        if raw(payload, "connected").as_deref() != Some("true")
            || field(payload, "units").as_deref() != Some("mm")
        {
            return Err("缺少已连接设备的毫米坐标回执".into());
        }
        let port = field(payload, "port").ok_or("缺少会话设备路径")?;
        let baud = num(payload, "baud")
            .and_then(|n| u32::try_from(n).ok())
            .ok_or("缺少会话设备波特率")?;
        let xyz = Coordinates::parse(payload)?;
        let reference_generation = reported_generation(payload)?;
        if self.link.as_ref().is_some_and(|d| {
            d.matches(&port, baud)
                && d.reference_generation.is_some()
                && reference_generation.is_none()
        }) {
            return Err("设备回执缺少之前已报告的坐标参考代号".into());
        }
        let id = field(payload, "device_id").filter(|v| !v.trim().is_empty());
        let model = field(payload, "controller");
        let firmware = field(payload, "firmware");
        let label = match (field(payload, "controller"), field(payload, "firmware")) {
            (Some(model), Some(version)) => Some(format!("{model} {version}")),
            (model, version) => model.or(version),
        }
        .or_else(|| {
            self.link
                .as_ref()
                .filter(|d| d.matches(&port, baud))
                .and_then(|d| d.label.clone())
        });
        let changed = self.link.as_ref().is_none_or(|d| {
            !d.matches(&port, baud)
                || d.reference_generation != reference_generation
                || (d.label.is_some() && label.is_some() && d.label != label)
                || (d.id.is_some() && model.is_some() && firmware.is_some() && d.id != id)
                || d.offset
                    .zip(xyz.offset)
                    .is_some_and(|(a, b)| !same_xyz(a, b, 0.000_001))
        });
        if changed {
            self.invalidate_calibration();
        }
        self.pressure_sample = raw(payload, "pressure")
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite())
            .zip(field(payload, "pressure_unit").filter(|unit| !unit.trim().is_empty()));
        self.probe_pin = match raw(payload, "probe_active").as_deref() {
            Some("true") => Some(true),
            Some("false") => Some(false),
            _ => field(payload, "pins").map(|pins| pins.contains('P')),
        };
        self.mpos = xyz.machine;
        self.wpos = xyz.work;
        self.work_offset = xyz.offset;
        // A jog acknowledgement may omit identity. Retain a previously verified
        // value only for the same still-owned device; full status updates replace it.
        let previous = self.link.as_ref().filter(|_| !changed);
        let is_status = model.is_some() && firmware.is_some();
        let id = if is_status {
            id
        } else {
            id.or_else(|| previous.and_then(|d| d.id.clone()))
        };
        let model = model.or_else(|| previous.and_then(|d| d.model.clone()));
        let firmware = firmware.or_else(|| previous.and_then(|d| d.firmware.clone()));
        let offset = xyz.offset.or_else(|| previous.and_then(|d| d.offset));
        if let Some(new_id) = id.as_deref()
            && self.link.as_ref().and_then(|device| device.id.as_deref()) != Some(new_id)
            && let Ok(Some([swap, x, y])) = crate::workspace_profile::load_axes(new_id)
        {
            self.form.swap_xy = swap;
            self.form.flip_x = x;
            self.form.flip_y = y;
            self.invalidate_plan();
        }
        self.link = Some(Device {
            reference_generation,
            id,
            model,
            firmware,
            offset,
            #[cfg(unix)]
            tty: tty_identity(&port),
            port,
            baud,
            label,
        });
        self.needs_device_status = false;
        Ok(())
    }

    // Some completed commands return an envelope without XYZ. Check mode in
    // particular can reset the retained controller even at unchanged WCO.
    fn operation_reference(&mut self, payload: &str) -> Result<()> {
        let Some(device) = self.link.as_mut() else {
            return Ok(());
        };
        if !device_envelope(payload, device) {
            return Ok(());
        }
        let observed = reported_generation(payload)?;
        if device.reference_generation.is_some() && observed.is_none() {
            return Err("设备回执缺少之前已报告的坐标参考代号".into());
        }
        if observed != device.reference_generation {
            device.reference_generation = observed;
            self.mark_device_stale();
        }
        Ok(())
    }

    fn apply(&mut self, action: &Action, f: &Form, payload: &str) -> Result<String> {
        match action {
            Action::Screen(request) => {
                self.screen = Some((self.generation, payload.to_string()));
                Ok(if request.operation == "write.preview" {
                    "屏幕书写预览就绪"
                } else {
                    "屏幕书写已完成"
                }
                .into())
            }
            Action::Park { .. } => {
                if raw(payload, "parked").as_deref() != Some("true") {
                    return Err("回左上角未取得到位回执".into());
                }
                self.wpos = coords(payload, "work_position")?;
                self.work_offset = coords(payload, "work_offset")?;
                Ok("已抬笔并回到纸张左上角。".into())
            }
            Action::Reset => {
                self.mark_device_stale();
                self.invalidate_plan();
                Ok(format!(
                    "控制器已复位 · {} · 请重新核对工作原点和笔高",
                    field(payload, "state").unwrap_or_default()
                ))
            }
            Action::Preview => {
                if f != &self.form {
                    return Err("预览期间参数已变化，请重新生成".into());
                }
                let code = json::field(payload, "gcode")?;
                let paths = preview_paths(&code, f.pen_up)?;
                let msg = format!("预览 · {} 笔 · seed {}", paths.len(), f.seed);
                self.plan = Some((f.clone(), code.clone()));
                self.gcode = code;
                self.strokes = paths;
                self.sent = false;
                Ok(msg)
            }
            Action::Connect
            | Action::Refresh
            | Action::CalibrationStatus
            | Action::CalibrationCapture(_)
            | Action::SetPenUp
            | Action::SetPenDown => {
                let previous_epoch = self.link.as_ref().map(|_| self.calibration_epoch);
                if let Err(e) = self.position(payload) {
                    self.mark_device_stale();
                    return Err(e);
                }
                if let Some(step) = action.calibration_step() {
                    if previous_epoch.is_some_and(|epoch| epoch != self.calibration_epoch) {
                        return Err("校准期间设备身份已变化，笔位未记录".into());
                    }
                    if f != &self.form || field(payload, "state").as_deref() != Some("Idle") {
                        return Err("标定需要空闲控制器和相同的参数".into());
                    }
                    let work = self
                        .wpos
                        .ok_or("本次回执缺少 WPos 或同帧 MPos/WCO，笔位未记录")?;
                    let z = work[2];
                    if !self.connected() {
                        return Err("所选设备与标定回执不符".into());
                    }
                    match action {
                        Action::SetPenUp => {
                            self.form.pen_up = z;
                            self.invalidate_plan();
                        }
                        Action::SetPenDown => {
                            self.form.pen_down = z;
                            self.invalidate_plan();
                        }
                        _ => {}
                    }
                    self.calibration = Some(CalibrationReceipt {
                        epoch: self.calibration_epoch,
                        probe_active: self.probe_pin,
                        request_id: self.generation,
                        step,
                        work,
                    });
                    return Ok(if step == CalibrationStep::Status {
                        format!("已核对工作坐标 Z={z:.3} mm")
                    } else {
                        format!("已记录工作坐标 Z={z:.3} mm")
                    });
                }
                if self.form.port.is_empty()
                    && let Some(d) = &self.link
                {
                    self.form.port = d.port.clone();
                    self.form.baud = d.baud;
                    self.invalidate_plan();
                }
                Ok(
                    if let Some((path, baud, identity)) = self.connected_device() {
                        format!(
                            "{} · {path} · {baud} · {}",
                            identity.unwrap_or("控制器"),
                            field(payload, "state").unwrap_or_else(|| "已刷新".into())
                        )
                    } else {
                        "会话未连接控制器".into()
                    },
                )
            }
            Action::Disconnect => {
                self.position(payload)?;
                Ok("已断开控制器".into())
            }
            Action::Mount(kind, _) => {
                let epoch = self.calibration_epoch;
                self.position(payload)?;
                if epoch != self.calibration_epoch
                    || !self.connected()
                    || self.form.port != f.port
                    || self.form.baud != f.baud
                    || field(payload, "state").as_deref() != Some("Idle")
                {
                    return Err("示教回执与当前设备或会话不匹配".into());
                }
                let work = self.wpos.ok_or("示教缺少最终 WPos")?;
                let offset = self.work_offset.ok_or("示教缺少同帧工作偏移")?;
                self.mount = Some(MountReceipt {
                    request_id: self.generation,
                    epoch,
                    action: *kind,
                    work,
                    work_offset: Some(offset),
                });
                Ok(format!(
                    "示教回执 · X={:.3} Y={:.3} Z={:.3} mm",
                    work[0], work[1], work[2]
                ))
            }
            Action::Export(_) => Ok(format!("已导出 {payload}")),
            Action::Check => {
                self.operation_reference(payload)?;
                Ok(format!("控制器校验完成 · {payload}"))
            }
            Action::Send | Action::Jog { .. } | Action::PenUp | Action::PenDown => {
                self.clear_samples();
                if field(payload, "units").as_deref() == Some("mm") {
                    self.position(payload)?
                } else {
                    self.operation_reference(payload)?;
                }
                Ok(format!("控制器回执 · {payload}"))
            }
            Action::Cancel => unreachable!(),
        }
    }

    pub fn poll(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::ScreenSettled(s) => {
                    self.tried_revision = Some((s.pid, s.rev));
                    self.snapshot = Some(s);
                    self.screen_job = None;
                }
                Event::Accepted(s) => {
                    if self.screen_running {
                        self.screen_job = Some(s.clone());
                    }
                    if self.auto_refresh {
                        // A failed observer job also advances the session revision. Do not submit
                        // the same failing query again when its terminal snapshot arrives.
                        self.tried_revision = Some((s.pid, s.rev.saturating_add(1)));
                    }
                    self.status = format!(
                        "{} · 作业 #{}",
                        self.session.name,
                        s.active.unwrap_or_default()
                    );
                    self.snapshot = Some(s);
                }
                Event::Cancelled(out) => {
                    self.cancel_pending = false;
                    self.status = out
                        .map(|_| "已请求取消，等待设备回执".into())
                        .unwrap_or_else(|e| e);
                }
                Event::Done(action, f, r) => {
                    let cancelled = self
                        .stop
                        .take()
                        .is_some_and(|flag| flag.load(Ordering::Acquire));
                    self.auto_refresh = false;
                    if std::mem::take(&mut self.screen_running) {
                        // Only this window dispatches the one parsed screen call. A stale
                        // terminal revision is reconciled by normal session observation.
                        self.status = match r {
                            Ok((payload, _)) => {
                                if cancelled {
                                    "已请求取消屏幕作业，请核对屏幕输出".into()
                                } else {
                                    self.apply(&action, &f, &payload).unwrap_or_else(|e| e)
                                }
                            }
                            Err(e) => format!("屏幕作业未确认完成 · {e}"),
                        };
                        continue;
                    }
                    if cancelled
                        && (action.calibration_step().is_some()
                            || matches!(action, Action::Mount(..)))
                    {
                        self.mark_device_stale();
                        self.status = "校准已取消，笔位未记录".into();
                        continue;
                    }
                    self.status = match r {
                        Ok((payload, snapshot)) => {
                            if !matches!(action, Action::Export(_)) && snapshot.is_none() {
                                self.mark_device_stale();
                                if !matches!(action, Action::Preview) {
                                    self.status = "作业已完成，共享会话已变化，正在核对设备".into();
                                    continue;
                                }
                            }
                            if !matches!(action, Action::Export(_)) && snapshot.is_some() {
                                self.snapshot = snapshot;
                            }
                            self.apply(&action, &f, &payload).unwrap_or_else(|e| {
                                if !matches!(action, Action::Preview | Action::Export(_))
                                    && action.calibration_step().is_none()
                                {
                                    self.mark_device_stale();
                                }
                                e
                            })
                        }
                        Err(e) => {
                            if !matches!(action, Action::Preview | Action::Export(_)) {
                                self.mark_device_stale();
                            }
                            if e.contains("controller stopped in Hold") {
                                "机器已停稳。控制器仍有未执行指令，复位并重新对笔后再写。".into()
                            } else if e.contains("controller stop could not be verified") {
                                "未能确认机器停稳，请在机器旁立即停止。".into()
                            } else if e.contains("controller is Idle") && cancelled {
                                "已取消，控制器回报空闲。".into()
                            } else if (e.contains("unknown") || e.contains("unsupported"))
                                && e.contains("plotter.")
                            {
                                format!("{e} · 请将 ZUSE_BIN 指向包含 plotter.* 的新版 zuse")
                            } else {
                                format!("作业未确认完成 · {e}")
                            }
                        }
                    }
                }
                Event::Observed(result, generation) => {
                    self.observing = false;
                    if generation != self.generation || self.owns_job() {
                        continue;
                    }
                    match result {
                        Ok((s, last)) => {
                            if self.snapshot.as_ref().is_some_and(|old| old.pid != s.pid) {
                                self.forget_device();
                                self.tried_revision = None;
                            }
                            let changed = self.snapshot.as_ref() != Some(&s);
                            if changed {
                                let pending_external = self.snapshot.as_ref().is_some_and(|old| {
                                    old.pid == s.pid
                                        && old.active.is_none()
                                        && old.last == s.last
                                        && old.rev.checked_add(1) == Some(s.rev)
                                        && old.last.unwrap_or(0).checked_add(1) == s.active
                                }) && s.active.is_some();
                                let operation =
                                    self.snapshot.as_ref().zip(last.as_deref()).and_then(
                                        |(old, receipt)| observed_operation(old, &s, receipt),
                                    );
                                let own_screen = self.screen_job.as_ref().is_some_and(|job| {
                                    job.active
                                        .is_some_and(|id| contiguous_completion(job, &s, id))
                                });
                                if own_screen
                                    || matches!(
                                        operation.as_deref(),
                                        Some(
                                            "write.preview"
                                                | "write.mouse"
                                                | "write.touchscreen"
                                                | "write.touchpad"
                                        )
                                    )
                                {
                                    self.screen_job = None;
                                    self.tried_revision = Some((s.pid, s.rev));
                                } else if operation.as_deref() == Some("plotter.status") {
                                    self.needs_device_status = true;
                                    self.clear_samples();
                                } else if !pending_external {
                                    self.mark_device_stale();
                                }
                                // Pending external work hides receipts via busy(). Keep the
                                // last device sample until the actual operation is identified.

                                self.status = if let Some(id) = s.active {
                                    format!("{} · 外部作业 #{id} 运行中", self.session.name)
                                } else if let Some(id) = s.last {
                                    format!(
                                        "{} · 作业 #{id} {}",
                                        self.session.name,
                                        last.as_deref()
                                            .and_then(|r| field(r, "status"))
                                            .unwrap_or_else(|| "已结束".into())
                                    )
                                } else {
                                    format!("{} · 会话就绪", self.session.name)
                                };
                            }
                            if s.active.is_none()
                                && self.needs_device_status
                                && let Some(data) = last.as_deref().and_then(device_receipt)
                            {
                                let f = self.form.clone();
                                match self.apply(&Action::Refresh, &f, &data) {
                                    Ok(label) => self.status = label,
                                    Err(e) => {
                                        self.mark_device_stale();
                                        self.status = e;
                                    }
                                }
                            }
                            let revision = (s.pid, s.rev);
                            let reconcile = s.active.is_none()
                                && self.needs_device_status
                                && self.tried_revision != Some(revision)
                                && !self.cancel_pending;
                            self.snapshot = Some(s);
                            if reconcile {
                                self.tried_revision = Some(revision);
                                self.auto_refresh = true;
                                self.dispatch(Action::Refresh);
                            }
                        }
                        Err(e) => {
                            if self.snapshot.is_some() {
                                if !self.needs_device_status || self.calibration.is_some() {
                                    self.mark_device_stale();
                                }
                                self.status = format!("会话状态待核实 · {e}");
                            }
                        }
                    }
                }
            }
        }
        if self.owns_job()
            || self.observing
            || self.last_poll.elapsed() < Duration::from_millis(750)
        {
            return;
        }
        self.observing = true;
        self.last_poll = Instant::now();
        let (session, tx, generation, previous) = (
            self.session.clone(),
            self.tx.clone(),
            self.generation,
            self.snapshot.clone(),
        );
        thread::spawn(move || {
            let r = session.status().map(|s| {
                let last = if s.active.is_none() && previous.as_ref() != Some(&s) {
                    s.last.and_then(|id| session.result(id).ok())
                } else {
                    None
                };
                (s, last)
            });
            let _ = tx.send(Event::Observed(r, generation));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const PROGRAM: &str = "G21 G17 G90 G94\nG0 Z3.000\nG0 X10.000 Y20.000\nG1 Z0.000 F300.000\nG1 X12.500 Y18.500\nG0 Z3.000\nM2\n";

    #[test]
    fn applying_paper_profile_sets_origin_orientation_dimensions_and_heights_together() {
        use crate::workspace_profile::WorkspaceProfile;
        let profile = WorkspaceProfile::new([30., 10.], [10., 40.], 0., 5., Some(5.), 5.).unwrap();
        let mut controller = Controller::default();
        controller.apply_paper_profile(profile).unwrap();
        let form = &controller.form;
        assert_eq!(
            [form.x, form.y, form.area_width, form.area_height],
            [30., 10., 20., 30.]
        );
        assert_eq!([form.flip_x, form.flip_y], [true, true]);
        assert_eq!([form.pen_up, form.pen_down], [0., 5.]);
        controller.invalidate_calibration();
        assert!(controller.paper.is_none());
    }

    #[test]
    fn full_writer_form_generates_preview_at_controlled_feed() {
        let form = Form::default();
        let command = source(&Action::Preview, &form, "").unwrap();
        let result = zuse::runtime::Runtime::new()
            .execute(&command, zuse::runtime::Options::default())
            .unwrap();
        let payload = json::with_value(&result, |root| {
            root.get("results")
                .and_then(|steps| steps.elements().next())
                .and_then(|step| step.get("value"))
                .map(|value| value.raw().to_owned())
                .ok_or_else(|| result.clone())
        })
        .unwrap();
        let program = json::field(&payload, "gcode").unwrap();
        assert!(!program.contains("G0"));
        assert!(program.contains("F300.000"));
        assert!(!preview_paths(&program, form.pen_up).unwrap().is_empty());
    }

    #[test]
    fn preview_uses_exported_machine_coordinates() {
        assert_eq!(
            preview_paths(PROGRAM, 3.).unwrap(),
            vec![vec![(10., 20.), (12.5, 18.5)]]
        );
        assert!(preview_paths("G1 X0 Y0\n", 3.).is_err());
        let f = Form {
            seed: 91,
            ..Form::default()
        };
        assert!(
            source(&Action::Preview, &f, "")
                .unwrap()
                .contains("seed=91")
        );
        assert!(
            source(&Action::Send, &f, PROGRAM)
                .unwrap()
                .contains(&quote(PROGRAM))
        );
    }

    #[test]
    fn calibration_uses_work_z_and_rejects_missing_offset() {
        let mut c = Controller::default();
        let f = c.form.clone();
        let s = r#"{"connected":true,"port":"","baud":115200,"units":"mm","state":"Idle","machine_position":[8,9,10],"work_offset":[5,5,6]}"#;
        c.apply(&Action::SetPenUp, &f, s).unwrap();
        assert_eq!(
            (c.mpos, c.wpos, c.form.pen_up),
            (Some([8., 9., 10.]), Some([3., 4., 4.]), 4.)
        );
        let f = c.form.clone();
        assert!(c.apply(&Action::SetPenDown,&f,r#"{"connected":true,"port":"","baud":115200,"units":"mm","state":"Idle","machine_position":[1,2,3]}"#).is_err());
        assert!(
            source(
                &Action::Jog {
                    axis: 'X',
                    delta: 1000.
                },
                &f,
                ""
            )
            .is_ok()
        );
    }

    #[test]
    fn stale_or_consumed_plan_does_not_dispatch() {
        let mut c = Controller::default();
        let f = c.form.clone();
        let body = format!("{{\"gcode\":{}}}", quote(PROGRAM));
        c.apply(&Action::Preview, &f, &body).unwrap();
        c.position(r#"{"connected":true,"port":"","baud":115200,"units":"mm"}"#)
            .unwrap();
        c.form.seed += 1;
        c.dispatch(Action::Send);
        assert!(!c.busy());
        c.form = f;
        c.sent = true;
        c.dispatch(Action::Send);
        assert!(!c.busy());
    }

    #[test]
    fn verified_link_survives_a_failed_or_redundant_connect() {
        let mut c = Controller::default();
        let s = r#"{"connected":true,"port":"/dev/ttyUSB0","baud":115200,"units":"mm","controller":"BtWriter","firmware":"1.3a.20221114","state":"Idle"}"#;
        let f = c.form.clone();
        c.apply(&Action::Refresh, &f, s).unwrap();
        assert!(c.connected());
        assert_eq!(
            c.connected_device(),
            Some(("/dev/ttyUSB0", 115200, Some("BtWriter 1.3a.20221114")))
        );
        c.form.port = "/dev/different".into();
        let f = c.form.clone();
        c.apply(&Action::Refresh, &f, s).unwrap();
        assert_eq!(c.form.port, "/dev/different");
        assert!(!c.connected());
        c.last_poll = Instant::now();
        c.tx.send(Event::Done(
            Action::Connect,
            Box::new(f),
            Err("busy".into()),
        ))
        .unwrap();
        c.poll();
        assert!(c.connected_device().is_some());
        assert!(c.needs_device_status);
    }

    #[cfg(unix)]
    #[test]
    fn equivalent_tty_alias_preserves_selection_and_requires_matching_baud() {
        use std::os::unix::fs::symlink;
        let alias = env::temp_dir().join(request_id());
        symlink("/dev/null", &alias).unwrap();
        let mut c = Controller::default();
        c.form.port = alias.display().to_string();
        let f = c.form.clone();
        c.apply(
            &Action::Refresh,
            &f,
            r#"{"connected":true,"port":"/dev/null","baud":115200,"units":"mm"}"#,
        )
        .unwrap();
        assert!(c.connected());
        assert_eq!(c.form.port, alias.display().to_string());
        c.form.baud = 9600;
        assert!(!c.connected());
        fs::remove_file(alias).unwrap();
    }

    #[test]
    fn only_plotter_operation_receipts_identify_the_selected_machine() {
        let receipt = |op: &str| {
            format!(
                r#"{{"status":"completed","result":{{"status":"completed","results":[{{"operation":{},"value":{{"connected":false}}}}]}}}}"#,
                quote(op)
            )
        };
        assert!(device_receipt(&receipt("serial.status")).is_none());
        assert!(device_receipt(&receipt("plotter.status")).is_some());
    }

    #[test]
    fn only_local_calibration_receipts_advance_and_pin_is_not_pressure() {
        let mut c = Controller::default();
        let good = r#"{"connected":true,"port":"","baud":115200,"units":"mm","state":"Idle","work_position":[2,3,0.5],"pins":"P","pressure":null}"#;
        let f = c.form.clone();
        c.apply(&Action::Refresh, &f, good).unwrap();
        assert!(c.calibration_receipt().is_none());
        assert_eq!(c.probe_active(), Some(true));
        assert_eq!(c.pressure(), None);
        c.generation = 7;
        c.apply(&Action::CalibrationStatus, &f, good).unwrap();
        assert_eq!(
            c.calibration_receipt().unwrap().step,
            CalibrationStep::Status
        );
        assert_eq!(c.calibration_receipt().unwrap().request_id, 7);
        assert_eq!(c.calibration_receipt().unwrap().work, [2., 3., 0.5]);
        assert_eq!(c.form.pen_down, f.pen_down);
        c.generation = 8;
        c.apply(&Action::SetPenDown, &f, good).unwrap();
        assert_eq!(c.calibration_receipt().unwrap().request_id, 8);
        assert_eq!(
            c.calibration_receipt().unwrap().step,
            CalibrationStep::PenDown
        );
        assert_eq!(c.form.pen_down, 0.5);
        let f = c.form.clone();
        let bad = good.replace(r#""work_position""#, r#""machine_position""#);
        assert!(c.apply(&Action::SetPenUp, &f, &bad).is_err());
        assert_eq!(c.form.pen_up, f.pen_up);
        let sample = good.replace(
            r#""pressure":null"#,
            r#""pressure":0.25,"pressure_unit":"N""#,
        );
        c.apply(&Action::Refresh, &f, &sample).unwrap();
        assert_eq!((c.pressure(), c.pressure_unit()), (Some(0.25), Some("N")));
        c.apply(&Action::Refresh, &f, &good.replace("null", "0.25"))
            .unwrap();
        assert_eq!(c.pressure(), None);
    }
    #[test]
    fn epoch_tracks_device_external_and_unknown_effects_not_own_verified_steps() {
        let mut c = Controller::default();
        let s = r#"{"connected":true,"port":"/dev/fake","baud":115200,"state":"Idle","units":"mm","work_position":[0,0,4],"pins":"P","firmware":"1.3a"}"#;
        c.form.port = "/dev/fake".into();
        let f = c.form.clone();
        c.apply(&Action::Refresh, &f, s).unwrap();
        let start = c.calibration_epoch();
        assert!(start > 0);
        for action in [
            Action::CalibrationStatus,
            Action::SetPenUp,
            Action::SetPenDown,
        ] {
            let f = c.form.clone();
            c.apply(&action, &f, s).unwrap();
            let receipt = c.calibration_receipt().unwrap();
            assert_eq!(receipt.epoch, start);
            assert_eq!(receipt.probe_active, Some(true));
        }
        let f = c.form.clone();
        c.apply(
            &Action::Jog {
                axis: 'Z',
                delta: 0.1,
            },
            &f,
            s,
        )
        .unwrap();
        c.apply(&Action::Refresh, &f, s).unwrap();
        assert_eq!(c.calibration_epoch(), start);
        c.stop = Some(Arc::new(AtomicBool::new(false)));
        c.dispatch(Action::Cancel);
        assert!(c.calibration_epoch() > start && c.calibration_receipt().is_none());
        c.stop = None;
        c.snapshot = Some(Snapshot {
            pid: 1,
            rev: 1,
            active: None,
            last: Some(4),
        });
        c.last_poll = Instant::now();
        let ext = Snapshot {
            pid: 1,
            rev: 2,
            active: None,
            last: Some(5),
        };
        // Simulate an externally completed move. A fresh correlated plotter job is required.
        c.tried_revision = Some((1, 2));
        let cancelled = c.calibration_epoch();
        c.tx.send(Event::Observed(Ok((ext, None)), c.generation))
            .unwrap();
        c.poll();
        assert!(c.calibration_epoch() > cancelled);
        let prev = c.calibration_epoch();
        c.tx.send(Event::Done(
            Action::Jog {
                axis: 'Z',
                delta: 0.1,
            },
            Box::new(f.clone()),
            Err("transport lost".into()),
        ))
        .unwrap();
        c.poll();
        assert!(c.calibration_epoch() > prev);
        let before = c.form.pen_down;
        assert!(
            c.apply(
                &Action::SetPenDown,
                &f,
                &s.replace("1.3a", "other firmware")
            )
            .is_err()
        );
        assert_eq!(c.form.pen_down, before);
    }

    #[test]
    fn another_windows_contiguous_passive_status_preserves_captures_without_resubmitting() {
        let mut c = Controller::default();
        let status = r#"{"connected":true,"port":"/dev/fake","baud":115200,"units":"mm","state":"Idle","work_position":[1,2,3]}"#;
        c.form.port = "/dev/fake".into();
        let f = c.form.clone();
        c.apply(&Action::SetPenDown, &f, status).unwrap();
        c.last_poll = Instant::now();
        c.snapshot = Some(Snapshot {
            pid: 1,
            rev: 10,
            active: None,
            last: Some(4),
        });
        let epoch = c.calibration_epoch();
        let active = Snapshot {
            pid: 1,
            rev: 11,
            active: Some(5),
            last: Some(4),
        };
        let end = Snapshot {
            pid: 1,
            rev: 12,
            active: None,
            last: Some(5),
        };
        let receipt = |ops: &str| {
            format!(
                r#"{{"job_id":5,"status":"completed","result":{{"status":"completed","results":[{ops}]}}}}"#
            )
        };
        let step = format!(r#"{{"operation":"plotter.status","value":{status}}}"#);
        c.tx.send(Event::Observed(Ok((active.clone(), None)), c.generation))
            .unwrap();
        c.poll();
        assert!(c.calibration_receipt().is_none());
        assert_eq!(c.calibration_epoch(), epoch);
        c.tx.send(Event::Observed(
            Ok((end.clone(), Some(receipt(&step)))),
            c.generation,
        ))
        .unwrap();
        c.poll();
        assert_eq!(c.calibration_receipt().unwrap().epoch, epoch);
        assert!(!c.busy());
        assert!(c.tried_revision.is_none());
        let mixed = receipt(&format!(r#"{{"operation":"plotter.jog"}},{step}"#));
        assert!(observed_operation(&active, &end, &mixed).is_none());
        let gap = Snapshot {
            pid: 1,
            rev: 8,
            active: None,
            last: Some(2),
        };
        assert!(observed_operation(&gap, &end, &receipt(&step)).is_none());
    }

    #[test]
    fn legacy_sampling_requires_exact_zero_effect_prepare_failure() {
        let rejected = r#"{"status":"failed","result":{"status":"failed","stage":"prepare","error_code":"preflight_error","error":"plotter.status: unknown argument work","completed":0,"results":[]}}"#;
        assert!(legacy_work_rejection(rejected));
        for other in [
            rejected.replace("prepare", "execute"),
            rejected.replace("argument work", "argument other"),
            rejected.replace(r#""completed":0"#, r#""completed":1"#),
        ] {
            assert!(!legacy_work_rejection(&other));
        }
        for a in [
            Action::CalibrationStatus,
            Action::SetPenUp,
            Action::SetPenDown,
        ] {
            assert_eq!(source(&a, &Form::default(), "").unwrap(), WORK_STATUS);
        }
    }

    #[test]
    fn external_cancel_is_bound_to_displayed_target() {
        let s =
            Snapshot::parse(r#"{"pid":500,"state_revision":8,"active_job_id":3,"last_job_id":2}"#)
                .unwrap();
        let c = Controller {
            snapshot: Some(s),
            ..Controller::default()
        };
        assert_eq!(c.cancel_target(), Some((500, 3, 8)));
        assert!(!c.owns_job());
        assert!(c.busy());
        assert!(value(r#"{"status":"failed","result":{"error":"unconfirmed block"}}"#).is_err());
    }
    const MOUNT_FRAME: &str = r#"{"connected":true,"port":"writer-sim","baud":115200,"controller":"BtWriter","firmware":"1.3a.20221114","device_id":"unit-a","state":"Idle","units":"mm","report_units":"mm","work_position":[4,5,6],"work_offset":[1,2,3]}"#;
    const ACK: &str = r#"{"connected":true,"port":"writer-sim","baud":115200,"state":"Idle"}"#;
    const BOX: MachineBounds = MachineBounds {
        min: [0., 1., 2.],
        max: [5., 6., 7.],
    };
    const BOUND_ACK: &str = r#"{"connected":true,"port":"writer-sim","baud":115200,"bounds":{"x_min":0,"x_max":5,"y_min":1,"y_max":6,"z_min":2,"z_max":7},"work_offset":[1,2,3]}"#;

    #[test]
    fn held_jog_correlates_observed_endpoint_and_single_axis_with_retained_device() {
        let frame = MOUNT_FRAME.replace(
            r#""baud":115200"#,
            r#""baud":115200,"reference_generation":12"#,
        );
        let mut c = Controller::default();
        c.position(&frame).unwrap();
        let device = c.link.as_ref().unwrap();
        let program = held_jog_program('Y', -0.16, 40., device).unwrap();
        assert_eq!(
            program,
            r#"plotter.jog_held(axis="y",delta=-0.16,feed=40,expected_generation=12,firmware="1.3a.20221114",device_id="unit-a",wco_x=1,wco_y=2,wco_z=3,timeout=12000)"#
        );
        let held = MountAction::Jog {
            axis: 'Y',
            delta: -0.16,
            feed: 40.,
        };
        let receipt=frame.replace(r#""state":"Idle""#,r#""state":"Idle","axis":"Y","from_mm":5.163,"target_mm":5.003,"axis_tolerance_mm":0.006751,"feed_mm_min":40"#);
        let snapshot = Some(Snapshot {
            pid: 1,
            rev: 2,
            active: None,
            last: Some(7),
        });
        assert!(confirmed_held_jog(held, device, (receipt.clone(), snapshot.clone())).is_ok());
        for bad in [
            receipt.replace(r#""axis":"Y""#, r#""axis":"X""#),
            receipt.replace("5.163", "5.3"),
            receipt.replace("5.003", "5.03"),
            receipt.replace("generation\":12", "generation\":13"),
            receipt.replace("0.006751", "2.0"),
        ] {
            assert!(confirmed_held_jog(held, device, (bad, snapshot.clone())).is_err());
        }
        assert!(confirmed_held_jog(held, device, (receipt, None)).is_err());
    }

    // Simulate a named-session CLI's correlated, contiguous finished jobs without a port.
    fn simulate_mount(
        kind: MountAction,
        reference: Option<[f64; 3]>,
        outputs: &[Result<&str>],
    ) -> (Result<String>, Vec<String>) {
        simulate_mount_on(MOUNT_FRAME, kind, reference, outputs)
    }
    fn simulate_mount_on(
        initial: &str,
        kind: MountAction,
        reference: Option<[f64; 3]>,
        outputs: &[Result<&str>],
    ) -> (Result<String>, Vec<String>) {
        let mut c = Controller::default();
        c.position(initial).unwrap();
        let snapshot = Snapshot {
            pid: 1,
            rev: 0,
            active: None,
            last: None,
        };
        let mut calls = vec![];
        let result = mount_job(
            &Action::Mount(kind, reference),
            Some(&snapshot),
            c.link.as_ref(),
            |_, program, last| {
                let i = calls.len();
                assert_eq!(last.rev, 2 * i as u64);
                calls.push(program.to_string());
                let output = outputs
                    .get(i)
                    .expect("unexpected operation")
                    .as_ref()
                    .map_err(|e| e.clone())?;
                Ok((
                    output.to_string(),
                    Some(Snapshot {
                        rev: last.rev + 2,
                        last: Some(i as u64 + 1),
                        ..last.clone()
                    }),
                ))
            },
        );
        (result.map(|v| v.0), calls)
    }

    #[test]
    fn teaching_jogs_use_one_linear_axis_and_the_calibration_feed() {
        for (axis, delta) in [('x', -10.), ('Y', 0.001), ('z', 0.5)] {
            let feed = if axis.eq_ignore_ascii_case(&'z') {
                40.
            } else {
                300.
            };
            let p = MountAction::Jog { axis, delta, feed }
                .program(None, None)
                .unwrap();
            assert!(
                p.contains(if axis.eq_ignore_ascii_case(&'z') {
                    "feed=40"
                } else {
                    "feed=300"
                }) && p.contains(&format!("axis=\"{}\"", axis.to_ascii_lowercase()))
            );
        }
        for (axis, delta) in [
            ('a', 0.1),
            ('Z', 0.501),
            ('X', 10.001),
            ('y', f64::NAN),
            ('z', 0.),
        ] {
            assert!(
                MountAction::Jog {
                    axis,
                    delta,
                    feed: 40.
                }
                .program(None, None)
                .is_err()
            );
        }
        assert!(
            MountAction::MoveZ(f64::INFINITY)
                .program(None, None)
                .is_err()
        );
        assert!(MountAction::ApplyBounds(BOX).program(None, None).is_err());
        assert!(
            MountAction::ApplyBounds(MachineBounds {
                min: BOX.max,
                max: BOX.min
            })
            .program(Some([0.; 3]), None)
            .is_err()
        );
    }

    #[test]
    fn scoped_pen_samples_and_mount_receipts_never_change_write_heights() {
        let mut c = Controller::default();
        c.apply(&Action::Refresh, &c.form.clone(), MOUNT_FRAME)
            .unwrap();
        let f = c.form.clone();
        c.generation = 73;
        for step in [CalibrationStep::PenUp, CalibrationStep::PenDown] {
            c.apply(&Action::CalibrationCapture(step), &f, MOUNT_FRAME)
                .unwrap();
            let r = c.calibration_receipt().unwrap();
            assert_eq!((r.request_id, r.step, r.work[2]), (73, step, 6.));
            assert_eq!(c.form, f);
        }
        let k = MountAction::Status;
        c.apply(&Action::Mount(k, None), &f, MOUNT_FRAME).unwrap();
        let r = c.mount_receipt().unwrap();
        assert_eq!(
            (r.request_id, r.work, r.work_offset, r.action),
            (73, [4., 5., 6.], Some([1., 2., 3.]), k)
        );
        assert_eq!(c.hardware_id(), Some("unit-a"));
        assert_eq!(c.controller_identity(), Some(("BtWriter", "1.3a.20221114")));
        let epoch = c.calibration_epoch();
        c.position(&MOUNT_FRAME.replace("[1,2,3]", "[2,2,3]"))
            .unwrap();
        assert_ne!(c.calibration_epoch(), epoch);
        assert!(c.mount_receipt().is_none());
        assert_eq!(c.form, f);
    }

    #[test]
    fn mount_z_waits_for_same_device_verified_final_work_position() {
        let final_frame = MOUNT_FRAME.replace("[4,5,6]", "[4,5,2.5]");
        let k = MountAction::MoveZ(2.5);
        let (out, calls) = simulate_mount(
            k,
            Some([1., 2., 3.]),
            &[Ok(MOUNT_FRAME), Ok(ACK), Ok(&final_frame)],
        );
        assert_eq!(out.unwrap(), final_frame);
        assert_eq!(
            calls,
            [
                MOUNT_STATUS,
                "plotter.pen(z=2.5,feed=40,timeout=300000)",
                MOUNT_STATUS
            ]
        );
        for frame in [
            MOUNT_FRAME.to_string(),
            final_frame.replace("unit-a", "unit-b"),
            final_frame.replace("[1,2,3]", "[0,0,0]"),
            final_frame.replace("Idle", "Run"),
        ] {
            let (out, calls) = simulate_mount(k, None, &[Ok(MOUNT_FRAME), Ok(ACK), Ok(&frame)]);
            assert!(out.is_err());
            assert_eq!(calls.len(), 3);
        }
        let (out, calls) = simulate_mount(
            k,
            None,
            &[Ok(MOUNT_FRAME), Err("timeout, outcome unknown".into())],
        );
        assert!(out.is_err());
        assert_eq!(calls.len(), 2);
        let (out, calls) = simulate_mount(k, Some([0.; 3]), &[Ok(MOUNT_FRAME)]);
        assert!(out.is_err());
        assert_eq!(calls, [MOUNT_STATUS]);
    }

    #[test]
    fn fresh_bounds_compare_all_six_limits_and_work_reference() {
        let k = MountAction::ApplyBounds(BOX);
        let wco = Some([1., 2., 3.]);
        let (out, calls) =
            simulate_mount(k, wco, &[Ok(MOUNT_FRAME), Ok(BOUND_ACK), Ok(MOUNT_FRAME)]);
        assert!(out.is_ok());
        assert_eq!(calls.len(), 3);
        assert!(calls[1].contains("wco_x=1,wco_y=2,wco_z=3"));
        for wrong in [
            BOUND_ACK.replace("x_max\":5", "x_max\":4"),
            BOUND_ACK.replace("[1,2,3]", "[1,2,4]"),
            BOUND_ACK.replace("writer-sim", "other"),
        ] {
            let (out, calls) = simulate_mount(k, wco, &[Ok(MOUNT_FRAME), Ok(&wrong)]);
            assert!(out.is_err());
            assert_eq!(calls.len(), 2);
        }
        let (out, calls) = simulate_mount(
            k,
            wco,
            &[
                Ok(MOUNT_FRAME),
                Err("unsupported plotter.set_bounds".into()),
            ],
        );
        assert!(out.is_err());
        assert_eq!(calls.len(), 2);
    }

    #[test]
    fn read_only_mount_status_requires_same_frame_work_offset() {
        let k = MountAction::Status;
        for frame in [
            MOUNT_FRAME.replace(",\"work_offset\":[1,2,3]", ""),
            MOUNT_FRAME.replace("Idle", "Alarm"),
            MOUNT_FRAME.replace("writer-sim", "other"),
        ] {
            let (out, calls) = simulate_mount(k, None, &[Ok(&frame)]);
            assert!(out.is_err());
            assert_eq!(calls, [MOUNT_STATUS]);
        }
        let both = MOUNT_FRAME.replace("\"work_offset\":[1,2,3]", "\"machine_position\":[5,7,9]");
        let (out, calls) = simulate_mount(k, None, &[Ok(&both)]);
        assert!(out.is_ok());
        assert_eq!(calls, [MOUNT_STATUS]);
    }

    #[test]
    fn screen_accepts_exactly_one_parsed_operation_in_its_selected_mode() {
        for (src, p) in [
            ("write.preview(text=\"a;plotter.connect()\")", true),
            ("write.mouse()", false),
            ("write.touchscreen()", false),
            ("write.touchpad()", false),
        ] {
            assert_eq!(ScreenRequest::new(src.into(), p).unwrap().source, src);
        }
        let mut c = Controller::default();
        for (src, p) in [
            ("write.mouse()", true),
            ("write.preview()", false),
            ("plotter.pen(z=2)", false),
            ("write.preview();plotter.disconnect()", true),
            ("write.mouse()\n+", false),
            ("# comment", true),
        ] {
            assert_eq!(c.dispatch_screen(src.into(), p), None);
            assert!(c.screen_receipt().is_none() && !c.owns_job());
        }
    }

    #[test]
    fn another_windows_single_screen_job_preserves_plotter_position_and_calibration() {
        let mut c = Controller::default();
        c.apply(&Action::Refresh, &c.form.clone(), MOUNT_FRAME)
            .unwrap();
        c.apply(
            &Action::CalibrationCapture(CalibrationStep::PenUp),
            &c.form.clone(),
            MOUNT_FRAME,
        )
        .unwrap();
        c.last_poll = Instant::now();
        let epoch = c.calibration_epoch();
        c.snapshot = Some(Snapshot {
            pid: 3,
            rev: 10,
            active: None,
            last: Some(4),
        });
        let active = Snapshot {
            pid: 3,
            rev: 11,
            active: Some(5),
            last: Some(4),
        };
        let end = Snapshot {
            pid: 3,
            rev: 12,
            active: None,
            last: Some(5),
        };
        let out = r#"{"job_id":5,"status":"completed","result":{"status":"completed","results":[{"operation":"write.touchscreen","value":{"strokes":2}}]}}"#;
        c.tx.send(Event::Observed(Ok((active, None)), c.generation))
            .unwrap();
        c.poll();
        assert!(c.calibration_receipt().is_none());
        c.tx.send(Event::Observed(Ok((end, Some(out.into()))), c.generation))
            .unwrap();
        c.poll();
        assert_eq!(c.calibration_receipt().unwrap().epoch, epoch);
        assert!(!c.needs_device_status && !c.owns_job());
        assert_eq!(c.wpos, Some([4., 5., 6.]));
        assert_eq!(c.work_offset(), Some([1., 2., 3.]));
        assert!(c.screen_receipt().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn screen_preview_and_output_use_named_session_without_touching_a_serial_port() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(request_id());
        fs::create_dir(&dir).unwrap();
        let exe = dir.join("zuse-test");
        fs::write(&exe,r##"#!/bin/sh
set -eu
d=${0%/*}
job=0
if [ -e "$d/job" ]; then read -r job < "$d/job"; fi
case "$2" in
status)
    if [ "$job" -eq 0 ]; then last=null; else last=$job; fi
    printf '{"ok":true,"session":"screen-test","pid":1,"state_revision":%s,"active_job_id":null,"last_job_id":%s}\n' "$((2*job))" "$last"
    ;;
observe|script)
    [ "$3" = "screen-test" ] && [ "$4" = "-" ] && [ "$5" = "--request-id" ] && [ "$8" = "$((2*job))" ] && [ "${10}" = "600000" ]
    body=$(cat)
    printf '%s %s\n' "$2" "$body" >> "$d/log"
    job=$((job+1)); printf '%s\n' "$job" > "$d/job"
    printf '{"ok":true,"session":"screen-test","status":"accepted","job_id":%s,"request_id":"%s","state_revision":%s}\n' "$job" "$6" "$((2*job-1))"
    ;;
result)
    case "$job" in 1) op=write.preview;; 2) op=write.mouse;; *) op=other;; esac
    printf '{"ok":true,"session":"screen-test","job_id":%s,"status":"completed","result":{"status":"completed","results":[{"operation":"%s","value":{"lines":2}}]}}\n' "$job" "$op"
    ;;
*) exit 3;;
esac
"##).unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
        let mut c = Controller {
            session: Session {
                bin: exe.into_os_string(),
                name: "screen-test".into(),
            },
            ..Controller::default()
        };
        let epoch = c.calibration_epoch();
        let mut tokens = Vec::new();
        for (src, preview, match_expected) in [
            ("write.preview(text=\"abc\")", true, true),
            ("write.mouse(text=\"abc\")", false, true),
            ("write.touchpad(text=\"abc\")", false, false),
        ] {
            let token = c.dispatch_screen(src.into(), preview).unwrap();
            tokens.push(token);
            assert_eq!(c.dispatch_screen(src.into(), preview), None);
            assert!(c.screen_receipt().is_none());
            let deadline = Instant::now() + Duration::from_secs(5);
            while c.busy() {
                c.poll();
                assert!(Instant::now() < deadline, "{}", c.status);
                thread::sleep(Duration::from_millis(5));
            }
            if match_expected {
                assert_eq!(c.screen_receipt(), Some((token, r#"{"lines":2}"#)));
            } else {
                assert!(c.screen_receipt().is_none());
                assert!(c.status.contains("回执与请求"));
            }
        }
        assert_eq!(tokens, [1, 2, 3]);
        assert_eq!(c.calibration_epoch(), epoch);
        assert_eq!(
            fs::read_to_string(dir.join("log")).unwrap(),
            "observe write.preview(text=\"abc\")\nscript write.mouse(text=\"abc\")\nscript write.touchpad(text=\"abc\")\n"
        );
        fs::remove_dir_all(dir).unwrap();
    }
    fn with_generation(frame: &str, value: &str) -> String {
        let rest = frame.strip_prefix('{').unwrap();
        format!(r#"{{"reference_generation":{value},{rest}"#)
    }

    #[test]
    fn reset_invalidates_partial_teaching_even_with_the_same_identity_wco_and_xyz() {
        let zero = with_generation(MOUNT_FRAME, "0");
        let one = with_generation(MOUNT_FRAME, "1");
        let mut c = Controller::default();
        c.apply(&Action::Refresh, &c.form.clone(), &zero).unwrap();
        let form = c.form.clone();
        c.apply(
            &Action::CalibrationCapture(CalibrationStep::PenUp),
            &form,
            &zero,
        )
        .unwrap();
        c.apply(&Action::Mount(MountAction::Status, None), &form, &zero)
            .unwrap();
        let epoch = c.calibration_epoch();
        c.apply(&Action::Refresh, &form, &zero).unwrap();
        assert_eq!(epoch, c.calibration_epoch());
        assert!(c.mount_receipt().is_some());
        assert!(
            c.apply(
                &Action::CalibrationCapture(CalibrationStep::PenDown),
                &form,
                &one
            )
            .is_err()
        );
        assert!(c.calibration_epoch() > epoch);
        assert!(c.calibration_receipt().is_none() && c.mount_receipt().is_none());
        c.apply(
            &Action::CalibrationCapture(CalibrationStep::PenUp),
            &form,
            &one,
        )
        .unwrap();
        let epoch = c.calibration_epoch();
        c.apply(&Action::Check, &form, &with_generation(ACK, "2"))
            .unwrap();
        assert!(c.calibration_epoch() > epoch);
        assert!(c.needs_device_status && c.calibration_receipt().is_none());
        assert_eq!(c.form, form);
    }

    #[test]
    fn switched_or_missing_generations_abort_before_motion_or_at_the_first_unconfirmed_receipt() {
        let seven = with_generation(MOUNT_FRAME, "7");
        let eight = with_generation(MOUNT_FRAME, "8");
        let final_seven = seven.replace("[4,5,6]", "[4,5,5]");
        let final_eight = eight.replace("[4,5,6]", "[4,5,5]");
        let ack = with_generation(ACK, "7");
        let kind = MountAction::MoveZ(5.);
        let run =
            |outputs: &[Result<&str>]| simulate_mount_on(&seven, kind, Some([1., 2., 3.]), outputs);
        for changed in [
            &eight,
            &MOUNT_FRAME.to_owned(),
            &with_generation(MOUNT_FRAME, "null"),
        ] {
            let (result, calls) = run(&[Ok(changed)]);
            assert!(result.is_err());
            assert_eq!(calls, [MOUNT_STATUS]);
        }
        let (out, calls) = run(&[Ok(&seven), Ok(&with_generation(ACK, "8"))]);
        assert!(out.is_err());
        assert_eq!(calls.len(), 2);
        let (out, calls) = run(&[Ok(&seven), Ok(&ack), Ok(&final_eight)]);
        assert!(out.is_err());
        assert_eq!(calls.len(), 3);
        let (out, calls) = run(&[Ok(&seven), Ok(&ack), Ok(&final_seven)]);
        assert!(out.is_ok());
        assert_eq!(calls.len(), 3);
    }

    #[test]
    fn set_bounds_binds_the_session_generation_and_refuses_an_unconfirmed_echo() {
        let status = with_generation(MOUNT_FRAME, "9182736450");
        let ack = with_generation(BOUND_ACK, "9182736450");
        let run = |outputs: &[Result<&str>]| {
            simulate_mount_on(
                &status,
                MountAction::ApplyBounds(BOX),
                Some([1., 2., 3.]),
                outputs,
            )
        };
        let (result, calls) = run(&[Ok(&status), Ok(&ack), Ok(&status)]);
        assert!(result.is_ok());
        assert!(calls[1].ends_with(",expected_generation=9182736450)"));
        let (result, calls) = run(&[Ok(&status), Ok(BOUND_ACK)]);
        assert!(result.is_err());
        assert_eq!(calls.len(), 2);
        let (result, calls) = run(&[
            Ok(&status),
            Err("reference changed before applying bounds".into()),
        ]);
        assert!(result.is_err());
        assert_eq!(calls.len(), 2);
        let (result, calls) = simulate_mount(
            MountAction::ApplyBounds(BOX),
            Some([1., 2., 3.]),
            &[Ok(MOUNT_FRAME), Ok(BOUND_ACK), Ok(MOUNT_FRAME)],
        );
        assert!(result.is_ok());
        assert!(!calls[1].contains("expected_generation"));
    }

    #[test]
    fn generation_capability_changes_cannot_merge_with_legacy_samples() {
        let mut c = Controller::default();
        c.apply(&Action::Refresh, &c.form.clone(), MOUNT_FRAME)
            .unwrap();
        let form = c.form.clone();
        c.apply(
            &Action::CalibrationCapture(CalibrationStep::PenUp),
            &form,
            MOUNT_FRAME,
        )
        .unwrap();
        let epoch = c.calibration_epoch();
        let fresh = with_generation(MOUNT_FRAME, "2");
        assert!(
            c.apply(
                &Action::CalibrationCapture(CalibrationStep::PenDown),
                &form,
                &fresh
            )
            .is_err()
        );
        assert!(c.calibration_epoch() > epoch && c.calibration_receipt().is_none());
        assert!(c.apply(&Action::Refresh, &form, MOUNT_FRAME).is_err());
        assert_eq!(c.link.as_ref().unwrap().reference_generation, Some(2));
        for malformed in ["null", "-1", "3.2", "\"2\"", "9223372036854775808"] {
            assert!(reported_generation(&with_generation(ACK, malformed)).is_err());
        }
    }
}
