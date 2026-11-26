use crate::controller::{Action, Controller};
use zgpui::native::{ButtonStyle, Canvas, Rect, Ui};

const INK: u32 = 0x222222ff;
const MUTED: u32 = 0x696965ff;
const RULE: u32 = 0xdededdff;
const PAPER: u32 = 0xf6f6f3ff;

// Keep partially typed numbers in the editor. Validate and transfer them as
// a group when building a plan, never on each digit of a keyboard event.
pub struct WriterView {
    geometry: [String; 8],
    mapping: [String; 4],
    advanced: bool,
    confirm_cancel: Option<(u64, u64, u64)>,
    export_path: String,
    baud: String,
    step: String,
    seed: String,
    notice: String,
    scroll_y: f32,
    max_scroll: f32,
}

const MAP_LABELS: [&str; 4] = [
    "字形 X 偏移幅度",
    "字形 Y 偏移幅度",
    "校准参考长度",
    "实测距离 mm",
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
    pub fn new(c: &mut Controller) -> Self {
        if c.form.text.is_empty() {
            c.form.text = "你好 Zuse".into();
        }
        c.dispatch(Action::Preview);
        let f = &c.form;
        Self {
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
            ]
            .map(|v| v.to_string()),
            advanced: false,
            confirm_cancel: None,
            export_path: String::new(),
            baud: f.baud.to_string(),
            step: f.jog_step.to_string(),
            seed: f.seed.to_string(),
            notice: String::new(),
            scroll_y: 0.0,
            max_scroll: 0.0,
        }
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
        let mut mapping = [0.; 4];
        for (i, v) in mapping.iter_mut().enumerate() {
            *v = number(&self.mapping[i], MAP_LABELS[i])?;
        }
        let f = &mut c.form;
        [
            f.offset_x,
            f.offset_y,
            f.reference_units,
            f.reference_distance,
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
        let wide = w >= 830.0;
        let pad = if wide { 38.0 } else { 16.0 };
        let gap = if wide { 38.0 } else { 18.0 };
        let width = (w - 2.0 * pad).max(200.);
        let left = if wide {
            (width * 0.30).clamp(242., 338.)
        } else {
            width
        };
        let (rx, ry, right) = if wide {
            (pad + left + gap, 94., width - left - gap)
        } else {
            (pad, 772., width)
        };
        let content_h = ry + 708.;
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
        } else if c.connected() {
            "已连接"
        } else {
            "未连接"
        };
        let session = format!("共享会话  {}", c.session_name());
        let job = c
            .active_job()
            .map(|id| format!("{state}  #{id}"))
            .unwrap_or_else(|| state.to_owned());
        let state = job.as_str();
        canvas.ellipsis(
            &session,
            pad + title_width + 22.,
            y + 29.,
            (w - 2. * pad - title_width - 32. - canvas.measure(state, 14, false)).max(0.),
            14,
            false,
            MUTED,
        );
        let sw = canvas.measure(state, 14, false);
        canvas.text(state, w - pad - sw, y + 29., 14, false, MUTED);
        canvas.line((pad, y + 66.), (w - pad, y + 66.), 1., RULE);
        let mut frame = Frame { canvas, ui };
        self.machine(&mut frame, c, pad, y + 94., left, closing);
        self.writing(&mut frame, c, rx, y + ry, right, closing);
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
        let editing = !c.busy() && !c.connected() && !closing;
        f.field(
            1,
            x,
            y + 41.,
            w - 88.,
            "串口",
            &mut c.form.port,
            "端口路径",
            editing,
        );
        if f.button(
            2,
            Rect::new(x + w - 80., y + 62., 80., 34.),
            "下一端口",
            false,
            editing,
        ) {
            match zuse::serial::ports().and_then(|s| choices(&s, None)) {
                Ok(ports) if !ports.is_empty() => {
                    c.form.port = next(&ports, &c.form.port);
                    self.notice.clear();
                }
                Ok(_) => self.notice = "没有发现串口，可手动输入端口路径。".into(),
                Err(e) => self.notice = e,
            }
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
            if c.connected() { "断开" } else { "连接" },
            true,
            !c.busy() && !closing,
        ) {
            if c.connected() {
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
        if f.button(
            5,
            Rect::new(x, y + 309., w, 33.),
            "刷新状态与坐标",
            false,
            !c.busy() && !closing,
        ) {
            self.act(c, Action::Refresh);
        }
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
                "校准参数"
            },
            false,
            editing,
        ) {
            self.advanced = !self.advanced;
        }
        let text_w = w * 0.60;
        let mut changed = f.field(
            30,
            x,
            y + 41.,
            text_w - 8.,
            "文字",
            &mut c.form.text,
            "输入要书写的文字",
            editing,
        );
        changed |= f.field(
            31,
            x + text_w,
            y + 41.,
            w - text_w - 88.,
            "单线字体",
            &mut c.form.font,
            "builtin",
            editing,
        );
        if f.button(
            32,
            Rect::new(x + w - 80., y + 62., 80., 34.),
            "下一字体",
            false,
            editing,
        ) {
            match zuse::font_store::cli(&["list"]).and_then(|s| choices(&s, Some("fonts"))) {
                Ok(fonts) if !fonts.is_empty() => {
                    c.form.font = next(&fonts, &c.form.font);
                    changed = true;
                }
                Ok(_) => {}
                Err(e) => self.notice = e,
            }
        }
        let pw = (w - 16.) / 3.;
        if self.advanced {
            for (i, label) in MAP_LABELS.iter().enumerate() {
                changed |= f.field(
                    70 + i as u64,
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
                    75 + i as u64,
                    Rect::new(x + i as f32 * (pw + 8.), y + 259., pw, 34.),
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
        let area = Rect::new(x, y + 377., w, 152.);
        paint_strokes(f.canvas, area, &c.strokes);
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
            w - 94.,
            "导出路径",
            &mut self.export_path,
            "文件名或 .gcode 路径",
            editing,
        );
        if f.button(
            82,
            Rect::new(x + w - 86., y + 651., 86., 34.),
            "导出",
            false,
            editing && !c.gcode.is_empty() && !self.export_path.trim().is_empty(),
        ) {
            self.act(c, Action::Export(self.export_path.trim().into()));
        }
        let status = if self.notice.is_empty() {
            &c.status
        } else {
            &self.notice
        };
        f.canvas.ellipsis(status, x, y + 600., w, 14, false, MUTED);
    }
}

fn position(v: Option<[f64; 3]>) -> String {
    v.map(|[x, y, z]| format!("X {x:.2}    Y {y:.2}    Z {z:.2}"))
        .unwrap_or_else(|| "X —     Y —     Z —".into())
}

fn choices(json: &str, key: Option<&str>) -> Result<Vec<String>, String> {
    zuse::json::with_value(json, |v| {
        let list = match key {
            Some(k) => v.get(k).ok_or("名称列表缺失")?,
            None => v,
        };
        list.elements().map(zuse::json::text).collect()
    })
}
fn next(list: &[String], current: &str) -> String {
    let i = list
        .iter()
        .position(|s| s == current)
        .map_or(0, |i| (i + 1) % list.len());
    list[i].clone()
}

struct Frame<'a, 'b> {
    canvas: &'a mut Canvas<'b>,
    ui: &'a mut Ui,
}
impl Frame<'_, '_> {
    fn heading(&mut self, s: &str, x: f32, y: f32) {
        self.canvas.text(s, x, y, 21, true, INK);
    }
    fn caption(&mut self, s: &str, x: f32, y: f32) {
        self.canvas.text(s, x, y, 13, false, MUTED);
    }
    #[allow(clippy::too_many_arguments)]
    fn field(
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
            hint,
            false,
            enabled,
        )
    }
    fn button(&mut self, id: u64, r: Rect, label: &str, primary: bool, enabled: bool) -> bool {
        self.ui.button(
            self.canvas,
            id,
            r,
            label,
            if primary {
                ButtonStyle::Primary
            } else {
                ButtonStyle::Secondary
            },
            enabled,
        )
    }
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
