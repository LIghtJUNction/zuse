//! User-installed single-line fonts. See `zuse font help` for the storage paths.
use crate::{handwriting::Font, json::quote};
use std::{
    env,
    ffi::OsString,
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

const BUILTIN: &str = "builtin";
const MAX_BYTES: u64 = 1024 * 1024;
const EXT: &str = "zfont";
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(test)]
static NEXT: AtomicU64 = AtomicU64::new(0);
const HELP: &str = "zuse font list | add NAME FILE | remove NAME | help\nFonts are UTF-8 single-line stroke definitions (up to 1 MiB per file). The name builtin is reserved. Use a lowercase ASCII letter followed by lowercase letters, digits, '-' or '_' (up to 64 characters). Set ZUSE_FONT_DIR to an absolute directory to override storage. Defaults to $XDG_DATA_HOME/zuse/fonts, %LOCALAPPDATA%/zuse/fonts on Windows, or $HOME/.local/share/zuse/fonts. Fonts are stored as NAME.zfont. Remove a font explicitly before replacing it.";

fn directory() -> Result<PathBuf, String> {
    fn value(name: &str) -> Option<OsString> {
        env::var_os(name).filter(|v| !v.is_empty())
    }
    if let Some(dir) = value("ZUSE_FONT_DIR") {
        let path = PathBuf::from(dir);
        return if path.is_absolute() {
            Ok(path)
        } else {
            Err("ZUSE_FONT_DIR must be absolute".into())
        };
    }
    if let Some(data) = value("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return Ok(data.join("zuse/fonts"));
    }
    #[cfg(windows)]
    if let Some(data) = value("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return Ok(data.join("zuse/fonts"));
    }
    value("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(|p| p.join(".local/share/zuse/fonts"))
        .ok_or_else(|| "set ZUSE_FONT_DIR to an absolute font directory".into())
}

fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    let reserved = matches!(name, "con" | "prn" | "aux" | "nul")
        || ((name.starts_with("com") || name.starts_with("lpt"))
            && bytes.len() == 4
            && matches!(bytes[3], b'1'..=b'9'));
    !reserved
        && (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'_'))
}

fn font_path(dir: &Path, name: &str) -> Result<PathBuf, String> {
    if name == BUILTIN || !valid_name(name) {
        return Err(format!("invalid or reserved font name {name:?}"));
    }
    Ok(dir.join(format!("{name}.{EXT}")))
}

fn read_font(path: &Path) -> Result<String, String> {
    let report = |e: io::Error| format!("{}: {e}", path.display());
    let metadata = fs::metadata(path).map_err(report)?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return Err(format!(
            "{}: expected a regular font file up to 1 MiB",
            path.display()
        ));
    }
    let mut text = String::new();
    File::open(path)
        .map_err(report)?
        .take(MAX_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(report)?;
    if text.len() as u64 > MAX_BYTES {
        return Err(format!("{}: font exceeds 1 MiB", path.display()));
    }
    Ok(text)
}

pub fn load(name: &str) -> Result<Font, String> {
    if name == BUILTIN {
        return Ok(Font::builtin());
    }
    let path = font_path(&directory()?, name)?;
    Font::parse(&read_font(&path)?).map_err(|e| format!("{}: {e}", path.display()))
}

fn publish(dir: &Path, destination: &Path, bytes: &[u8]) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    crate::output_file::publish(destination, bytes)
}

fn list(dir: &Path) -> Result<String, String> {
    let mut names = vec![BUILTIN.to_owned()];
    match fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|e| e.to_string())?;
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some(EXT) {
                    continue;
                }
                if let Some(name) = path.file_stem().and_then(|s| s.to_str())
                    && name != BUILTIN
                    && valid_name(name)
                    && entry.file_type().map_err(|e| e.to_string())?.is_file()
                {
                    names.push(name.to_owned());
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => (),
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    }
    names.sort_unstable();
    let names = names.iter().map(|n| quote(n)).collect::<Vec<_>>().join(",");
    Ok(format!(
        r#"{{"directory":{},"fonts":[{names}]}}"#,
        quote(&dir.to_string_lossy())
    ))
}

