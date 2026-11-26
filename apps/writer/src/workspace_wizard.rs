use crate::controller::{Controller, MachineBounds, MountAction};
use crate::workspace_profile::{Binding, WorkspaceProfile, load_profile, save_profile};
use zgpui::native::{ButtonStyle, Canvas, Rect, Ui};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Step {
    #[default]
    TopLeft,
    BottomRight,
    Mount,
    Fit,
    Restore,
}

#[derive(Clone, Copy)]
enum Request {
    Corner(usize),
    Jog,
    Load,
    Position,
    Positioned,
    Apply { restore: bool },
    Applied { restore: bool },
}

#[derive(Default)]
pub(crate) struct WorkspaceWizard {
    step: Step,
    epoch: Option<u64>,
    offset: Option<[f64; 3]>,
    binding: Option<Binding>,
    corners: [Option<[f64; 2]>; 2],
    plan: Option<WorkspaceProfile>,
    pending: Option<(u64, Request)>,
    jog_index: usize,
    notice: String,
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

impl WorkspaceWizard {
    pub(crate) fn height(&self, _width: f32) -> f32 {
        530.
    }

    pub(crate) fn pending(&self) -> bool {
        self.pending.is_some()
    }

    fn request(&mut self, token: Option<u64>, request: Request, c: &Controller) {
        self.pending = token.map(|token| (token, request));
        self.notice = if token.is_some() {
            "等待控制器回读…".into()
        } else {
            c.status.clone()
        };
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
            .ok_or_else(|| format!("未取得有效坐标回执。{}", c.status))?;
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
            Request::Positioned | Request::Applied { .. } | Request::Jog
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
                self.notice = format!("已记录 X {x:.3}  Y {y:.3} mm");
            }
            Request::Jog => self.notice = "点动完成。".into(),
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
        let epoch = c.calibration_epoch();
        if self.epoch.is_some_and(|previous| previous != epoch) {
            *self = Self {
                notice: "控制器坐标基准已变化，请重新校准。".into(),
                ..Self::default()
            };
        }
        self.epoch = Some(epoch);
        if !c.busy()
            && let Some((token, request)) = self.pending.take()
        {
            match self.receive(token, request, c) {
                Ok(done) => return done,
                Err(error) => self.notice = error,
            }
        }
        None
    }
}
const INK: u32 = 0x202124ff;
const MUTED: u32 = 0x5f6368ff;
const STEPS: [f64; 3] = [0.5, 2., 10.];

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
        let ready = !closing && c.connected() && !c.busy() && !self.pending();
        let mut panel = Panel { canvas, ui, area };
        panel.text("纸面与装笔校准", 0., 23, true);
        let title = match self.step {
            Step::TopLeft => "1 / 3  记录左上角",
            Step::BottomRight => "2 / 3  记录右下角",
            Step::Mount | Step::Fit => "3 / 3  装笔",
            Step::Restore => "载入已保存的校准",
        };
        panel.text(title, 48., 18, true);
        match self.step {
            Step::TopLeft | Step::BottomRight => {
                let first = self.step == Step::TopLeft;
                panel.report(
                    if first {
                        "先取下笔或松开笔夹。固定纸张，点动到可写区域的左上角，给夹具留出间隙。"
                    } else {
                        "点动到可写区域的右下角。两点之间就是允许写字的范围。"
                    },
                    81.,
                );
                for (index, step) in STEPS.into_iter().enumerate() {
                    if panel.button(
                        1010 + index as u64,
                        panel.cell(158., index, 3, 35.),
                        &format!("{step} mm"),
                        index == self.jog_index,
                        ready,
                    ) {
                        self.jog_index = index;
                    }
                }
                for (index, axis) in ['X', 'Y'].into_iter().enumerate() {
                    for (row, sign) in [1., -1.].into_iter().enumerate() {
                        let delta = sign * STEPS[self.jog_index];
                        let label = format!("{axis} {}", if sign > 0. { "+" } else { "−" });
                        if panel.pair(
                            1020 + (index * 2 + row) as u64,
                            211. + row as f32 * 46.,
                            index,
                            &label,
                            false,
                            ready,
                        ) {
                            let token = c.jog_calibration(axis, delta);
                            self.request(token, Request::Jog, c);
                        }
                    }
                }
                if panel.pair(
                    1041,
                    321.,
                    1,
                    if first {
                        "记录左上角"
                    } else {
                        "记录右下角"
                    },
                    true,
                    ready,
                ) {
                    self.read(Request::Corner(usize::from(!first)), c);
                }
                if first {
                    if panel.pair(1040, 321., 0, "载入已保存的校准", false, ready) {
                        self.read(Request::Load, c);
                    }
                    if panel.pair(1042, 369., 0, "单独对笔", false, ready) {
                        return Outcome::Secondary;
                    }
                } else if panel.pair(1043, 321., 0, "重新记录左上角", false, ready) {
                    *self = Self::default();
                }
            }
            Step::Mount => {
                panel.report(
                    "保持当前 XY，确认笔已取下或松开。程序只沿 Z 轴低速移到装笔高度。",
                    85.,
                );
                if panel.pair(1050, 193., 1, "定位装笔", true, ready) {
                    self.read(Request::Position, c);
                }
            }
            Step::Fit => {
                panel.report(
                    "笔夹已到位。把笔尖轻轻放到纸面，锁紧笔夹，再点击保存。",
                    85.,
                );
                panel.report("书写方向和边界由左上角、右下角自动确定。", 154.);
                if let Some(plan) = &self.plan {
                    panel.text(
                        &format!(
                            "Z {:.3} → {:.3} mm  ({})",
                            plan.pen_down_z,
                            plan.pen_up_z,
                            if plan.up_sign() > 0. { "+Z" } else { "−Z" }
                        ),
                        188.,
                        14,
                        false,
                    );
                }
                if panel.pair(1051, 217., 1, "已装笔，保存校准", true, ready) {
                    self.read(Request::Apply { restore: false }, c);
                }
            }
            Step::Restore => {
                panel.report("已找到档案。确认纸张、笔和机器实体原点仍与保存时一致，再使用。载入不会移动机器。", 85.);
                if panel.pair(1052, 193., 0, "重新校准", false, ready) {
                    *self = Self::default();
                }
                if panel.pair(1053, 193., 1, "确认原点，使用档案", true, ready) {
                    self.read(Request::Apply { restore: true }, c);
                }
            }
        }
        panel.report(
            if self.notice.is_empty() {
                &c.status
            } else {
                &self.notice
            },
            419.,
        );
        Outcome::Stay
    }
}
