//! Stage contact and clearance heights from fresh Writer session samples.
use crate::controller::{Action, CalibrationStep, Controller};
use zgpui::native::{ButtonStyle, Canvas, Rect, Ui};

const INK: u32 = 0x222222ff;
const MUTED: u32 = 0x696965ff;
const RULE: u32 = 0xdededdff;
const STEPS: [f64; 3] = [0.10, 0.25, 0.50];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sample {
    Read,
    Contact,
    Clear,
    Finish,
}

impl Sample {
    fn step(self) -> CalibrationStep {
        match self {
            Self::Read | Self::Finish => CalibrationStep::Status,
            Self::Contact => CalibrationStep::PenDown,
            Self::Clear => CalibrationStep::PenUp,
        }
    }
}

#[derive(Default)]
struct Draft {
    down: Option<f64>,
    up: Option<f64>,
    current: Option<f64>,
    pending: Option<(u64, Sample)>,
}

// Match the precision used by the Writer's physical G-code coordinates.
fn micron(z: f64) -> Option<i64> {
    if !(-10000.0..=10000.0).contains(&z) {
        return None;
    }
    let rounded = format!("{z:.3}").parse::<f64>().ok()?;
    Some((rounded * 1000.).round() as i64)
}

impl Draft {
    fn receive(
        &mut self,
        token: u64,
        step: CalibrationStep,
        z: f64,
    ) -> Result<Option<(f64, f64)>, &'static str> {
        let (expected, wanted) = self.pending.take().ok_or("请重新读取工作 Z")?;
        if expected != token || wanted.step() != step {
            self.current = None;
            return Err("采样回执已变化，请重新读取工作 Z");
        }
        self.current = None;
        let tick = micron(z).ok_or("工作 Z 超出写字机坐标范围")?;
        self.current = Some(z);
        match wanted {
            Sample::Read => {}
            Sample::Contact => {
                self.down = Some(z);
                self.up = None;
            }
            Sample::Clear => {
                let down = self.down.ok_or("请先记录笔尖接触纸面的高度")?;
                if micron(down) == Some(tick) {
                    return Err("两个高度相同，请确认笔尖离纸后再记录");
                }
                self.up = Some(z);
            }
            Sample::Finish => {
                let down = self.down.ok_or("缺少落笔位置")?;
                let up = self.up.ok_or("缺少抬笔位置")?;
                if micron(up) != Some(tick) || micron(down) == Some(tick) {
                    self.up = None;
                    return Err("工作 Z 已变化，请观察离纸位置并重新记录");
                }
                return Ok(Some((down, up)));
            }
        }
        Ok(None)
    }
}

#[derive(Default, PartialEq, Eq, Clone, Copy)]
enum ProbeTest {
    #[default]
    Unknown,
    Pressed,
    Verified,
}

