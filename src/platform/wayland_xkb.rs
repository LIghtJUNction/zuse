//! Resolve targeted text against the compositor's actual XKB map.
//! No keyboard, keymap or compositor configuration is changed by this module.
use super::{Result, keymap};
use crate::platform::dylib::Library;
use std::{
    collections::BTreeMap,
    ffi::{CString, c_char, c_int, c_void},
    ptr,
};

type Handle = *mut c_void;
unsafe extern "C" {
    // xkb_keymap_get_as_string returns memory owned by the C allocator.
    fn free(pointer: *mut c_void);
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Stroke {
    pub code: u32,
    pub modifiers: &'static str,
}

/// A foreground chord in an unchanged native keymap, with Num Lock retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LockedChord {
    pub code: u32,
    pub depressed: u32,
    pub locked: u32,
}
struct Api {
    context_new: unsafe extern "C" fn(c_int) -> Handle,
    context_unref: unsafe extern "C" fn(Handle),
    keymap_new: unsafe extern "C" fn(Handle, *const c_char, c_int, c_int) -> Handle,
    keymap_unref: unsafe extern "C" fn(Handle),
    keymap_string: unsafe extern "C" fn(Handle, c_int) -> *mut c_char,
    state_new: unsafe extern "C" fn(Handle) -> Handle,
    state_unref: unsafe extern "C" fn(Handle),
    mod_index: unsafe extern "C" fn(Handle, *const c_char) -> u32,
    update_mask: unsafe extern "C" fn(Handle, u32, u32, u32, u32, u32, u32) -> u32,
    update_key: unsafe extern "C" fn(Handle, u32, c_int) -> u32,
    serialize_mods: unsafe extern "C" fn(Handle, u32) -> u32,
    one_sym: unsafe extern "C" fn(Handle, u32) -> u32,
    keysym_from_name: unsafe extern "C" fn(*const c_char, c_int) -> u32,
    utf32: unsafe extern "C" fn(Handle, u32) -> u32,
}
struct Map {
    api: Api,
    context: Handle,
    keymap: Handle,
    state: Handle,
    _library: Library,
}
impl Map {
    fn open(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 2_097_152 || bytes.last() != Some(&0) {
            return Err("invalid compositor XKB map".into());
        }
        let library = Library::open(c"libxkbcommon.so.0")?;
        // SAFETY: signatures match the installed libxkbcommon public C ABI.
        let api = unsafe {
            Api {
                context_new: library.symbol(c"xkb_context_new")?,
                context_unref: library.symbol(c"xkb_context_unref")?,
                keymap_new: library.symbol(c"xkb_keymap_new_from_string")?,
                keymap_unref: library.symbol(c"xkb_keymap_unref")?,
                keymap_string: library.symbol(c"xkb_keymap_get_as_string")?,
                state_new: library.symbol(c"xkb_state_new")?,
                state_unref: library.symbol(c"xkb_state_unref")?,
                mod_index: library.symbol(c"xkb_keymap_mod_get_index")?,
                update_mask: library.symbol(c"xkb_state_update_mask")?,
                update_key: library.symbol(c"xkb_state_update_key")?,
                serialize_mods: library.symbol(c"xkb_state_serialize_mods")?,
                one_sym: library.symbol(c"xkb_state_key_get_one_sym")?,
                keysym_from_name: library.symbol(c"xkb_keysym_from_name")?,
                utf32: library.symbol(c"xkb_state_key_get_utf32")?,
            }
        };
        let mut this = Self {
            api,
            context: ptr::null_mut(),
            keymap: ptr::null_mut(),
            state: ptr::null_mut(),
            _library: library,
        };
        // SAFETY: source bytes are NUL-terminated and retained for parsing.
        // Flags are NO_FLAGS and format is XKB_KEYMAP_FORMAT_TEXT_V1.
        unsafe {
            this.context = (this.api.context_new)(0);
            if this.context.is_null() {
                return Err("XKB context creation failed".into());
            }
            this.keymap = (this.api.keymap_new)(this.context, bytes.as_ptr().cast(), 1, 0);
            if this.keymap.is_null() {
                return Err("compositor XKB map cannot be parsed".into());
            }
            this.state = (this.api.state_new)(this.keymap);
            if this.state.is_null() {
                return Err("XKB state creation failed".into());
            }
        }
        Ok(this)
    }
    fn index(&self) -> BTreeMap<u32, Stroke> {
        let mut index = BTreeMap::new();
        // Targeted Hyprland shortcuts use layout group 0. Only plain and
        // Shift strokes are supported; other levels fail before any input.
        let shift = unsafe { (self.api.mod_index)(self.keymap, c"Shift".as_ptr()) };
        let variants = [
            (0, ""),
            (if shift < 32 { 1u32 << shift } else { 0 }, "SHIFT"),
        ];
        for (mask, modifiers) in variants {
            if modifiers == "SHIFT" && mask == 0 {
                continue;
            }
            unsafe {
                (self.api.update_mask)(self.state, mask, 0, 0, 0, 0, 0);
            }
            // Hyprland interprets 272..351 as mouse buttons; never produce
            // those values from text, even if the map contains such keys.
            for code in 8..=255 {
                let unicode = unsafe { (self.api.utf32)(self.state, code) };
                if unicode != 0 {
                    index.entry(unicode).or_insert(Stroke { code, modifiers });
                }
            }
        }
        index
    }

