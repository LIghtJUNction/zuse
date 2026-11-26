use crate::controller::{Controller, MachineBounds, MountAction};
use crate::workspace_profile::{Binding, WorkspaceProfile, load_profile, save_profile};
use zgpui::native::{ButtonStyle, Canvas, Rect, Ui};

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Stage {
    #[default]
    Teach,
    Quick,
    Review,
    Move,
    Fix,
    Test,
    Save,
    Stopped,
}

#[derive(Clone, Copy)]
enum Target {
    A,
    B,
    Upper,
    Lower,
}

#[derive(Clone, Copy)]
enum Operation {
    Sample(Target),
    Load,
    Jog(char, f64),
    QuickStart,
    BeforeMove,
    Move,
    BeforeApply,
    Apply,
    Test,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Origin {
    #[default]
    Taught,
    NeedsCheck,
    Checked,
}

#[derive(Default)]
pub(crate) struct WorkspaceWizard {
    stage: Stage,
    origin: Origin,
    epoch: Option<u64>,
    offset: Option<[f64; 3]>,
    taught_binding: Option<Binding>,
    corners: [Option<[f64; 2]>; 2],
    ends: [Option<f64>; 2],
    mount: String,
    clearance: String,
    jog_index: usize,
    pending: Option<(u64, Operation)>,
    plan: Option<WorkspaceProfile>,
    tested: bool,
    quick: bool,
    confirm_auto_bounds: bool,
    notice: String,
}

pub(crate) enum Outcome {
    Stay,
    Secondary,
    Done(String),
}

fn binding(c: &Controller, offset: [f64; 3]) -> Result<Binding, String> {
    let id = c
        .hardware_id()
        .ok_or("设备未报告可核实的唯一标识，本次标定仅用于当前会话。")?;
    let (controller, firmware) = c
        .controller_identity()
        .ok_or("缺少控制器或固件标识，本次标定仅用于当前会话。")?;
    Binding::new(id, controller, firmware, offset)
}

fn in_range(p: &WorkspaceProfile, work: [f64; 3]) -> bool {
    (0..3).all(|i| (p.min[i]..=p.max[i]).contains(&work[i]))
}

fn at_mount(p: &WorkspaceProfile, work: [f64; 3]) -> bool {
    in_range(p, work) && (p.mount_z - work[2]).abs() <= 0.001
}

type Measurements = ([f64; 2], [f64; 2], f64, f64);

impl WorkspaceWizard {
    pub(crate) fn height(&self, width: f32) -> f32 {
        if width < 430. { 640. } else { 580. }
    }

    pub(crate) fn pending(&self) -> bool {
        self.pending.is_some()
    }

    fn request(&mut self, token: Option<u64>, op: Operation, c: &Controller) {
        self.pending = token.map(|id| (id, op));
        self.notice = if token.is_some() {
            "等待控制器回读…".into()
        } else {
            c.status.clone()
        };
    }

    fn read(&mut self, op: Operation, c: &mut Controller) {
        let token = c.refresh_mount_reference();
        self.request(token, op, c);
    }

    fn values(&self) -> Result<Measurements, String> {
        let [Some(a), Some(b)] = self.corners else {
            return Err("请记录两个 XY 对角。".into());
        };
        let [Some(upper), Some(lower)] = self.ends else {
            return Err("请记录 Z 的物理上端和靠纸端。".into());
        };
        Ok((a, b, upper, lower))
    }

    fn review(&mut self) -> Result<(), String> {
        let (a, b, upper, lower) = self.values()?;
        let mid = f64::midpoint(upper, lower);
        let p = WorkspaceProfile::new(a, b, upper, lower, None, 2.0f64.min((upper - mid).abs()))?;
        self.mount = p.mount_z.to_string();
        self.clearance = p.clearance.to_string();
        self.stage = Stage::Review;
        self.notice.clear();
        Ok(())
    }

    fn profile(&self) -> Result<WorkspaceProfile, String> {
        let (a, b, upper, lower) = self.values()?;
        let mount = self
            .mount
            .trim()
            .parse()
            .map_err(|_| "请输入有效的安装高度。")?;
        let clearance = self
            .clearance
            .trim()
            .parse()
            .map_err(|_| "请输入有效的抬笔间隙。")?;
        WorkspaceProfile::new(a, b, upper, lower, Some(mount), clearance)
    }

