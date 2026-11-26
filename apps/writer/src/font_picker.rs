use crate::controller::Form;
use crate::file_dialog::{FileDialog, FileKind};
use crate::view::Frame;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use zgpui::native::Rect;

const PER_PAGE: usize = 8;

enum Installed {
    Official,
    Local(String),
}

fn is_gfont(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("gfont"))
}

fn suggest_alias(path: &str, fonts: &[String]) -> String {
    let stem = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let mut base = String::from("local");
    for part in stem
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
    {
        base.push('-');
        for ch in part.chars() {
            if ch.is_ascii_alphanumeric() {
                base.push(ch.to_ascii_lowercase());
            } else {
                base.push_str(&format!("u{:x}", ch as u32));
            }
            if base.len() >= 46 {
                break;
            }
        }
        if base.len() >= 46 {
            break;
        }
    }
    if base == "local" {
        base.push_str("-font");
    }
    let mut alias = base.clone();
    let mut suffix = 2;
    while fonts.contains(&alias) {
        alias = format!("{base}-{suffix}");
        suffix += 1;
    }
    alias
}

fn import_local(name: String, path: String, text: Option<String>) -> Result<Installed, String> {
    if !FileKind::Font.accepts(&path) {
        return Err("请选择 .gfont 或 .zfont 字体文件。".into());
    }
    if is_gfont(&path) {
        let mut cmd = std::process::Command::new("python3");
        cmd.args([
            "-c",
            include_str!("../../../scripts/import_gfont.py"),
            "--install",
            &name,
        ])
        .arg("--zuse")
        .arg(crate::controller::engine_binary());
        if let Some(text) = text {
            // Only unique code points are needed by the converter.
            let chars: std::collections::BTreeSet<_> = text.chars().collect();
            cmd.arg("--text").arg(chars.into_iter().collect::<String>());
        }
        let out = cmd
            .arg("--")
            .arg(&path)
            .output()
            .map_err(|e| format!("无法运行 Python 3 字体转换器：{e}"))?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
        }
    } else {
        zuse::font_store::cli(&["add", &name, &path])?;
    }
    Ok(Installed::Local(name))
}

pub(crate) struct FontPicker {
    fonts: Vec<String>,
    page: usize,
    path: String,
    name: String,
    manual: bool,
    reveal_selection: bool,
    dialog: Option<FileDialog>,
    mixed: bool,
    article_only: bool,
    scroll_y: f32,
    max_scroll: f32,
    notice: String,
    installing: Option<Receiver<Result<Installed, String>>>,
}

