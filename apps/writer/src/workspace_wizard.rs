use crate::controller::{Controller, MachineBounds, MountAction};
use crate::workspace_profile::draft::Draft;
use crate::workspace_profile::{Binding, WorkspaceProfile, load_profile, save_profile};
use std::time::{Duration, Instant};
use zgpui::native::{ButtonStyle, Canvas, Event, Rect, Ui};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Step {
    #[default]
    TopLeft,
    BottomRight,
    Mount,
    Fit,
    Restore,
    VerifyDraft,
}

#[derive(Clone, Copy)]
enum Request {
    Corner(usize),
    Jog(MountAction),
    Load,
    Resume,
    Position,
    Positioned,
    Apply { restore: bool },
    Applied { restore: bool },
}

#[derive(Clone, Copy)]
struct JogButton {
    area: Rect,
    axis: char,
    sign: f64,
}

#[derive(Debug, PartialEq)]
enum KeyJog {
    Unhandled,
    Discard,
    Segment(char, f64),
}

struct Hold {
    button: JogButton,
    armed_at: Instant,
    repeated: bool,
    stopped: bool,
}

#[derive(Default)]
pub(crate) struct WorkspaceWizard {
    step: Step,
    epoch: Option<u64>,
    draft: Option<Draft>,
    checked_disk: bool,
    verify_when_idle: bool,
    offset: Option<[f64; 3]>,
    binding: Option<Binding>,
    corners: [Option<[f64; 2]>; 2],
    plan: Option<WorkspaceProfile>,
    pending: Option<(u64, Request)>,
    jog_buttons: Vec<JogButton>,
    hold: Option<Hold>,
    suppress_click: bool,
    keyboard: bool,
    keyboard_button: Option<Rect>,
    notice: String,
    details: bool,
    last_feed: [Option<f64>; 2],
    drawn_size: Option<(f32, f32)>,
}

pub(crate) enum Outcome {
    Stay,
    Secondary,
    Done(String),
}

fn binding(controller: &Controller, offset: [f64; 3]) -> Option<Binding> {
    let (model, firmware) = controller.controller_identity()?;
    Binding::new(controller.hardware_id()?, model, firmware, offset).ok()
}

fn inside(plan: &WorkspaceProfile, work: [f64; 3]) -> bool {
    (0..3).all(|i| (plan.min[i]..=plan.max[i]).contains(&work[i]))
}

fn fitted(plan: &WorkspaceProfile, work: [f64; 3]) -> bool {
    inside(plan, work) && (plan.mount_z - work[2]).abs() <= 0.001
}

fn held_feed(feed: f64) -> Result<f64, String> {
    if (40. ..=500.).contains(&feed) {
        Ok(feed)
    } else {
        Err(format!(
            "当前请求 {feed} mm/min。长按或键盘连续点动要求 40–500 mm/min，请调整速度后重试。"
        ))
    }
}

// 240 ms of commanded travel. At 40..=500 mm/min this is 0.16..=2 mm.
fn segment_mm(feed: f64) -> f64 {
    (feed * 0.24 / 60. * 1000.).round() / 1000.
}

impl WorkspaceWizard {
    pub(crate) fn height(&self, width: f32) -> f32 {
        self.drawn_size
            .filter(|(previous, _)| (previous - width).abs() < 1.)
            .map_or(565., |(_, height)| height)
    }

    pub(crate) fn pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn holding(&self) -> bool {
        self.hold.as_ref().is_some_and(|hold| !hold.stopped)
    }

    pub(crate) fn pointer(&mut self, event: &Event, ready: bool) {
        if matches!(event, Event::Down(x, y) if !self.keyboard_button.is_some_and(|r| r.contains(*x, *y)))
            || matches!(
                event,
                Event::Scroll(_) | Event::Resize | Event::Close | Event::Text(_)
            )
            || matches!(event, Event::Key { code, control, shift, .. }
                if *control || *shift || !matches!(code, 13 | 32 | 1001..=1004))
        {
            self.keyboard = false;
        }
        match *event {
            Event::Down(x, y) if ready => {
                self.hold = self
                    .jog_buttons
                    .iter()
                    .find(|button| button.area.contains(x, y))
                    .map(|button| Hold {
                        button: *button,
                        armed_at: Instant::now(),
                        repeated: false,
                        stopped: false,
                    });
            }
            Event::Up(..) => {
                self.suppress_click = self.hold.take().is_some_and(|hold| {
                    hold.repeated || hold.stopped || hold.armed_at.elapsed() >= POINTER_HOLD_DELAY
                });
            }
            Event::Move(x, y) => {
                if let Some(hold) = &mut self.hold
                    && !hold.button.area.contains(x, y)
                {
                    hold.stopped = true;
                }
            }
            Event::Scroll(_) | Event::Resize | Event::Close => {
                if let Some(hold) = &mut self.hold {
                    hold.stopped = true;
                }
            }
            _ => {}
        }
    }

