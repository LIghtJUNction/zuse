use crate::alignment::Alignment;
use crate::article::Article;
use crate::controller::{Action, Controller, Form};
use crate::devices::{self, DeviceCandidate};
use crate::font_picker::FontPicker;
use crate::screen_output::ScreenOutput;
use crate::workspace_wizard::{Outcome, WorkspaceWizard};
use std::io::Read;
use zgpui::native::{ButtonStyle, Canvas, Event, Rect, Ui};

const INK: u32 = 0x202124ff;
const MUTED: u32 = 0x5f6368ff;
const RULE: u32 = 0xe8eaedff;
const PAPER: u32 = 0xf1f3f4ff;

// Keep partially typed numbers in the editor. Validate and transfer them as
// a group when building a plan, never on each digit of a keyboard event.
pub struct WriterView {
    screen_output: ScreenOutput,
    screen_mode: bool,
    show_origin_hint: bool,
    workspace: Option<WorkspaceWizard>,
    alignment: Option<Alignment>,
    picker: DevicePicker,
    geometry: [String; 8],
    mapping: [String; 12],
    advanced: bool,
    fonts: Option<FontPicker>,
    article: Option<Article>,
    dark: bool,
    english: bool,
    confirm_reset: bool,
    confirm_cancel: Option<(u64, u64, u64)>,
    file_path: String,
    baud: String,
    step: String,
    seed: String,
    notice: String,
    scroll_y: f32,
    max_scroll: f32,
}

const PAGE_SIZE: usize = 3;
const DEVICE_ROW: f32 = 70.0;

#[derive(Default)]
struct DevicePicker {
    candidates: Vec<DeviceCandidate>,
    page: usize,
    expanded: bool,
    automatic: Option<String>,
    edited: bool,
}

impl DevicePicker {
    fn update(&mut self, candidates: Vec<DeviceCandidate>, port: &mut String, allow_auto: bool) {
        if allow_auto
            && !self.edited
            && (port.is_empty() || self.automatic.as_ref() == Some(port))
            && !candidates.iter().any(|device| device.path == *port)
        {
            self.automatic = match candidates.as_slice() {
                [only] => Some(only.path.clone()),
                _ => None,
            };
            *port = self.automatic.clone().unwrap_or_default();
        }
        self.candidates = candidates;
        self.page = self
            .candidates
            .iter()
            .position(|d| d.path == *port)
            .map_or_else(
                || self.page.min(self.pages().saturating_sub(1)),
                |i| i / PAGE_SIZE,
            );
    }

    fn pages(&self) -> usize {
        self.candidates.len().div_ceil(PAGE_SIZE)
    }

    fn rows(&self) -> std::ops::Range<usize> {
        let first = self.page * PAGE_SIZE;
        first..(first + PAGE_SIZE).min(self.candidates.len())
    }

    fn height(&self) -> f32 {
        if self.candidates.is_empty() {
            return 70.0;
        }
        if !self.expanded {
            return 44.0;
        }
        44.0 + self.rows().len() as f32 * DEVICE_ROW + if self.pages() > 1 { 36.0 } else { 0.0 }
    }
}

const MAP_LABELS: [&str; 12] = [
    "左右偏移 ±%",
    "上下偏移 ±%",
    "校准参考长度",
    "实测距离 mm",
    "书写宽度（0 不限）",
    "书写高度（0 不限）",
    "逐字倾斜 ±°",
    "字号变化 ±%",
    "左边距",
    "上边距",
    "右边距",
    "下边距",
];

const LABELS: [&str; 8] = [
    "字高",
    "字距",
    "行距",
    "起点 X mm",
    "起点 Y mm",
    "进给 mm/min",
    "抬笔 Z mm",
    "落笔 Z mm",
];

fn number(value: &str, name: &str) -> Result<f64, String> {
    value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
        .ok_or_else(|| format!("请为「{name}」输入有效数值"))
}