    fn load(&mut self, c: &Controller, offset: [f64; 3]) -> Result<(), String> {
        let key = binding(c, offset)?;
        let p = load_profile(&key)?.ok_or("此设备、固件和工作坐标基准尚无已存档案。")?;
        self.corners = [Some(p.corner_a), Some(p.corner_b)];
        self.ends = [Some(p.upper_z), Some(p.lower_z)];
        self.mount = p.mount_z.to_string();
        self.clearance = p.clearance.to_string();
        self.offset = Some(key.work_offset());
        self.taught_binding = Some(key);
        self.origin = Origin::NeedsCheck;
        self.stage = Stage::Review;
        self.notice = "已读入档案。请先核实机台仍使用同一物理原点。".into();
        Ok(())
    }

    fn finish(&mut self, c: &mut Controller, session_only: bool) -> Result<String, String> {
        let p = self.plan.as_ref().ok_or("请重新确认行程。")?;
        let offset = self.offset.ok_or("工作偏移未确认。")?;
        if c.work_offset() != Some(offset)
            || !c.connected()
            || self.epoch != Some(c.calibration_epoch())
        {
            return Err("设备或工作偏移已变更，请重新标定。".into());
        }
        let current = binding(c, offset).ok();
        if self.taught_binding.is_some() && current != self.taught_binding {
            return Err("设备身份与示教记录不符，请重新确认机台。".into());
        }
        let label = if self.quick {
            "已应用本次装笔高度。"
        } else if let Some(key) = self.taught_binding.as_ref().filter(|_| !session_only) {
            save_profile(key, p)?;
            "已保存机台档案并应用行程。"
        } else {
            "已应用行程，仅本次会话有效。"
        };
        c.form.pen_up = p.pen_up_z;
        c.form.pen_down = p.pen_down_z;
        Ok(format!(
            "{label} 抬笔 {:.3} mm，落笔 {:.3} mm。",
            p.pen_up_z, p.pen_down_z
        ))
    }