    // With zgpui 0.1.9 the only keyboard evidence is a Key event. A fresh
    // press/repeat authorizes one segment; neither timers nor job completions
    // extend that authorization. Never retain a keypress while a job is busy.
    fn key_jog(&self, event: &Event, fresh: bool, idle: bool) -> KeyJog {
        let Event::Key {
            code,
            control: false,
            shift: false,
            ..
        } = event
        else {
            return KeyJog::Unhandled;
        };
        let (axis, sign) = match code {
            1001 => ('X', -1.),
            1002 => ('X', 1.),
            1003 => ('Y', 1.),
            1004 => ('Y', -1.),
            _ => return KeyJog::Unhandled,
        };
        if !self.keyboard || !matches!(self.step, Step::TopLeft | Step::BottomRight) {
            return KeyJog::Unhandled;
        }
        if fresh && idle && !self.pending() && !self.holding() {
            KeyJog::Segment(axis, sign)
        } else {
            KeyJog::Discard
        }
    }

    // Some(redraw) means the event belongs to the pad. Busy repeats need no paint.
    pub(crate) fn keyboard_input(
        &mut self,
        event: &Event,
        fresh: bool,
        c: &mut Controller,
    ) -> Option<bool> {
        match self.key_jog(event, fresh, c.connected() && !c.busy()) {
            KeyJog::Unhandled => None,
            KeyJog::Discard => Some(false),
            KeyJog::Segment(axis, sign) => {
                self.segment(axis, sign, c);
                Some(true)
            }
        }
    }

    fn segment(&mut self, axis: char, sign: f64, c: &mut Controller) {
        let feed = match c.form.xy_jog_feed().and_then(held_feed) {
            Ok(speed) => speed,
            Err(error) => {
                self.stop_holding();
                self.notice = error;
                return;
            }
        };
        let delta = sign * segment_mm(feed);
        let token = c.jog_held(axis, delta, feed);
        if token.is_none() {
            self.stop_holding();
        }
        self.request(
            token,
            Request::Jog(MountAction::Jog { axis, delta, feed }),
            c,
        );
    }

    fn stop_holding(&mut self) {
        self.keyboard = false;
        if let Some(hold) = &mut self.hold {
            hold.stopped = true;
        }
    }

    fn repeat_jog(&mut self, ready: bool, c: &mut Controller) {
        let Some(hold) = &mut self.hold else { return };
        let elapsed = hold.armed_at.elapsed();
        if elapsed >= Duration::from_secs(12) {
            hold.stopped = true;
        }
        if !ready || hold.stopped || elapsed < POINTER_HOLD_DELAY {
            return;
        }
        hold.repeated = true;
        let (axis, sign) = (hold.button.axis, hold.button.sign);
        self.segment(axis, sign, c);
    }

    fn request(&mut self, token: Option<u64>, request: Request, c: &Controller) {
        self.pending = token.map(|token| (token, request));
        self.notice = if token.is_some() {
            "等待控制器回读…".into()
        } else {
            c.status.clone()
        };
    }

    fn verify_pending(&mut self) {
        self.step = Step::VerifyDraft;
        self.pending = None;
        self.stop_holding();
        self.plan = None;
        self.corners = [None, None];
        self.offset = None;
        self.binding = None;
        self.verify_when_idle = true;
        self.notice = "角点草稿已保留，等待核对后台会话和工作坐标。".into();
    }

    fn discard(&mut self, c: &Controller) {
        match Draft::clear(c.session_name()) {
            Ok(()) => {
                *self = Self {
                    checked_disk: true,
                    ..Self::default()
                }
            }
            Err(e) => self.notice = format!("未能清除角点草稿：{e}"),
        }
    }

    fn checkpoint(&mut self, c: &Controller, force: bool) {
        let (Some(stamp), Some(binding), [Some(top), bottom]) =
            (c.corner_stamp(), self.binding.as_ref(), self.corners)
        else {
            if force {
                self.notice.push_str(" 当前窗口保留角点，后台身份未确认。");
            }
            return;
        };
        if !force
            && self
                .draft
                .as_ref()
                .is_some_and(|d| stamp.job.saturating_sub(d.stamp.job) < 8)
        {
            return;
        }
        let draft = Draft::new(stamp, binding, top, bottom);
        if let Err(e) = draft.save(c.session_name()) {
            self.notice = format!("角点仅在当前窗口保留：{e}");
        }
        self.draft = Some(draft);
    }

    fn read(&mut self, request: Request, c: &mut Controller) {
        let token = c.refresh_mount_reference();
        self.request(token, request, c);
    }

