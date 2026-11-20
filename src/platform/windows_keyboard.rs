//! Win32 window-key message encoding, kept independent of the native API.
//!
//! This plans one self-contained chord with no initially held modifiers. Posting
//! these messages does not change the target thread's keyboard-state table and
//! does not prove application delivery. SendInput remains the foreground path.
use super::{Result, split_chord};

fn key_code(name: &str) -> Result<u16> {
    let lower = name.to_ascii_lowercase();
    let code = match lower.as_str() {
        "ctrl" | "control" => 0x11,
        "shift" => 0x10,
        "alt" => 0x12,
        "super" | "meta" | "win" | "cmd" => 0x5b,
        "enter" | "return" => 0x0d,
        "esc" | "escape" => 0x1b,
        "tab" => 0x09,
        "backspace" => 0x08,
        "space" => 0x20,
        "delete" | "del" => 0x2e,
        "insert" => 0x2d,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "capslock" => 0x14,
        "plus" | "equal" => 0xbb,
        "minus" => 0xbd,
        "comma" => 0xbc,
        "period" => 0xbe,
        "slash" => 0xbf,
        _ if lower.len() == 1 && lower.as_bytes()[0].is_ascii_alphanumeric() => {
            lower.as_bytes()[0].to_ascii_uppercase() as u16
        }
        _ if lower.starts_with('f') => {
            let n: u16 = lower[1..]
                .parse()
                .map_err(|_| format!("unknown key {name}"))?;
            if !(1..=24).contains(&n) {
                return Err("function key must be F1..F24".into());
            }
            0x6f + n
        }
        _ => return Err(format!("unknown key {name}")),
    };
    Ok(code)
}
pub(super) fn keys(chord: &str) -> Result<Vec<u16>> {
    split_chord(chord)?.into_iter().map(key_code).collect()
}
const WM_KEYDOWN: u32 = 0x0100;
const WM_KEYUP: u32 = 0x0101;
const WM_SYSKEYDOWN: u32 = 0x0104;
const WM_SYSKEYUP: u32 = 0x0105;
const VK_CONTROL: u16 = 0x11;
const VK_MENU: u16 = 0x12;
const VK_F10: u16 = 0x79;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct WindowKeyMessage {
    pub message: u32,
    pub vk: u16,
    pub lparam: u32,
}

