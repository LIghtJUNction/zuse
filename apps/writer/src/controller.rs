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
    pub offset_x: f64,
    pub offset_y: f64,
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
            offset_x: 0.,
            offset_y: 0.,
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
pub enum Action {
    Preview,
    Connect,
    Refresh,
    Disconnect,
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

fn source(action: &Action, f: &Form, program: &str) -> Result<String> {
    let text = match action {
        Action::Preview => format!(
            "write.gcode(text={},font={},size={},advance={},line_height={},x={},y={},offset_x={},offset_y={},reference_units={},reference_distance={},swap_xy={},flip_x={},flip_y={},seed={},pen_up={},pen_down={},feed={})",
            quote(&f.text),
            quote(&f.font),
            f.size,
            f.advance,
            f.line_height,
            f.x,
            f.y,
            f.offset_x,
            f.offset_y,
            f.reference_units,
            f.reference_distance,
            f.swap_xy,
            f.flip_x,
            f.flip_y,
            f.seed,
            f.pen_up,
            f.pen_down,
            f.feed
        ),
        Action::Connect => format!("plotter.connect(port={},baud={})", quote(&f.port), f.baud),
        Action::Refresh | Action::SetPenUp | Action::SetPenDown => "plotter.status()".into(),
        Action::Disconnect => "plotter.disconnect()".into(),
        Action::Jog { axis, delta } => {
            let a = axis.to_ascii_lowercase();
            if !matches!(a, 'x' | 'y' | 'z')
                || !delta.is_finite()
                || delta.abs() < 0.001
                || delta.abs() > 10.
            {
                return Err("点动每次限单轴 0.001–10 mm".into());
            }
            // A 10 mm move at F40 needs 15 s before the final status query.
            format!("plotter.jog(axis=\"{a}\",delta={delta},feed=40,timeout=20000)")
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
        Action::Send => format!("plotter.send(program={},timeout=300000)", quote(program)),
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
fn preview_paths(program: &str) -> Result<Strokes> {
    let mut strokes = Vec::new();
    let mut next = None;
    let mut drawing = false;
    for line in program.lines() {
        if line.starts_with("G0 Z") {
            drawing = false;
            continue;
        }
        if line.starts_with("G1 Z") {
            let start = next.take().ok_or("落笔前缺少 XY 起点")?;
            strokes.push(vec![start]);
            drawing = true;
            continue;
        }
        if matches!(line, "G21 G17 G90 G94" | "M2") {
            continue;
        }
        let words: Vec<_> = line.split_ascii_whitespace().collect();
        if let [motion, x, y] = words.as_slice() {
            let xy = |s: &str, axis| {
                s.strip_prefix(axis)
                    .and_then(|v| v.parse::<f64>().ok())
                    .filter(|v| v.is_finite())
                    .ok_or("笔迹坐标无效")
            };
            let pt = (xy(x, 'X')?, xy(y, 'Y')?);
            match *motion {
                "G0" if !drawing => {
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
        Err(_) if expected.is_none() && matches!(action, Action::Connect | Action::Preview) => {
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
    let command = if matches!(
        action,
        Action::Refresh | Action::SetPenUp | Action::SetPenDown
    ) {
        "observe"
    } else {
        "script"
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
            "300000",
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
    let until = Instant::now() + Duration::from_secs(320);
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
            return Ok((value(&out)?, current.then_some(after)));
        }
        if Instant::now() > until {
            return Err("等待作业超时，结果未知；未重发".into());
        }
        thread::sleep(Duration::from_millis(160));
    }
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

#[derive(Debug)]
struct Device {
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

pub struct Controller {
    pub form: Form,
    pub strokes: Strokes,
    pub gcode: String,
    pub status: String,
    pub mpos: Option<[f64; 3]>,
    pub wpos: Option<[f64; 3]>,
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
            form: Form::default(),
            strokes: vec![],
            gcode: String::new(),
            status: "就绪".into(),
            mpos: None,
            wpos: None,
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
    fn mark_device_stale(&mut self) {
        self.needs_device_status = true;
        self.mpos = None;
        self.wpos = None;
    }
    fn forget_device(&mut self) {
        self.link = None;
        self.mpos = None;
        self.wpos = None;
    }
    fn invalidate_plan(&mut self) {
        self.plan = None;
        self.gcode.clear();
        self.strokes.clear();
    }
    pub fn dispatch(&mut self, action: Action) {
        if matches!(action, Action::Cancel) {
            if self.cancel_pending {
                return;
            }
            if let Some(flag) = &self.stop {
                flag.store(true, Ordering::Release);
                self.status = "已请求取消，等待作业回执".into();
            } else if let Some(target) = self.snapshot.clone().filter(|s| s.active.is_some()) {
                self.cancel_pending = true;
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
                | Action::Send
                | Action::Check
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
            self.sent = true
        }
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = Some(stop.clone());
        self.generation += 1;
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
        let m = coords(payload, "machine_position")?;
        let w = coords(payload, "work_position")?;
        let off = coords(payload, "work_offset")?;
        let mpos = m.or_else(|| {
            w.zip(off)
                .map(|(w, o)| std::array::from_fn(|i| w[i] + o[i]))
        });
        let wpos = w.or_else(|| {
            m.zip(off)
                .map(|(m, o)| std::array::from_fn(|i| m[i] - o[i]))
        });
        if mpos.iter().chain(&wpos).flatten().any(|n| !n.is_finite()) {
            return Err("GRBL 坐标超出范围".into());
        }
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
        self.mpos = mpos;
        self.wpos = wpos;
        self.link = Some(Device {
            #[cfg(unix)]
            tty: tty_identity(&port),
            port,
            baud,
            label,
        });
        self.needs_device_status = false;
        Ok(())
    }

    fn apply(&mut self, action: &Action, f: &Form, payload: &str) -> Result<String> {
        match action {
            Action::Preview => {
                if f != &self.form {
                    return Err("预览期间参数已变化，请重新生成".into());
                }
                let code = json::field(payload, "gcode")?;
                let paths = preview_paths(&code)?;
                let msg = format!("预览 · {} 笔 · seed {}", paths.len(), f.seed);
                self.plan = Some((f.clone(), code.clone()));
                self.gcode = code;
                self.strokes = paths;
                self.sent = false;
                Ok(msg)
            }
            Action::Connect | Action::Refresh | Action::SetPenUp | Action::SetPenDown => {
                self.position(payload)?;
                if matches!(action, Action::SetPenUp | Action::SetPenDown) {
                    if f != &self.form || field(payload, "state").as_deref() != Some("Idle") {
                        return Err("标定需要空闲控制器和相同的参数".into());
                    }
                    let z = self
                        .wpos
                        .ok_or("缺少当前工作坐标 WPos 或 MPos/WCO，无法记录笔位")?[2];
                    if !self.connected() {
                        return Err("所选设备与标定回执不符".into());
                    }
                    if matches!(action, Action::SetPenUp) {
                        self.form.pen_up = z
                    } else {
                        self.form.pen_down = z
                    }
                    self.invalidate_plan();
                    return Ok(format!("已记录工作坐标 Z={z:.3}"));
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
            Action::Export(_) => Ok(format!("已导出 {payload}")),
            Action::Check => Ok(format!("控制器校验完成 · {payload}")),
            Action::Send | Action::Jog { .. } | Action::PenUp | Action::PenDown => {
                self.mpos = None;
                self.wpos = None;
                if field(payload, "units").as_deref() == Some("mm") {
                    self.position(payload)?
                }
                Ok(format!("控制器回执 · {payload}"))
            }
            Action::Cancel => unreachable!(),
        }
    }

    pub fn poll(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Accepted(s) => {
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
                    self.stop = None;
                    self.auto_refresh = false;
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
                                if !matches!(action, Action::Preview | Action::Export(_)) {
                                    self.mark_device_stale();
                                }
                                e
                            })
                        }
                        Err(e) => {
                            if !matches!(action, Action::Preview | Action::Export(_)) {
                                self.mark_device_stale();
                            }
                            if (e.contains("unknown") || e.contains("unsupported"))
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
                                self.mark_device_stale();
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
                                if let Ok(label) = self.apply(&Action::Refresh, &f, &data) {
                                    self.status = label;
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
                                self.mark_device_stale();
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
    fn preview_uses_exported_machine_coordinates() {
        assert_eq!(
            preview_paths(PROGRAM).unwrap(),
            vec![vec![(10., 20.), (12.5, 18.5)]]
        );
        assert!(preview_paths("G1 X0 Y0\n").is_err());
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
            .is_err()
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
}