    fn receive(
        &mut self,
        token: u64,
        request: Request,
        c: &mut Controller,
    ) -> Result<Option<String>, String> {
        let reply = c
            .mount_receipt()
            .filter(|reply| reply.request_id == token && Some(reply.epoch) == self.epoch)
            .copied()
            .ok_or_else(|| {
                format!(
                    "未取得有效坐标回执。{}",
                    c.mount_error(token).unwrap_or(&c.status)
                )
            })?;
        let offset = reply.work_offset.ok_or("控制器未报告工作偏移。")?;
        if c.work_offset() != Some(offset) || self.offset.is_some_and(|old| old != offset) {
            return Err("工作偏移已改变，请重新记录角点。".into());
        }
        let identity = binding(c, offset);
        if self.binding.is_some() && identity != self.binding {
            return Err("设备身份已改变，请重新记录角点。".into());
        }
        if !matches!(
            request,
            Request::Positioned | Request::Applied { .. } | Request::Jog(_)
        ) && reply.action != MountAction::Status
        {
            return Err("坐标读取回执与请求不符。".into());
        }
        match request {
            Request::Corner(index) => {
                let [x, y, _] = reply.work;
                if index == 1 && self.corners[0].is_some_and(|[left, top]| left == x || top == y) {
                    return Err("两个角点必须围成有宽度和高度的区域。".into());
                }
                self.offset = Some(offset);
                self.binding = identity;
                self.corners[index] = Some([x, y]);
                self.step = if index == 0 {
                    Step::BottomRight
                } else {
                    Step::Mount
                };
                self.notice.clear();
                self.checkpoint(c, true);
            }
            Request::Jog(expected) => {
                if reply.action != expected {
                    return Err("点动回执与请求不符，请核对位置。".into());
                }
                if let MountAction::Jog { axis, .. } = expected
                    && let Some(index) = ['X', 'Y'].iter().position(|&value| value == axis)
                {
                    self.last_feed[index] = reply.sent_feed_mm_min;
                }
                self.notice.clear();
                self.checkpoint(c, false);
            }
            Request::Resume => {
                let saved = self.draft.as_ref().ok_or("没有待核实的角点")?;
                let identity = identity.ok_or("无法核实设备身份")?;
                if !saved.matches(&identity) {
                    return Err("设备或工作偏移与角点草稿不符".into());
                }
                self.corners = [Some(saved.top), saved.bottom];
                self.binding = Some(identity);
                self.offset = Some(offset);
                self.step = if saved.bottom.is_some() {
                    Step::Mount
                } else {
                    Step::BottomRight
                };
                self.notice = "已核实并恢复角点。请确认纸张仍固定在原位。".into();
                self.checkpoint(c, true);
            }
            Request::Load => {
                let identity = identity.ok_or("此设备没有可核实的档案标识。")?;
                self.plan =
                    Some(load_profile(&identity)?.ok_or("当前设备和工作坐标下没有已保存的标定。")?);
                self.offset = Some(offset);
                self.binding = Some(identity);
                self.step = Step::Restore;
                self.notice.clear();
            }
            Request::Position => {
                let [Some(top), Some(bottom)] = self.corners else {
                    return Err("请先记录两个角点。".into());
                };
                let plan = if matches!(c.controller_identity(), Some(("BtWriter", "1.3a.20221114")))
                    && offset == [0.; 3]
                {
                    WorkspaceProfile::new(top, bottom, 0., 5., Some(5.), 5.)?
                } else {
                    return Err("此机型或工作偏移没有已验证的装笔高度，请先使用单独对笔。".into());
                };
                if !inside(&plan, reply.work) {
                    return Err("当前笔头超出纸面或可用装笔高度。请用低速点动核对位置。".into());
                }
                let token = c.move_mount_z(plan.mount_z);
                self.plan = Some(plan);
                self.request(token, Request::Positioned, c);
            }
            Request::Positioned => {
                let plan = self.plan.as_ref().ok_or("缺少装笔位置。")?;
                if reply.action != MountAction::MoveZ(plan.mount_z) || !fitted(plan, reply.work) {
                    return Err("装笔位置未确认，请检查设备。".into());
                }
                self.step = Step::Fit;
                self.notice.clear();
                self.checkpoint(c, true);
            }
            Request::Apply { restore } => {
                let plan = self.plan.as_ref().ok_or("缺少纸面范围。")?;
                if !inside(plan, reply.work) || (!restore && !fitted(plan, reply.work)) {
                    return Err("当前位置与装笔档案不符，未应用范围。".into());
                }
                let limits = MachineBounds {
                    min: plan.min,
                    max: plan.max,
                };
                let token = c.apply_calibration_bounds_at(limits, offset);
                self.request(token, Request::Applied { restore }, c);
            }
            Request::Applied { restore } => {
                let plan = self.plan.as_ref().ok_or("缺少纸面范围。")?;
                if reply.action
                    != MountAction::ApplyBounds(MachineBounds {
                        min: plan.min,
                        max: plan.max,
                    })
                    || !inside(plan, reply.work)
                {
                    return Err("控制器未确认纸面范围。".into());
                }
                if !restore {
                    let identity = self.binding.as_ref().ok_or("无法识别设备，档案未保存。")?;
                    save_profile(identity, plan)?;
                }
                c.apply_paper_profile(plan.clone())?;
                self.draft = None;
                Draft::clear(c.session_name())?;
                return Ok(Some(
                    if restore {
                        "已载入纸面和笔高档案。"
                    } else {
                        "已保存纸面范围、书写方向和笔高。"
                    }
                    .into(),
                ));
            }
        }
        Ok(None)
    }

