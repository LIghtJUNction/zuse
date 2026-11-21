//! Syntax shared by Hyprland input preparation and flat-program preflight.
use super::{Result, split_chord};

pub(crate) fn chord(chord: &str) -> Result<(&str, Vec<&'static str>)> {
    let parts = split_chord(chord)?;
    let last = *parts.last().ok_or("empty key chord")?;
    let key = match last.to_ascii_lowercase().as_str() {
        "enter" | "return" => "Return",
        "esc" | "escape" => "Escape",
        "backspace" => "BackSpace",
        "tab" => "Tab",
        "space" => "space",
        "left" => "Left",
        "right" => "Right",
        "up" => "Up",
        "down" => "Down",
        "delete" | "del" => "Delete",
        "pageup" => "Page_Up",
        "pagedown" => "Page_Down",
        "home" => "Home",
        "end" => "End",
        _ => last,
    };
    let raw_code = key.strip_prefix("code:");
    if let Some(code) = raw_code {
        if !code.bytes().all(|b| b.is_ascii_digit())
            || !code.parse::<u16>().is_ok_and(|n| (8..=255).contains(&n))
        {
            return Err("XKB keycode must be in 8..255".into());
        }
    } else if !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Err("use XKB key names or code:8..255 in chords".into());
    }
    let mut modifiers = Vec::new();
    for p in &parts[..parts.len() - 1] {
        modifiers.push(match p.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => "CTRL",
            "alt" => "ALT",
            "shift" => "SHIFT",
            "super" | "meta" | "win" | "cmd" => "SUPER",
            _ => return Err(format!("unsupported modifier {p}")),
        });
    }
    Ok((key, modifiers))
}

pub(crate) fn selector(id: &str) -> Result<String> {
    if id == "active" {
        return Ok("active".into());
    }
    let hex = id
        .strip_prefix("0x")
        .ok_or("Hyprland window ID must be 0x hexadecimal or active")?;
    if hex.is_empty() || hex.len() > 16 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid Hyprland window ID".into());
    }
    Ok(format!("address:0x{hex}"))
}