impl ProbeTest {
    fn observe(&mut self, state: Option<bool>) {
        *self = match (*self, state) {
            (_, None) => Self::Unknown,
            (Self::Unknown, Some(true)) => Self::Pressed,
            (Self::Pressed, Some(false)) => Self::Verified,
            _ => *self,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Start,
    Seek,
    RecordContact,
    AtContact,
    BeforeReturn,
    Return,
    RecordClear,
    Ready,
}
#[derive(Clone, Copy, Debug, PartialEq)]
enum Next {
    Read(Sample),
    Jog(f64),
}

struct Cycle {
    origin: f64,
    direction: i8,
    steps: u8,
    phase: Phase,
}
impl Cycle {
    fn new(origin: f64, direction: i8) -> Option<Self> {
        (micron(origin).is_some() && matches!(direction, -1 | 1)).then_some(Self {
            origin,
            direction,
            steps: 0,
            phase: Phase::Start,
        })
    }
    fn observe(
        &mut self,
        sample: Sample,
        z: f64,
        pin: Option<bool>,
    ) -> Result<Option<Next>, &'static str> {
        let pressed = pin.ok_or("探针反馈缺失，自动对笔已停止")?;
        let delta = 0.1 * f64::from(self.direction);
        let expected = self.origin + delta * f64::from(self.steps);
        if micron(z).is_none() || (z - expected).abs() > 0.02 {
            return Err("工作 Z 与点动目标不符，自动对笔已停止");
        }
        match (self.phase, sample) {
            (Phase::Start, Sample::Read) if !pressed => {
                self.phase = Phase::Seek;
                self.steps = 1;
                Ok(Some(Next::Jog(delta)))
            }
            (Phase::Seek, Sample::Read) if pressed => {
                self.phase = Phase::RecordContact;
                Ok(Some(Next::Read(Sample::Contact)))
            }
            (Phase::Seek, Sample::Read) if self.steps < 10 => {
                self.steps += 1;
                Ok(Some(Next::Jog(delta)))
            }
            (Phase::Seek, Sample::Read) => Err("累计已点动 1.00 mm，未见探针触发；已停止"),
            (Phase::RecordContact, Sample::Contact) if pressed => {
                self.phase = Phase::AtContact;
                Ok(None)
            }
            (Phase::BeforeReturn | Phase::Return, Sample::Read) if self.steps > 0 => {
                self.phase = Phase::Return;
                self.steps -= 1;
                Ok(Some(Next::Jog(-delta)))
            }
            (Phase::Return, Sample::Read) if !pressed => {
                self.phase = Phase::RecordClear;
                Ok(Some(Next::Read(Sample::Clear)))
            }
            (Phase::RecordClear, Sample::Clear) if !pressed => {
                self.phase = Phase::Ready;
                Ok(None)
            }
            (Phase::Ready, Sample::Finish) if !pressed => Ok(None),
            _ => Err("探针状态或采样阶段已变化，自动对笔已停止"),
        }
    }
}

#[derive(Default)]
pub(crate) struct Alignment {
    draft: Draft,
    epoch: Option<u64>,
    step_index: usize,
    after_jog: bool,
    button_generation: bool,
    note: String,
    automatic: bool,
    direction: i8,
    probe_test: ProbeTest,
    cycle: Option<Cycle>,
    next: Option<Next>,
}

impl Alignment {
    pub(crate) fn height(&self, _width: f32) -> f32 {
        571.
    }

    fn request(&mut self, c: &mut Controller, sample: Sample) {
        if self.draft.pending.is_some() || c.busy() || !c.connected() {
            return;
        }
        if let Some(id) = c.dispatch_calibration(sample.step()) {
            self.draft.pending = Some((id, sample));
            self.draft.current = None;
            self.note = "正在读取控制器的工作 Z…".into();
        } else {
            self.note = "未取得采样请求，请刷新连接状态".into();
        }
    }

