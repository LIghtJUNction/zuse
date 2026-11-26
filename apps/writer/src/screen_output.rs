//! Screen-coordinate handwriting, sharing the named session's submission and cancellation path.
use crate::controller::{Action, Controller, Form};
use std::time::{Duration, Instant};
use zgpui::native::{ButtonStyle, Canvas, GridLayout, Rect, Ui, colors};
use zuse::{json, pen_input};

const DELAY: Duration = Duration::from_secs(3);
type Strokes = Vec<Vec<(f64, f64)>>;

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Output {
    #[default]
    Mouse,
    Touchscreen,
    Touchpad,
}

impl Output {
    fn operation(self) -> &'static str {
        match self {
            Self::Mouse => "write.mouse",
            Self::Touchscreen => "write.touchscreen",
            Self::Touchpad => "write.touchpad",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Mouse => "鼠标",
            Self::Touchscreen => "触摸屏",
            Self::Touchpad => "虚拟触摸板",
        }
    }
}

struct Fields {
    x: String,
    y: String,
    scale: String,
    width: String,
    height: String,
    interval: String,
}
impl Default for Fields {
    fn default() -> Self {
        Self {
            x: "200".into(),
            y: "150".into(),
            scale: "4".into(),
            width: "1920".into(),
            height: "1080".into(),
            interval: "8".into(),
        }
    }
}

#[derive(Clone, PartialEq)]
struct Plan {
    arguments: String,
    target: Output,
    surface: Option<(u16, u16)>,
    interval: u32,
}

impl Plan {
    fn new(f: &Form, fields: &Fields, target: Output) -> Result<Self, String> {
        if f.text.trim().is_empty() {
            return Err("请输入书写内容".into());
        }
        let number = |v: &str, name: &str| {
            v.trim()
                .parse::<f64>()
                .ok()
                .filter(|n| n.is_finite())
                .ok_or_else(|| format!("请检查{name}"))
        };
        let x = number(&fields.x, "起点 X")?;
        let y = number(&fields.y, "起点 Y")?;
        let scale = number(&fields.scale, "像素比例")?;
        if scale <= 0. {
            return Err("像素比例须大于 0".into());
        }
        if [f.size, f.advance, f.line_height, f.offset_x, f.offset_y]
            .iter()
            .any(|n| !n.is_finite())
            || f.seed > i64::MAX as u64
        {
            return Err("请检查字号、字距、偏移和随机种子".into());
        }
        let interval = fields
            .interval
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|n| *n <= 1000)
            .ok_or("间隔有效范围为 0–1000 ms")?;
        let dimension = |v: &str| {
            v.trim()
                .parse::<u16>()
                .ok()
                .filter(|n| *n >= 2)
                .ok_or("触摸面宽高有效范围为 2–65535 px")
        };
        let surface = if target == Output::Mouse {
            None
        } else {
            Some((dimension(&fields.width)?, dimension(&fields.height)?))
        };
        let arguments = format!(
            "text={},font={},size={},advance={},line_height={},area_width={},area_height={},margin_left={},margin_top={},margin_right={},margin_bottom={},offset_x_ratio={},offset_y_ratio={},tilt_degrees={},size_ratio={},seed={},x={x},y={y},reference_units=1,reference_distance={scale}",
            json::quote(&f.text),
            json::quote(&f.font),
            f.size,
            f.advance,
            f.line_height,
            f.area_width,
            f.area_height,
            f.margins[0],
            f.margins[1],
            f.margins[2],
            f.margins[3],
            f.offset_x / 100.,
            f.offset_y / 100.,
            f.tilt_degrees,
            f.size_percent / 100.,
            f.seed
        );
        if arguments.len() + 128 > 256 * 1024 {
            return Err("文字超过会话脚本容量".into());
        }
        Ok(Self {
            arguments,
            target,
            surface,
            interval,
        })
    }
    fn preview(&self) -> String {
        format!("write.preview({})", self.arguments)
    }
    fn send(&self) -> String {
        let surface = self
            .surface
            .map_or_else(String::new, |(w, h)| format!(",width={w},height={h}"));
        format!(
            "{}({},interval={}{})",
            self.target.operation(),
            self.arguments,
            self.interval,
            surface
        )
    }
    fn validate(&self, strokes: &Strokes) -> Result<(), String> {
        let target = match (self.target, self.surface) {
            (Output::Mouse, None) => pen_input::Target::Mouse,
            (Output::Touchscreen, Some((width, height))) => {
                pen_input::Target::Touchscreen { width, height }
            }
            (Output::Touchpad, Some((width, height))) => {
                pen_input::Target::Touchpad { width, height }
            }
            _ => return Err("触摸面尺寸缺失".into()),
        };
        if strokes.is_empty() {
            return Err("当前文字没有可输出的笔画".into());
        }
        pen_input::validate(target, strokes, self.interval).map(|_| ())
    }
}

