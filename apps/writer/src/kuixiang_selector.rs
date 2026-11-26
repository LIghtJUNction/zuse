//! Resolve the desktop's selected KuiXiang installation and persist explicit GUI choices.
use crate::{
    controller::Form,
    kuixiang_config::{self, Defaults},
    workspace_profile::{config_root, write_atomic},
};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Choice {
    Common,
    Native,
    Flatpak,
}
impl Choice {
    fn key(self) -> &'static str {
        match self {
            Self::Common => "common",
            Self::Native => "native",
            Self::Flatpak => "flatpak",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Common => "共同值",
            Self::Native => "原生 Bottles",
            Self::Flatpak => "Flatpak",
        }
    }
    fn root(self, home: Option<&Path>) -> Result<Option<PathBuf>, String> {
        if self == Self::Common {
            return Ok(None);
        }
        kuixiang_config::selected_root(self.key().as_ref(), home).map(Some)
    }
}

pub(crate) struct Sources {
    pub choices: Vec<Choice>,
    paper_sizes: Vec<(Choice, [f64; 2])>,
    pub active: Option<Choice>,
    home: Option<PathBuf>,
}

impl Sources {
    pub fn from_home(home: Option<&Path>) -> Self {
        let mut choices = vec![];
        let mut paper_sizes = vec![];
        for choice in [Choice::Native, Choice::Flatpak] {
            if let Some(size) = choice.root(home).ok().flatten().and_then(|p| kuixiang_config::paper_size(&p)) {
                choices.push(choice);
                paper_sizes.push((choice, size));
            }
        }
        if !choices.is_empty() {
            choices.insert(0, Choice::Common);
        }
        Self {
            choices,
            paper_sizes,
            active: Some(Choice::Common),
            home: home.map(Path::to_owned),
        }
    }
    pub fn paper_label(&self, choice: Choice) -> String {
        if choice == Choice::Common {
            let label = if self.paper_sizes.len() > 1 { "使用两份配置的共同设置" } else { "跟随已安装的配置" };
            return crate::locale::text(label).into();
        }
        let label = crate::locale::text(choice.label());
        self.paper_sizes.iter().find(|(c, _)| *c == choice)
            .map_or_else(|| label.to_owned(), |(_, [w, h])| format!("{w} × {h} mm · {label}"))
    }

    pub fn mark_explicit(&mut self, root: &Path) {
        self.active = self.choices.iter().copied().find(|choice| {
            choice.root(self.home.as_deref()).ok().flatten().as_deref() == Some(root)
        });
    }
    pub fn saved_root(&mut self, preference: &Path) -> Result<Option<PathBuf>, String> {
        match fs::symlink_metadata(preference) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
            Ok(_) => {}
        }
        let raw = kuixiang_config::read_file(preference)?;
        let key = zuse::json::with_value(&raw, zuse::json::text)?;
        let choice = [Choice::Common, Choice::Native, Choice::Flatpak]
            .into_iter()
            .find(|c| c.key() == key)
            .ok_or("保存的奎享来源无效")?;
        if choice != Choice::Common && !self.choices.contains(&choice) {
            return Err(format!(
                "保存的{}安装已不可用，使用当前可读取的共同值。",
                choice.label()
            ));
        }
        self.active = Some(choice);
        choice.root(self.home.as_deref())
    }
    pub fn load(&self, choice: Choice) -> Result<Defaults, String> {
        if !self.choices.contains(&choice) {
            return Err("奎享安装不可用".into());
        }
        kuixiang_config::discover(
            choice.root(self.home.as_deref())?.as_deref(),
            self.home.as_deref(),
        )
    }
    pub fn persist(&self, choice: Choice) -> Result<(), String> {
        write_atomic(
            &preference()?,
            &format!("{}\n", zuse::json::quote(choice.key())),
        )
    }
}

pub(crate) fn preference() -> Result<PathBuf, String> {
    Ok(config_root()?.with_file_name("kuixiang-source.json"))
}

pub(crate) struct Menu {
    pub sources: Sources,
    pub previous: Form,
}
