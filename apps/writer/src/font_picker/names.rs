//! Writer-only labels for engine font aliases. Font bytes and the CLI stay unchanged.
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::Path;
use std::time::UNIX_EPOCH;

#[derive(Default)]
pub(crate) struct FontNames(BTreeMap<String, String>);

const SIDECARS: &str = ".writer-names";

fn stamp(font: &Path) -> Option<String> {
    let file = font.symlink_metadata().ok()?;
    if !file.is_file() {
        return None;
    }
    let modified = file
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let stamp = format!("{}:{modified}", file.len());
    #[cfg(unix)]
    let stamp = {
        use std::os::unix::fs::MetadataExt;
        format!("{stamp}:{}:{}", file.dev(), file.ino())
    };
    Some(stamp)
}

fn label(dir: &Path, alias: &str) -> Option<String> {
    let mut text = String::new();
    File::open(dir.join(SIDECARS).join(alias))
        .ok()?
        .take(2049)
        .read_to_string(&mut text)
        .ok()?;
    if text.len() > 2048 {
        return None;
    }
    let (saved, name) = text.trim_end().split_once('\n')?;
    if name.is_empty() || name.chars().any(char::is_control) {
        return None;
    }
    let actual = stamp(&dir.join(format!("{alias}.zfont")))?;
    (saved == actual).then(|| name.to_owned())
}

impl FontNames {
    // Aliases come from font_store::list, which checks file names and the engine grammar.
    pub(super) fn from_dir(dir: &Path, aliases: &[String]) -> Self {
        let names = aliases
            .iter()
            .filter_map(|alias| label(dir, alias).map(|name| (alias.clone(), name)))
            .collect();
        Self(names)
    }

    pub(crate) fn label<'a>(&'a self, alias: &'a str) -> &'a str {
        if alias == "builtin" {
            crate::locale::text("内置字体")
        } else {
            self.0.get(alias).map_or(alias, String::as_str)
        }
    }

    pub(crate) fn stack(&self, aliases: &str) -> String {
        aliases
            .split(',')
            .map(|name| self.label(name.trim()))
            .collect::<Vec<_>>()
            .join(" / ")
    }
}

pub(super) fn save(dir: &Path, alias: &str, source: &Path) -> Result<String, String> {
    let font = dir.join(format!("{alias}.zfont"));
    let installed = stamp(&font).ok_or("未找到已安装的字体文件")?;
    let stem = source.file_stem().and_then(|s| s.to_str()).unwrap_or(alias);
    let stem = stem
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let stem = if stem.is_empty() { alias } else { &stem };
    let names = dir.join(SIDECARS);
    fs::create_dir_all(&names).map_err(|e| e.to_string())?;
    // Two Writer windows importing different aliases may reach this point together.
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(names.join(".lock"))
        .map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?;
    // Read labels of live aliases only. Removed or replaced fonts do not reserve a name.
    let mut used = std::collections::BTreeSet::from(["内置字体".to_owned(), "builtin".to_owned()]);
    for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().is_some_and(|ext| ext == "zfont")
            && let Some(other) = path.file_stem().and_then(|s| s.to_str())
            && other != alias
        {
            used.insert(label(dir, other).unwrap_or_else(|| other.to_owned()));
        }
    }
    let mut name = stem.to_owned();
    let mut suffix = 2;
    while used.contains(&name) {
        name = format!("{stem}（{suffix}）");
        suffix += 1;
    }
    if stamp(&font).as_ref() != Some(&installed) {
        return Err("字体文件已更改，请重新打开字体列表".into());
    }
    crate::workspace_profile::write_atomic(&names.join(alias), &format!("{installed}\n{name}\n"))?;
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    #[test]
    fn installed_aliases_keep_distinct_labels_across_restart_and_replacement() {
        let dir = std::env::temp_dir().join(format!(
            "zuse-font-labels-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&dir).unwrap();
        let aliases = ["local-first", "local-next", "legacy"].map(String::from);
        let source = Path::new("样本-行楷.gfont");
        assert!(save(&dir, &aliases[0], source).is_err());
        assert!(!dir.join(SIDECARS).exists());
        for alias in &aliases {
            fs::write(dir.join(format!("{alias}.zfont")), "U+0041 0,0 1,1\n").unwrap();
        }
        let results = std::thread::scope(|s| {
            let workers: Vec<_> = aliases[..2]
                .iter()
                .map(|alias| s.spawn(|| save(&dir, alias, source).unwrap()))
                .collect();
            workers
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_ne!(results[0], results[1]);
        assert!(results.iter().any(|s| s == "样本-行楷"));
        assert!(results.iter().any(|s| s == "样本-行楷（2）"));
        let reopened = FontNames::from_dir(&dir, &aliases);
        assert_eq!(reopened.label(&aliases[0]), results[0]);
        assert_eq!(
            reopened.stack("local-first,legacy,local-next"),
            format!("{} / legacy / {}", results[0], results[1])
        );
        assert_eq!(
            fs::read_to_string(dir.join("local-first.zfont")).unwrap(),
            "U+0041 0,0 1,1\n"
        );
        fs::rename(dir.join("local-first.zfont"), dir.join("removed.font")).unwrap();
        fs::write(dir.join("local-first.zfont"), "U+0042 0,0 1,1\n").unwrap();
        let reopened = FontNames::from_dir(&dir, &aliases);
        assert_eq!(reopened.label("local-first"), "local-first");
        assert_eq!(reopened.label("local-next"), results[1]);
        fs::remove_dir_all(dir).unwrap();
    }
}