    fn poll(&mut self, c: &mut Controller) -> Option<String> {
        if !self.checked_disk {
            self.checked_disk = true;
            match Draft::load(c.session_name()) {
                Ok(Some(draft)) => {
                    self.draft = Some(draft);
                    self.verify_pending();
                }
                Ok(None) => {}
                Err(e) => self.notice = format!("角点草稿无法读取：{e}"),
            }
        }
        let epoch = c.calibration_epoch();
        if self.epoch.is_some_and(|last| last != epoch) {
            self.last_feed = [None; 2];
            if self.draft.is_some() {
                if self.step != Step::VerifyDraft {
                    self.verify_pending();
                }
            } else if self.corners[0].is_some() || self.plan.is_some() || self.pending.is_some() {
                *self = Self {
                    checked_disk: true,
                    notice: "坐标基准无法核实，请重新记录角点。".into(),
                    ..Self::default()
                };
            }
        }
        self.epoch = Some(epoch);
        if self.verify_when_idle
            && !c.busy()
            && let Some(saved) = &self.draft
            && let Some(token) = c.verify_corners(saved.clone())
        {
            self.verify_when_idle = false;
            self.request(Some(token), Request::Resume, c);
        }
        if !c.busy()
            && let Some((token, request)) = self.pending.take()
        {
            match self.receive(token, request, c) {
                Ok(done) => return done,
                Err(error) => {
                    self.stop_holding();
                    self.notice = if matches!(request, Request::Resume) {
                        format!("角点草稿未通过核对。{error}")
                    } else {
                        error
                    };
                }
            }
        }
        None
    }
}
const INK: u32 = 0x202124ff;
const MUTED: u32 = 0x5f6368ff;
const POINTER_HOLD_DELAY: Duration = Duration::from_millis(280);

struct Panel<'a, 'b> {
    canvas: &'a mut Canvas<'b>,
    ui: &'a mut Ui,
    area: Rect,
}

impl Panel<'_, '_> {
    fn text(&mut self, s: &str, y: f32, size: i32, bold: bool) {
        self.canvas.ellipsis(
            crate::locale::text(s),
            self.area.x,
            self.area.y + y,
            self.area.w,
            size,
            bold,
            if bold { INK } else { MUTED },
        );
    }
    fn cell(&self, y: f32, i: usize, n: usize, h: f32) -> Rect {
        let w = (self.area.w - 8.0 * (n - 1) as f32) / n as f32;
        Rect::new(self.area.x + i as f32 * (w + 8.), self.area.y + y, w, h)
    }
    fn button(&mut self, id: u64, r: Rect, s: &str, primary: bool, enabled: bool) -> bool {
        self.ui.button(
            self.canvas,
            id,
            r,
            crate::locale::text(s),
            if primary {
                ButtonStyle::Primary
            } else {
                ButtonStyle::Secondary
            },
            enabled,
        )
    }
    fn pair(&mut self, id: u64, y: f32, col: usize, s: &str, primary: bool, enabled: bool) -> bool {
        self.button(id, self.cell(y, col, 2, 38.), s, primary, enabled)
    }
    fn report(&mut self, s: &str, y: f32) -> f32 {
        let mut line = String::new();
        let mut row = 0;
        for ch in crate::locale::text(s).chars() {
            line.push(ch);
            if self.canvas.measure(&line, 14, false) > self.area.w - 4. {
                line.pop();
                self.text(&line, y + row as f32 * 21., 14, false);
                row += 1;
                if row == 4 {
                    return y + row as f32 * 21.;
                }
                line.clear();
                line.push(ch);
            }
        }
        self.text(&line, y + row as f32 * 21., 14, false);
        y + (row + 1) as f32 * 21.
    }

    fn distance(&mut self, c: &mut Controller, y: f32, enabled: bool) -> Option<String> {
        self.text("每点一次移动 (mm)", y, 15, true);
        let y = y + 25.;
        let field = self.cell(y, 0, 3, 35.);
        if self.ui.input(
            self.canvas,
            1010,
            field,
            &mut c.jog_distance_text,
            "1",
            false,
            enabled,
        ) && let Err(error) = c.edited_jog_distance()
        {
            return Some(error);
        }
        for (column, (label, mm)) in [("微调 1 mm", 1.), ("粗调 10 mm", 10.)]
            .into_iter()
            .enumerate()
        {
            if self.button(
                1011 + column as u64,
                self.cell(y, column + 1, 3, 35.),
                label,
                false,
                enabled,
            ) && let Err(error) = c.set_jog_distance_mm(mm)
            {
                return Some(error);
            }
        }
        None
    }
}