    fn receive(
        &mut self,
        c: &mut Controller,
        op: Operation,
        id: u64,
        epoch: u64,
    ) -> Result<Option<String>, String> {
        let failure = || format!("未取得此次操作的有效到位回执。{}", c.status);
        match op {
            Operation::Sample(_)
            | Operation::Load
            | Operation::QuickStart
            | Operation::BeforeMove
            | Operation::BeforeApply => {
                let r = c
                    .mount_receipt()
                    .filter(|r| {
                        r.epoch == epoch && r.request_id == id && r.action == MountAction::Status
                    })
                    .ok_or_else(failure)?;
                let work = r.work;
                if !work.iter().all(|v| v.is_finite()) {
                    return Err("缺少有效毫米工作坐标。".into());
                }
                let offset = r
                    .work_offset
                    .filter(|o| o.iter().all(|v| v.is_finite()))
                    .ok_or("无法核实工作偏移 WCO，请刷新设备。")?;
                if self.offset.is_some_and(|o| o != offset) {
                    return Err("工作偏移已改变，请重新示教。".into());
                }
                let observed = binding(c, offset).ok();
                if self.taught_binding.is_some() && observed != self.taught_binding {
                    return Err("设备或固件身份与示教记录不符，请重新示教。".into());
                }
                match op {
                    Operation::Sample(target) => {
                        if self.offset.is_none() {
                            self.taught_binding = observed;
                        }
                        self.offset = Some(offset);
                        match target {
                            Target::A => self.corners[0] = Some([work[0], work[1]]),
                            Target::B => self.corners[1] = Some([work[0], work[1]]),
                            Target::Upper => self.ends[0] = Some(work[2]),
                            Target::Lower => self.ends[1] = Some(work[2]),
                        }
                        self.notice = "已记录本次读取的毫米工作坐标。".into();
                    }
                    Operation::Load => self.load(c, offset)?,
                    Operation::QuickStart => {
                        if !matches!(c.controller_identity(), Some(("BtWriter", "1.3a.20221114")))
                            || offset != [0.; 3]
                            || !(0. ..=5.).contains(&work[2])
                        {
                            return Err("快速装笔需要已识别的 BtWriter、零工作偏移，且当前 Z 在 0–5 mm 内。".into());
                        }
                        self.quick = true;
                        self.offset = Some(offset);
                        self.taught_binding = observed;
                        // A 5 mm trial area around the current tip. It is not published as machine travel.
                        let [x, y, _] = work.map(|value| (value * 1000.).round() / 1000.);
                        let plan = WorkspaceProfile::new(
                            [x - 0.01, y - 0.01],
                            [x + 5.02, y + 0.01],
                            0.,
                            5.,
                            Some(5.),
                            5.,
                        )?;
                        let token = c.move_mount_z(plan.mount_z);
                        self.plan = Some(plan);
                        self.request(token, Operation::Move, c);
                    }
                    Operation::BeforeMove => {
                        let p = self.plan.as_ref().ok_or("请重新确认行程。")?;
                        if !in_range(p, work) {
                            return Err("当前 XYZ 超出示教范围，无法执行安装定位。".into());
                        }
                        if self.origin == Origin::NeedsCheck {
                            return Err("请先重新核对实机物理原点及方向。".into());
                        }
                        let token = c.move_mount_z(p.mount_z);
                        self.request(token, Operation::Move, c);
                    }
                    Operation::BeforeApply => {
                        let p = self.plan.as_ref().ok_or("请重新确认行程。")?;
                        if !at_mount(p, work) {
                            return Err("当前位置不在安装位。先松开或取下笔，再重新定位。".into());
                        }
                        if self.quick {
                            self.stage = Stage::Test;
                            self.tested = false;
                            self.notice = "确认双手离开机器，再试写。".into();
                            return Ok(None);
                        }
                        let bounds = MachineBounds {
                            min: p.min,
                            max: p.max,
                        };
                        let taught = self
                            .taught_binding
                            .as_ref()
                            .map(Binding::work_offset)
                            .or(self.offset)
                            .ok_or("缺少示教工作偏移。")?;
                        let token = c.apply_calibration_bounds_at(bounds, taught);
                        self.request(token, Operation::Apply, c);
                    }
                    _ => unreachable!(),
                }
            }
            Operation::Jog(_, _) | Operation::Move | Operation::Apply | Operation::Test => {
                let r = c
                    .mount_receipt()
                    .filter(|r| r.epoch == epoch && r.request_id == id)
                    .ok_or_else(failure)?;
                if self.offset.is_some_and(|o| r.work_offset != Some(o)) {
                    return Err("无法确认回执与示教使用相同工作偏移。".into());
                }
                match (op, &r.action) {
                    (Operation::Jog(axis, delta), MountAction::Jog { axis: a, delta: d })
                        if axis == *a && delta == *d =>
                    {
                        self.notice = format!("已点动 {axis} {delta:+.2} mm。");
                    }
                    (Operation::Move, MountAction::MoveZ(z)) => {
                        let p = self.plan.as_ref().ok_or("请重新确认行程。")?;
                        if *z != p.mount_z || !at_mount(p, r.work) {
                            return Err("安装位尚未读回到位，请检查当前位置。".into());
                        }
                        self.stage = Stage::Fix;
                        self.notice = format!("空笔夹已到位，工作 Z = {:.3} mm。", r.work[2]);
                    }
                    (Operation::Apply, MountAction::ApplyBounds(bounds)) => {
                        let p = self.plan.as_ref().ok_or("请重新确认行程。")?;
                        if bounds.min != p.min || bounds.max != p.max || !at_mount(p, r.work) {
                            return Err("控制器回读的共享范围或安装位置不匹配。".into());
                        }
                        self.stage = Stage::Test;
                        self.tested = false;
                        self.notice = "已应用行程。确认笔尖周围是空白纸面，再试写。".into();
                    }
                    (Operation::Test, MountAction::TestInk { .. }) => {
                        self.tested = true;
                        self.notice = "试写已结束，笔已抬高。请观察纸面。".into();
                    }
                    _ => return Err("回执类型与请求不符，请重新确认。".into()),
                }
            }
        }
        Ok(None)
    }

