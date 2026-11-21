use super::Result;
use std::path::Path;
#[path = "wayland.rs"]
mod wayland;
#[path = "x11.rs"]
mod x11;

// One desktop owns this fixed 280-byte table; keeping it inline avoids a heap allocation.
#[allow(clippy::large_enum_variant)]
enum Backend {
    X11(x11::X11),
    Wayland(wayland::Wayland),
}
pub struct Desktop {
    inner: Backend,
}
impl Desktop {
    pub fn connect() -> Result<Self> {
        let selected = std::env::var("ZUSE_BACKEND").unwrap_or_default();
        let wayland = std::env::var("WAYLAND_DISPLAY").is_ok_and(|s| !s.is_empty());
        let inner = match selected.as_str() {
            "x11" => Backend::X11(x11::X11::connect()?),
            "wayland" => Backend::Wayland(wayland::Wayland::connect()?),
            "" if wayland => Backend::Wayland(wayland::Wayland::connect()?),
            "" => Backend::X11(x11::X11::connect()?),
            _ => return Err("ZUSE_BACKEND must be x11 or wayland".into()),
        };
        Ok(Self { inner })
    }
    pub fn backend(&self) -> &'static str {
        match &self.inner {
            Backend::X11(_) => "x11",
            Backend::Wayland(_) => "wayland",
        }
    }
    pub fn capabilities(&self) -> String {
        match &self.inner {
            Backend::X11(x) => format!(
                "{{\"backend\":\"x11\",\"native\":true,\"keyboard_state\":true,\"num_lock\":true,\"pointer\":{},\"keyboard\":{},\"windows\":true,\"capture\":true,\"backgroundInput\":\"XSendEvent; application dependent\",\"wine\":\"X11/XWayland windows; Wine installation optional\"}}",
                x.has_input(),
                x.has_input()
            ),
            Backend::Wayland(w) => w.capabilities(),
        }
    }
    pub fn position(&self) -> Result<(i32, i32)> {
        match &self.inner {
            Backend::X11(x) => x.position(),
            Backend::Wayland(w) => w.position(),
        }
    }
    pub fn move_to(&mut self, x: i32, y: i32) -> Result<()> {
        match &mut self.inner {
            Backend::X11(b) => b.move_to(x, y),
            Backend::Wayland(b) => b.move_to(x, y),
        }
    }
    pub fn button(&mut self, b: &str, a: &str) -> Result<()> {
        match &mut self.inner {
            Backend::X11(x) => x.button(b, a),
            Backend::Wayland(w) => w.button(b, a),
        }
    }
    pub fn key(&mut self, chord: &str) -> Result<()> {
        match &mut self.inner {
            Backend::X11(x) => x.key(chord),
            Backend::Wayland(w) => w.key(chord),
        }
    }
    pub fn keyboard_status(&self) -> Result<String> {
        match &self.inner {
            Backend::X11(x) => x.keyboard_status(),
            Backend::Wayland(w) => w.keyboard_status(),
        }
    }
    pub fn hold_key(
        &mut self,
        key: &str,
        duration_ms: u64,
        window: &str,
        cancellation: &crate::observation::Cancellation,
    ) -> Result<String> {
        self.hold_key_with_other_keys(key, duration_ms, window, false, cancellation)
    }
    /// Opt in to other ordinary keys while retaining target and ownership checks.
    pub fn hold_key_with_other_keys(
        &mut self,
        key: &str,
        duration_ms: u64,
        window: &str,
        allow_other_keys: bool,
        cancellation: &crate::observation::Cancellation,
    ) -> Result<String> {
        match &mut self.inner {
            Backend::X11(x) => x.hold_key(key, duration_ms, window, allow_other_keys, cancellation),
            Backend::Wayland(w) => {
                w.hold_key(key, duration_ms, window, allow_other_keys, cancellation)
            }
        }
    }
    pub fn num_lock(&self, action: &str) -> Result<String> {
        match &self.inner {
            Backend::X11(x) => x.num_lock(action),
            Backend::Wayland(w) => w.num_lock(action),
        }
    }
    pub fn type_text(&mut self, text: &str) -> Result<()> {
        match &mut self.inner {
            Backend::X11(x) => x.type_text(text),
            Backend::Wayland(w) => w.type_text(text),
        }
    }
    pub fn scroll(&mut self, x: i32, y: i32) -> Result<()> {
        match &mut self.inner {
            Backend::X11(b) => b.scroll(x, y),
            Backend::Wayland(b) => b.scroll(x, y),
        }
    }
    pub fn windows(&self) -> Result<String> {
        match &self.inner {
            Backend::X11(x) => x.windows(),
            Backend::Wayland(w) => w.windows(),
        }
    }
    pub fn window_hit_test(&self, id: &str, x: i32, y: i32) -> Result<bool> {
        match &self.inner {
            Backend::X11(display) => display.window_hit_test(id, x, y),
            Backend::Wayland(_) => Err("unsupported_state: this Wayland backend cannot prove the input surface at a point; use native accessibility actions or a browser target".into()),
        }
    }
    pub fn focus(&mut self, id: &str) -> Result<()> {
        match &mut self.inner {
            Backend::X11(x) => x.focus(id),
            Backend::Wayland(w) => w.focus(id),
        }
    }
    pub fn screenshot(&self, path: &Path, window: Option<&str>) -> Result<String> {
        match &self.inner {
            Backend::X11(x) => x.screenshot(path, window),
            Backend::Wayland(w) => w.screenshot(path, window),
        }
    }
    pub fn type_into(&mut self, id: &str, text: &str) -> Result<()> {
        match &mut self.inner {
            Backend::X11(x) => x.type_into(id, text),
            Backend::Wayland(w) => w.type_into(id, text),
        }
    }
    pub fn key_into(&mut self, id: &str, chord: &str) -> Result<()> {
        match &mut self.inner {
            Backend::X11(x) => x.key_into(id, chord),
            Backend::Wayland(w) => w.key_into(id, chord),
        }
    }
}