fn xy_readout(label: &str, [x, y]: [f64; 2]) -> String {
    format!("{}  X {x:.3}   Y {y:.3} mm", crate::locale::text(label))
}

fn feed_readout(label: &str, speeds: [Option<f64>; 2]) -> String {
    let [x, y] = speeds.map(|speed| speed.map_or_else(|| "—".into(), |mm| mm.to_string()));
    format!("{}  X {x}   Y {y} mm/min", crate::locale::text(label))
}

impl WorkspaceWizard {
    pub(crate) fn draw(
        &mut self,
        canvas: &mut Canvas<'_>,
        ui: &mut Ui,
        c: &mut Controller,
        area: Rect,
        closing: bool,
    ) -> Outcome {
        if let Some(message) = self.poll(c) {
            return Outcome::Done(message);
        }
        let corner = matches!(self.step, Step::TopLeft | Step::BottomRight);
        self.jog_buttons.clear();
        self.keyboard_button = None;
        if !corner || closing || !c.connected() {
            self.stop_holding();
            self.keyboard = false;
        }
        let ready = !closing && c.connected() && !c.busy() && !self.pending();
        if corner {
            self.repeat_jog(ready, c);
        }
        let ready = ready && !self.pending();
        let suppress_click = std::mem::take(&mut self.suppress_click);
        let mut panel = Panel { canvas, ui, area };
        panel.text("纸面与装笔校准", 0., 23, true);
        let connected = c.connected();
        let waiting = self.step == Step::VerifyDraft;
        let (title, instruction) = if !connected {
            (
                "先连接写字机",
                "插好 USB 线，在「设备」里选中写字机，再点「连接」。可以先设置每次移动的距离。",
            )
        } else {
            match self.step {
                Step::TopLeft => (
                    "1 / 3  找到纸张左上角",
                    "先取下笔，固定纸张。把空笔架移到想开始写字的左上角，避开纸夹，再记住这里。",
                ),
                Step::BottomRight => (
                    "2 / 3  找到纸张右下角",
                    "把空笔架移到可写区域的右下角，给纸边和纸夹留出空隙，再记住这里。",
                ),
                Step::Mount => (
                    "3 / 3  移到装笔高度",
                    "纸面的两个角都已记住。确认笔夹是空的或已经松开，再让它上下移动到装笔高度。",
                ),
                Step::Fit => (
                    "3 / 3  装上笔",
                    "笔夹已到位。放入笔，让笔尖轻轻碰到纸面，锁紧笔夹，再保存。",
                ),
                Step::Restore => (
                    "使用已保存的校准",
                    "请确认纸张、笔和机器原点仍在原位。确认后会应用档案中的范围和笔高。",
                ),
                Step::VerifyDraft if self.verify_when_idle || self.pending() => (
                    "继续上次的角点记录",
                    "正在核对上次记录使用的设备和坐标。核对通过后可以从原来的步骤继续。",
                ),
                Step::VerifyDraft => (
                    "核对上次的角点",
                    "查看下方的核对结果。可以重新核对，也可以放弃旧记录，从第一个角重新开始。",
                ),
            }
        };
        panel.text(title, 38., 18, true);
        let mut y = panel.report(instruction, 69.) + 10.;
        if connected {
            if !c.busy()
                && c.work_offset().is_some()
                && let Some([x, y_now, _]) = c.wpos
            {
                panel.text(&xy_readout("笔架现在", [x, y_now]), y, 15, true);
            } else {
                panel.text("笔架现在 · 等待坐标回读", y, 14, false);
            }
            y += 24.;
        }
        let saved = if waiting {
            self.draft
                .as_ref()
                .map(|draft| [Some(draft.top), draft.bottom])
                .unwrap_or_default()
        } else if self.step == Step::Restore {
            self.plan
                .as_ref()
                .map(|plan| [Some(plan.corner_a), Some(plan.corner_b)])
                .unwrap_or_default()
        } else {
            self.corners
        };
        let labels = if waiting {
            ["草稿左上角（待核对）", "草稿右下角（待核对）"]
        } else if self.step == Step::Restore {
            ["档案中的左上角", "档案中的右下角"]
        } else {
            ["已记住的左上角", "已记住的右下角"]
        };
        for (point, label) in saved.into_iter().zip(labels) {
            if let Some(point) = point {
                panel.text(&xy_readout(label, point), y, 14, false);
                y += 22.;
            }
        }
        y += 8.;
        if !connected || corner {
            if let Some(error) = panel.distance(c, y, !closing && (!connected || !c.busy())) {
                self.notice = error;
            }
            y += 72.;
        }
        if connected && corner {
            y = panel.report("先微调一小步看方向。同一轴的「增大」和「减小」方向相反。长按连续移动，松开停止续发。", y) + 5.;
            let controls = [
                ('X', -1., "X 减小"),
                ('X', 1., "X 增大"),
                ('Y', -1., "Y 减小"),
                ('Y', 1., "Y 增大"),
            ];
            for (i, (axis, sign, label)) in controls.into_iter().enumerate() {
                let area = panel.cell(y, i, 4, 38.);
                self.jog_buttons.push(JogButton { area, axis, sign });
                if panel.button(1020 + i as u64, area, label, false, ready) && !suppress_click {
                    match crate::jog_settings::parse_distance(&c.jog_distance_text)
                        .and_then(|mm| c.form.xy_jog_feed().map(|feed| (sign * mm, feed)))
                    {
                        Ok((delta, feed)) => {
                            let token = c.jog_calibration(axis, delta, feed);
                            self.request(
                                token,
                                Request::Jog(MountAction::Jog { axis, delta, feed }),
                                c,
                            );
                        }
                        Err(error) => self.notice = error,
                    }
                }
            }
            y += 49.;
            let first = self.step == Step::TopLeft;
            let record = if first {
                "记为纸面左上角"
            } else {
                "记为纸面右下角"
            };
            if panel.button(1041, panel.cell(y, 0, 1, 39.), record, true, ready) {
                self.read(Request::Corner(usize::from(!first)), c);
            }
            y += 50.;
            if let Ok(requested) = c.form.xy_jog_feed() {
                let speeds = ['X', 'Y']
                    .map(|axis| c.xy_jog_feed_limit(axis).map(|limit| requested.min(limit)));
                panel.text(&feed_readout("预计移动速度", speeds), y, 14, false);
                y += 21.;
            }
            if self.last_feed.iter().any(Option::is_some) {
                panel.text(&feed_readout("上次实发速度", self.last_feed), y, 14, false);
                y += 21.;
            }
            let label = if self.details {
                "收起速度和其他选项"
            } else {
                "速度和其他选项"
            };
            if panel.button(1046, panel.cell(y, 0, 1, 32.), label, false, !closing) {
                self.details = !self.details;
                self.keyboard = false;
            }
            y += 40.;
            if self.details {
                panel.text("设定移动速度 (mm/min)", y, 14, true);
                y += 23.;
                let field = panel.cell(y, 0, 2, 34.);
                let feed_editable = !closing && !c.busy() && !self.pending();
                panel.ui.input(
                    panel.canvas,
                    1044,
                    field,
                    &mut c.form.jog_feed,
                    "mm/min",
                    false,
                    feed_editable,
                );
                let key_area = panel.cell(y, 1, 2, 34.);
                self.keyboard_button = Some(key_area);
                let key_label = if self.keyboard {
                    "停用键盘方向键"
                } else {
                    "启用键盘方向键"
                };
                if panel.button(
                    1045,
                    key_area,
                    key_label,
                    self.keyboard,
                    ready || self.keyboard,
                ) {
                    self.keyboard = !self.keyboard;
                }
                y += 42.;
                if let Ok(requested) = c.form.xy_jog_feed() {
                    for axis in ['X', 'Y'] {
                        panel.text(&c.xy_jog_feed_preview(axis, requested), y, 13, false);
                        y += 19.;
                    }
                }
                if let Some(source) = &c.jog_source {
                    panel.text(
                        &format!("{}  {source}", crate::locale::text("速度来源")),
                        y,
                        13,
                        false,
                    );
                    y += 20.;
                }
                y = panel.report(
                    "长按或方向键使用 40–500 mm/min，每段最多 2 mm，松开停止续发。",
                    y,
                ) + 5.;
                if first {
                    if panel.pair(1040, y, 0, "查找已保存的校准", false, ready) {
                        self.read(Request::Load, c);
                    }
                    if panel.pair(1042, y, 1, "单独对笔", false, ready) {
                        return Outcome::Secondary;
                    }
                } else if panel.button(
                    1043,
                    panel.cell(y, 0, 1, 38.),
                    "重新记录左上角",
                    false,
                    ready,
                ) {
                    self.discard(c);
                }
                y += 48.;
            }
        } else if connected {
            match self.step {
                Step::VerifyDraft => {
                    if panel.pair(
                        1060,
                        y,
                        0,
                        "放弃草稿，重新记录",
                        false,
                        !closing && !c.busy() && !self.pending(),
                    ) {
                        self.discard(c);
                    }
                    if panel.pair(1061, y, 1, "重新核对草稿", true, ready) {
                        self.verify_when_idle = true;
                    }
                }
                Step::Mount => {
                    if panel.button(
                        1050,
                        panel.cell(y, 0, 1, 38.),
                        "把笔夹移到装笔高度",
                        true,
                        ready,
                    ) {
                        self.read(Request::Position, c);
                    }
                }
                Step::Fit => {
                    if panel.button(
                        1051,
                        panel.cell(y, 0, 1, 38.),
                        "笔已夹紧，保存校准",
                        true,
                        ready,
                    ) {
                        self.read(Request::Apply { restore: false }, c);
                    }
                }
                Step::Restore => {
                    if panel.pair(1052, y, 0, "重新校准", false, ready) {
                        self.discard(c);
                    }
                    if panel.pair(1053, y, 1, "确认原点，使用档案", true, ready) {
                        self.read(Request::Apply { restore: true }, c);
                    }
                }
                Step::TopLeft | Step::BottomRight => unreachable!(),
            }
            y += 49.;
        }
        if !self.notice.is_empty() {
            y = panel.report(&self.notice, y);
        }
        self.drawn_size = Some((area.w, y + 12.));
        Outcome::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn corner_round_trip_with_external_jobs_and_window_restart() {
        use std::{fs, os::unix::fs::PermissionsExt, process::Command, thread};
        if std::env::var_os("ZUSE_CORNER_TEST_CHILD").is_none() {
            let root = std::env::temp_dir().join(format!(
                "zuse-corner-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir(&root).unwrap();
            let bin = root.join("mock-zuse");
            fs::write(&bin, include_str!("../tests/corner_session.py")).unwrap();
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
            for stage in ["capture", "reopen-left", "reopen-both"] {
                let result = Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "workspace_wizard::tests::corner_round_trip_with_external_jobs_and_window_restart", "--nocapture"])
                    .env("ZUSE_CORNER_TEST_CHILD",stage)
                    .env("ZUSE_CORNER_DAEMON_PID",std::process::id().to_string())
                    .env("ZUSE_BIN",&bin)
                    .env("ZUSE_WRITER_SESSION","corner-test")
                    .env("XDG_CONFIG_HOME",&root)
                    .output().unwrap();
                if !result.status.success() {
                    let _ = fs::remove_dir_all(&root);
                    panic!(
                        "{} {}",
                        String::from_utf8_lossy(&result.stdout),
                        String::from_utf8_lossy(&result.stderr)
                    );
                }
            }
            fs::remove_dir_all(root).unwrap();
            return;
        }
        let advance =
            |c: &mut Controller,
             w: &mut WorkspaceWizard,
             until: &dyn Fn(&Controller, &WorkspaceWizard) -> bool| {
                let end = Instant::now() + Duration::from_secs(12);
                loop {
                    c.poll();
                    w.poll(c);
                    if until(c, w) {
                        return;
                    }
                    assert!(
                        Instant::now() < end,
                        "{} / {} / step {}",
                        c.status,
                        w.notice,
                        w.step as u8
                    );
                    thread::sleep(Duration::from_millis(10));
                }
            };
        let inject = |ops: &[&str]| {
            assert!(
                Command::new(std::env::var_os("ZUSE_BIN").unwrap())
                    .arg("inject")
                    .args(ops)
                    .status()
                    .unwrap()
                    .success()
            )
        };
        let (mut c, mut w) = (Controller::default(), WorkspaceWizard::default());
        let stage = std::env::var("ZUSE_CORNER_TEST_CHILD").unwrap();
        if stage == "capture" {
            advance(&mut c, &mut w, &|c, _| c.connected() && !c.busy());
            w.read(Request::Corner(0), &mut c);
            advance(&mut c, &mut w, &|_, w| {
                w.step == Step::BottomRight && !w.pending()
            });
            assert_eq!(w.corners, [Some([0., 0.]), None]);
            let first = w.draft.as_ref().unwrap().stamp.job;
            inject(&["plotter.jog"]);
            advance(&mut c, &mut w, &|_, w| {
                w.step == Step::BottomRight
                    && !w.pending()
                    && w.draft.as_ref().is_some_and(|d| d.stamp.job > first)
            });
            let one = w.draft.as_ref().unwrap().stamp.job;
            inject(&["plotter.jog", "plotter.jog_held", "plotter.status"]);
            inject(&["plotter.status"]);
            inject(&["plotter.jog"]);
            advance(&mut c, &mut w, &|_, w| {
                w.step == Step::BottomRight
                    && !w.pending()
                    && w.draft.as_ref().is_some_and(|d| d.stamp.job > one)
            });
            assert_eq!(w.corners, [Some([0., 0.]), None]);
            return;
        }
        if stage == "reopen-left" {
            advance(&mut c, &mut w, &|_, w| {
                w.step == Step::BottomRight && !w.pending()
            });
            assert_eq!(w.corners, [Some([0., 0.]), None]);
            w.read(Request::Corner(1), &mut c);
            advance(&mut c, &mut w, &|_, w| {
                w.step == Step::Mount && !w.pending()
            });
            assert_eq!(w.corners, [Some([0., 0.]), Some([6., 2.])]);
            assert!(load_profile(w.binding.as_ref().unwrap()).unwrap().is_none());
            return;
        }
        assert_eq!(stage, "reopen-both");
        advance(&mut c, &mut w, &|_, w| {
            w.step == Step::Mount && !w.pending()
        });
        assert_eq!(w.corners, [Some([0., 0.]), Some([6., 2.])]);
        inject(&["plotter.reset", "plotter.status"]);
        advance(&mut c, &mut w, &|_, w| {
            w.step == Step::VerifyDraft && !w.pending() && !w.verify_when_idle
        });
        assert_eq!(w.corners, [None, None]);
        assert!(c.paper.is_none());
        assert!(w.notice.contains("plotter.reset"), "{}", w.notice);
    }

    #[test]
    fn coordinates_and_feed_distinguish_live_values_and_missing_receipts() {
        assert_eq!(
            xy_readout("已记住的左上角", [0., -3.5]),
            "已记住的左上角  X 0.000   Y -3.500 mm"
        );
        assert_eq!(
            feed_readout("上次实发速度", [None, Some(280.)]),
            "上次实发速度  X —   Y 280 mm/min"
        );
    }

    fn arrow(code: i32) -> Event {
        Event::Key {
            code,
            control: false,
            shift: false,
            text: String::new(),
        }
    }

    #[test]
    fn only_fresh_focused_arrows_authorize_one_xy_segment() {
        let mut w = WorkspaceWizard::default();
        assert_eq!(w.key_jog(&arrow(1002), true, true), KeyJog::Unhandled);
        w.keyboard = true;
        for (code, axis, sign) in [
            (1001, 'X', -1.),
            (1002, 'X', 1.),
            (1003, 'Y', 1.),
            (1004, 'Y', -1.),
        ] {
            assert_eq!(
                w.key_jog(&arrow(code), true, true),
                KeyJog::Segment(axis, sign)
            );
            assert_eq!(w.key_jog(&arrow(code), false, true), KeyJog::Discard);
            assert_eq!(w.key_jog(&arrow(code), true, false), KeyJog::Discard);
        }
        for loss in [
            arrow(9),
            Event::Down(0., 0.),
            Event::Text("7".into()),
            Event::Resize,
        ] {
            w.keyboard = true;
            w.pointer(&loss, true);
            assert_eq!(w.key_jog(&arrow(1002), true, true), KeyJog::Unhandled);
        }
        w.keyboard = true;
        w.step = Step::Mount;
        assert_eq!(w.key_jog(&arrow(1003), true, true), KeyJog::Unhandled);
    }

    #[test]
    fn release_and_receipts_do_not_send_queued_arrows() {
        let mut w = WorkspaceWizard {
            keyboard: true,
            ..WorkspaceWizard::default()
        };
        let mut travelled = 0.;
        // 30 Hz keyboard repeat against a 300 ms simulated motion receipt.
        // Release after 15 mm, with the final accepted segment still in flight.
        for tick in 0..180 {
            if tick % 9 == 0 && w.pending.take().is_some() {
                travelled += segment_mm(500.);
            }
            if tick >= 65 {
                assert!(!w.holding()); // No timer can manufacture a keyboard jog.
                continue;
            }
            if let KeyJog::Segment(axis, sign) = w.key_jog(&arrow(1002), true, true) {
                let expected = MountAction::Jog {
                    axis,
                    delta: sign * segment_mm(500.),
                    feed: 500.,
                };
                w.pending = Some((tick, Request::Jog(expected)));
            }
        }
        assert_eq!(travelled, 16.);
        assert_eq!([40., 300., 500.].map(segment_mm), [0.16, 1.2, 2.]);
        w.pending = Some((
            200,
            Request::Jog(MountAction::Jog {
                axis: 'X',
                delta: 2.,
                feed: 500.,
            }),
        ));
        let mut c = Controller::default();
        assert!(w.poll(&mut c).is_none()); // No receipt for 200: inhibits further motion.
        assert!(!w.keyboard);
    }

    #[test]
    fn hold_is_disarmed_on_pointer_exit_and_release_does_not_add_a_click() {
        let mut wizard = WorkspaceWizard {
            jog_buttons: vec![JogButton {
                area: Rect::new(0., 0., 40., 30.),
                axis: 'X',
                sign: 1.,
            }],
            ..WorkspaceWizard::default()
        };
        wizard.pointer(&Event::Down(5., 5.), true);
        assert!(wizard.holding());
        wizard.pointer(&Event::Move(42., 5.), false);
        assert!(!wizard.holding());
        wizard.pointer(&Event::Up(5., 5.), true);
        assert!(wizard.suppress_click);
        wizard.pointer(&Event::Down(5., 5.), true);
        wizard.pointer(&Event::Up(5., 5.), true);
        assert!(!wizard.suppress_click);
    }
}
