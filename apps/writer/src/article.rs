use crate::locale::text;
use zgpui::native::{ButtonStyle, Canvas, Event, Rect, Ui, colors};

const MAX_BYTES: usize = 1024 * 1024;

pub(crate) struct Article {
    draft: String,
    cursor: usize,
    first: usize,
    select_all: bool,
}

impl Article {
    pub(crate) fn new(value: &str) -> Self {
        Self {
            draft: value.into(),
            cursor: value.len(),
            first: 0,
            select_all: false,
        }
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
        let rect = Rect::new(x, y + 57., w, (h - 75.).max(100.));
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
            true,
        )
    }

    pub(crate) fn reveal_cursor(&mut self, height: f32) {
        let count = ((height - 121.) / 25.).max(1.) as usize;
        let row = self.row();
        if row >= self.first + count {
            self.first = row + 1 - count;
        }
    }
}
