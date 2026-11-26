use crate::controller::Form;
use crate::view::Frame;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use zgpui::native::Rect;

const PER_PAGE: usize = 8;

pub(crate) struct FontPicker {
    fonts: Vec<String>,
    page: usize,
    path: String,
    name: String,
    mixed: bool,
    notice: String,
    installing: Option<Receiver<Result<String, String>>>,
}

impl FontPicker {
    pub(crate) fn new(current: &str) -> Self {
        let mut picker = Self {
            fonts: Vec::new(),
            page: 0,
            path: String::new(),
            name: String::new(),
            mixed: current.contains(','),
            notice: String::new(),
            installing: None,
        };
        picker.refresh();
        picker
    }

    fn refresh(&mut self) {
        match zuse::font_store::cli(&["list"])
            .and_then(|value| super::view::choices(&value, "fonts"))
        {
            Ok(fonts) => {
                self.fonts = fonts;
                self.page = self.page.min(self.pages() - 1);
            }
            Err(error) => self.notice = error,
        }
    }

    fn pages(&self) -> usize {
        self.fonts.len().div_ceil(PER_PAGE).max(1)
    }

    pub(crate) fn pending(&self) -> bool {
        self.installing.is_some()
    }

    fn poll(&mut self) {
        let Some(receiver) = &self.installing else {
            return;
        };
        self.notice = match receiver.try_recv() {
            Ok(Ok(_)) => "官方字体集已就绪。".into(),
            Ok(Err(error)) => error,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => "下载任务已结束，请重试。".into(),
        };
        self.installing = None;
        self.refresh();
    }

    /// Returns true when the browser should return to the writing workspace.
    pub(crate) fn draw(&mut self, frame: &mut Frame<'_, '_>, area: Rect, form: &mut Form) -> bool {
        self.poll();
        let Rect { x, y, w, .. } = area;
        frame.heading("字体库", x, y);
        if frame.button(
            600,
            Rect::new(x + w - 112., y - 3., 112., 34.),
            "返回书写",
            true,
            !self.pending(),
        ) {
            return true;
        }
        let available = zuse::font_store::OFFICIAL_FONTS.join(" · ");
        frame.note(&format!("官方字体集  {available}"), x, y + 48., w);
        if frame.button(
            601,
            Rect::new(x, y + 76., 167., 34.),
            "一键安装官方字体集",
            false,
            !self.pending(),
        ) {
            let (tx, rx) = mpsc::channel();
            self.installing = Some(rx);
            self.notice = "正在从 Zuse Release 下载字体…".into();
            std::thread::spawn(move || {
                let _ = tx.send(zuse::font_store::cli(&["install"]));
            });
        }
        let mixed = if self.mixed {
            "混排 · 开"
        } else {
            "混排 · 关"
        };
        if frame.button(
            602,
            Rect::new(x + 181., y + 76., 100., 34.),
            mixed,
            self.mixed,
            true,
        ) {
            self.mixed = !self.mixed;
            if !self.mixed {
                form.font
                    .truncate(form.font.find(',').unwrap_or(form.font.len()));
            }
        }
        frame.note(&format!("当前字体  {}", form.font), x, y + 130., w);
        for (i, name) in self
            .fonts
            .iter()
            .skip(self.page * PER_PAGE)
            .take(PER_PAGE)
            .enumerate()
        {
            let selected = form.font.split(',').any(|entry| entry.trim() == name);
            if frame.button(
                610 + i as u64,
                Rect::new(x, y + 159. + i as f32 * 36., w, 32.),
                name,
                selected,
                true,
            ) {
                if !self.mixed {
                    form.font.clone_from(name);
                    return true;
                }
                let mut stack: Vec<_> = form.font.split(',').map(str::trim).collect();
                if selected {
                    stack.retain(|entry| *entry != name);
                } else if stack.len() < 4 {
                    stack.push(name);
                } else {
                    self.notice = "混排最多选择四款字体。".into();
                }
                form.font = if stack.is_empty() {
                    "builtin".into()
                } else {
                    stack.join(",")
                };
            }
        }
        let pager = y + 450.;
        if frame.button(
            620,
            Rect::new(x, pager, 80., 32.),
            "上一页",
            false,
            self.page > 0,
        ) {
            self.page -= 1;
        }
        if frame.button(
            621,
            Rect::new(x + 88., pager, 80., 32.),
            "下一页",
            false,
            self.page + 1 < self.pages(),
        ) {
            self.page += 1;
        }
        frame.note(
            &format!("{} / {}", self.page + 1, self.pages()),
            x + 180.,
            pager + 8.,
            w - 180.,
        );
        let can_import = !self.pending();
        let input_w = (w - 148.) / 2.;
        frame.field(
            630,
            x,
            y + 504.,
            input_w,
            "本地 .zfont 文件",
            &mut self.path,
            "/…/my-font.zfont",
            can_import,
        );
        frame.field(
            631,
            x + input_w + 8.,
            y + 504.,
            input_w,
            "字体名称",
            &mut self.name,
            "my-font",
            can_import,
        );
        if frame.button(
            632,
            Rect::new(x + w - 128., y + 525., 128., 34.),
            "导入自定义字体",
            false,
            !self.pending() && !self.name.trim().is_empty() && !self.path.trim().is_empty(),
        ) {
            match zuse::font_store::cli(&["add", self.name.trim(), self.path.trim()]) {
                Ok(_) => {
                    self.notice = format!("已导入 {}", self.name.trim());
                    form.font = self.name.trim().to_owned();
                    self.refresh();
                }
                Err(error) => self.notice = error,
            }
        }
        frame.note(&self.notice, x, y + 582., w);
        false
    }
}