impl WriterView {
    pub fn new(c: &mut Controller, align: bool) -> Self {
        if c.form.text.is_empty() {
            c.form.text = "你好 Zuse".into();
        }
        let mut picker = DevicePicker {
            expanded: true,
            ..Default::default()
        };
        picker.update(devices::discover(), &mut c.form.port, true);
        c.dispatch(Action::Refresh);
        let f = &c.form;
        Self {
            screen_mode: false,
            show_origin_hint: false,
            screen_output: ScreenOutput::default(),
            workspace: align.then(WorkspaceWizard::default),
            alignment: None,
            picker,
            geometry: [
                f.size,
                f.advance,
                f.line_height,
                f.x,
                f.y,
                f.feed,
                f.pen_up,
                f.pen_down,
            ]
            .map(|v| v.to_string()),
            mapping: [
                f.offset_x,
                f.offset_y,
                f.reference_units,
                f.reference_distance,
                f.area_width,
                f.area_height,
                f.tilt_degrees,
                f.size_percent,
                f.margins[0],
                f.margins[1],
                f.margins[2],
                f.margins[3],
            ]
            .map(|v| v.to_string()),
            advanced: false,
            fonts: None,
            article: None,
            dark: false,
            english: false,
            confirm_reset: false,
            confirm_cancel: None,
            file_path: String::new(),
            baud: f.baud.to_string(),
            step: f.jog_step.to_string(),
            seed: f.seed.to_string(),
            notice: String::new(),
            scroll_y: 0.0,
            max_scroll: 0.0,
        }
    }
    pub fn article_input(&mut self, event: &Event, ui: &mut Ui, height: f32) -> bool {
        let Some(article) = self.article.as_mut() else {
            return false;
        };
        if matches!(event, Event::Key { code: 27, .. }) {
            self.article = None;
            return true;
        }
        if article.input(event, ui) {
            if matches!(event, Event::Key { .. } | Event::Text(_)) {
                article.reveal_cursor(height - 32.);
            }
            return true;
        }
        false
    }
    pub fn dark(&self) -> bool {
        self.dark
    }
    pub fn aligning(&self) -> bool {
        self.workspace.is_some()
    }
    pub fn needs_tick(&self) -> bool {
        (self.screen_mode && self.screen_output.needs_tick())
            || self.fonts.as_ref().is_some_and(FontPicker::pending)
    }
    pub fn screen(&self) -> (bool, bool, bool) {
        (
            self.workspace.is_some(),
            self.alignment.is_some(),
            self.screen_mode,
        )
    }
    pub fn can_navigate(&self) -> bool {
        self.workspace.as_ref().is_none_or(|w| !w.pending())
    }
    pub fn back(&mut self) {
        if self.alignment.take().is_none() {
            self.workspace = None;
        }
        self.scroll_y = 0.;
        self.notice.clear();
    }
    fn finish_alignment(&mut self, c: &mut Controller, message: String) {
        self.workspace = None;
        self.alignment = None;
        self.scroll_y = 0.;
        self.geometry[6] = c.form.pen_up.to_string();
        self.geometry[7] = c.form.pen_down.to_string();
        self.dirty(c);
        self.notice = message;
    }
    pub fn sync_baud(&mut self, value: u32) {
        self.baud = value.to_string();
    }
    pub fn sync_heights(&mut self, old: (f64, f64), c: &Controller) {
        if old.0 != c.form.pen_up {
            self.geometry[6] = c.form.pen_up.to_string();
        }
        if old.1 != c.form.pen_down {
            self.geometry[7] = c.form.pen_down.to_string();
        }
    }
    pub fn scroll(&mut self, delta: i32) {
        self.scroll_y = (self.scroll_y + delta.signum() as f32 * 56.0).clamp(0.0, self.max_scroll);
    }
    fn dirty(&mut self, c: &mut Controller) {
        c.strokes.clear();
        c.gcode.clear();
        self.notice = "参数已修改，请更新笔迹预览。".into();
    }
    fn copy_geometry(&self, c: &mut Controller) -> Result<(), String> {
        let mut v = [0.; 8];
        for (i, n) in v.iter_mut().enumerate() {
            *n = number(&self.geometry[i], LABELS[i])?;
        }
        let seed = self
            .seed
            .trim()
            .parse::<u64>()
            .map_err(|_| "随机种子需为非负整数")?;
        let mut mapping = [0.; 12];
        for (i, v) in mapping.iter_mut().enumerate() {
            *v = number(&self.mapping[i], MAP_LABELS[i])?;
        }
        let f = &mut c.form;
        [
            f.offset_x,
            f.offset_y,
            f.reference_units,
            f.reference_distance,
            f.area_width,
            f.area_height,
            f.tilt_degrees,
            f.size_percent,
            f.margins[0],
            f.margins[1],
            f.margins[2],
            f.margins[3],
        ] = mapping;
        [
            f.size,
            f.advance,
            f.line_height,
            f.x,
            f.y,
            f.feed,
            f.pen_up,
            f.pen_down,
        ] = v;
        f.seed = seed;
        Ok(())
    }
    fn act(&mut self, c: &mut Controller, action: Action) {
        self.notice.clear();
        let old = (c.form.pen_up, c.form.pen_down);
        c.dispatch(action);
        self.sync_heights(old, c);
    }
    pub fn draw(
        &mut self,
        canvas: &mut Canvas<'_>,
        ui: &mut Ui,
        c: &mut Controller,
        w: f32,
        h: f32,
        closing: bool,
    ) {
        if let Some(article) = self.article.as_mut() {
            let width = (w - 32.).min(940.);
            if article.draw(canvas, ui, Rect::new((w - width) / 2., 24., width, h - 32.)) {
                c.form.text = self.article.take().expect("open article").finish();
                self.dirty(c);
                self.screen_output.deactivate();
            }
            return;
        }
        if let Some(picker) = &mut self.fonts {
            let width = (w - 32.).min(740.);
            let mut frame = Frame { canvas, ui };
            let area = Rect::new((w - width) / 2., 24., width, h - 24.);
            let previous = c.form.font.clone();
            if picker.draw(&mut frame, area, &mut c.form) {
                self.fonts = None;
            }
            if previous != c.form.font {
                self.dirty(c);
                self.screen_output.deactivate();
            }
            return;
        }
        let wide = w >= 830.0;
        let pad = if wide { 38.0 } else { 16.0 };
        let gap = if wide { 38.0 } else { 18.0 };
        let width = (w - 2.0 * pad).max(200.);
        let left = if wide {
            (width * 0.30).clamp(242., 338.)
        } else {
            width
        };
        let device_extra = self.picker.height()
            + if c.connected_device().is_some() {
                56.0
            } else {
                0.0
            };
        let aligning = self.aligning();
        let stacked_target = !wide && !aligning;
        let top = if w < 520. { 112. } else { 94. } + if stacked_target { 48. } else { 0. };
        let machine_height = if self.screen_mode {
            445.
        } else {
            device_extra + if aligning { 370. } else { 657. }
        };
        let (rx, right) = if wide {
            (pad + left + gap, width - left - gap)
        } else {
            (pad, width)
        };
        let panel_height = self
            .alignment
            .as_ref()
            .map(|a| a.height(right))
            .or_else(|| self.workspace.as_ref().map(|m| m.height(right)))
            .unwrap_or_else(|| {
                if self.screen_mode {
                    self.screen_output.height(right)
                } else {
                    if self.advanced { 981. } else { 860. }
                }
            });
        // A connected narrow window opens directly on the calibration controls.
        let panel_first = aligning && c.connected_device().is_some();
        let (my, ry) = if wide {
            (top, top)
        } else if panel_first {
            (top + panel_height + gap, top)
        } else {
            (top, top + machine_height + gap)
        };
        let content_h = (ry + panel_height).max(my + machine_height);
        self.max_scroll = (content_h + 20. - h).max(0.);
        self.scroll_y = self.scroll_y.min(self.max_scroll);
        let y = -self.scroll_y;
        let title_width = canvas.text("Zuse Writer", pad, y + 15., 30, true, INK);
        let state = if closing {
            "正在取消并关闭"
        } else if c.busy() {
            if c.owns_job() {
                "本机作业中"
            } else {
                "共享作业中"
            }
        } else if self.screen_mode {
            "屏幕输入"
        } else if c.connected_device().is_some() {
            "已连接"
        } else {
            "未连接"
        };
        let state = crate::locale::text(state);
        let session = format!("{}  {}", crate::locale::text("共享会话"), c.session_name());
        let job = c
            .active_job()
            .map(|id| format!("{state}  #{id}"))
            .unwrap_or_else(|| state.to_owned());
        let state = job.as_str();
        let nav_width = if aligning { 100. } else { 78. };
        let nav_x = w - pad - nav_width;
        let nav_y = y + 16.;
        let sw = canvas.measure(state, 14, false);
        let state_right = if wide && !aligning {
            nav_x - 244.
        } else {
            nav_x - 16.
        };
        if w < 520. {
            canvas.ellipsis(
                &session,
                pad,
                y + 62.,
                (width - sw - 14.).max(0.),
                13,
                false,
                MUTED,
            );
            canvas.text(state, w - pad - sw, y + 62., 13, false, MUTED);
        } else {
            let sx = pad + title_width + 22.;
            canvas.ellipsis(
                &session,
                sx,
                y + 29.,
                (state_right - 16. - sw - sx).max(0.),
                14,
                false,
                MUTED,
            );
            canvas.text(state, state_right - sw, y + 29., 14, false, MUTED);
        }
        canvas.line((pad, y + top - 28.), (w - pad, y + top - 28.), 1., RULE);
        let mut frame = Frame { canvas, ui };
        if !aligning {
            let tx = if wide { nav_x - 229. } else { pad };
            let ty = if wide { nav_y } else { y + top - 73. };
            frame.caption("输出到", tx, ty + 9.);
            for (i, label) in ["写字机", "屏幕输入"].iter().enumerate() {
                let selected = i == 1;
                let r = Rect::new(
                    tx + 47. + i as f32 * 77.,
                    ty,
                    if selected { 90. } else { 72. },
                    33.,
                );
                if frame.button(
                    180 + i as u64,
                    r,
                    label,
                    self.screen_mode == selected,
                    !closing && !c.busy(),
                ) && self.screen_mode != selected
                {
                    self.screen_output.deactivate();
                    self.screen_mode = selected;
                    self.scroll_y = 0.;
                    self.confirm_cancel = None;
                    self.notice.clear();
                    return;
                }
            }
        }
        if !self.screen_mode
            && frame.button(
                170,
                Rect::new(nav_x, nav_y, nav_width, 35.),
                if self.alignment.is_some() {
                    "返回向导"
                } else if aligning {
                    "返回书写"
                } else {
                    "对笔"
                },
                !aligning,
                !closing && !c.busy() && self.can_navigate(),
            )
        {
            if aligning {
                self.back();
            } else {
                self.workspace = Some(WorkspaceWizard::default());
                self.confirm_cancel = None;
                self.scroll_y = 0.;
                self.notice.clear();
            }
            return;
        }
        if self.screen_mode {
            let valid = self.screen_form(
                &mut frame,
                c,
                Rect::new(pad, y + my, left, machine_height),
                closing,
            );
            if !valid {
                self.screen_output.deactivate();
            }
            self.screen_output.draw(
                frame.canvas,
                frame.ui,
                c,
                Rect::new(rx, y + ry, right, panel_height),
                closing || !valid,
            );
            return;
        }
        self.machine(&mut frame, c, pad, y + my, left, closing);
        if let Some(alignment) = &mut self.alignment {
            if alignment.draw(
                frame.canvas,
                frame.ui,
                c,
                Rect::new(rx, y + ry, right, panel_height),
                closing,
            ) {
                let message = format!(
                    "已应用对笔结果，抬笔 {:.3} mm，落笔 {:.3} mm。请更新预览。",
                    c.form.pen_up, c.form.pen_down
                );
                self.finish_alignment(c, message);
            }
        } else if let Some(workspace) = &mut self.workspace {
            match workspace.draw(
                frame.canvas,
                frame.ui,
                c,
                Rect::new(rx, y + ry, right, panel_height),
                closing,
            ) {
                Outcome::Done(message) => {
                    self.show_origin_hint = true;
                    self.finish_alignment(c, message)
                }
                Outcome::Secondary => {
                    self.alignment = Some(Alignment::default());
                    self.scroll_y = 0.;
                }
                Outcome::Stay => {}
            }
        } else {
            self.writing(&mut frame, c, rx, y + ry, right, closing);
        }
    }
    fn machine(
        &mut self,
        f: &mut Frame<'_, '_>,
        c: &mut Controller,
        x: f32,
        y: f32,
        w: f32,
        closing: bool,
    ) {
        f.heading("设备", x, y);
        let appearance = if self.dark { "浅色" } else { "深色" };
        if f.button(
            702,
            Rect::new(x + w - 140., y, 66., 28.),
            appearance,
            false,
            !closing,
        ) {
            self.dark = !self.dark;
        }
        let language = if self.english {
            "简体中文"
        } else {
            "English"
        };
        if f.button(
            703,
            Rect::new(x + w - 70., y, 70., 28.),
            language,
            false,
            !closing,
        ) {
            self.english = !self.english;
            crate::locale::set_english(self.english);
        }
        let linked = c.connected_device().is_some();
        let editing = !c.busy() && !linked && !closing;
        let height = self.device_list(f, c, x, y + 40., w, closing);
        let mut y = y + height;
        if let Some((port, baud, firmware)) = c.connected_device() {
            f.canvas.ellipsis(
                &format!("已连接 · {}", firmware.unwrap_or("固件未报告型号")),
                x,
                y + 41.,
                w,
                15,
                true,
                INK,
            );
            f.canvas
                .ellipsis(&format!("{port} · {baud}"), x, y + 65., w, 13, false, MUTED);
            y += 56.;
        }
        if f.field(
            1,
            x,
            y + 41.,
            w,
            "串口路径",
            &mut c.form.port,
            "手动输入端口",
            editing,
        ) {
            self.picker.edited = true;
            self.picker.automatic = None;
        }
        f.field(
            3,
            x,
            y + 112.,
            w * 0.52,
            "波特率",
            &mut self.baud,
            "115200",
            editing,
        );
        if f.button(
            4,
            Rect::new(x + w * 0.52 + 8., y + 133., w * 0.48 - 8., 34.),
            if linked { "断开" } else { "连接" },
            true,
            !c.busy() && !closing && (linked || !c.form.port.trim().is_empty()),
        ) {
            if linked {
                self.act(c, Action::Disconnect);
            } else if let Ok(baud) = self.baud.trim().parse::<u32>() {
                c.form.baud = baud;
                self.act(c, Action::Connect);
            } else {
                self.notice = "波特率需为正整数。".into();
            }
        }
        let enabled = c.connected() && !c.busy() && !closing;
        f.caption("工作坐标 · mm", x, y + 197.);
        f.canvas
            .ellipsis(&position(c.wpos), x, y + 222., w, 16, true, INK);
        f.caption("机器坐标 · mm", x, y + 253.);
        f.canvas
            .ellipsis(&position(c.mpos), x, y + 277., w, 15, false, INK);
        let reset_width = (w * 0.40).max(85.);
        if f.button(
            5,
            Rect::new(x, y + 309., w - reset_width - 8., 33.),
            "刷新状态与坐标",
            false,
            !c.busy() && !closing,
        ) {
            self.confirm_reset = false;
            self.act(c, Action::Refresh);
        }
        let reset = if self.confirm_reset {
            "确认复位"
        } else {
            "复位控制器"
        };
        if f.button(
            704,
            Rect::new(x + w - reset_width, y + 309., reset_width, 33.),
            reset,
            self.confirm_reset,
            enabled,
        ) {
            if self.confirm_reset {
                self.confirm_reset = false;
                self.act(c, Action::Reset);
            } else {
                self.confirm_reset = true;
                self.notice =
                    crate::locale::text("复位会清除队列并使旧校准失效。再次点击确认。").into();
            }
        }
        if self.aligning() {
            f.canvas
                .ellipsis(&c.status, x, y + 350., w, 13, false, MUTED);
            return;
        }
        f.heading("手动调节", x, y + 369.);
        if f.button(
            705,
            Rect::new(x + w - 136., y + 361., 136., 32.),
            "回到左上角",
            false,
            enabled,
        ) {
            let target = || -> Result<Action, String> {
                Ok(Action::Park {
                    x: number(&self.geometry[3], LABELS[3])?,
                    y: number(&self.geometry[4], LABELS[4])?,
                    up: number(&self.geometry[6], LABELS[6])?,
                })
            };
            match target() {
                Ok(action) => self.act(c, action),
                Err(error) => self.notice = error,
            }
        }
        f.field(
            6,
            x,
            y + 407.,
            w,
            "点动步长 mm",
            &mut self.step,
            "1",
            !c.busy() && !closing,
        );
        let bw = (w - 16.) / 3.;
        for (i, axis) in ['X', 'Y', 'Z'].into_iter().enumerate() {
            for (row, sign) in [(0, 1.), (1, -1.)] {
                let label = format!("{axis} {}", if sign > 0. { "+" } else { "−" });
                if f.button(
                    10 + (i * 2 + row) as u64,
                    Rect::new(
                        x + i as f32 * (bw + 8.),
                        y + 477. + row as f32 * 37.,
                        bw,
                        31.,
                    ),
                    &label,
                    false,
                    enabled,
                ) {
                    match number(&self.step, "点动步长") {
                        Ok(n) if n > 0. => {
                            c.form.jog_step = n;
                            self.act(
                                c,
                                Action::Jog {
                                    axis,
                                    delta: sign * n,
                                },
                            );
                        }
                        _ => self.notice = "点动步长需为正数。".into(),
                    }
                }
            }
        }
        let half = (w - 8.) / 2.;
        for (i, label) in ["记录抬笔 Z", "记录落笔 Z"].into_iter().enumerate() {
            if f.button(
                20 + i as u64,
                Rect::new(x + i as f32 * (half + 8.), y + 559., half, 34.),
                label,
                false,
                enabled && c.wpos.is_some(),
            ) {
                self.act(
                    c,
                    if i == 0 {
                        Action::SetPenUp
                    } else {
                        Action::SetPenDown
                    },
                );
            }
        }
        for (i, label) in ["移至抬笔位", "移至落笔位"].into_iter().enumerate() {
            if f.button(
                23 + i as u64,
                Rect::new(x + i as f32 * (half + 8.), y + 603., half, 34.),
                label,
                false,
                enabled,
            ) {
                if let Ok(z) = number(&self.geometry[6 + i], LABELS[6 + i]) {
                    if i == 0 {
                        c.form.pen_up = z;
                        self.act(c, Action::PenUp);
                    } else {
                        c.form.pen_down = z;
                        self.act(c, Action::PenDown);
                    }
                } else {
                    self.notice = "笔位需为有限的 Z 坐标。".into();
                }
            }
        }
    }
    fn device_list(
        &mut self,
        f: &mut Frame<'_, '_>,
        c: &mut Controller,
        x: f32,
        y: f32,
        w: f32,
        closing: bool,
    ) -> f32 {
        let editing = !c.busy() && c.connected_device().is_none() && !closing;
        let list = &mut self.picker;
        let count = list.candidates.len();
        let open = if list.expanded { "▾" } else { "▸" };
        let title = format!("{open} USB 设备 ({count})");
        if f.button(90, Rect::new(x, y, w - 76., 32.), &title, false, count > 0) {
            list.expanded = !list.expanded;
        }
        if f.button(
            91,
            Rect::new(x + w - 68., y, 68., 32.),
            "扫描",
            false,
            !closing,
        ) {
            list.update(devices::discover(), &mut c.form.port, editing);
            list.expanded = true;
        }
        if list.candidates.is_empty() {
            f.canvas.ellipsis(
                "未发现 USB 串口，可手动输入路径",
                x,
                y + 42.,
                w,
                13,
                false,
                MUTED,
            );
        } else if list.expanded {
            for (row, i) in list.rows().enumerate() {
                let device = &list.candidates[i];
                let selected = c.form.port == device.path;
                let top = y + 40. + row as f32 * DEVICE_ROW;
                let rect = Rect::new(x, top, w, DEVICE_ROW - 6.);
                if f.button(100 + i as u64, rect, "", selected, editing) {
                    c.form.port.clone_from(&device.path);
                    list.edited = true;
                    list.automatic = None;
                }
                let ink = if selected && editing { 0xffffffff } else { INK };
                let secondary = if selected && editing {
                    0xddddddff
                } else {
                    MUTED
                };
                let label = if selected {
                    format!("✓ {}", device.label)
                } else {
                    device.label.clone()
                };
                f.canvas
                    .ellipsis(&label, x + 10., top + 5., w - 20., 15, true, ink);
                f.canvas.ellipsis(
                    &device.detail,
                    x + 10.,
                    top + 26.,
                    w - 20.,
                    12,
                    false,
                    secondary,
                );
                f.canvas.ellipsis(
                    &device.path,
                    x + 10.,
                    top + 44.,
                    w - 20.,
                    12,
                    false,
                    secondary,
                );
            }
            if list.pages() > 1 {
                let top = y + 40. + list.rows().len() as f32 * DEVICE_ROW;
                let pages = list.pages();
                if f.button(
                    92,
                    Rect::new(x, top, 58., 28.),
                    "上一页",
                    false,
                    list.page > 0,
                ) {
                    list.page -= 1;
                }
                f.canvas.text(
                    &format!("{} / {pages}", list.page + 1),
                    x + 69.,
                    top + 6.,
                    13,
                    false,
                    MUTED,
                );
                if f.button(
                    93,
                    Rect::new(x + w - 58., top, 58., 28.),
                    "下一页",
                    false,
                    list.page + 1 < pages,
                ) {
                    list.page += 1;
                }
            }
        }
        list.height()
    }
    fn text_font(
        &mut self,
        f: &mut Frame<'_, '_>,
        c: &mut Controller,
        area: Rect,
        stacked: bool,
        editing: bool,
    ) -> bool {
        let (x, y, w) = (area.x, area.y, area.w);
        let text_w = if stacked { w } else { w * 0.60 };
        let font_x = if stacked { x } else { x + text_w };
        let font_y = if stacked { y + 106. } else { y + 41. };
        let mut changed = f.field(
            30,
            x,
            y + 41.,
            if stacked { w - 90. } else { text_w - 98. },
            "文字",
            &mut c.form.text,
            "输入要书写的文字",
            editing,
        );
        if f.button(
            33,
            Rect::new(
                x + if stacked { w } else { text_w - 8. } - 82.,
                y + 62.,
                82.,
                34.,
            ),
            "编辑文章",
            false,
            editing,
        ) {
            self.article = Some(Article::new(&c.form.text));
        }
        changed |= f.field(
            31,
            font_x,
            font_y,
            x + w - font_x - 88.,
            "单线字体",
            &mut c.form.font,
            "builtin",
            editing,
        );
        if f.button(
            32,
            Rect::new(x + w - 80., font_y + 21., 80., 34.),
            "字体库",
            false,
            editing,
        ) {
            self.fonts = Some(FontPicker::new(&c.form.font));
        }
        changed
    }

