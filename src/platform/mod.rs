//! Operating-system desktop adapters.
use std::path::Path;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) mod dylib;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "macos", test))]
mod macos_focus;
#[cfg(any(target_os = "linux", target_os = "windows"))]
mod png;
#[cfg(target_os = "linux")]
pub(crate) mod pointer_x11;
#[cfg(target_os = "linux")]
pub(crate) mod wayland_marker;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(any(target_os = "windows", test))]
mod windows_keyboard;
#[cfg(target_os = "linux")]
pub use linux::Desktop;
#[cfg(target_os = "macos")]
pub use macos::Desktop;
#[cfg(target_os = "windows")]
pub use windows::Desktop;
pub type Result<T> = std::result::Result<T, String>;

#[cfg(not(target_os = "linux"))]
impl Desktop {
    pub fn hold_key(
        &mut self,
        _key: &str,
        _duration_ms: u64,
        _window: &str,
        _cancellation: &crate::observation::Cancellation,
    ) -> Result<String> {
        self.hold_key_with_other_keys(_key, _duration_ms, _window, false, _cancellation)
    }
    pub fn hold_key_with_other_keys(
        &mut self,
        _key: &str,
        _duration_ms: u64,
        _window: &str,
        _allow_other_keys: bool,
        _cancellation: &crate::observation::Cancellation,
    ) -> Result<String> {
        Err("unsupported_state: native key holds currently require Linux X11 or Hyprland; use browser.hold for an explicitly connected browser page".into())
    }
    pub fn window_hit_test(&self, _id: &str, _x: i32, _y: i32) -> Result<bool> {
        Err(
            "unsupported_state: native window hit testing is not implemented for this backend"
                .into(),
        )
    }

    pub fn keyboard_status(&self) -> Result<String> {
        Err("unsupported_state: explicit keyboard state inspection is currently supported on Linux X11 and Hyprland".into())
    }

    pub fn num_lock(&self, _action: &str) -> Result<String> {
        Err("unsupported_state: explicit Num Lock control is currently supported on Linux X11 and Hyprland".into())
    }
}

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
