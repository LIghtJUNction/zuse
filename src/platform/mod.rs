//! Operating-system desktop adapters.
use std::path::Path;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "linux", target_os = "windows", test))]
mod png;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "linux")]
pub use linux::Desktop;
#[cfg(target_os = "macos")]
pub use macos::Desktop;
#[cfg(target_os = "windows")]
pub use windows::Desktop;
pub type Result<T> = std::result::Result<T, String>;

pub(crate) fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < ' ' => {
                use std::fmt::Write;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
pub(crate) fn image_metadata(path: &Path, width: u32, height: u32, backend: &str) -> String {
    format!(
        "{{\"path\":{},\"width\":{},\"height\":{},\"mimeType\":\"image/png\",\"backend\":{}}}",
        quote(&path.to_string_lossy()),
        width,
        height,
        quote(backend)
    )
}
pub(crate) fn validate_text(text: &str) -> Result<()> {
    if text.contains('\0') {
        return Err("input text contains a NUL byte".into());
    }
    if text.len() > 1_048_576 {
        return Err("input text exceeds the 1 MiB limit".into());
    }
    Ok(())
}
pub(crate) fn split_chord(chord: &str) -> Result<Vec<&str>> {
    if chord.is_empty() || chord.len() > 256 || chord.contains('\0') {
        return Err("key chord must contain 1..256 non-NUL bytes".into());
    }
    let parts: Vec<_> = chord.split('+').collect();
    if parts.len() > 8 || parts.iter().any(|s| s.is_empty()) {
        return Err("key chord must contain 1..8 nonempty keys separated by +".into());
    }
    Ok(parts)
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
pub struct Desktop;
#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
impl Desktop {
    pub fn connect() -> Result<Self> {
        Err("desktop control requires Linux, Windows or macOS".into())
    }
    pub fn backend(&self) -> &'static str {
        "unsupported"
    }
    pub fn capabilities(&self) -> String {
        "{\"backend\":\"unsupported\"}".into()
    }
    pub fn position(&self) -> Result<(i32, i32)> {
        Err("unsupported platform".into())
    }
    pub fn move_to(&mut self, _: i32, _: i32) -> Result<()> {
        Err("unsupported platform".into())
    }
    pub fn button(&mut self, _: &str, _: &str) -> Result<()> {
        Err("unsupported platform".into())
    }
    pub fn key(&mut self, _: &str) -> Result<()> {
        Err("unsupported platform".into())
    }
    pub fn type_text(&mut self, _: &str) -> Result<()> {
        Err("unsupported platform".into())
    }
    pub fn scroll(&mut self, _: i32, _: i32) -> Result<()> {
        Err("unsupported platform".into())
    }
    pub fn windows(&self) -> Result<String> {
        Err("unsupported platform".into())
    }
    pub fn focus(&mut self, _: &str) -> Result<()> {
        Err("unsupported platform".into())
    }
    pub fn screenshot(&self, _: &Path, _: Option<&str>) -> Result<String> {
        Err("unsupported platform".into())
    }
    pub fn type_into(&mut self, _: &str, _: &str) -> Result<()> {
        Err("unsupported platform".into())
    }
    pub fn key_into(&mut self, _: &str, _: &str) -> Result<()> {
        Err("unsupported platform".into())
    }
}