fn read_preview(value: &str) -> Result<Strokes, String> {
    json::with_value(value, |root| {
        let list = root.get("strokes").ok_or("预览回执缺少笔迹")?;
        let mut strokes = Vec::new();
        let mut count = 0;
        for stroke in list.elements() {
            let mut points = Vec::new();
            for pt in stroke.elements() {
                let mut xy = pt.elements();
                let coord = |v: Option<&str>| {
                    v.and_then(|v| v.parse::<f64>().ok())
                        .filter(|n| n.is_finite())
                        .ok_or("笔迹坐标无效")
                };
                let x = coord(xy.next().map(|n| n.raw()))?;
                let y = coord(xy.next().map(|n| n.raw()))?;
                if xy.next().is_some() {
                    return Err("笔迹顶点必须只有两个坐标".into());
                }
                points.push((x, y));
            }
            if points.is_empty() {
                return Err("笔迹回执含有空笔画".into());
            }
            count += points.len();
            strokes.push(points);
        }
        if root.get("points").and_then(|n| n.integer().ok()) != Some(count as i64) {
            return Err("预览回执点数不一致".into());
        }
        Ok(strokes)
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Timer {
    Waiting(u64),
    Fire,
    Expired,
}

fn countdown(due: Instant, now: Instant) -> Timer {
    if let Some(left) = due.checked_duration_since(now)
        && !left.is_zero()
    {
        Timer::Waiting(left.as_millis().div_ceil(1000).max(1) as u64)
    } else if now.saturating_duration_since(due) > Duration::from_secs(1) {
        Timer::Expired
    } else {
        Timer::Fire
    }
}

#[cfg(target_os = "linux")]
fn focused_writer(snapshot: &str, own_pid: u32) -> bool {
    json::with_value(snapshot, |root| {
        Ok(root.elements().any(|window| {
            if window.get("focused").and_then(|v| v.boolean()) != Some(true)
                || window.get("mapped").and_then(|v| v.boolean()) == Some(false)
            {
                return false;
            }
            if window.get("pid").and_then(|v| v.integer().ok()) == Some(i64::from(own_pid)) {
                return true;
            }
            // Match another Writer instance as well as X11 windows without _NET_WM_PID.
            window
                .get("class")
                .and_then(|v| json::text(v).ok())
                .is_some_and(|s| s.eq_ignore_ascii_case("zuse"))
                && window
                    .get("title")
                    .and_then(|v| json::text(v).ok())
                    .as_deref()
                    == Some("Zuse Writer · 写字机")
        }))
    })
    .unwrap_or(false)
}

struct Preview {
    plan: Plan,
    strokes: Strokes,
}
enum Pending {
    Preview(u64, Plan),
    Send(u64),
}

#[derive(Default)]
pub struct ScreenOutput {
    target: Output,
    fields: Fields,
    preview: Option<Preview>,
    pending: Option<Pending>,
    due: Option<Instant>,
    notice: String,
}

struct ScreenLayout {
    targets: GridLayout,
    fields: GridLayout,
    actions: GridLayout,
    preview: Rect,
    minimum_height: f32,
}

impl ScreenLayout {
    const TARGET_H: f32 = 32.;
    const FIELD_H: f32 = 52.;
    const ACTION_H: f32 = 34.;

    fn new(area: Rect) -> Self {
        let grid = |y, min_width, columns| {
            GridLayout::new(Rect::new(area.x, y, area.w, area.h), min_width, columns, 8.)
        };
        let targets = GridLayout::new(
            Rect::new(area.x, area.y, area.w.min(376.), area.h),
            100.,
            3,
            8.,
        );
        let fields_y = area.y + targets.height(3, Self::TARGET_H) + 12.;
        let fields = grid(fields_y, 139., 3);
        let actions_y = fields_y + fields.height(6, Self::FIELD_H) + 8.;
        let actions = grid(actions_y, 156., 2);
        let preview_y = actions_y + actions.height(2, Self::ACTION_H) + 12.;
        let minimum_height = preview_y - area.y + 164. + 40.;
        let preview_h = 164. + (area.h - minimum_height).max(0.);
        Self {
            targets,
            fields,
            actions,
            preview: Rect::new(area.x, preview_y, area.w, preview_h),
            minimum_height,
        }
    }
}

impl ScreenOutput {
    pub fn height(&self, width: f32) -> f32 {
        ScreenLayout::new(Rect::new(0., 0., width, 0.)).minimum_height
    }
    pub fn needs_tick(&self) -> bool {
        self.due.is_some()
    }
    /// Call when leaving this panel; an old countdown must never send when this view is reopened.
    pub fn deactivate(&mut self) {
        if self.due.take().is_some() {
            self.notice = "已取消倒计时".into();
        }
    }

    fn settle(&mut self, c: &Controller) {
        let Some(p) = self.pending.take() else {
            return;
        };
        let expected = match &p {
            Pending::Preview(id, _) | Pending::Send(id) => *id,
        };
        let Some((_, value)) = c.screen_receipt().filter(|(id, _)| *id == expected) else {
            if c.busy() {
                self.pending = Some(p);
            } else {
                self.notice = c.status.clone();
            }
            return;
        };
        match p {
            Pending::Preview(_, plan) => {
                match read_preview(value).and_then(|strokes| {
                    plan.validate(&strokes)?;
                    Ok(strokes)
                }) {
                    Ok(strokes) => {
                        self.notice = format!("{} 笔 · 屏幕坐标，Y 向下", strokes.len());
                        self.preview = Some(Preview { plan, strokes });
                    }
                    Err(e) => self.notice = e,
                }
            }
            Pending::Send(_) => {
                self.notice = if json::field(value, "delivery").as_deref() == Ok("submitted") {
                    "屏幕输入已提交".into()
                } else {
                    "会话已返回，请核对输出回执".into()
                };
            }
        }
    }

    fn update_timer(
        &mut self,
        c: &mut Controller,
        plan: Option<&Plan>,
        closing: bool,
        now: Instant,
    ) {
        let Some(due) = self.due else {
            return;
        };
        if closing || c.busy() || self.preview.as_ref().map(|p| &p.plan) != plan || plan.is_none() {
            self.deactivate();
            return;
        }
        match countdown(due, now) {
            Timer::Waiting(_) => {}
            Timer::Expired => {
                self.due = None;
                self.notice = "倒计时已过期，请重新确认发送".into();
            }
            Timer::Fire => {
                self.due = None;
                #[cfg(target_os = "linux")]
                {
                    let still_focused = zuse::platform::Desktop::connect()
                        .and_then(|desktop| desktop.windows())
                        .is_ok_and(|rows| focused_writer(&rows, std::process::id()));
                    if still_focused {
                        self.notice =
                            "写字窗口仍在前台，已取消发送。请重新确认并切换到外部画布。".into();
                        return;
                    }
                    if countdown(due, Instant::now()) == Timer::Expired {
                        self.notice = "倒计时已过期，请重新确认发送".into();
                        return;
                    }
                }
                let preview = self
                    .preview
                    .as_ref()
                    .expect("a matching preview was checked");
                match c.dispatch_screen(preview.plan.send(), false) {
                    Some(id) => {
                        self.pending = Some(Pending::Send(id));
                        self.notice = "正在向外部画布发送笔迹…".into();
                    }
                    None => self.notice = c.status.clone(),
                }
            }
        }
    }

    pub fn draw(
        &mut self,
        canvas: &mut Canvas<'_>,
        ui: &mut Ui,
        c: &mut Controller,
        area: Rect,
        closing: bool,
    ) {
        self.settle(c);
        let layout = ScreenLayout::new(area);
        let editable = !closing && !c.busy() && self.due.is_none() && self.pending.is_none();
        let targets = [Output::Mouse, Output::Touchscreen, Output::Touchpad];
        for (i, target) in targets.into_iter().enumerate() {
            let selected = self.target == target;
            let cell = layout.targets.cell(i, ScreenLayout::TARGET_H);
            let style = if selected {
                ButtonStyle::Secondary
            } else {
                ButtonStyle::Quiet
            };
            if ui.button(
                canvas,
                2700 + i as u64,
                cell,
                crate::locale::text(target.label()),
                style,
                editable,
            ) && self.target != target
            {
                self.target = target;
                self.preview = None;
                self.notice.clear();
            }
            if selected {
                let mid = cell.x + cell.w / 2.;
                let y = cell.y + cell.h - 4.;
                canvas.line(
                    (mid - 10., y),
                    (mid + 10., y),
                    2.,
                    if editable {
                        colors::BLUE
                    } else {
                        colors::MUTED
                    },
                );
            }
        }
        let show_size = self.target != Output::Mouse;
        let inputs = [
            ("起点 X · px", &mut self.fields.x, true),
            ("起点 Y · px", &mut self.fields.y, true),
            ("比例 · px/字形单位", &mut self.fields.scale, true),
            ("笔点间隔 · ms", &mut self.fields.interval, true),
            ("触摸面宽 · px", &mut self.fields.width, show_size),
            ("触摸面高 · px", &mut self.fields.height, show_size),
        ];
        for (i, (label, value, enabled)) in inputs.into_iter().enumerate() {
            let cell = layout.fields.cell(i, ScreenLayout::FIELD_H);
            canvas.ellipsis(
                crate::locale::text(label),
                cell.x,
                cell.y,
                cell.w,
                13,
                false,
                colors::MUTED,
            );
            ui.input(
                canvas,
                2710 + i as u64,
                Rect::new(cell.x, cell.y + 20., cell.w, cell.h - 20.),
                value,
                "",
                false,
                editable && enabled,
            );
        }
        let plan = Plan::new(&c.form, &self.fields, self.target);
        if self
            .preview
            .as_ref()
            .is_some_and(|p| Some(&p.plan) != plan.as_ref().ok())
        {
            self.preview = None;
            self.notice = "参数已更改，请重新预览".into();
        }
        if ui.button(
            canvas,
            2720,
            layout.actions.cell(0, ScreenLayout::ACTION_H),
            crate::locale::text("预览屏幕笔迹"),
            ButtonStyle::Secondary,
            editable,
        ) {
            self.preview = None;
            match &plan {
                Ok(p) => match c.dispatch_screen(p.preview(), true) {
                    Some(id) => {
                        self.pending = Some(Pending::Preview(id, p.clone()));
                        self.notice = "正在生成屏幕预览…".into();
                    }
                    None => self.notice = c.status.clone(),
                },
                Err(e) => self.notice = e.clone(),
            }
        }
        let active = self.due.is_some();
        let own_send = matches!(self.pending, Some(Pending::Send(_))) && c.owns_job();
        let send_label = if active {
            "取消倒计时"
        } else if own_send {
            "停止屏幕输入"
        } else {
            "发送到画布"
        };
        if ui.button(
            canvas,
            2721,
            layout.actions.cell(1, ScreenLayout::ACTION_H),
            crate::locale::text(send_label),
            ButtonStyle::Primary,
            !closing && (active || own_send || (editable && self.preview.is_some())),
        ) {
            if active {
                self.deactivate();
            } else if own_send {
                c.dispatch(Action::Cancel);
            } else {
                self.due = Some(Instant::now() + DELAY);
            }
        }
        // Process the cancel button before servicing a deadline on this frame.
        self.update_timer(c, plan.as_ref().ok(), closing, Instant::now());
        let preview = layout.preview;
        paint(
            canvas,
            preview,
            self.preview.as_ref().map(|p| p.strokes.as_slice()),
        );
        let message = if let Some(due) = self.due {
            match countdown(due, Instant::now()) {
                Timer::Waiting(s) => format!("{s} 秒后发送，请切换到目标画布"),
                _ => "即将发送到目标画布".into(),
            }
        } else if self.notice.is_empty() {
            if self.target == Output::Touchpad {
                "触摸板发送间接触点，指针映射由系统决定".into()
            } else {
                "发送前有 3 秒切换到目标画布".into()
            }
        } else {
            self.notice.clone()
        };
        let message = crate::locale::text(&message);
        let y = preview.y + preview.h + 12.;
        let mut text_width = preview.w;
        if canvas.measure(message, 14, false) > preview.w {
            let label = crate::locale::text("复制状态");
            let copy_width = (canvas.measure(label, 14, false) + 24.).min(preview.w);
            text_width = (preview.w - copy_width - 8.).max(0.);
            if ui.button(
                canvas,
                2722,
                Rect::new(preview.x + preview.w - copy_width, y - 5., copy_width, 27.),
                label,
                ButtonStyle::Quiet,
                !closing,
            ) {
                ui.clipboard = Some(message.to_owned());
            }
        }
        canvas.ellipsis(message, preview.x, y, text_width, 14, false, colors::MUTED);
    }
}

fn paint(canvas: &mut Canvas<'_>, area: Rect, strokes: Option<&[Vec<(f64, f64)>]>) {
    canvas.rect(area, 8., colors::LINE);
    canvas.rect(area.inset(1.), 7., colors::BG);
    let Some(strokes) = strokes else {
        let label = crate::locale::text("等待屏幕笔迹预览");
        let width = canvas.measure(label, 14, false);
        canvas.text(
            label,
            area.x + ((area.w - width) / 2.).max(12.),
            area.y + (area.h - 16.) / 2.,
            14,
            false,
            colors::MUTED,
        );
        return;
    };
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
    let [x0, y0, x1, y1] = bounds;
    if !x0.is_finite() {
        return;
    }
    let inner = area.inset(14.);
    let scale = (inner.w as f64 / (x1 - x0).max(1.)).min(inner.h as f64 / (y1 - y0).max(1.));
    let xy = |(x, y): (f64, f64)| {
        (
            inner.x + (inner.w - (scale * (x1 - x0)) as f32) / 2. + ((x - x0) * scale) as f32,
            inner.y + (inner.h - (scale * (y1 - y0)) as f32) / 2. + ((y - y0) * scale) as f32,
        )
    };
    canvas.clip(area);
    for stroke in strokes {
        for p in stroke.windows(2) {
            canvas.line(xy(p[0]), xy(p[1]), 1.6, colors::TEXT);
        }
        if let [p] = stroke.as_slice() {
            let (x, y) = xy(*p);
            canvas.rect(Rect::new(x - 0.8, y - 0.8, 1.6, 1.6), 0.8, colors::TEXT);
        }
    }
    canvas.unclip();
}

#[cfg(test)]
mod tests {
    use super::*;
    use zuse::runtime::{Options, Runtime};

    fn form() -> Form {
        Form {
            text: "Z".into(),
            ..Form::default()
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn only_visible_focused_writer_windows_block_submission() {
        let title_class = r#""class":"Zuse","title":"Zuse Writer · 写字机""#;
        let rows = |fields: &str| format!("[{{{title_class},{fields}}}]");
        assert!(focused_writer(r#"[{"focused":true,"pid":17}]"#, 17));
        assert!(focused_writer(
            &rows(r#""focused":true,"mapped":true,"pid":null"#),
            17
        ));
        assert!(focused_writer(
            &rows(r#""focused":true,"mapped":true,"pid":19"#),
            17
        ));
        for row in [
            rows(r#""focused":false"#),
            rows(r#""focused":true,"mapped":false"#),
            rows(r#""focused":null"#),
            r#"[{"focused":true,"pid":19,"class":"Paint","title":"Canvas"}]"#.into(),
            r#"[{"focused":true,"class":"Browser","title":"Zuse Writer · 写字机"}]"#.into(),
            r#"[{"focused":true,"class":"Zuse","title":"Other app"}]"#.into(),
            "invalid".into(),
        ] {
            assert!(!focused_writer(&row, 17), "{row}");
        }
    }

    #[test]
    fn shared_layout_and_explicit_seed_match_each_output_schema_without_dispatch() {
        let mut f = form();
        f.offset_x = 20.;
        f.offset_y = 40.;
        f.seed = 17;
        let fields = Fields::default();
        for target in [Output::Mouse, Output::Touchscreen, Output::Touchpad] {
            let plan = Plan::new(&f, &fields, target).unwrap();
            assert!(
                plan.preview()
                    .contains("x=200,y=150,reference_units=1,reference_distance=4")
            );
            assert!(plan.send().starts_with(&format!(
                "{}({},interval=8",
                target.operation(),
                plan.arguments
            )));
            assert_eq!(
                plan.send().contains("width=1920,height=1080"),
                target != Output::Mouse
            );
            assert!(plan.send().contains(
                "offset_x_ratio=0.2,offset_y_ratio=0.4,tilt_degrees=3,size_ratio=0.04,seed=17"
            ));
            let dry = Options {
                dry_run: true,
                ..Default::default()
            };
            assert!(zuse::runtime::check(&plan.send(), dry).is_ok());
        }
    }

    #[test]
    fn real_read_only_preview_keeps_positive_down_axes_and_matches_touch_extent() {
        let f = Form {
            area_width: 100.,
            area_height: 100.,
            ..form()
        };
        let plan = Plan::new(&f, &Fields::default(), Output::Touchscreen).unwrap();
        let reply = Runtime::new()
            .execute(
                &plan.preview(),
                Options {
                    read_only: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let val = json::with_value(&reply, |v| {
            Ok(v.get("results")
                .unwrap()
                .elements()
                .next()
                .unwrap()
                .get("value")
                .unwrap()
                .raw()
                .to_owned())
        })
        .unwrap();
        let strokes = read_preview(&val).unwrap();
        assert!(plan.validate(&strokes).is_ok());
        assert!(
            strokes
                .iter()
                .flatten()
                .all(|(x, y)| *x >= 200. && *y >= 150.)
        );
        let small = Fields {
            width: "50".into(),
            height: "50".into(),
            ..Fields::default()
        };
        let small_plan = Plan::new(&f, &small, Output::Touchscreen).unwrap();
        assert!(small_plan.validate(&strokes).is_err());
        assert!(
            Plan::new(&f, &small, Output::Mouse)
                .unwrap()
                .validate(&strokes)
                .is_ok()
        );
    }

    #[test]
    fn invalid_fields_or_corrupt_preview_cannot_arm_a_send() {
        let f = form();
        for fields in [
            Fields {
                scale: "-1".into(),
                ..Fields::default()
            },
            Fields {
                x: "NaN".into(),
                ..Fields::default()
            },
            Fields {
                width: "1".into(),
                ..Fields::default()
            },
            Fields {
                interval: "1001".into(),
                ..Fields::default()
            },
        ] {
            assert!(Plan::new(&f, &fields, Output::Touchpad).is_err());
        }
        assert!(read_preview(r#"{"points":1,"strokes":[[[0,0],[1,2]]]}"#).is_err());
        assert!(read_preview(r#"{"points":1,"strokes":[[[1,2,3]]]}"#).is_err());
        assert!(read_preview(r#"{"points":1,"strokes":[[["0",2]]]}"#).is_err());
        let original = Plan::new(&f, &Fields::default(), Output::Mouse).unwrap();
        let edited = Plan::new(
            &Form {
                text: "Changed".into(),
                ..f
            },
            &Fields::default(),
            Output::Mouse,
        )
        .unwrap();
        assert!(original != edited);
    }

    #[test]
    fn changed_text_or_closing_disarms_even_when_send_is_due() {
        let mut controller = Controller::default();
        let original = Plan::new(&form(), &Fields::default(), Output::Mouse).unwrap();
        let edited = Plan::new(
            &Form {
                text: "different".into(),
                ..form()
            },
            &Fields::default(),
            Output::Mouse,
        )
        .unwrap();
        for (current, closing) in [(&edited, false), (&original, true)] {
            let now = Instant::now();
            let mut panel = ScreenOutput {
                preview: Some(Preview {
                    plan: original.clone(),
                    strokes: vec![vec![(1., 2.)]],
                }),
                due: Some(now),
                ..Default::default()
            };
            panel.update_timer(&mut controller, Some(current), closing, now);
            assert!(!panel.needs_tick());
            assert!(panel.pending.is_none());
        }
    }

    #[test]
    fn countdown_is_opt_in_and_disarms_after_switch_or_long_suspension() {
        let now = Instant::now();
        let due = now + DELAY;
        assert_eq!(countdown(due, now), Timer::Waiting(3));
        assert_eq!(countdown(due, due), Timer::Fire);
        assert_eq!(countdown(due, due + Duration::from_secs(2)), Timer::Expired);
        let mut panel = ScreenOutput::default();
        assert!(!panel.needs_tick());
        panel.due = Some(due);
        panel.deactivate();
        assert!(!panel.needs_tick());
    }
}