    fn screen_form(
        &mut self,
        f: &mut Frame<'_, '_>,
        c: &mut Controller,
        area: Rect,
        closing: bool,
    ) -> bool {
        let (x, y, w) = (area.x, area.y, area.w);
        let editing = !closing && !c.busy();
        f.heading("书写内容", x, y);
        let mut changed = self.text_font(f, c, area, true, editing);
        let width = (w - 8.) / 2.;
        let [size, advance, line, ..] = &mut self.geometry;
        let [ox, oy, ..] = &mut self.mapping;
        for (i, (label, buffer)) in [
            ("字号", size),
            ("字距", advance),
            ("行距", line),
            ("随机种子", &mut self.seed),
            (MAP_LABELS[0], ox),
            (MAP_LABELS[1], oy),
        ]
        .into_iter()
        .enumerate()
        {
            changed |= f.field(
                190 + i as u64,
                x + (i % 2) as f32 * (width + 8.),
                y + 176. + (i / 2) as f32 * 64.,
                width,
                label,
                buffer,
                "",
                editing,
            );
        }
        if changed {
            self.screen_output.deactivate();
            self.dirty(c);
        }
        match self.copy_screen_params(c) {
            Ok(()) => true,
            Err(e) => {
                f.canvas.ellipsis(&e, x, y + 384., w, 14, false, INK);
                false
            }
        }
    }