    fn poll(&mut self, c: &mut Controller) -> Option<String> {
        let epoch = c.calibration_epoch();
        let shifted = self
            .offset
            .zip(c.work_offset())
            .is_some_and(|(a, b)| a != b);
        if self.epoch.is_some_and(|prior| prior != epoch) || shifted {
            let had =
                self.offset.is_some() || self.pending.is_some() || self.stage == Stage::Stopped;
            *self = Self::default();
            if had {
                self.stage = Stage::Stopped;
                self.notice = "设备或坐标基准已变更，旧测量已清除。".into();
            }
        }
        self.epoch = Some(epoch);
        if c.busy() {
            return None;
        }
        if let Some((id, op)) = self.pending.take() {
            match self.receive(c, op, id, epoch) {
                Ok(done) => return done,
                Err(e) => {
                    if self.stage != Stage::Save
                        && matches!(
                            op,
                            Operation::Jog(..)
                                | Operation::QuickStart
                                | Operation::BeforeMove
                                | Operation::Move
                                | Operation::BeforeApply
                                | Operation::Apply
                        )
                    {
                        self.stage = Stage::Stopped;
                    }
                    self.notice = e;
                }
            }
        }
        None
    }
}

const INK: u32 = 0x202124ff;
const MUTED: u32 = 0x5f6368ff;
const RULE: u32 = 0xe8eaedff;
const STEPS: [f64; 3] = [0.1, 0.25, 0.5];

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
    fn report(&mut self, s: &str, y: f32) {
        let mut line = String::new();
        let mut row = 0;
        for ch in crate::locale::text(s).chars() {
            line.push(ch);
            if self.canvas.measure(&line, 14, false) > self.area.w - 4. {
                line.pop();
                self.text(&line, y + row as f32 * 21., 14, false);
                row += 1;
                if row == 3 {
                    return;
                }
                line.clear();
                line.push(ch);
            }
        }
        self.text(&line, y + row as f32 * 21., 14, false);
    }
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
        let ready = !closing && !c.busy() && self.pending.is_none() && c.connected();
        let mut f = Panel { canvas, ui, area };
        let secondary = Rect::new(area.x + area.w - 96., area.y, 96., 32.);
        f.canvas.ellipsis(
            "行程与装笔",
            area.x,
            area.y,
            (area.w - 104.).max(0.),
            21,
            true,
            INK,
        );
        if self.stage == Stage::Teach
            && f.button(
                1000,
                secondary,
                "单独对笔",
                false,
                !closing && !c.busy() && !self.pending(),
            )
        {
            return Outcome::Secondary;
        }
        let title = match self.stage {
            Stage::Stopped => "操作已暂停",
            Stage::Quick => "1 / 3   快速装笔",
            Stage::Teach => "1 / 5   示教机台行程",
            Stage::Review => "2 / 5   核对行程与装笔高度",
            Stage::Move => "3 / 5   空笔夹定位",
            Stage::Fix if self.quick => "2 / 3   贴纸装笔",
            Stage::Fix => "4 / 5   贴纸装笔",
            Stage::Test | Stage::Save if self.quick => "3 / 3   检查试写",
            Stage::Test | Stage::Save => "5 / 5   检查试写",
        };
        f.text(title, 46., 17, true);
        let report_y = match self.stage {
            Stage::Stopped => {
                f.report(
                    "请检查设备状态和笔夹位置。重新示教前，取下笔或松开笔夹。",
                    90.,
                );
                if f.pair(1080, 493., 0, "返回示教", false, !closing && !c.busy()) {
                    *self = Self::default();
                }
                146.
            }
            Stage::Teach => {
                self.teach(&mut f, c, ready);
                565.
            }
            Stage::Quick => {
                f.report(
                    "先松开或取下笔，保持当前 XY。笔尖右方要有至少 5 mm 空白纸面。",
                    80.,
                );
                f.report(
                    "程序只沿 Z 轴低速移动。到位后，把笔尖轻放到纸上并锁紧。",
                    150.,
                );
                if f.pair(1093, 233., 0, "返回示教", false, ready) {
                    *self = Self::default();
                }
                if f.pair(1094, 233., 1, "定位装笔", true, ready) {
                    self.read(Operation::QuickStart, c);
                }
                295.
            }
            Stage::Review => {
                self.range(&mut f, ready);
                if self.origin == Origin::Taught {
                    334.
                } else {
                    398.
                }
            }
            Stage::Move => {
                self.move_page(&mut f, c, ready);
                290.
            }
            Stage::Test => {
                if let Some(done) = self.test_ink(&mut f, c, ready) {
                    return Outcome::Done(done);
                }
                320.
            }
            Stage::Fix | Stage::Save => {
                if let Some(done) = self.fix(&mut f, c, ready) {
                    return Outcome::Done(done);
                }
                if self.stage == Stage::Save {
                    459.
                } else {
                    392.
                }
            }
        };
        f.canvas.line(
            (area.x, area.y + report_y),
            (area.x + area.w, area.y + report_y),
            1.,
            RULE,
        );
        let note = if self.notice.is_empty() && !c.connected() {
            "请先在设备栏选择串口并连接。"
        } else if self.notice.is_empty() {
            &c.status
        } else {
            &self.notice
        };
        f.report(note, report_y + 11.);
        Outcome::Stay
    }

    fn teach(&mut self, f: &mut Panel<'_, '_>, c: &mut Controller, ready: bool) {
        f.report(
            "先取下笔或松开笔夹。观察机台，逐点移动至可安全使用的边界。",
            74.,
        );
        for (i, step) in STEPS.into_iter().enumerate() {
            let r = f.cell(115., i, 3, 35.);
            if f.button(
                1010 + i as u64,
                r,
                &format!("{step} mm"),
                i == self.jog_index,
                ready,
            ) {
                self.jog_index = i;
            }
        }
        for (i, axis) in ['X', 'Y', 'Z'].into_iter().enumerate() {
            for (j, sign) in [1., -1.].into_iter().enumerate() {
                let delta = sign * STEPS[self.jog_index];
                let r = f.cell(169. + 43. * j as f32, i, 3, 35.);
                let label = format!("{axis} {}", if sign > 0. { "+" } else { "−" });
                if f.button(1020 + (2 * i + j) as u64, r, &label, false, ready) {
                    let token = c.jog_calibration(axis, delta);
                    self.request(token, Operation::Jog(axis, delta), c);
                }
            }
        }
        f.text(
            "在两个对角位置分别记录 XY，在两个 Z 端点分别记录高度。",
            264.,
            14,
            false,
        );
        for (i, target) in [Target::A, Target::B, Target::Upper, Target::Lower]
            .into_iter()
            .enumerate()
        {
            let col = i % 2;
            let y = 297. + 60. * (i / 2) as f32;
            let name = ["对角 A", "对角 B", "物理上端", "靠纸下端"][i];
            let val = match target {
                Target::A | Target::B => {
                    self.corners[col].map(|a| format!("{:.2}, {:.2}", a[0], a[1]))
                }
                Target::Upper | Target::Lower => self.ends[col].map(|v| format!("Z {:.2}", v)),
            };
            let label = val.map_or_else(|| format!("记录{name}"), |v| format!("{name}  {v}"));
            if f.pair(1030 + i as u64, y, col, &label, false, ready) {
                self.read(Operation::Sample(target), c)
            }
        }
        if f.pair(1095, 478., 0, "快速装笔", false, ready) {
            *self = Self {
                stage: Stage::Quick,
                ..Self::default()
            };
        }
        let label = if self.confirm_auto_bounds {
            "确认归零 · 清空机台"
        } else {
            "自动校准机台边界"
        };
        if f.pair(1096, 478., 1, label, self.confirm_auto_bounds, ready) {
            if self.confirm_auto_bounds {
                self.confirm_auto_bounds = false;
                self.notice.clear();
                c.dispatch(crate::controller::Action::AutoBounds);
            } else {
                self.confirm_auto_bounds = true;
                self.notice = "归零前取下笔，确认限位开关正常，且机台行程内没有夹具或障碍。".into();
            }
        }
        let recorded =
            self.corners.iter().all(Option::is_some) && self.ends.iter().all(Option::is_some);
        if f.pair(1040, 435., 0, "载入已存行程", false, ready) {
            self.read(Operation::Load, c)
        }
        if f.pair(1041, 435., 1, "核对行程 →", true, ready && recorded)
            && let Err(e) = self.review()
        {
            self.notice = e;
        }
    }

    fn range(&mut self, f: &mut Panel<'_, '_>, ready: bool) {
        let Ok((a, b, upper, lower)) = self.values() else {
            return;
        };
        f.text(
            &format!("X   {:.3} 至 {:.3} mm", a[0].min(b[0]), a[0].max(b[0])),
            78.,
            15,
            false,
        );
        f.text(
            &format!("Y   {:.3} 至 {:.3} mm", a[1].min(b[1]), a[1].max(b[1])),
            105.,
            15,
            false,
        );
        f.text(
            &format!(
                "Z   {:.3} 至 {:.3} mm    ·    抬高方向 {}",
                upper.min(lower),
                upper.max(lower),
                if upper > lower { "+Z" } else { "−Z" }
            ),
            132.,
            15,
            false,
        );
        for (i, (name, value)) in [
            ("安装工作 Z · mm", &mut self.mount),
            ("抬笔间隙 · mm", &mut self.clearance),
        ]
        .into_iter()
        .enumerate()
        {
            let r = f.cell(196., i, 2, 36.);
            f.canvas
                .ellipsis(name, r.x, r.y - 22., r.w, 13, false, MUTED);
            f.ui.input(f.canvas, 1050 + i as u64, r, value, "mm", false, ready);
        }
        let result = self.profile();
        match &result {
            Ok(p) => f.text(
                &format!(
                    "落笔 Z {:.3}    抬笔 Z {:.3}    抬高方向 {}",
                    p.pen_down_z,
                    p.pen_up_z,
                    if p.up_sign() > 0. { "+Z" } else { "−Z" }
                ),
                247.,
                14,
                false,
            ),
            Err(e) => f.text(e, 247., 14, false),
        }
        let row = if self.origin == Origin::Taught {
            286.
        } else {
            let r = Rect::new(f.area.x, f.area.y + 286., f.area.w, 38.);
            let verified = self.origin == Origin::Checked;
            if f.button(
                1054,
                r,
                if verified {
                    "已重新核对实机物理原点及方向"
                } else {
                    "我已在实机重新核对物理原点及方向"
                },
                verified,
                ready && !verified,
            ) {
                self.origin = Origin::Checked;
                self.notice.clear();
            }
            350.
        };
        if f.pair(1052, row, 0, "重新示教", false, ready) {
            *self = Self::default();
        } else if f.pair(
            1053,
            row,
            1,
            "确认行程 →",
            true,
            ready && result.is_ok() && self.origin != Origin::NeedsCheck,
        ) {
            self.plan = result.ok();
            self.stage = Stage::Move;
            self.notice.clear();
        }
    }

    fn move_page(&mut self, f: &mut Panel<'_, '_>, c: &mut Controller, ready: bool) {
        let Some(p) = &self.plan else { return };
        f.report("确认笔已取下或松开，且空笔夹的移动路径畅通。", 80.);
        f.text(
            &format!("目标工作 Z    {:.3} mm", p.mount_z),
            135.,
            20,
            true,
        );
        let back = f.cell(210., 0, 2, 38.);
        if f.button(1060, back, "返回核对", false, ready) {
            self.stage = Stage::Review;
            self.notice.clear()
        }
        if f.pair(1061, 210., 1, "空笔夹移到安装高度", true, ready) {
            self.read(Operation::BeforeMove, c)
        }
        f.text("保持当前 XY，只沿 Z 轴低速定位。", 261., 14, false);
    }

    fn test_ink(
        &mut self,
        f: &mut Panel<'_, '_>,
        c: &mut Controller,
        ready: bool,
    ) -> Option<String> {
        f.report(
            "笔尖附近留出至少 5 mm 空白纸面。程序将试写两段短线，中间抬笔。",
            80.,
        );
        f.report(
            "两段都出墨且中间无拖痕才保存。没出墨或有拖痕时，松开笔夹，再返回装笔。",
            139.,
        );
        if !self.tested && f.pair(1090, 218., 0, "试写两段短线", true, ready) {
            let p = self.plan.as_ref()?;
            let offset = self.offset?;
            let bounds = MachineBounds {
                min: p.min,
                max: p.max,
            };
            let token = c.test_calibration_ink(bounds, p.pen_up_z, p.pen_down_z, offset);
            self.request(token, Operation::Test, c);
        }
        if self.tested && f.pair(1091, 218., 0, "出墨正常 · 保存", true, ready) {
            self.stage = Stage::Save;
            match self.finish(c, false) {
                Ok(message) => return Some(message),
                Err(error) => self.notice = error,
            }
        }
        if f.pair(1092, 218., 1, "返回装笔", false, ready) {
            self.stage = if self.quick {
                Stage::Quick
            } else {
                Stage::Move
            };
            if self.quick {
                self.plan = None;
                self.offset = None;
                self.taught_binding = None;
            }
            self.tested = false;
            self.notice = "请先松开或取下笔，再定位空笔夹。".into();
        }
        None
    }

    fn fix(&mut self, f: &mut Panel<'_, '_>, c: &mut Controller, ready: bool) -> Option<String> {
        let p = self.plan.as_ref()?;
        f.report("将笔放入笔夹，让笔尖轻轻接触纸面，再锁紧笔夹。", 80.);
        f.text(
            &format!("纸面 Z {:.3}    抬笔 Z {:.3}", p.pen_down_z, p.pen_up_z),
            137.,
            18,
            true,
        );
        let durable = self.taught_binding.is_some();
        f.text(
            if durable {
                "此设备可保存机台档案。"
            } else {
                "设备身份未核实，标定仅用于本次会话。"
            },
            176.,
            14,
            false,
        );
        f.report(
            "书写起点 X/Y 须位于纸面内。默认轴向可从 X 最小、Y 最大处向内留边。",
            244.,
        );
        if self.stage == Stage::Save {
            let fallback = f.pair(1072, 402., 0, "仅本次使用", false, ready);
            if fallback || f.pair(1073, 402., 1, "重新保存档案", true, ready) {
                match self.finish(c, fallback) {
                    Ok(s) => return Some(s),
                    Err(e) => self.notice = e,
                }
            }
        } else {
            let r = Rect::new(f.area.x, f.area.y + 321., f.area.w, 38.);
            let action = if self.quick {
                "笔尖已贴纸并锁紧"
            } else {
                "笔尖已贴纸并锁紧 · 应用行程"
            };
            if f.button(1071, r, action, true, ready) {
                self.read(Operation::BeforeApply, c)
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_needs_all_measurements_and_never_clips_edited_clearance() {
        let mut w = WorkspaceWizard {
            corners: [Some([5., -1.]), Some([-3., 8.])],
            ends: [Some(-4.), None],
            ..Default::default()
        };
        assert!(w.review().is_err() && w.stage == Stage::Teach);
        w.ends[1] = Some(-1.);
        w.review().unwrap();
        assert!(w.stage == Stage::Review);
        let p = w.profile().unwrap();
        assert_eq!((p.mount_z, p.clearance, p.pen_up_z), (-2.5, 1.5, -4.));
        w.clearance = "1.6".into();
        assert!(w.profile().is_err());
        assert_eq!(w.clearance, "1.6");
    }

    #[test]
    fn measured_position_must_match_mount_and_full_envelope() {
        let p = WorkspaceProfile::new([-2., 2.], [3., 4.], 5., 0., Some(1.), 2.).unwrap();
        assert!(at_mount(&p, [-2., 4., 1.]));
        assert!(!at_mount(&p, [-2., 4., 1.002]));
        assert!(!at_mount(&p, [-2., 4.01, 1.]));
        assert!(!at_mount(&p, [f64::NAN, 4., 1.]));
    }
}