    fn modifier(&self, name: &std::ffi::CStr) -> Result<u32> {
        // SAFETY: the parsed keymap and NUL-terminated modifier name are live.
        let index = unsafe { (self.api.mod_index)(self.keymap, name.as_ptr()) };
        if index >= 32 {
            return Err(format!(
                "unsupported_state: keyboard map has no supported {} modifier",
                name.to_string_lossy()
            ));
        }
        Ok(1u32 << index)
    }

    fn canonical_bytes(&self) -> Result<Vec<u8>> {
        // SAFETY: the live parsed keymap returns an allocated, NUL-terminated
        // string in TEXT_V1 format; the public ABI requires free() afterward.
        let pointer = unsafe { (self.api.keymap_string)(self.keymap, 1) };
        if pointer.is_null() {
            return Err("cannot serialize the compositor XKB map".into());
        }
        let bytes = unsafe { std::ffi::CStr::from_ptr(pointer).to_bytes_with_nul() };
        let result = if bytes.len() <= 2_097_152 {
            Ok(bytes.to_vec())
        } else {
            Err("serialized compositor XKB map exceeds the size limit".into())
        };
        unsafe { free(pointer.cast()) };
        result
    }

    fn num_lock_mask(&self) -> Result<u32> {
        // Hyprland's numLock observation refers to XKB_MOD_NAME_NUM (Mod2).
        // Confirm the real Num_Lock key action in the actual map produces
        // that mask; do not assume the modifier's numeric index or synthesize
        // custom lock actions that the compositor observation cannot describe.
        let observed = self.modifier(c"Mod2")?;
        let mut resolved = None;
        for code in 8..=255 {
            // SAFETY: only this local XKB state is changed; no native input.
            unsafe {
                (self.api.update_mask)(self.state, 0, 0, 0, 0, 0, 0);
                if (self.api.one_sym)(self.state, code) != 0xff7f {
                    continue; // XKB_KEY_Num_Lock
                }
                (self.api.update_key)(self.state, code, 1); // XKB_KEY_DOWN
                (self.api.update_key)(self.state, code, 0); // XKB_KEY_UP
                let locked = (self.api.serialize_mods)(self.state, 1 << 2);
                let depressed = (self.api.serialize_mods)(self.state, 1);
                let latched = (self.api.serialize_mods)(self.state, 1 << 1);
                if locked != observed || depressed != 0 || latched != 0 {
                    return Err(
                        "unsupported_state: custom Num Lock key action cannot be preserved".into(),
                    );
                }
                resolved = Some(locked);
            }
        }
        // Leave the local state in the exact supported base state for lookup.
        let locked = resolved
            .ok_or("unsupported_state: keyboard map has no verifiable Num Lock key action")?;
        unsafe { (self.api.update_mask)(self.state, 0, 0, locked, 0, 0, 0) };
        Ok(locked)
    }
}

