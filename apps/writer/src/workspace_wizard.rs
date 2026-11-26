use crate::controller::{Controller, MachineBounds, MountAction};
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
}

#[derive(Clone, Copy)]
enum Request {
    Corner(usize),
    Jog(MountAction),
    Load,
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
    offset: Option<[f64; 3]>,
    binding: Option<Binding>,
    corners: [Option<[f64; 2]>; 2],
    plan: Option<WorkspaceProfile>,
    pending: Option<(u64, Request)>,
    jog_index: usize,
    jog_feed: Option<String>,
    jog_buttons: Vec<JogButton>,
    hold: Option<Hold>,
    suppress_click: bool,
    keyboard: bool,
    keyboard_button: Option<Rect>,
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

// 240 ms of commanded travel. At 40..=500 mm/min this is 0.16..=2 mm.
fn segment_mm(feed: f64) -> f64 {
    (feed * 0.24 / 60. * 1000.).round() / 1000.
}

impl WorkspaceWizard {
    pub(crate) fn height(&self, _width: f32) -> f32 {
        565.
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
                self.suppress_click = self
                    .hold
                    .take()
                    .is_some_and(|hold| hold.repeated || hold.stopped);
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
        let feed = match self.jog_feed() {
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

    fn jog_feed(&self) -> Result<f64, String> {
        self.jog_feed
            .as_deref()
            .unwrap_or("300")
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|speed| (40. ..=500.).contains(speed))
            .ok_or_else(|| "XY 点动速度需为 40–500 mm/min。".into())
    }

    fn repeat_jog(&mut self, ready: bool, c: &mut Controller) {
        let Some(hold) = &mut self.hold else { return };
        let elapsed = hold.armed_at.elapsed();
        if elapsed >= Duration::from_secs(12) {
            hold.stopped = true;
        }
        if !ready || hold.stopped || elapsed < Duration::from_millis(280) {
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
                self.notice = format!("已记录 X {x:.3}  Y {y:.3} mm");
            }
            Request::Jog(expected) => {
                if reply.action != expected {
                    return Err("点动回执与请求不符，请核对位置。".into());
                }
                self.notice = "点动完成。".into();
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
                Err(error) => {
                    self.stop_holding();
                    self.notice = error;
                }
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
        let corner = matches!(self.step, Step::TopLeft | Step::BottomRight);
        self.jog_buttons.clear();
        self.keyboard_button = None;
        if !corner || closing || !c.connected() {
            self.stop_holding();
        }
        let ready = !closing && c.connected() && !c.busy() && !self.pending();
        if corner {
            self.repeat_jog(ready, c);
        }
        let ready = ready && !self.pending();
        let suppress_click = std::mem::take(&mut self.suppress_click);
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
                        let area = panel.cell(211. + row as f32 * 46., index, 2, 38.);
                        self.jog_buttons.push(JogButton { area, axis, sign });
                        if panel.button(1020 + (index * 2 + row) as u64, area, &label, false, ready)
                            && !suppress_click
                        {
                            match self.jog_feed() {
                                Ok(feed) => {
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
                }
                panel.text("← X−    → X+    ↑ Y+    ↓ Y−", 303., 14, false);
                panel.text("XY 点动速度 · mm/min", 334., 14, false);
                let speed_rect = panel.cell(355., 0, 2, 34.);
                panel.ui.input(
                    panel.canvas,
                    1044,
                    speed_rect,
                    self.jog_feed.get_or_insert_with(|| "300".into()),
                    "40–500",
                    false,
                    ready,
                );
                let key_area = panel.cell(355., 1, 2, 34.);
                self.keyboard_button = Some(key_area);
                if panel.button(
                    1045,
                    key_area,
                    if self.keyboard {
                        "停用键盘方向键"
                    } else {
                        "启用键盘方向键"
                    },
                    self.keyboard,
                    ready || self.keyboard,
                ) {
                    self.keyboard = !self.keyboard;
                }

                panel.report(
                    "按住方向键或轴按钮移动。松开停止续发，已发段最多 2 mm。",
                    490.,
                );
                if panel.pair(
                    1041,
                    405.,
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
                    if panel.pair(1040, 405., 0, "载入已保存的校准", false, ready) {
                        self.read(Request::Load, c);
                    }
                    if panel.pair(1042, 451., 0, "单独对笔", false, ready) {
                        return Outcome::Secondary;
                    }
                } else if panel.pair(1043, 405., 0, "重新记录左上角", false, ready) {
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
            525.,
        );
        Outcome::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