fn command(dir: &Path, args: &[&str]) -> Result<String, String> {
    match *args {
        ["list"] => list(dir),
        ["add", name, source] => {
            let dest = font_path(dir, name)?;
            let text = read_font(Path::new(source))?;
            Font::parse(&text).map_err(|e| format!("{source}: {e}"))?;
            publish(dir, &dest, text.as_bytes()).map_err(|e| format!("{}: {e}", dest.display()))?;
            Ok(format!(r#"{{"added":{}}}"#, quote(name)))
        }
        ["remove", name] => {
            let dest = font_path(dir, name)?;
            fs::remove_file(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
            Ok(format!(r#"{{"removed":{}}}"#, quote(name)))
        }
        _ => Err(HELP.into()),
    }
}

pub fn cli(args: &[&str]) -> Result<String, String> {
    if matches!(args, [] | ["help" | "-h" | "--help"]) {
        return Ok(format!(r#"{{"help":{}}}"#, quote(HELP)));
    }
    command(&directory()?, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir = env::temp_dir().join(format!("zuse-font-test-{}-{id}", std::process::id()));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn builtin_and_names_are_portable() {
        assert!(load(BUILTIN).is_ok());
        for name in [
            "builtin", "../font", "a/b", "A", "", ".", "c:font", "nul", "com1",
        ] {
            assert!(font_path(Path::new("fonts"), name).is_err(), "{name}");
        }
        assert!(font_path(Path::new("fonts"), "latin-cjk_2").is_ok());
    }

    #[test]
    fn invalid_sources_cannot_install() {
        let tmp = Scratch::new();
        let dir = tmp.0.join("fonts");
        let source = tmp.0.join("source");
        let args = ["add", "user", source.to_str().unwrap()];
        fs::write(&source, b"this is not a font definition").unwrap();
        assert!(command(&dir, &args).is_err());
        File::create(&source)
            .unwrap()
            .set_len(MAX_BYTES + 1)
            .unwrap();
        assert!(command(&dir, &args).is_err());
        assert!(!dir.exists());
    }

    #[test]
    fn installed_unicode_font_survives_duplicate_add() {
        let tmp = Scratch::new();
        let dir = tmp.0.join("fonts");
        let source = tmp.0.join("source");
        let args = ["add", "kanji", source.to_str().unwrap()];
        let strokes = "二 0,.25 1,.25 | 0,.75 1,.75";
        fs::write(&source, strokes).unwrap();
        assert_eq!(command(&dir, &args).unwrap(), r#"{"added":"kanji"}"#);
        fs::write(&source, "三 0,0 1,1").unwrap();
        assert!(command(&dir, &args).is_err());
        let installed = read_font(&font_path(&dir, "kanji").unwrap()).unwrap();
        assert_eq!(installed, strokes);
        assert!(Font::parse(&installed).is_ok());
    }

    #[test]
    fn concurrent_publish_has_one_winner_and_keeps_complete_contents() {
        let tmp = Scratch::new();
        let dest = font_path(&tmp.0, "sample").unwrap();
        std::thread::scope(|s| {
            let first = s.spawn(|| publish(&tmp.0, &dest, b"first"));
            let second = s.spawn(|| publish(&tmp.0, &dest, b"second"));
            let a = first.join().unwrap().is_ok();
            let b = second.join().unwrap().is_ok();
            assert_ne!(a, b);
            assert_eq!(
                fs::read(&dest).unwrap(),
                if a { &b"first"[..] } else { &b"second"[..] }
            );
        });
        assert_eq!(fs::read_dir(&tmp.0).unwrap().count(), 1);
        assert!(
            command(&tmp.0, &["list"])
                .unwrap()
                .contains(r#""fonts":["builtin","sample"]"#)
        );
        assert!(command(&tmp.0, &["remove", "sample"]).is_ok());
        assert!(!dest.exists());
    }
}