/// Validate and encode the complete chord before the first PostMessageW call.
///
/// The mapping callback supplies MAPVK_VK_TO_VSC_EX results, including E0/E1
/// prefixes. Invalid keys, duplicate aliases and unmapped keys therefore fail
/// before any key-down message can be posted.
pub(super) fn window_key_messages(
    chord: &str,
    mut map_scan: impl FnMut(u16) -> u32,
) -> Result<Vec<WindowKeyMessage>> {
    let virtual_keys = keys(chord)?;
    for (index, &vk) in virtual_keys.iter().enumerate() {
        if virtual_keys[..index].contains(&vk) {
            return Err(format!("window key chord repeats virtual key 0x{vk:02x}"));
        }
    }
    let mut mapped = Vec::with_capacity(virtual_keys.len());
    for vk in virtual_keys {
        let scan = map_scan(vk);
        if scan & 0xff == 0 || !matches!(scan >> 8, 0 | 0xe0 | 0xe1) {
            return Err(format!(
                "Windows could not map virtual key 0x{vk:02x} to a scan code"
            ));
        }
        // Only E0 is the Win32 extended-key flag; prefixes are not part of the
        // eight-bit scan-code field and must not spill into reserved bits.
        let flags = ((scan & 0xff) << 16) | if scan >> 8 == 0xe0 { 1 << 24 } else { 0 };
        mapped.push((vk, flags));
    }

    let mut events = Vec::with_capacity(mapped.len() * 2);
    let mut alt = false;
    let mut control = false;
    let mut alt_menu_pending = false;
    for (&(vk, scan_flags), down) in mapped
        .iter()
        .map(|key| (key, true))
        .chain(mapped.iter().rev().map(|key| (key, false)))
    {
        // Alt alone activates the menu on release. After an Alt shortcut, the
        // final Alt release is WM_KEYUP (the documented Alt+P sequence), not a
        // second menu activation. Ctrl suppresses ordinary Alt system chords;
        // its release while Alt is held and F10 have their own system messages.
        let system = if vk == VK_F10 {
            true
        } else if vk == VK_MENU {
            if down { !control } else { alt_menu_pending }
        } else if vk == VK_CONTROL {
            !down && alt
        } else {
            alt && !control
        };
        if system {
            alt_menu_pending = vk == VK_MENU && down;
        }
        if vk == VK_MENU {
            alt = down;
        } else if vk == VK_CONTROL {
            control = down;
        }

        let message = match (system, down) {
            (false, true) => WM_KEYDOWN,
            (false, false) => WM_KEYUP,
            (true, true) => WM_SYSKEYDOWN,
            (true, false) => WM_SYSKEYUP,
        };
        events.push(WindowKeyMessage {
            message,
            vk,
            lparam: 1
                | scan_flags
                // WM_KEYDOWN/WM_KEYUP reserve the context bit as zero, even
                // when Ctrl has made an Alt chord a nonsystem sequence.
                | if system && alt { 1 << 29 } else { 0 }
                | if down { 0 } else { 0xc000_0000 },
        });
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(vk: u16) -> u32 {
        match vk {
            0x10 => 0x2a, // Shift
            0x11 => 0x1d, // Control
            0x12 => 0x38, // Alt
            0x41 => 0x1e, // A
            0x50 => 0x19, // P
            0x73 => 0x3e, // F4
            0x79 => 0x44, // F10
            _ => panic!("missing scan-code fixture for {vk:#x}"),
        }
    }

    fn tuples(events: Vec<WindowKeyMessage>) -> Vec<(u32, u16, u32)> {
        events
            .into_iter()
            .map(|event| (event.message, event.vk, event.lparam))
            .collect()
    }

    #[test]
    fn plain_chord_preserves_press_order_and_releases_in_reverse() {
        assert_eq!(
            tuples(window_key_messages("Ctrl+Shift+A", scan).unwrap()),
            [
                (0x100, 0x11, 0x001d_0001),
                (0x100, 0x10, 0x002a_0001),
                (0x100, 0x41, 0x001e_0001),
                (0x101, 0x41, 0xc01e_0001),
                (0x101, 0x10, 0xc02a_0001),
                (0x101, 0x11, 0xc01d_0001),
            ]
        );
    }

    #[test]
    fn navigation_and_windows_keys_preserve_the_e0_extended_flag() {
        for (name, vk, scan, expected) in [
            ("pageup", 0x21, 0xe049, 0x0149_0001),
            ("pagedown", 0x22, 0xe051, 0x0151_0001),
            ("end", 0x23, 0xe04f, 0x014f_0001),
            ("home", 0x24, 0xe047, 0x0147_0001),
            ("left", 0x25, 0xe04b, 0x014b_0001),
            ("up", 0x26, 0xe048, 0x0148_0001),
            ("right", 0x27, 0xe04d, 0x014d_0001),
            ("down", 0x28, 0xe050, 0x0150_0001),
            ("insert", 0x2d, 0xe052, 0x0152_0001),
            ("delete", 0x2e, 0xe053, 0x0153_0001),
            ("win", 0x5b, 0xe05b, 0x015b_0001),
        ] {
            assert_eq!(
                tuples(
                    window_key_messages(name, |actual_vk| {
                        assert_eq!(actual_vk, vk);
                        scan
                    })
                    .unwrap()
                ),
                [(0x100, vk, expected), (0x101, vk, expected | 0xc000_0000),],
                "{name}"
            );
        }
    }

    #[test]
    fn alt_shortcut_uses_system_messages_and_normal_final_alt_release() {
        // Microsoft's documented Alt+P sequence, excluding WM_SYSCHAR, which
        // is generated by the receiving application's TranslateMessage call.
        assert_eq!(
            tuples(window_key_messages("Alt+P", scan).unwrap()),
            [
                (0x104, 0x12, 0x2038_0001),
                (0x104, 0x50, 0x2019_0001),
                (0x105, 0x50, 0xe019_0001),
                (0x101, 0x12, 0xc038_0001),
            ]
        );
        assert_eq!(
            tuples(window_key_messages("Alt+F4", scan).unwrap()),
            [
                (0x104, 0x12, 0x2038_0001),
                (0x104, 0x73, 0x203e_0001),
                (0x105, 0x73, 0xe03e_0001),
                (0x101, 0x12, 0xc038_0001),
            ]
        );
    }

    #[test]
    fn alt_alone_clears_context_on_system_key_release() {
        assert_eq!(
            tuples(window_key_messages("alt", scan).unwrap()),
            [(0x104, 0x12, 0x2038_0001), (0x105, 0x12, 0xc038_0001),]
        );
    }

    #[test]
    fn f10_is_a_system_key_without_alt() {
        assert_eq!(
            tuples(window_key_messages("f10", scan).unwrap()),
            [(0x104, 0x79, 0x0044_0001), (0x105, 0x79, 0xc044_0001),]
        );
        assert_eq!(
            tuples(window_key_messages("Ctrl+F10", scan).unwrap()),
            [
                (0x100, 0x11, 0x001d_0001),
                (0x104, 0x79, 0x0044_0001),
                (0x105, 0x79, 0xc044_0001),
                (0x101, 0x11, 0xc01d_0001),
            ]
        );
    }

    #[test]
    fn control_alt_chord_uses_nonsystem_messages_with_zero_context() {
        assert_eq!(
            tuples(window_key_messages("Ctrl+Alt+P", scan).unwrap()),
            [
                (0x100, 0x11, 0x001d_0001),
                (0x100, 0x12, 0x0038_0001),
                (0x100, 0x50, 0x0019_0001),
                (0x101, 0x50, 0xc019_0001),
                (0x101, 0x12, 0xc038_0001),
                (0x101, 0x11, 0xc01d_0001),
            ]
        );
    }

    #[test]
    fn control_pressed_after_alt_uses_system_keyup_before_alt_release() {
        assert_eq!(
            tuples(window_key_messages("Alt+Ctrl+P", scan).unwrap()),
            [
                (0x104, 0x12, 0x2038_0001),
                (0x100, 0x11, 0x001d_0001),
                (0x100, 0x50, 0x0019_0001),
                (0x101, 0x50, 0xc019_0001),
                (0x105, 0x11, 0xe01d_0001),
                (0x101, 0x12, 0xc038_0001),
            ]
        );
    }

    #[test]
    fn invalid_chords_are_rejected_before_native_mapping() {
        for chord in [
            "",
            "Ctrl++A",
            "Ctrl+unknown-key",
            "Ctrl+Control+A",
            "Alt+alt",
            "F0",
            "F25",
            "A\0",
            "a+b+c+d+e+f+g+h+i",
        ] {
            assert!(
                window_key_messages(chord, |_| panic!("must validate first")).is_err(),
                "{chord:?}"
            );
        }
    }

    #[test]
    fn scan_mapping_is_snapshotted_once_and_invalid_maps_return_no_plan() {
        let mut requested = Vec::new();
        let events = window_key_messages("Ctrl+Shift+A", |vk| {
            requested.push(vk);
            scan(vk)
        })
        .unwrap();
        assert_eq!(requested, [0x11, 0x10, 0x41]);
        assert_eq!(events.len(), 6);

        for invalid in [0, 0xe000, 0x0101, 0x0001_e04b] {
            let mut requested = Vec::new();
            let result = window_key_messages("Ctrl+A", |vk| {
                requested.push(vk);
                if vk == 0x41 { invalid } else { scan(vk) }
            });
            assert!(result.unwrap_err().contains("virtual key 0x41"));
            assert_eq!(requested, [0x11, 0x41]);
        }
    }

    #[test]
    fn nonsystem_context_and_reserved_bits_are_zero_for_modifier_orders() {
        let names = ["ctrl", "alt", "shift", "a", "p", "f10"];
        for first in names {
            for second in names {
                if second == first {
                    continue;
                }
                for third in names {
                    if third == first || third == second {
                        continue;
                    }
                    let chord = format!("{first}+{second}+{third}");
                    for event in window_key_messages(&chord, scan).unwrap() {
                        if matches!(event.message, 0x100 | 0x101) {
                            assert_eq!(event.lparam & (1 << 29), 0, "{chord}: {event:?}");
                        }
                        assert_eq!(event.lparam & 0x1e00_0000, 0, "{chord}: {event:?}");
                        assert_eq!(event.lparam & 0xffff, 1, "{chord}: {event:?}");
                        let release = matches!(event.message, 0x101 | 0x105);
                        assert_eq!(
                            event.lparam & 0xc000_0000,
                            if release { 0xc000_0000 } else { 0 },
                            "{chord}: {event:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn foreground_key_parser_keeps_existing_alias_and_duplicate_behavior() {
        assert_eq!(keys("control+Ctrl+a").unwrap(), [0x11, 0x11, 0x41]);
        assert_eq!(keys("super+F24").unwrap(), [0x5b, 0x87]);
        assert_eq!(keys("enter+del").unwrap(), [0x0d, 0x2e]);
    }
}