    fn stop_cycle(&mut self, reason: &str) {
        self.cycle = None;
        self.next = None;
        self.after_jog = false;
        self.draft = Draft::default();
        self.probe_test = ProbeTest::Unknown;
        self.direction = 0;
        self.note = reason.to_owned();
    }
    fn advance(&mut self, c: &mut Controller, closing: bool) {
        if closing || !c.connected() {
            if self.cycle.is_some() {
                self.stop_cycle("连接已变化，自动对笔已停止")
            }
            return;
        }
        if c.busy() || self.draft.pending.is_some() {
            return;
        }
        if let Some(next) = self.next.take() {
            match next {
                Next::Read(k) => {
                    self.request(c, k);
                    if self.draft.pending.is_none() {
                        self.stop_cycle("未取得采样请求，已停止")
                    }
                }
                Next::Jog(delta) => {
                    let Some(current) = self.draft.current.and_then(micron) else {
                        self.stop_cycle("当前工作 Z 未验证，已停止");
                        return;
                    };
                    if c.wpos.and_then(|p| micron(p[2])) != Some(current)
                        || self.draft.current.and_then(|z| micron(z + delta)).is_none()
                    {
                        self.stop_cycle("当前工作 Z 或点动目标无效，已停止");
                        return;
                    }
                    self.draft.current = None;
                    c.dispatch(Action::Jog { axis: 'Z', delta });
                    if c.owns_job() {
                        self.after_jog = true;
                        self.note = "等待点动作业和探针回执".into();
                    } else {
                        self.stop_cycle("点动未被接收，已停止");
                    }
                }
            }
        }
    }
    fn reconcile(&mut self, c: &mut Controller, closing: bool) -> bool {
        let epoch = c.calibration_epoch();
        if self.epoch.replace(epoch).is_some_and(|old| old != epoch) {
            self.stop_cycle("设备或共享作业状态已变化，请重新读取工作 Z");
        }
        if closing {
            self.stop_cycle("正在关闭");
            return false;
        }
        if !c.connected() {
            if self.cycle.is_some()
                || self.draft.pending.is_some()
                || self.draft.current.is_some()
                || self.draft.down.is_some()
                || self.probe_test != ProbeTest::Unknown
            {
                self.stop_cycle("连接已断开，请重新验证工作 Z 和探针");
            }
            return false;
        }
        if c.busy() {
            // Freeze manual captures until the controller classifies the external receipt.
            // Motion or unknown effects change its epoch. A proven passive status does not.
            if self.cycle.is_some() && c.active_job().is_some() && !c.owns_job() {
                self.stop_cycle("共享会话由外部作业占用，自动对笔已停止")
            }
            return false;
        }
        if let Some((id, kind)) = self.draft.pending {
            let Some(r) = c
                .calibration_receipt()
                .filter(|r| r.epoch == epoch && r.request_id == id && r.step == kind.step())
            else {
                self.stop_cycle("未取得本次有效的工作 Z，请刷新后重试");
                return false;
            };
            if !c.connected() || closing {
                self.stop_cycle("设备连接已变化");
                return false;
            }
            if let Some(cycle) = self.cycle.as_mut() {
                match cycle.observe(kind, r.work[2], r.probe_active) {
                    Ok(next) => self.next = next,
                    Err(e) => {
                        self.stop_cycle(e);
                        return false;
                    }
                }
            } else if kind == Sample::Read && self.automatic {
                self.probe_test.observe(r.probe_active);
            }
            match self.draft.receive(id, kind.step(), r.work[2]) {
                Ok(Some((down, up))) => {
                    c.form.pen_down = down;
                    c.form.pen_up = up;
                    c.strokes.clear();
                    c.gcode.clear();
                    c.status = format!("对笔完成 · 落笔 {down:.3} mm · 抬笔 {up:.3} mm");
                    return true;
                }
                Ok(None) if self.cycle.is_some() => {
                    let cycle = self.cycle.as_ref().unwrap();
                    self.note = match cycle.phase {
                        Phase::AtContact => "已触发并记录落笔。确认后可返回起始离纸位。".into(),
                        Phase::Ready => "起始离纸位已复核，可以完成对笔。".into(),
                        Phase::Return | Phase::RecordClear => {
                            format!("正在返回，距起始离纸位 {} 步", cycle.steps)
                        }
                        _ => format!("接近纸面 · 第 {} / 10 步", cycle.steps),
                    }
                }
                Ok(None) => {
                    self.note = match kind {
                        Sample::Read => "工作 Z 已更新",
                        Sample::Contact => "落笔位置已记录，请选择使笔尖离纸的方向",
                        Sample::Clear => "两点已记录，可以完成对笔",
                        Sample::Finish => unreachable!(),
                    }
                    .into()
                }
                Err(e) => {
                    if self.cycle.is_some() {
                        self.stop_cycle(e)
                    } else {
                        self.note = e.into()
                    }
                }
            }
        }
        if self.after_jog {
            self.after_jog = false;
            self.request(c, Sample::Read);
            if self.cycle.is_some() && self.draft.pending.is_none() {
                self.stop_cycle("点动后未取得工作 Z 和探针回执，已停止");
            }
        }
        false
    }