    fn copy_screen_params(&self, c: &mut Controller) -> Result<(), String> {
        let size = number(&self.geometry[0], LABELS[0])?;
        let advance = number(&self.geometry[1], LABELS[1])?;
        let line = number(&self.geometry[2], LABELS[2])?;
        let mut variation = [0.; 12];
        for (i, value) in variation.iter_mut().enumerate() {
            *value = number(&self.mapping[i], MAP_LABELS[i])?;
        }
        let seed = self
            .seed
            .trim()
            .parse()
            .map_err(|_| "随机种子需为非负整数")?;
        let f = &mut c.form;
        (f.size, f.advance, f.line_height, f.seed) = (size, advance, line, seed);
        [
            f.offset_x,
            f.offset_y,
            _,
            _,
            f.area_width,
            f.area_height,
            f.tilt_degrees,
            f.size_percent,
            f.margins[0],
            f.margins[1],
            f.margins[2],
            f.margins[3],
        ] = variation;
        Ok(())
    }
    fn writing(
        &mut self,
        f: &mut Frame<'_, '_>,
        c: &mut Controller,
        x: f32,
        y: f32,
        w: f32,
        closing: bool,
    ) {
        let editing = !c.busy() && !closing;
        f.heading("书写内容", x, y);
        if f.button(
            35,
            Rect::new(x + w - 110., y - 2., 110., 32.),
            if self.advanced {
                "书写参数"
            } else {
                "纸张与字迹"
            },
            false,
            editing,
        ) {
            self.advanced = !self.advanced;
        }
        let mut changed = self.text_font(f, c, Rect::new(x, y, w, 0.), false, editing);
        let pw = (w - 16.) / 3.;
        if self.advanced {
            for (i, label) in MAP_LABELS.iter().enumerate() {
                changed |= f.field(
                    200 + i as u64,
                    x + (i % 3) as f32 * (pw + 8.),
                    y + 114. + (i / 3) as f32 * 66.,
                    pw,
                    label,
                    &mut self.mapping[i],
                    "",
                    editing,
                );
            }
            for (i, (label, value)) in [
                ("交换 X/Y", &mut c.form.swap_xy),
                ("反转 X", &mut c.form.flip_x),
                ("反转 Y", &mut c.form.flip_y),
            ]
            .into_iter()
            .enumerate()
            {
                let label = format!("{label} · {}", if *value { "开" } else { "关" });
                if f.button(
                    220 + i as u64,
                    Rect::new(x + i as f32 * (pw + 8.), y + 380., pw, 34.),
                    &label,
                    *value,
                    editing,
                ) {
                    *value = !*value;
                    changed = true;
                }
            }
        } else {
            for (i, label) in LABELS.iter().take(8).enumerate() {
                changed |= f.field(
                    40 + i as u64,
                    x + (i % 3) as f32 * (pw + 8.),
                    y + 114. + (i / 3) as f32 * 66.,
                    pw,
                    label,
                    &mut self.geometry[i],
                    "",
                    editing,
                );
            }
            changed |= f.field(
                48,
                x + 2. * (pw + 8.),
                y + 246.,
                pw,
                "随机种子",
                &mut self.seed,
                "0",
                editing,
            );
        }
        if changed {
            self.dirty(c);
        }
        if self.show_origin_hint && !self.advanced {
            f.canvas.ellipsis(
                "起点 X/Y 请留在纸面内，默认轴向从 X 最小、Y 最大处向内留边。",
                x,
                y + 306.,
                w,
                13,
                false,
                MUTED,
            );
        }
        let y = y + if self.advanced { 121. } else { 0. };
        f.heading("笔迹预览", x, y + 331.);
        if f.button(
            50,
            Rect::new(x + w - 110., y + 327., 110., 34.),
            "更新预览",
            false,
            editing,
        ) {
            match self.copy_geometry(c) {
                Ok(()) => self.act(c, Action::Preview),
                Err(e) => {
                    self.dirty(c);
                    self.notice = e;
                }
            }
        }
        let area = Rect::new(x, y + 377., w, 304.);
        paint_page(f.canvas, area, &c.form, &c.strokes);
        let y = y + 152.;
        let third = (w - 16.) / 3.;
        if f.button(
            53,
            Rect::new(x, y + 548., third, 39.),
            "校验程序",
            false,
            editing && c.connected() && !c.gcode.is_empty(),
        ) {
            self.act(c, Action::Check);
        }
        if f.button(
            51,
            Rect::new(x + third + 8., y + 548., third, 39.),
            "发送到写字机",
            true,
            editing && c.connected() && !c.gcode.is_empty(),
        ) {
            self.act(c, Action::Send);
        }
        let target = c.cancel_target();
        if self.confirm_cancel.is_some() && (self.confirm_cancel != target || c.owns_job()) {
            self.notice.clear();
            self.confirm_cancel = None;
        }
        let confirm = self.confirm_cancel.is_some();
        let label = if confirm {
            "确认取消共享作业"
        } else if c.busy() && !c.owns_job() {
            "取消共享作业"
        } else {
            "取消作业"
        };
        if f.button(
            52,
            Rect::new(x + 2. * (third + 8.), y + 548., third, 39.),
            label,
            confirm,
            c.busy() && !closing && (c.owns_job() || target.is_some()),
        ) {
            if c.owns_job() || (confirm && target == self.confirm_cancel) {
                self.confirm_cancel = None;
                self.act(c, Action::Cancel);
            } else if let Some((_, job, _)) = target {
                self.confirm_cancel = target;
                self.notice = format!("再次点击确认取消共享作业 #{job}。");
            }
        }
        f.field(
            81,
            x,
            y + 630.,
            w - 174.,
            "文本或 G-code 文件",
            &mut self.file_path,
            "输入文件路径",
            editing,
        );
        if f.button(
            83,
            Rect::new(x + w - 166., y + 651., 72., 34.),
            "读文本",
            false,
            editing && !self.file_path.trim().is_empty(),
        ) {
            match read_article(self.file_path.trim()) {
                Ok(text) => {
                    c.form.text = text;
                    self.dirty(c);
                }
                Err(error) => self.notice = error,
            }
        }
        if f.button(
            82,
            Rect::new(x + w - 86., y + 651., 86., 34.),
            "导出",
            false,
            editing && !c.gcode.is_empty() && !self.file_path.trim().is_empty(),
        ) {
            self.act(c, Action::Export(self.file_path.trim().into()));
        }
        let status = if self.notice.is_empty() {
            &c.status
        } else {
            &self.notice
        };
        f.canvas.ellipsis(status, x, y + 600., w, 14, false, MUTED);
    }
}