/// Compare actual maps after libxkbcommon's deterministic V1 serialization.
/// This does not infer a map from its display name or change keyboard state.
pub(super) fn normalized_keymap(bytes: &[u8]) -> Result<Vec<u8>> {
    Map::open(bytes)?.canonical_bytes()
}

impl Drop for Map {
    fn drop(&mut self) {
        // SAFETY: all objects originate from this loaded ABI. Release them
        // before Library drops; null objects represent partial construction.
        unsafe {
            if !self.state.is_null() {
                (self.api.state_unref)(self.state);
            }
            if !self.keymap.is_null() {
                (self.api.keymap_unref)(self.keymap);
            }
            if !self.context.is_null() {
                (self.api.context_unref)(self.context);
            }
        }
    }
}
pub(super) fn resolve(text: &str, original: &[u8]) -> Result<Vec<Stroke>> {
    keymap::validate(text)?;
    let map = Map::open(original)?;
    let index = map.index();
    text.chars().map(|ch| {
        let scalar = if ch == '\n' { '\r' as u32 } else { ch as u32 };
        index.get(&scalar).copied().ok_or_else(|| format!(
            "unsupported_character: U+{:04X} is not available in the current keyboard map's plain/Shift levels; targeted text was not sent",
            ch as u32))
    }).collect()
}

/// Resolve fully before creating a virtual keyboard or submitting a key.
pub(super) fn locked_chord(key: &str, modifiers: &[&str], original: &[u8]) -> Result<LockedChord> {
    chord_in_state(key, modifiers, original, true)
}

pub(super) fn chord_in_state(
    key: &str,
    modifiers: &[&str],
    original: &[u8],
    num_lock: bool,
) -> Result<LockedChord> {
    let map = Map::open(original)?;
    let locked = if num_lock { map.num_lock_mask()? } else { 0 };
    unsafe {
        (map.api.update_mask)(map.state, 0, 0, locked, 0, 0, 0);
    }
    // Hyprland suppresses only byte-identical recipient keymap updates.
    // Re-parsing can normalize even a compositor-produced map on some XKB
    // versions. A changed map would reset the client's lock state again when
    // this owned keyboard is removed, after our final modifiers event.
    if map.canonical_bytes()? != original {
        return Err("unsupported_state: keyboard map does not roundtrip identically; Num Lock restoration cannot be guaranteed".into());
    }
    let mut depressed = 0;
    for name in modifiers {
        depressed |= map.modifier(match *name {
            "CTRL" => c"Control",
            "ALT" => c"Mod1",
            "SHIFT" => c"Shift",
            "SUPER" => c"Mod4",
            _ => return Err("unsupported_state: unknown chord modifier".into()),
        })?;
    }
    let code = if let Some(number) = key.strip_prefix("code:") {
        number
            .parse::<u32>()
            .ok()
            .filter(|code| (8..=255).contains(code))
            .ok_or("XKB keycode must be in 8..255")?
    } else {
        let name = CString::new(key).map_err(|_| "keyboard key contains NUL")?;
        // XKB_KEYSYM_CASE_INSENSITIVE preserves the existing chord API.
        let symbol = unsafe { (map.api.keysym_from_name)(name.as_ptr(), 1) };
        if symbol == 0 {
            return Err("unknown XKB key name".into());
        }
        (8..=255)
            .find(|code| unsafe { (map.api.one_sym)(map.state, *code) == symbol })
            .ok_or("key is unavailable in the active keyboard map")?
    };
    let symbol = unsafe { (map.api.one_sym)(map.state, code) };
    if symbol == 0 || matches!(symbol, 0xff7f | 0xffe5 | 0xffe6) {
        return Err("unsupported_state: Num Lock preservation supports ordinary mapped keys, not lock changes".into());
    }
    Ok(LockedChord {
        code,
        depressed,
        locked,
    })
}