    fn draw_automatic(
        &mut self,
        canvas: &mut Canvas<'_>,
        ui: &mut Ui,
        c: &mut Controller,
        area: Rect,
        verified: bool,
        closing: bool,
    ) {
        let (x, y, w) = (area.x, area.y, area.w);
        canvas.text("自动对笔 · 接触探针", x, y + 184., 17, true, INK);
        let hint = match self.probe_test {
            ProbeTest::Unknown => "手动按住探针，点击「读取工作 Z」验证触发。",
            ProbeTest::Pressed => "松开探针，再点「读取工作 Z」验证释放。",
            ProbeTest::Verified => "探针已验证。选择靠近纸面的方向，每次 0.10 mm，最多 1.00 mm。",
        };
        canvas.ellipsis(hint, x, y + 215., w, 14, false, MUTED);
        let half = (w - 8.) / 2.;
        for (i, dir) in [-1, 1].into_iter().enumerate() {
            let style = if self.direction == dir {
                ButtonStyle::Primary
            } else {
                ButtonStyle::Secondary
            };
            if ui.button(
                canvas,
                5400 + i as u64,
                Rect::new(x + i as f32 * (half + 8.), y + 261., half, 38.),
                if dir < 0 {
                    "向 Z− 靠近"
                } else {
                    "向 Z+ 靠近"
                },
                style,
                self.cycle.is_none() && !c.busy() && !closing,
            ) {
                self.direction = dir;
            }
        }
        let phase = self.cycle.as_ref().map(|s| s.phase);
        let returning = phase == Some(Phase::AtContact);
        let enable = verified
            && (returning
                || (self.cycle.is_none()
                    && self.probe_test == ProbeTest::Verified
                    && c.probe_active() == Some(false)
                    && self.direction != 0));
        let id = if self.button_generation { 5410 } else { 5411 };
        if ui.button(
            canvas,
            id,
            Rect::new(x, y + 318., w, 38.),
            if returning {
                "返回起始离纸位"
            } else {
                "从当前已确认离纸高度开始"
            },
            ButtonStyle::Primary,
            enable,
        ) {
            self.button_generation = !self.button_generation;
            if returning {
                self.cycle.as_mut().unwrap().phase = Phase::BeforeReturn;
            } else {
                self.cycle = Cycle::new(self.draft.current.unwrap(), self.direction);
                self.draft.down = None;
                self.draft.up = None;
            }
            self.next = Some(Next::Read(Sample::Read));
        }
        if ui.button(
            canvas,
            5420,
            Rect::new(x, y + 380., w, 38.),
            "停止自动对笔",
            ButtonStyle::Secondary,
            self.cycle.is_some() && !closing,
        ) {
            self.stop_cycle("已停止后续点动，请核对笔位");
            if c.owns_job() {
                c.dispatch(Action::Cancel);
            }
        }
        let height = |n: Option<f64>| n.map_or_else(|| "未记录".into(), |z| format!("{z:.3} mm"));
        canvas.ellipsis(
            &format!(
                "落笔 {}    抬笔 {}",
                height(self.draft.down),
                height(self.draft.up)
            ),
            x,
            y + 435.,
            w,
            15,
            false,
            INK,
        );
        if ui.button(
            canvas,
            5430,
            Rect::new(x, y + 474., w, 38.),
            "完成对笔",
            ButtonStyle::Primary,
            verified && phase == Some(Phase::Ready),
        ) {
            self.request(c, Sample::Finish);
        }
        canvas.ellipsis(&self.note, x, y + 524., w, 13, false, MUTED);
        if c.probe_active().is_none() {
            canvas.ellipsis(
                "无可验证探针，可使用手动对笔或摄像头试线。",
                x,
                y + 547.,
                w,
                13,
                false,
                MUTED,
            );
        }
    }
    pub(crate) fn draw(
        &mut self,
        canvas: &mut Canvas<'_>,
        ui: &mut Ui,
        c: &mut Controller,
        area: Rect,
        closing: bool,
    ) -> bool {
        if self.reconcile(c, closing) {
            return true;
        }
        let (x, y, w) = (area.x, area.y, area.w);
        let idle = c.connected() && !c.busy() && !closing && self.draft.pending.is_none();
        let z = self.draft.current;
        let verified =
            idle && z.is_some() && c.wpos.and_then(|p| micron(p[2])) == z.and_then(micron);
        canvas.text("对笔", x, y, 25, true, INK);
        let tab = if self.automatic {
            "手动对笔"
        } else {
            "接触探针"
        };
        if ui.button(
            canvas,
            5090,
            Rect::new(x + w - 108., y, 108., 34.),
            tab,
            ButtonStyle::Secondary,
            !c.busy() && self.cycle.is_none() && !closing,
        ) {
            self.stop_cycle("");
            self.automatic = !self.automatic;
            self.direction = 0;
        }
        canvas.ellipsis(
            if self.automatic {
                "先验证探针，再从已离纸位置逐步接近。"
            } else {
                "观察笔尖与纸面，选择 Z 轴移动方向。"
            },
            x,
            y + 39.,
            w,
            15,
            false,
            MUTED,
        );
        canvas.text("当前工作 Z", x, y + 70., 13, false, MUTED);
        let read_w = 110_f32.min(w * 0.43);
        let shown = c
            .wpos
            .filter(|_| c.connected())
            .map_or_else(|| "—".to_owned(), |p| format!("{:.3} mm", p[2]));
        canvas.ellipsis(&shown, x, y + 91., w - read_w - 8., 21, true, INK);
        if ui.button(
            canvas,
            5000,
            Rect::new(x + w - read_w, y + 80., read_w, 36.),
            "读取工作 Z",
            ButtonStyle::Secondary,
            idle && self.cycle.is_none(),
        ) {
            self.request(c, Sample::Read)
        }
        let pressure = match (c.pressure(), c.pressure_unit()) {
            (Some(v), Some(u)) if v.is_finite() && !u.is_empty() => format!("笔压 {v} {u}"),
            _ => "设备未报告笔压".into(),
        };
        let probe = match c.probe_active() {
            Some(true) => " · 探针已触发",
            Some(false) => " · 探针未触发",
            None => "",
        };
        canvas.ellipsis(
            &format!("{pressure}{probe}"),
            x,
            y + 139.,
            w,
            14,
            false,
            MUTED,
        );
        canvas.line((x, y + 177.), (x + w, y + 177.), 1., RULE);
        if self.automatic {
            self.draw_automatic(canvas, ui, c, area, verified, closing);
            self.advance(c, closing);
            return false;
        }
        canvas.text("单次点动步长", x, y + 184., 15, true, INK);
        let third = (w - 16.) / 3.;
        for (i, step) in STEPS.iter().enumerate() {
            let style = if self.step_index == i {
                ButtonStyle::Primary
            } else {
                ButtonStyle::Secondary
            };
            if ui.button(
                canvas,
                5010 + i as u64,
                Rect::new(x + (third + 8.) * i as f32, y + 213., third, 36.),
                &format!("{step:.2} mm"),
                style,
                !c.busy() && !closing,
            ) {
                self.step_index = i;
            }
        }
        let increment = STEPS[self.step_index];
        let half = (w - 8.) / 2.;
        // Each activation changes widget IDs. An autorepeating Return/Space
        // stays focused on the retired ID, and pointer activation requires release.
        let base = if self.button_generation { 5100 } else { 5200 };
        for (index, (sign, delta)) in [("−", -increment), ("+", increment)]
            .into_iter()
            .enumerate()
        {
            if ui.button(
                canvas,
                base + index as u64,
                Rect::new(x + index as f32 * (half + 8.), y + 265., half, 40.),
                &format!("Z {sign}{increment:.2} mm"),
                ButtonStyle::Secondary,
                verified,
            ) {
                self.button_generation = !self.button_generation;
                self.draft.current = None;
                self.draft.up = None;
                c.dispatch(Action::Jog { axis: 'Z', delta });
                self.after_jog = c.owns_job();
                self.note = "等待点动作业与工作坐标回执".into();
            }
        }
        let (phase, instruction, record) = if self.draft.down.is_none() {
            (
                "记录落笔位置",
                "看到笔尖刚接触纸面时，记录此刻的工作 Z。",
                "已接触纸面，记录落笔",
            )
        } else {
            (
                "记录抬笔位置",
                "看到笔尖离开纸面后，记录此刻的工作 Z。",
                "已离开纸面，记录抬笔",
            )
        };
        canvas.text(phase, x, y + 322., 17, true, INK);
        canvas.ellipsis(instruction, x, y + 349., w, 14, false, MUTED);
        let next = if self.draft.down.is_none() {
            Sample::Contact
        } else {
            Sample::Clear
        };
        if ui.button(
            canvas,
            base + 4,
            Rect::new(x, y + 380., w, 38.),
            record,
            ButtonStyle::Secondary,
            verified && self.draft.up.is_none(),
        ) {
            self.button_generation = !self.button_generation;
            self.request(c, next);
        }
        let height = |v: Option<f64>| v.map_or_else(|| "未记录".into(), |n| format!("{n:.3} mm"));
        canvas.ellipsis(
            &format!(
                "落笔 {}    抬笔 {}",
                height(self.draft.down),
                height(self.draft.up)
            ),
            x,
            y + 435.,
            w,
            15,
            false,
            INK,
        );
        if ui.button(
            canvas,
            base + 5,
            Rect::new(x, y + 474., w, 38.),
            "完成对笔",
            ButtonStyle::Primary,
            verified && self.draft.up.is_some(),
        ) {
            self.button_generation = !self.button_generation;
            self.request(c, Sample::Finish);
        }
        canvas.ellipsis(&self.note, x, y + 532., w, 13, false, MUTED);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(
        d: &mut Draft,
        kind: Sample,
        token: u64,
        z: f64,
    ) -> Result<Option<(f64, f64)>, &'static str> {
        d.pending = Some((token, kind));
        d.receive(token, kind.step(), z)
    }