impl FontPicker {
    pub(crate) fn new(current: &str) -> Self {
        let mut picker = Self {
            fonts: Vec::new(),
            page: 0,
            path: String::new(),
            name: String::new(),
            manual: false,
            reveal_selection: false,
            dialog: None,
            mixed: current.contains(','),
            article_only: false,
            scroll_y: 0.,
            max_scroll: 0.,
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
        self.installing.is_some() || self.dialog.is_some()
    }

    pub(crate) fn scroll(&mut self, delta: i32) {
        self.scroll_y = (self.scroll_y + delta.signum() as f32 * 56.).clamp(0., self.max_scroll);
    }

    fn poll(&mut self, form: &mut Form) {
        if let Some(answer) = self.dialog.as_ref().and_then(FileDialog::poll) {
            self.dialog = None;
            match answer {
                Ok(Some(path)) => {
                    self.refresh();
                    self.name = suggest_alias(&path, &self.fonts);
                    self.path = path;
                    self.notice.clear();
                    self.reveal_selection = true;
                }
                Ok(None) => {
                    self.notice.clear();
                }
                Err(error) => {
                    self.manual = true;
                    self.notice = error;
                }
            }
        }
        let Some(receiver) = &self.installing else {
            return;
        };
        self.notice = match receiver.try_recv() {
            Ok(Ok(Installed::Official)) => "官方字体集已就绪。".into(),
            Ok(Ok(Installed::Local(name))) => {
                form.font.clone_from(&name);
                format!(
                    "已导入 {} · {name}",
                    Path::new(&self.path)
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                )
            }
            Ok(Err(error)) => {
                self.refresh();
                if self.fonts.contains(&self.name) && !self.path.is_empty() {
                    self.name = suggest_alias(&self.path, &self.fonts);
                    format!("名称已使用。可用「{}」导入，已有字体会保留。", self.name)
                } else {
                    format!("导入失败，请检查文件格式、名称及读取权限。{error}")
                }
            }
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => "字体任务已结束，请重试。".into(),
        };
        self.installing = None;
        self.refresh();
    }

    /// Returns true when the browser should return to the writing workspace.
    pub(crate) fn draw(&mut self, frame: &mut Frame<'_, '_>, area: Rect, form: &mut Form) -> bool {
        self.poll(form);
        let Rect { x, w, h, .. } = area;
        // Keep the pager stable across pages without reserving eight empty rows.
        let pager_top = 162. + 36. * self.fonts.len().min(PER_PAGE) as f32;
        let input_top = pager_top + 50.;
        let name_top = input_top + if self.manual { 100. } else { 42. };
        let gfont = is_gfont(self.path.trim());
        let notice_top = if self.path.is_empty() && !self.manual {
            input_top + 42.
        } else {
            name_top + if gfont { 118. } else { 72. }
        };
        self.max_scroll = (notice_top + 30. - h).max(0.);
        self.scroll_y = if std::mem::take(&mut self.reveal_selection) {
            self.max_scroll
        } else {
            self.scroll_y.min(self.max_scroll)
        };
        let y = area.y - self.scroll_y;
        frame.heading("字体库", x, y);
        if frame.quiet_button(
            600,
            Rect::new(x + w - 112., y - 3., 112., 34.),
            "返回书写",
            !self.pending(),
        ) {
            return true;
        }
        let available = zuse::font_store::OFFICIAL_FONTS.join(" · ");
        frame.note(&format!("官方字体集  {available}"), x, y + 48., w);
        let button_width = ((w - 16.) / 3.).min(174.);
        if frame.button(
            634,
            Rect::new(x, y + 76., button_width, 34.),
            "选择字体文件…",
            true,
            !self.pending(),
        ) {
            self.dialog = Some(FileDialog::open(FileKind::Font, "选择字体文件"));
            self.notice.clear();
        }
        if frame.button(
            601,
            Rect::new(x + button_width + 8., y + 76., button_width, 34.),
            "安装官方字体",
            false,
            !self.pending(),
        ) {
            let (tx, rx) = mpsc::channel();
            self.installing = Some(rx);
            self.notice = "正在从 Zuse Release 下载字体…".into();
            std::thread::spawn(move || {
                let _ = tx.send(zuse::font_store::cli(&["install"]).map(|_| Installed::Official));
            });
        }
        let mixed = if self.mixed {
            "混排 · 开"
        } else {
            "混排 · 关"
        };
        if frame.button(
            602,
            Rect::new(
                x + 2. * (button_width + 8.),
                y + 76.,
                button_width.min(100.),
                34.,
            ),
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
            let id = 610 + i as u64;
            let row = Rect::new(x, y + 159. + i as f32 * 36., w, 32.);
            let clicked = if selected {
                frame.button(id, row, &format!("✓  {name}"), false, true)
            } else {
                frame.quiet_button(id, row, name, true)
            };
            if clicked {
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
        let pager = y + pager_top;
        if frame.quiet_button(620, Rect::new(x, pager, 80., 32.), "上一页", self.page > 0) {
            self.page -= 1;
        }
        if frame.quiet_button(
            621,
            Rect::new(x + 88., pager, 80., 32.),
            "下一页",
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
        let filename = Path::new(&self.path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        frame.note(
            if filename.is_empty() {
                crate::locale::text("支持 .gfont 和 .zfont")
            } else {
                filename
            },
            x,
            y + input_top + 9.,
            (w - 132.).max(1.),
        );
        if frame.quiet_button(
            635,
            Rect::new(x + w - 126., y + input_top, 126., 32.),
            if self.manual {
                "收起文件路径"
            } else {
                "手工输入路径"
            },
            can_import,
        ) {
            self.manual = !self.manual;
        }
        if self.manual {
            let previous = self.path.clone();
            if frame.field(
                630,
                x,
                y + input_top + 39.,
                w,
                "字体文件路径",
                &mut self.path,
                "/…/my-font.gfont",
                can_import,
            ) && (self.name.is_empty()
                || self.name == suggest_alias(previous.trim(), &self.fonts))
            {
                self.name = suggest_alias(self.path.trim(), &self.fonts);
            }
        }
        if self.path.is_empty() && !self.manual {
            frame.note(&self.notice, x, y + notice_top, w);
            return false;
        }
        let import_width = frame.button_width("导入自定义字体").max(128.).min(w - 48.);
        frame.field(
            631,
            x,
            y + name_top,
            (w - import_width - 8.).max(24.),
            "保存名称（自动生成）",
            &mut self.name,
            "local-font",
            can_import,
        );
        if frame.button(
            632,
            Rect::new(x + w - import_width, y + name_top + 21., import_width, 34.),
            "导入自定义字体",
            false,
            !self.pending() && !self.name.trim().is_empty() && !self.path.trim().is_empty(),
        ) {
            let path = self.path.trim().to_owned();
            let name = self.name.trim().to_owned();
            let text = (gfont && self.article_only && !form.text.trim().is_empty())
                .then(|| form.text.clone());
            let (tx, rx) = mpsc::channel();
            self.installing = Some(rx);
            self.notice = "正在导入字体…".into();
            std::thread::spawn(move || {
                let _ = tx.send(import_local(name, path, text));
            });
        }
        let subset_width = frame.button_width("仅导入当前文章用字").max(166.).min(w);
        if gfont
            && frame.button(
                633,
                Rect::new(x, y + name_top + 68., subset_width, 32.),
                "仅导入当前文章用字",
                self.article_only,
                !self.pending() && !form.text.trim().is_empty(),
            )
        {
            self.article_only = !self.article_only;
        }
        frame.note(&self.notice, x, y + notice_top, w);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proposes_legal_distinct_names_for_chinese_stems_and_conflicts() {
        let first = suggest_alias("/home/样本/行楷.GFONT", &[]);
        let second = suggest_alias("/home/样本/行楷.GFONT", std::slice::from_ref(&first));
        assert!(first.starts_with("local-"));
        assert!(
            first
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        );
        assert_eq!(second, format!("{first}-2"));
        assert_ne!(first, suggest_alias("隶书.gfont", &[]));
        assert_eq!(suggest_alias("CON.zfont", &[]), "local-con");
        assert!(suggest_alias(&format!("{}.gfont", "字".repeat(200)), &[]).len() <= 64);
    }
}