/// Classify simultaneous keys from their actions in the actual map, not from
/// assumed evdev positions. Remapped modifiers and layout/lock actions stay out.
pub(super) fn ordinary_keycodes(original: &[u8], num_lock: bool) -> Result<Vec<u32>> {
    let map = Map::open(original)?;
    let locked = if num_lock { map.num_lock_mask()? } else { 0 };
    let mut ordinary = Vec::new();
    for code in 8..=255 {
        // Only this private XKB state is changed; this submits no input.
        unsafe {
            (map.api.update_mask)(map.state, 0, 0, locked, 0, 0, 0);
            let symbol = (map.api.one_sym)(map.state, code);
            let down_changes = (map.api.update_key)(map.state, code, 1);
            let up_changes = (map.api.update_key)(map.state, code, 0);
            if symbol != 0
                && !crate::key_hold::modifier_keysym(symbol)
                && down_changes == 0
                && up_changes == 0
            {
                ordinary.push(code);
            }
        }
    }
    Ok(ordinary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restored_maps_are_compared_by_content_not_layout_display_name() {
        if Library::open(c"libxkbcommon.so.0").is_err() {
            return;
        }
        let map = |text| {
            let mut bytes = keymap::Batches::new(text)
                .next()
                .unwrap()
                .keymap
                .into_bytes();
            bytes.push(0);
            bytes
        };
        let original = map("zuse");
        let different = map("safe");
        // Both maps report the same synthetic "zuse" layout name.
        assert_ne!(
            normalized_keymap(&original).unwrap(),
            normalized_keymap(&different).unwrap()
        );
        let mut equivalent = vec![b'\n'];
        equivalent.extend_from_slice(&original);
        assert_eq!(
            normalized_keymap(&original).unwrap(),
            normalized_keymap(&equivalent).unwrap()
        );
        assert!(normalized_keymap(b"invalid").is_err());
    }

    fn num_keymap() -> Vec<u8> {
        let mut bytes = br#"xkb_keymap {
            xkb_keycodes "test" {
                minimum = 8; maximum = 255;
                <AD05> = 28; <LCTL> = 37; <LFSH> = 50;
                <NMLK> = 77; <KP1> = 87;
            };
            xkb_types "test" {
                virtual_modifiers NumLock;
                type "ONE_LEVEL" { modifiers = None; map[None] = Level1; };
                type "TWO_LEVEL" {
                    modifiers = Shift; map[None] = Level1; map[Shift] = Level2;
                };
                type "KEYPAD" {
                    modifiers = NumLock; map[None] = Level1; map[NumLock] = Level2;
                };
            };
            xkb_compatibility "test" {
                virtual_modifiers NumLock;
                interpret Num_Lock+AnyOfOrNone(all) {
                    virtualModifier = NumLock;
                    action = LockMods(modifiers=NumLock);
                };
            };
            xkb_symbols "test" {
                key <AD05> { type="TWO_LEVEL", [ t, T ] };
                key <LCTL> { [ Control_L ] };
                key <LFSH> { [ Shift_L ] };
                key <NMLK> { [ Num_Lock ] };
                key <KP1> { type="KEYPAD", [ KP_End, KP_1 ] };
                modifier_map Control { <LCTL> };
                modifier_map Shift { <LFSH> };
                modifier_map Mod2 { <NMLK> };
            };
        };"#
        .to_vec();
        bytes.push(0);
        Map::open(&bytes).unwrap().canonical_bytes().unwrap()
    }

    #[test]
    fn concurrent_hold_classifies_actual_map_actions_and_excludes_modifiers() {
        if Library::open(c"libxkbcommon.so.0").is_err() {
            return;
        }
        let original = num_keymap();
        for num_lock in [false, true] {
            let ordinary = ordinary_keycodes(&original, num_lock).unwrap();
            assert!(ordinary.contains(&28)); // T has no modifier action.
            assert!(ordinary.contains(&87)); // Keypad key, in either lock state.
            for code in [8, 37, 50, 77] {
                assert!(
                    !ordinary.contains(&code),
                    "unmapped/modifier/lock code {code}"
                );
            }
        }
        // A printable symbol with an explicit lock action is not ordinary.
        let custom = br#"xkb_keymap {
            xkb_keycodes "test" { minimum=8; maximum=255; <K001>=25; };
            xkb_types "test" { type "ONE_LEVEL" { modifiers=None; map[None]=Level1; }; };
            xkb_compatibility "test" {};
            xkb_symbols "test" {
                key <K001> { type="ONE_LEVEL", symbols[Group1]=[w], actions[Group1]=[LockMods(modifiers=Shift)] };
            };
        };"#;
        let mut bytes = custom.to_vec();
        bytes.push(0);
        assert!(!ordinary_keycodes(&bytes, false).unwrap().contains(&25));
    }

    #[test]
    fn num_lock_chord_keeps_keypad_meaning_after_control_is_released() {
        if Library::open(c"libxkbcommon.so.0").is_err() {
            return;
        }
        let original = num_keymap();
        let chord = locked_chord("T", &["CTRL"], &original).unwrap();
        assert_eq!(chord.code, 28);
        let map = Map::open(&original).unwrap();
        assert_eq!(chord.depressed, map.modifier(c"Control").unwrap());
        assert_eq!(chord.locked, map.modifier(c"Mod2").unwrap());
        // Verify the recipient semantics through the real XKB interpreter.
        // Clearing Num Lock, as send_shortcut did, changes this exact key.
        unsafe {
            (map.api.update_mask)(map.state, 0, 0, chord.locked, 0, 0, 0);
            assert_eq!((map.api.one_sym)(map.state, 87), 0xffb1); // KP_1
            (map.api.update_mask)(map.state, 0, 0, 0, 0, 0, 0);
            assert_eq!((map.api.one_sym)(map.state, 87), 0xff9c); // KP_End
        }
        assert_eq!(locked_chord("KP_1", &[], &original).unwrap().code, 87);
        assert_eq!(
            locked_chord("code:87", &[], &original).unwrap().locked,
            chord.locked
        );
        let shifted = locked_chord("t", &["SHIFT"], &original).unwrap();
        unsafe {
            (map.api.update_mask)(map.state, shifted.depressed, 0, shifted.locked, 0, 0, 0);
            assert_eq!((map.api.one_sym)(map.state, shifted.code), b'T' as u32);
        }
    }

    #[test]
    fn num_lock_chord_rejects_unknown_keys_and_unobservable_custom_lock_actions() {
        if Library::open(c"libxkbcommon.so.0").is_err() {
            return;
        }
        let original = num_keymap();
        for key in [
            "NoSuchKey",
            "Num_Lock",
            "code:77",
            "code:7",
            "code:256",
            "code:8",
        ] {
            assert!(locked_chord(key, &[], &original).is_err(), "{key}");
        }
        assert!(locked_chord("t", &["INVALID"], &original).is_err());
        let mut noncanonical = vec![b'\n'];
        noncanonical.extend_from_slice(&original);
        assert!(
            locked_chord("t", &[], &noncanonical)
                .unwrap_err()
                .contains("does not roundtrip identically")
        );
        let custom = String::from_utf8(original.clone())
            .unwrap()
            .replace("modifier_map Mod2", "modifier_map Mod3")
            .into_bytes();
        assert!(
            locked_chord("t", &[], &custom)
                .unwrap_err()
                .contains("custom Num Lock")
        );
        let no_action = String::from_utf8(original)
            .unwrap()
            .replace("LockMods(modifiers=NumLock)", "NoAction()")
            .into_bytes();
        assert!(locked_chord("t", &[], &no_action).is_err());
    }

    #[test]
    fn resolves_exact_map_codes_and_rejects_missing_before_dispatch() {
        let mut map = keymap::Batches::new("aA!中🙂\n\t")
            .next()
            .unwrap()
            .keymap
            .into_bytes();
        map.push(0);
        // This test uses the system parser, so callers on minimal systems
        // can run pure keymap planner tests without enabling native tests.
        if Library::open(c"libxkbcommon.so.0").is_err() {
            return;
        }
        let strokes = resolve("!中🙂", &map).unwrap();
        assert_eq!(
            strokes.iter().map(|s| s.code).collect::<Vec<_>>(),
            vec![11, 12, 13]
        );
        assert!(strokes.iter().all(|s| s.modifiers.is_empty()));
        assert!(
            resolve("aZ", &map)
                .unwrap_err()
                .starts_with("unsupported_character:")
        );
        assert!(resolve("a\u{1}", &map).is_err());
    }
}