    #[test]
    fn preserves_either_machine_direction_without_guessing_paper_height() {
        for (down, up) in [(0., 3.), (2., 0.)] {
            let mut d = Draft::default();
            assert!(d.current.is_none() && d.down.is_none() && d.up.is_none());
            capture(&mut d, Sample::Contact, 1, down).unwrap();
            capture(&mut d, Sample::Clear, 2, up).unwrap();
            assert_eq!(
                capture(&mut d, Sample::Finish, 3, up).unwrap(),
                Some((down, up))
            );
        }
    }
    #[test]
    fn stale_wrong_or_replayed_receipts_never_capture() {
        let mut d = Draft {
            pending: Some((10, Sample::Contact)),
            ..Draft::default()
        };
        assert!(d.receive(9, CalibrationStep::PenDown, 1.).is_err());
        assert!(d.receive(10, CalibrationStep::PenDown, 1.).is_err());
        d.pending = Some((11, Sample::Read));
        assert!(d.receive(11, CalibrationStep::PenUp, 0.).is_err());
        assert!(d.down.is_none() && d.up.is_none());
    }
    #[test]
    fn requires_distinct_finite_exportable_points_and_fresh_final_position() {
        let mut d = Draft::default();
        assert!(capture(&mut d, Sample::Clear, 1, 2.).is_err());
        assert!(capture(&mut d, Sample::Contact, 2, f64::NAN).is_err());
        capture(&mut d, Sample::Contact, 3, 0.).unwrap();
        assert!(capture(&mut d, Sample::Clear, 4, -0.0001).is_err());
        capture(&mut d, Sample::Clear, 5, 1.25).unwrap();
        assert!(capture(&mut d, Sample::Finish, 6, 1.35).is_err());
        assert!(d.up.is_none());
    }
    #[test]
    fn probe_requires_press_then_release_and_missing_feedback_resets_verification() {
        let mut p = ProbeTest::default();
        p.observe(Some(false));
        assert!(p == ProbeTest::Unknown);
        p.observe(Some(true));
        assert!(p == ProbeTest::Pressed);
        p.observe(Some(true));
        assert!(p == ProbeTest::Pressed);
        p.observe(Some(false));
        assert!(p == ProbeTest::Verified);
        p.observe(None);
        assert!(p == ProbeTest::Unknown);
        assert!(Cycle::new(5., 0).is_none());
    }
    #[test]
    fn advance_stops_on_contact_then_separate_return_restores_clear_height() {
        for dir in [-1, 1] {
            let step = 0.1 * f64::from(dir);
            let mut c = Cycle::new(5., dir).unwrap();
            let read = Sample::Read;
            assert_eq!(c.observe(read, 5., Some(false)), Ok(Some(Next::Jog(step))));
            assert_eq!(
                c.observe(read, 5. + step, Some(false)),
                Ok(Some(Next::Jog(step)))
            );
            assert_eq!(
                c.observe(read, 5. + 2. * step, Some(true)),
                Ok(Some(Next::Read(Sample::Contact)))
            );
            assert_eq!(
                c.observe(Sample::Contact, 5. + 2. * step, Some(true)),
                Ok(None)
            );
            assert_eq!(c.phase, Phase::AtContact);
            assert!(c.observe(read, 5. + 2. * step, Some(true)).is_err());
            c.phase = Phase::BeforeReturn;
            assert_eq!(
                c.observe(read, 5. + 2. * step, Some(true)),
                Ok(Some(Next::Jog(-step)))
            );
            assert_eq!(
                c.observe(read, 5. + step, Some(false)),
                Ok(Some(Next::Jog(-step)))
            );
            assert_eq!(
                c.observe(read, 5., Some(false)),
                Ok(Some(Next::Read(Sample::Clear)))
            );
            assert_eq!(c.observe(Sample::Clear, 5., Some(false)), Ok(None));
            assert_eq!(c.phase, Phase::Ready);
            assert_eq!(c.observe(Sample::Finish, 5., Some(false)), Ok(None));
        }
    }
    #[test]
    fn at_most_ten_steps_and_each_requires_position_and_pin() {
        let mut c = Cycle::new(0., -1).unwrap();
        for i in 0..10 {
            assert_eq!(
                c.observe(Sample::Read, -f64::from(i) * 0.1, Some(false)),
                Ok(Some(Next::Jog(-0.1)))
            );
        }
        assert!(c.observe(Sample::Read, -1., Some(false)).is_err());
        for (z, pin) in [
            (0., None),
            (0., Some(true)),
            (0.1, Some(false)),
            (f64::NAN, Some(false)),
        ] {
            assert!(
                Cycle::new(0., 1)
                    .unwrap()
                    .observe(Sample::Read, z, pin)
                    .is_err()
            );
        }
        let mut c = Cycle::new(5., 1).unwrap();
        c.observe(Sample::Read, 5., Some(false)).unwrap();
        assert!(c.observe(Sample::Read, 5., Some(false)).is_err());
        assert!(c.observe(Sample::Read, 5.1, None).is_err());
        c.observe(Sample::Read, 5.1, Some(true)).unwrap();
        assert!(c.observe(Sample::Contact, 5.1, Some(false)).is_err());
    }
}
