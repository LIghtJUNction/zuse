use crate::alignment::Alignment;
use crate::article::Article;
use crate::controller::{Action, Controller, Form};
use crate::devices::{self, DeviceCandidate};
use crate::font_picker::FontPicker;
use crate::screen_output::ScreenOutput;
use crate::workspace_wizard::{Outcome, WorkspaceWizard};
use std::io::Read;
use zgpui::native::{ButtonStyle, Canvas, Event, GridLayout, PaneLayout, Rect, Ui, colors};

const INK: u32 = 0x202124ff;
const MUTED: u32 = 0x5f6368ff;
const RULE: u32 = 0xe8eaedff;
const PAPER: u32 = 0xf0f4faff;

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
    choose_direction: bool,
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
            choose_direction: false,
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
            || self
                .workspace
                .as_ref()
                .is_some_and(WorkspaceWizard::holding)
    }
    pub fn calibration_pointer(&mut self, event: &Event, c: &Controller) {
        if self.alignment.is_none()
            && let Some(workspace) = &mut self.workspace
        {
            workspace.pointer(event, c.connected() && !c.busy() && !workspace.pending());
        }
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
        self.choose_direction = false;
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
        self.mapping[2] = c.form.reference_units.to_string();
        self.mapping[3] = c.form.reference_distance.to_string();
        self.mapping[4] = c.form.area_width.to_string();
        self.mapping[5] = c.form.area_height.to_string();
        self.geometry[3] = c.form.x.to_string();
        self.geometry[4] = c.form.y.to_string();
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
        if let Some(fonts) = &mut self.fonts {
            fonts.scroll(delta);
            return;
        }
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
            let area = Rect::new((w - width) / 2., 24., width, h - 32.);
            canvas.rect(area.inset(-8.), 12., colors::SURFACE);
            if article.draw(canvas, ui, area) {
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
            frame.canvas.rect(area.inset(-8.), 12., colors::SURFACE);
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
        let pad = (16. + (w - 740.).max(0.) * 0.08).min(38.);
        let width = (w - 2. * pad).max(1.);
        let gap = (width * 0.035).clamp(16., 38.);
        let aligning = self.aligning();
        let title_width = canvas.measure("Zuse Writer", 30, true);
        let nav_width = if aligning { 108. } else { 92. };
        let nav_x = w - pad - nav_width;
        let target_width = 230.;
        let wrapped_targets = !aligning && width < title_width + nav_width + target_width + 64.;
        let inline_targets = !aligning && !wrapped_targets;
        let target_x = if inline_targets {
            nav_x - 12. - target_width
        } else {
            pad
        };
        let details_right = if inline_targets {
            target_x - 12.
        } else {
            nav_x - 16.
        };
        let inline_details = details_right - (pad + title_width + 18.) >= 230.;
        let top = match (inline_details, wrapped_targets) {
            (_, true) => 151.,
            (false, false) => 111.,
            (true, false) => 94.,
        };
        let layout = PaneLayout::new(
            Rect::new(pad, top, width, (h - top - 16.).max(0.)),
            (width * 0.30).clamp(252., 350.),
            456.,
            gap,
        );
        let (_, right_width) = layout.widths();
        let sidebar_min = if self.screen_mode {
            412.
        } else {
            self.picker.height()
                + if c.connected_device().is_some() {
                    56.
                } else {
                    0.
                }
                + if aligning { 370. } else { 749. }
        };
        let panel_min = if let Some(alignment) = &self.alignment {
            alignment.height(right_width)
        } else if let Some(workspace) = &self.workspace {
            workspace.height(right_width)
        } else if self.screen_mode {
            self.screen_output.height(right_width)
        } else {
            Self::writing_height(right_width, self.advanced)
        };
        let panes = layout.arrange(
            sidebar_min,
            panel_min,
            self.scroll_y,
            aligning && c.connected_device().is_some(),
        );
        self.scroll_y = panes.scroll_y;
        self.max_scroll = panes.max_scroll;
        let y = -panes.scroll_y;
        // Paint the same rectangles used for input; both header and surfaces scroll together.
        canvas.rect(panes.main.inset(-8.), 12., colors::SURFACE);
        if layout.stacked() {
            canvas.rect(panes.sidebar.inset(-8.), 12., colors::SURFACE);
        }
        canvas.rect(Rect::new(0., y, w, top - 24.), 0., colors::SURFACE);
        canvas.text("Zuse Writer", pad, y + 15., 30, true, INK);
        let connected = c.connected_device().is_some();
        let state = if closing {
            "正在取消并关闭"
        } else if c.busy() {
            if c.owns_job() {
                "本机作业中"
            } else {
                "共享作业中"
            }
        } else if connected {
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
        let state = if c.busy() || closing {
            let link = crate::locale::text(if connected { "已连接" } else { "未连接" });
            format!("{link} · {job}")
        } else {
            job
        };
        let state = state.as_str();
        let nav_y = y + 16.;
        let (status_x, status_right, status_y) = if inline_details && !wrapped_targets {
            (pad + title_width + 18., details_right, y + 29.)
        } else {
            (pad, w - pad, y + 61.)
        };
        let status_space = (status_right - status_x).max(0.);
        let state_width = (canvas.measure(state, 14, false) + 16.).min(status_space * 0.58);
        let dot = Rect::new(status_right - state_width, status_y + 5., 8., 8.);
        canvas.rect(
            dot,
            4.,
            if connected {
                colors::BLUE
            } else {
                colors::MUTED
            },
        );
        if !connected {
            canvas.rect(dot.inset(1.5), 3., colors::SURFACE);
        }
        canvas.ellipsis(
            &session,
            status_x,
            status_y,
            (status_space - state_width - 14.).max(0.),
            14,
            false,
            MUTED,
        );
        canvas.ellipsis(
            state,
            dot.x + 14.,
            status_y,
            (state_width - 14.).max(0.),
            14,
            false,
            INK,
        );
        canvas.line((0., y + top - 24.), (w, y + top - 24.), 1., RULE);
        let mut frame = Frame { canvas, ui };
        if !aligning {
            let tx = target_x;
            let ty = if wrapped_targets { y + 87. } else { nav_y };
            frame.caption("输出到", tx, ty + 9.);
            frame
                .canvas
                .rect(Rect::new(tx + 56., ty - 2., 176., 36.), 12., colors::BG);
            for (i, label) in ["写字机", "屏幕输入"].iter().enumerate() {
                let selected = i == 1;
                let r = Rect::new(tx + 58. + i as f32 * 86., ty, 84., 32.);
                let active = self.screen_mode == selected;
                if active {
                    frame.canvas.rect(r, 10., colors::BLUE_PALE);
                }
                if frame.quiet_button(180 + i as u64, r, label, !closing && !c.busy())
                    && self.screen_mode != selected
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
            && frame.quiet_button(
                170,
                Rect::new(nav_x, nav_y, nav_width, 35.),
                if self.alignment.is_some() {
                    "返回向导"
                } else if aligning {
                    "返回书写"
                } else {
                    "对笔"
                },
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
            let valid = self.screen_form(&mut frame, c, panes.sidebar, closing);
            if !valid {
                self.screen_output.deactivate();
            }
            self.screen_output
                .draw(frame.canvas, frame.ui, c, panes.main, closing || !valid);
            return;
        }
        self.machine(
            &mut frame,
            c,
            panes.sidebar.x,
            panes.sidebar.y,
            panes.sidebar.w,
            closing,
        );
        if self.choose_direction {
            let (rx, y, right) = (panes.main.x, panes.main.y, panes.main.w);
            frame.heading("纸面方向", rx, y);
            frame.note(
                "以正常阅读纸张的方向为准。如果字上下镜像，就选反转纸面上下。",
                rx,
                y + 44.,
                right,
            );
            let width = (right - 8.) / 2.;
            for (index, (reverse, label)) in [(false, "保留纸面上下"), (true, "反转纸面上下")]
                .into_iter()
                .enumerate()
            {
                if frame.button(
                    706 + index as u64,
                    Rect::new(rx + index as f32 * (width + 8.), y + 82., width, 37.),
                    label,
                    false,
                    !closing && !c.busy(),
                ) {
                    match c.set_paper_direction(reverse, true) {
                        Ok(()) => {
                            self.choose_direction = false;
                            self.finish_alignment(
                                c,
                                "对笔与纸面方向已应用。请重新生成预览。".into(),
                            );
                        }
                        Err(error) => self.notice = error,
                    }
                    return;
                }
            }
            frame.note(&self.notice, rx, y + 136., right);
            return;
        }
        if let Some(alignment) = &mut self.alignment {
            if alignment.draw(frame.canvas, frame.ui, c, panes.main, closing) {
                self.choose_direction = true;
                self.notice.clear();
                self.scroll_y = 0.;
            }
        } else if let Some(workspace) = &mut self.workspace {
            match workspace.draw(frame.canvas, frame.ui, c, panes.main, closing) {
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
            self.writing(&mut frame, c, panes.main, closing);
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
        if f.quiet_button(
            702,
            Rect::new(x + w - 140., y, 66., 28.),
            appearance,
            !closing,
        ) {
            self.dark = !self.dark;
        }
        let language = if self.english {
            "简体中文"
        } else {
            "English"
        };
        if f.quiet_button(703, Rect::new(x + w - 70., y, 70., 28.), language, !closing) {
            self.english = !self.english;
            crate::locale::set_english(self.english);
        }
        let linked = c.connected_device().is_some();
        let editing = !c.busy() && !linked && !closing;
        let height = self.device_list(f, c, x, y + 40., w, closing);
        let mut y = y + height;
        if let Some((port, baud, firmware)) = c.connected_device() {
            f.canvas.ellipsis(
                &format!(
                    "{} · {}",
                    crate::locale::text("已连接"),
                    firmware.unwrap_or(crate::locale::text("固件未报告型号"))
                ),
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
            f.status(705, Rect::new(x, y + 345., w, 26.), &c.status);
            return;
        }
        f.canvas.line((x, y + 353.), (x + w, y + 353.), 1., RULE);
        f.heading("手动调节", x, y + 369.);
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
        let paper_ready = enabled && c.paper.is_some();
        {
            let [a, b] = c
                .paper
                .as_ref()
                .map_or([[0.; 2]; 2], |paper| [paper.corner_a, paper.corner_b]);
            let up = c.paper.as_ref().map_or(0., |paper| paper.pen_up_z);
            let down = c.paper.as_ref().map_or(0., |paper| paper.pen_down_z);
            let positions = [
                ("回到左上角", a),
                ("回到右下角", b),
                (
                    "回到中心",
                    std::array::from_fn(|i| f64::midpoint(a[i], b[i])),
                ),
            ];
            for (index, (label, [target_x, target_y])) in positions.into_iter().enumerate() {
                if f.button(
                    705 + index as u64,
                    Rect::new(x + index as f32 * (bw + 8.), y + 650., bw, 34.),
                    label,
                    false,
                    paper_ready,
                ) {
                    self.act(
                        c,
                        Action::Park {
                            x: target_x,
                            y: target_y,
                            up,
                        },
                    );
                }
            }
            for (index, label) in ["抬笔", "落笔"].into_iter().enumerate() {
                if f.button(
                    23 + index as u64,
                    Rect::new(x + index as f32 * (half + 8.), y + 695., half, 34.),
                    label,
                    false,
                    paper_ready,
                ) {
                    c.form.pen_up = up;
                    c.form.pen_down = down;
                    self.act(
                        c,
                        if index == 0 {
                            Action::PenUp
                        } else {
                            Action::PenDown
                        },
                    );
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
        let title = format!("{open} {} ({count})", crate::locale::text("USB 设备"));
        if f.button(90, Rect::new(x, y, w - 76., 32.), &title, false, count > 0) {
            list.expanded = !list.expanded;
        }
        if f.quiet_button(91, Rect::new(x + w - 68., y, 68., 32.), "扫描", !closing) {
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
                if selected {
                    f.canvas.rect(rect, 8., colors::BLUE_PALE);
                    f.canvas.rect(
                        Rect::new(rect.x, rect.y + 8., 3., rect.h - 16.),
                        1.5,
                        colors::BLUE,
                    );
                }
                if editing && f.quiet_button(100 + i as u64, rect, "", true) {
                    c.form.port.clone_from(&device.path);
                    list.edited = true;
                    list.automatic = None;
                }
                let ink = INK;
                let secondary = MUTED;
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
        let text_w = if stacked { w } else { (w - 8.) * 0.58 };
        let font_x = if stacked { x } else { x + text_w + 8. };
        let font_y = if stacked { y + 106. } else { y + 41. };
        let font_w = x + w - font_x;
        let button_width = |canvas: &mut Canvas<'_>, label: &str, group: f32| {
            (canvas.measure(crate::locale::text(label), 14, false) + 26.).min(group * 0.56)
        };
        let edit_w = button_width(f.canvas, "编辑文章", text_w);
        let library_w = button_width(f.canvas, "字体库", font_w);
        let mut changed = f.field(
            30,
            x,
            y + 41.,
            (text_w - edit_w - 8.).max(0.),
            "文字",
            &mut c.form.text,
            "输入要书写的文字",
            editing,
        );
        if f.button(
            33,
            Rect::new(x + text_w - edit_w, y + 62., edit_w, 34.),
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
            (font_w - library_w - 8.).max(0.),
            "单线字体",
            &mut c.form.font,
            "builtin",
            editing,
        );
        if f.button(
            32,
            Rect::new(x + w - library_w, font_y + 21., library_w, 34.),
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
    fn writing_height(width: f32, advanced: bool) -> f32 {
        WritingLayout::new(Rect::new(0., 0., width, 0.), advanced).minimum_height
    }

    fn writing(&mut self, f: &mut Frame<'_, '_>, c: &mut Controller, area: Rect, closing: bool) {
        let (x, y, w) = (area.x, area.y, area.w);
        let editing = !c.busy() && !closing;
        let layout = WritingLayout::new(area, self.advanced);
        let switch_label = if self.advanced {
            "书写参数"
        } else {
            "纸张与字迹"
        };
        let switch_width = (f
            .canvas
            .measure(crate::locale::text(switch_label), 14, false)
            + 26.)
            .min(w * 0.55);
        f.canvas.ellipsis(
            crate::locale::text("书写内容"),
            x,
            y,
            (w - switch_width - 8.).max(0.),
            21,
            true,
            INK,
        );
        if f.button(
            35,
            Rect::new(x + w - switch_width, y - 2., switch_width, 32.),
            switch_label,
            false,
            editing,
        ) {
            self.advanced = !self.advanced;
            return;
        }
        let mut changed = self.text_font(f, c, area, layout.stack_text, editing);
        if self.advanced {
            for (i, label) in MAP_LABELS.iter().enumerate() {
                let field = layout.parameter(i);
                changed |= f.field(
                    200 + i as u64,
                    field.x,
                    field.y,
                    field.w,
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
                if f.button(220 + i as u64, layout.flip(i), &label, *value, editing) {
                    *value = !*value;
                    changed = true;
                }
            }
        } else {
            for (i, label) in LABELS.iter().enumerate() {
                let field = layout.parameter(i);
                changed |= f.field(
                    40 + i as u64,
                    field.x,
                    field.y,
                    field.w,
                    label,
                    &mut self.geometry[i],
                    "",
                    editing,
                );
            }
            let field = layout.parameter(LABELS.len());
            changed |= f.field(
                48,
                field.x,
                field.y,
                field.w,
                "随机种子",
                &mut self.seed,
                "0",
                editing,
            );
        }
        if changed {
            self.dirty(c);
        }
        let preview_title = layout.preview.y - 46.;
        if self.show_origin_hint && !self.advanced {
            f.canvas.ellipsis(
                crate::locale::text("书写起点 X/Y 对应纸张左上角。"),
                x,
                preview_title - 25.,
                w,
                13,
                false,
                MUTED,
            );
        }
        let update_width =
            (f.canvas.measure(crate::locale::text("更新预览"), 14, false) + 26.).min(w * 0.5);
        f.canvas.ellipsis(
            crate::locale::text("笔迹预览"),
            x,
            preview_title,
            (w - update_width - 8.).max(0.),
            21,
            true,
            INK,
        );
        if f.button(
            50,
            Rect::new(x + w - update_width, preview_title - 4., update_width, 34.),
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
        paint_page(f.canvas, layout.preview, &c.form, &c.strokes);
        if f.button(
            53,
            layout.action(0),
            "校验程序",
            false,
            editing && c.connected() && !c.gcode.is_empty(),
        ) {
            self.act(c, Action::Check);
        }
        if f.button(
            51,
            layout.action(1),
            "发送到写字机",
            true,
            editing && c.connected() && c.paper.is_some() && !c.gcode.is_empty(),
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
        if f.ui.button(
            f.canvas,
            52,
            layout.action(2),
            crate::locale::text(label),
            if c.busy() {
                ButtonStyle::Danger
            } else {
                ButtonStyle::Secondary
            },
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
            layout.file_y,
            if layout.stack_file { w } else { w - 174. },
            "文本或 G-code 文件",
            &mut self.file_path,
            "输入文件路径",
            editing,
        );
        if f.quiet_button(
            83,
            layout.file_button(0),
            "读文本",
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
        if f.quiet_button(
            82,
            layout.file_button(1),
            "导出",
            editing && !c.gcode.is_empty() && !self.file_path.trim().is_empty(),
        ) {
            self.act(c, Action::Export(self.file_path.trim().into()));
        }
        let status = if self.notice.is_empty() {
            &c.status
        } else {
            &self.notice
        };
        f.status(84, Rect::new(x, layout.file_y - 31., w, 26.), status);
    }
}

/// The form, actions and file row use their natural height. The paper preview
/// occupies the space left between them, including any extra window height.
struct WritingLayout {
    area: Rect,
    stack_text: bool,
    parameters: GridLayout,
    flips: GridLayout,
    preview: Rect,
    actions: GridLayout,
    file_y: f32,
    stack_file: bool,
    minimum_height: f32,
}

impl WritingLayout {
    fn new(area: Rect, advanced: bool) -> Self {
        let stack_text = area.w < 500.;
        let stack_file = area.w < 385.;
        let row = |y| Rect::new(area.x, y, area.w, area.h);
        let parameters_y = area.y + if stack_text { 179. } else { 114. };
        let parameters = GridLayout::new(row(parameters_y), 134., 3, 8.);
        let count = if advanced {
            MAP_LABELS.len()
        } else {
            LABELS.len() + 1
        };
        let fields_end = parameters_y + parameters.height(count, 58.) + 8.;
        let flips_y = fields_end + 2.;
        let flips = GridLayout::new(row(flips_y), 134., 3, 8.);
        let heading_y = if advanced {
            flips_y + flips.height(3, 34.) + 38.
        } else {
            fields_end + 19.
        };
        let preview_y = heading_y + 46.;
        let actions_grid = |y| GridLayout::new(row(y), 170., 3, 8.);
        let actions_height = actions_grid(0.).height(3, 39.);
        let file_height = if stack_file { 97. } else { 55. };
        let footer = 16. + actions_height + 10. + 26. + file_height;
        let minimum_height = preview_y - area.y + 180. + footer;
        let preview_height = 180. + (area.h - minimum_height).max(0.);
        let preview = Rect::new(area.x, preview_y, area.w, preview_height);
        let actions_y = preview_y + preview_height + 16.;
        let actions = actions_grid(actions_y);
        let file_y = actions_y + actions_height + 36.;
        Self {
            area,
            stack_text,
            parameters,
            flips,
            preview,
            actions,
            file_y,
            stack_file,
            minimum_height,
        }
    }

    fn parameter(&self, index: usize) -> Rect {
        self.parameters.cell(index, 58.)
    }

    fn flip(&self, index: usize) -> Rect {
        self.flips.cell(index, 34.)
    }

    fn action(&self, index: usize) -> Rect {
        let mut button = self.actions.cell(index, 39.);
        if self.actions.columns() == 2 && index == 2 {
            button.w = self.area.w;
        }
        button
    }

    fn file_button(&self, index: usize) -> Rect {
        if self.stack_file {
            let row = Rect::new(self.area.x, self.file_y + 63., self.area.w, 34.);
            GridLayout::new(row, 80., 2, 8.).cell(index, 34.)
        } else {
            let (offset, width) = if index == 0 { (166., 72.) } else { (86., 86.) };
            Rect::new(
                self.area.x + self.area.w - offset,
                self.file_y + 21.,
                width,
                34.,
            )
        }
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
        self.canvas
            .ellipsis(crate::locale::text(label), x, y, w, 13, false, MUTED);
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
    fn status(&mut self, id: u64, area: Rect, text: &str) {
        let mut available = area.w;
        if self.canvas.measure(text, 14, false) > area.w {
            let label = crate::locale::text("复制状态");
            let width = self.canvas.measure(label, 15, false) + 20.;
            available = (area.w - width - 8.).max(0.);
            let button = Rect::new(area.x + area.w - width, area.y, width, area.h);
            if self.quiet_button(id, button, "复制状态", true) {
                self.ui.clipboard = Some(text.to_owned());
            }
        }
        self.canvas
            .ellipsis(text, area.x, area.y + 5., available, 14, false, MUTED);
    }

    pub(crate) fn quiet_button(&mut self, id: u64, rect: Rect, label: &str, enabled: bool) -> bool {
        self.ui.button(
            self.canvas,
            id,
            rect,
            crate::locale::text(label),
            ButtonStyle::Quiet,
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
    // The surrounding workspace follows the UI palette. The sheet, guides and
    // ink represent physical materials and keep their colors across themes.
    const SHEET: u32 = 0xfffdf7ff;
    const GRAPHITE: u32 = 0x272b2cff;
    const MARGIN_GUIDE: u32 = 0x8c9086ff;
    canvas.rect(area, 8., PAPER);
    let zoom = ((area.w - 42.) / width).min((area.h - 48.) / height);
    let page = Rect::new(
        area.x + (area.w - width * zoom) / 2.,
        area.y + 18. + (area.h - 48. - height * zoom).max(0.) * 0.5,
        width * zoom,
        height * zoom,
    );
    canvas.rect(
        Rect::new(page.x - 1., page.y + 2., page.w + 2., page.h + 2.),
        3.,
        0x17211b16,
    );
    canvas.rect(
        Rect::new(page.x - 0.7, page.y - 0.7, page.w + 1.4, page.h + 1.4),
        2.,
        0x27312b29,
    );
    canvas.rect(page, 1.5, SHEET);
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
        canvas.line(a, b, 1., MARGIN_GUIDE);
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
            canvas.line(point(segment[0]), point(segment[1]), 1.45, GRAPHITE);
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
    canvas.rect(area, 8., PAPER);
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
        let label = crate::locale::text("等待预览");
        let label_width = canvas.measure(label, 15, false);
        canvas.ellipsis(
            label,
            area.x + ((area.w - label_width) * 0.5).max(14.),
            area.y + area.h * 0.5 - 9.,
            (area.w - 28.).max(0.),
            15,
            false,
            MUTED,
        );
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