fn read_article(path: &str) -> Result<String, String> {
    const MAX_BYTES: u64 = 1024 * 1024;
    let mut article = String::new();
    std::fs::File::open(path)
        .and_then(|file| {
            if !file.metadata()?.is_file() {
                return Err(std::io::Error::other("需要普通文件"));
            }
            file.take(MAX_BYTES + 1).read_to_string(&mut article)
        })
        .map_err(|error| format!("读取 UTF-8 文本失败：{error}"))?;
    if article.len() > MAX_BYTES as usize {
        return Err("文本超过 1 MiB".into());
    }
    Ok(article.trim_start_matches('\u{feff}').to_owned())
}

fn position(v: Option<[f64; 3]>) -> String {
    v.map(|[x, y, z]| format!("X {x:.2}    Y {y:.2}    Z {z:.2}"))
        .unwrap_or_else(|| "X —     Y —     Z —".into())
}

pub(crate) fn choices(json: &str, key: &str) -> Result<Vec<String>, String> {
    zuse::json::with_value(json, |v| {
        let list = v.get(key).ok_or("名称列表缺失")?;
        list.elements().map(zuse::json::text).collect()
    })
}
pub(crate) struct Frame<'a, 'b> {
    canvas: &'a mut Canvas<'b>,
    ui: &'a mut Ui,
}
impl Frame<'_, '_> {
    pub(crate) fn heading(&mut self, s: &str, x: f32, y: f32) {
        self.canvas
            .text(crate::locale::text(s), x, y, 21, true, INK);
    }
    pub(crate) fn note(&mut self, s: &str, x: f32, y: f32, w: f32) {
        self.canvas
            .ellipsis(crate::locale::text(s), x, y, w, 13, false, MUTED);
    }
    fn caption(&mut self, s: &str, x: f32, y: f32) {
        self.canvas
            .text(crate::locale::text(s), x, y, 13, false, MUTED);
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn field(
        &mut self,
        id: u64,
        x: f32,
        y: f32,
        w: f32,
        label: &str,
        value: &mut String,
        hint: &str,
        enabled: bool,
    ) -> bool {
        self.caption(label, x, y);
        self.ui.input(
            self.canvas,
            id,
            Rect::new(x, y + 21., w, 34.),
            value,
            crate::locale::text(hint),
            false,
            enabled,
        )
    }
    pub(crate) fn button(
        &mut self,
        id: u64,
        r: Rect,
        label: &str,
        primary: bool,
        enabled: bool,
    ) -> bool {
        self.ui.button(
            self.canvas,
            id,
            r,
            crate::locale::text(label),
            if primary {
                ButtonStyle::Primary
            } else {
                ButtonStyle::Secondary
            },
            enabled,
        )
    }
}

fn paint_page(canvas: &mut Canvas<'_>, area: Rect, form: &Form, strokes: &[Vec<(f64, f64)>]) {
    let (width, height) = (form.area_width as f32, form.area_height as f32);
    let scale = form.reference_distance / form.reference_units;
    if width <= 0.0 || height <= 0.0 || !scale.is_finite() || scale <= 0.0 {
        paint_strokes(canvas, area, strokes);
        return;
    }
    canvas.rect(area, 8., 0xf1f3f4ff);
    let zoom = ((area.w - 42.) / width).min((area.h - 48.) / height);
    let page = Rect::new(
        area.x + (area.w - width * zoom) / 2.,
        area.y + 18.,
        width * zoom,
        height * zoom,
    );
    canvas.rect(
        Rect::new(page.x + 3., page.y + 3., page.w, page.h),
        2.,
        0xe8eaedff,
    );
    canvas.rect(page, 2., 0xffffffff);
    let [left, top, right, bottom] = form.margins.map(|n| n as f32 * zoom);
    let (x0, y0, x1, y1) = (
        page.x + left,
        page.y + top,
        page.x + page.w - right,
        page.y + page.h - bottom,
    );
    for (a, b) in [
        ((x0, y0), (x1, y0)),
        ((x1, y0), (x1, y1)),
        ((x1, y1), (x0, y1)),
        ((x0, y1), (x0, y0)),
    ] {
        canvas.line(a, b, 0.7, RULE);
    }
    let point = |(x, y): (f64, f64)| {
        let dx = (x - form.x) * if form.flip_x { -1. } else { 1. };
        let dy = (y - form.y) * if form.flip_y { -1. } else { 1. };
        let (dx, dy) = if form.swap_xy { (dy, dx) } else { (dx, dy) };
        (
            page.x + (dx / scale) as f32 * zoom,
            page.y - (dy / scale) as f32 * zoom,
        )
    };
    canvas.clip(page);
    for stroke in strokes {
        for segment in stroke.windows(2) {
            canvas.line(point(segment[0]), point(segment[1]), 1.3, INK);
        }
    }
    canvas.unclip();
    let caption = format!(
        "{:.1} × {:.1} mm  ·  {} 笔",
        f64::from(width) * scale,
        f64::from(height) * scale,
        strokes.len()
    );
    canvas.ellipsis(
        &caption,
        area.x + 14.,
        area.y + area.h - 22.,
        area.w - 28.,
        12,
        false,
        MUTED,
    );
}

fn paint_strokes(canvas: &mut Canvas<'_>, area: Rect, strokes: &[Vec<(f64, f64)>]) {
    canvas.rect(area, 7., PAPER);
    let mut bounds = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for &(x, y) in strokes.iter().flatten() {
        bounds = [
            bounds[0].min(x),
            bounds[1].min(y),
            bounds[2].max(x),
            bounds[3].max(y),
        ];
    }
    let [left, top, right, bottom] = bounds;
    if !left.is_finite() {
        canvas.text("等待预览", area.x + 18., area.y + 16., 15, false, MUTED);
        return;
    }
    let mut rect = area.inset(18.);
    rect.h = (rect.h - 20.).max(1.);
    let dx = (right - left) as f32;
    let dy = (bottom - top) as f32;
    let scale = (rect.w / dx.max(0.001)).min(rect.h / dy.max(0.001));
    let point = |(x, y): (f64, f64)| {
        (
            rect.x + (rect.w - dx * scale) / 2. + (x - left) as f32 * scale,
            rect.y + (rect.h - dy * scale) / 2. + (bottom - y) as f32 * scale,
        )
    };
    canvas.clip(area);
    for stroke in strokes {
        for p in stroke.windows(2) {
            canvas.line(point(p[0]), point(p[1]), 1.65, INK);
        }
    }
    canvas.unclip();
    canvas.text(
        &format!("{} 笔   ·   {:.1} × {:.1} mm", strokes.len(), dx, dy),
        area.x + 14.,
        area.y + area.h - 23.,
        12,
        false,
        MUTED,
    );
}

#[cfg(test)]
mod picker_tests {
    use super::*;

