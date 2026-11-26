use crate::locale::text;
use crate::file_dialog::{FileDialog, FileKind};
use std::sync::mpsc::{self,Receiver,TryRecvError};
use zgpui::native::{ButtonStyle, Canvas, Event, Rect, Ui, colors};

const MAX_BYTES: usize = 1024 * 1024;

pub(crate) struct Article {
    draft: String,
    cursor: usize,
    first: usize,
    select_all: bool,
    dialog: Option<FileDialog>,
    reading: Option<Receiver<Result<(String,String),String>>>,
    before_open: String,
    notice: String,
}

impl Article {
    pub(crate) fn new(value: &str) -> Self {
        Self {
            draft: value.into(),
            cursor: value.len(),
            first: 0,
            select_all: false,
            dialog: None,
            reading: None,
            before_open: String::new(),
            notice: String::new(),
        }
    }

    pub(crate) fn pending(&self) -> bool {
        self.dialog.is_some() || self.reading.is_some()
    }

    fn poll_file(&mut self) {
        if let Some(answer)=self.dialog.as_ref().and_then(FileDialog::poll) {
            self.dialog=None;
            match answer {
                Ok(Some(path)) => {
                    let (tx,rx)=mpsc::channel();
                    self.reading=Some(rx);
                    std::thread::spawn(move || {
                        let value=crate::view::read_article(&path).map(|v|(path,v));
                        let _=tx.send(value);
                    });
                }
                Ok(None) => self.notice.clear(),
                Err(error) => self.notice=error,
            }
        }
        let Some(reader)=&self.reading else { return; };
        self.notice=match reader.try_recv() {
            Ok(Ok((path, value))) if self.draft==self.before_open => {
                self.draft=value.replace("\r\n","\n").replace('\r',"\n");
                self.first=0;
                self.cursor=0;
                self.select_all=false;
                format!("已打开 {}", std::path::Path::new(&path).file_name().unwrap_or_default().to_string_lossy())
            }
            Ok(Ok(_)) => "草稿已有新的编辑。请重新选择文件以替换文章。".into(),
            Ok(Err(error)) => error,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => "读取文本失败，请重试。".into(),
        };
        self.reading=None;
        self.before_open.clear();
    }

    pub(crate) fn finish(self) -> String {
        self.draft
    }

    fn insert(&mut self, value: &str) {
        let content: String = value
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .chars()
            .filter(|ch| !ch.is_control() || *ch == '\n')
            .collect();
        let existing = if self.select_all { 0 } else { self.draft.len() };
        if !content.is_empty() && existing + content.len() <= MAX_BYTES {
            if self.select_all {
                self.draft.clear();
                self.cursor = 0;
            }
            self.draft.insert_str(self.cursor, &content);
            self.cursor += content.len();
            self.select_all = false;
        }
    }