    fn candidates(paths: &[&str]) -> Vec<DeviceCandidate> {
        paths
            .iter()
            .map(|path| DeviceCandidate {
                path: path.to_string(),
                label: path.to_string(),
                detail: String::new(),
            })
            .collect()
    }

    #[test]
    fn scan_preserves_user_input_and_existing_selection() {
        let mut picker = DevicePicker::default();
        let mut port = String::new();
        picker.update(candidates(&["a", "b"]), &mut port, true);
        assert!(port.is_empty());
        picker.update(candidates(&["b"]), &mut port, true);
        assert_eq!(port, "b");
        picker.update(candidates(&["a", "b"]), &mut port, true);
        assert_eq!(port, "b");
        picker.update(candidates(&[]), &mut port, true);
        assert!(port.is_empty());
        picker.edited = true;
        port = "/dev/partially-typed".into();
        picker.update(candidates(&["a"]), &mut port, true);
        assert_eq!(port, "/dev/partially-typed");
        port.clear();
        picker.update(candidates(&["a"]), &mut port, true);
        assert!(port.is_empty());
    }

    #[test]
    fn selection_and_pages_survive_rescan_while_connected() {
        let mut picker = DevicePicker::default();
        let mut port = String::from("f");
        picker.update(
            candidates(&["a", "b", "c", "d", "e", "f"]),
            &mut port,
            false,
        );
        assert_eq!(picker.rows(), 3..6);
        picker.update(candidates(&["a"]), &mut port, false);
        assert_eq!(picker.rows(), 0..1);
        assert_eq!(port, "f");
    }
}