    pub(crate) fn input(&mut self, event: &Event, ui: &mut Ui) -> bool {
        match event {
            Event::Text(value) => self.insert(value),
            Event::Key {
                code,
                control: true,
                ..
            } => match code {
                97 | 65 => self.select_all = true,
                99 | 67 => ui.clipboard = Some(self.draft.clone()),
                120 | 88 if self.select_all => {
                    ui.clipboard = Some(std::mem::take(&mut self.draft));
                    self.cursor = 0;
                    self.select_all = false;
                }
                _ => {}
            },
            Event::Key {
                code, text: value, ..
            } => {
                let (before, after) = self.draft.split_at(self.cursor);
                let left = before.rfind('\n').map_or(0, |n| n + 1);
                let right = self.cursor + after.find('\n').unwrap_or(after.len());
                let previous = before
                    .char_indices()
                    .next_back()
                    .map_or(0, |(index, _)| index);
                let next = self.cursor + after.chars().next().map_or(0, char::len_utf8);
                match code {
                    8 | 127 if self.select_all => {
                        self.draft.clear();
                        self.cursor = 0;
                        self.select_all = false;
                    }
                    8 => {
                        self.draft.replace_range(previous..self.cursor, "");
                        self.cursor = previous;
                    }
                    127 => self.draft.replace_range(self.cursor..next, ""),
                    13 => self.insert("\n"),
                    1001 => self.cursor = previous,
                    1002 => self.cursor = next,
                    1005 => self.cursor = left,
                    1006 => self.cursor = right,
                    1003 | 1004 => {
                        let column = self.draft[left..self.cursor].chars().count();
                        let target = if *code == 1003 {
                            self.draft[..left.saturating_sub(1)]
                                .rfind('\n')
                                .map_or(0, |n| n + 1)
                        } else {
                            (right + 1).min(self.draft.len())
                        };
                        if target != left {
                            let line = self.draft[target..].split('\n').next().unwrap_or("");
                            self.cursor = target
                                + line
                                    .char_indices()
                                    .nth(column)
                                    .map_or(line.len(), |(offset, _)| offset);
                        }
                    }
                    9 | 27 => return false,
                    _ => self.insert(value),
                }
                if matches!(code, 1001..=1006) {
                    self.select_all = false;
                }
                self.first = self.first.min(self.row());
            }
            Event::Scroll(delta) => {
                self.first = if *delta > 0 {
                    self.first.saturating_sub(3)
                } else {
                    (self.first + 3).min(self.draft.bytes().filter(|b| *b == b'\n').count())
                };
            }
            _ => return false,
        }
        true
    }

    fn row(&self) -> usize {
        self.draft[..self.cursor]
            .bytes()
            .filter(|b| *b == b'\n')
            .count()
    }

    pub(crate) fn draw(&mut self, canvas: &mut Canvas<'_>, ui: &mut Ui, area: Rect) -> bool {
        let Rect { x, y, w, h } = area;
        canvas.text(text("编辑文章"), x, y, 25, true, colors::TEXT);
        self.poll_file();
        if ui.button(canvas,711,Rect::new(x,y+44.,128.,34.),text("打开文本文件…"),ButtonStyle::Secondary,!self.pending()) {
            self.before_open=self.draft.clone();
            self.dialog=Some(FileDialog::open(FileKind::Text,"选择 UTF-8 文本文件"));
            self.notice.clear();
        }
        canvas.ellipsis(&self.notice,x+140.,y+51.,(w-140.).max(1.),13,false,colors::MUTED);
        let rect = Rect::new(x, y + 90., w, (h - 108.).max(100.));
        canvas.rect(rect, 8., colors::LINE);
        canvas.rect(rect.inset(1.), 7., colors::SURFACE);
        let row = self.row();
        let count = ((rect.h - 22.) / 25.).max(1.) as usize;
        let left = self.draft[..self.cursor].rfind('\n').map_or(0, |i| i + 1);
        let prefix = &self.draft[left..self.cursor];
        canvas.clip(rect.inset(8.));
        for (index, line) in self
            .draft
            .split('\n')
            .enumerate()
            .skip(self.first)
            .take(count)
        {
            let y = rect.y + 12. + (index - self.first) as f32 * 25.;
            let width = if index == row {
                canvas.measure(prefix, 16, false)
            } else {
                0.
            };
            let scroll = (width - rect.w + 36.).max(0.);
            if self.select_all {
                canvas.rect(
                    Rect::new(rect.x + 10., y - 2., rect.w - 20., 23.),
                    3.,
                    colors::BLUE_PALE,
                );
            }
            canvas.text(line, rect.x + 12. - scroll, y, 16, false, colors::TEXT);
            if index == row && !self.select_all {
                let caret = rect.x + 12. + width - scroll;
                canvas.line((caret, y), (caret, y + 20.), 1.2, colors::BLUE);
            }
        }
        canvas.unclip();
        ui.button(
            canvas,
            710,
            Rect::new(x + w - 122., y - 2., 122., 34.),
            text("应用文章"),
            ButtonStyle::Primary,
            !self.pending(),
        )
    }

    pub(crate) fn reveal_cursor(&mut self, height: f32) {
        let count = ((height - 154.) / 25.).max(1.) as usize;
        let row = self.row();
        if row >= self.first + count {
            self.first = row + 1 - count;
        }
    }
}
