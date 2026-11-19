//! Resolve targeted text against the compositor's actual XKB map.
//! No keyboard, keymap or compositor configuration is changed by this module.
use super::{Result, keymap};
use crate::platform::dylib::Library;
use std::{
    collections::BTreeMap,
    ffi::{c_char, c_int, c_void},
    ptr,
};

type Handle = *mut c_void;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Stroke {
    pub code: u32,
    pub modifiers: &'static str,
}
struct Api {
    context_new: unsafe extern "C" fn(c_int) -> Handle,
    context_unref: unsafe extern "C" fn(Handle),
    keymap_new: unsafe extern "C" fn(Handle, *const c_char, c_int, c_int) -> Handle,
    keymap_unref: unsafe extern "C" fn(Handle),
    state_new: unsafe extern "C" fn(Handle) -> Handle,
    state_unref: unsafe extern "C" fn(Handle),
    mod_index: unsafe extern "C" fn(Handle, *const c_char) -> u32,
    update_mask: unsafe extern "C" fn(Handle, u32, u32, u32, u32, u32, u32) -> u32,
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
                state_new: library.symbol(c"xkb_state_new")?,
                state_unref: library.symbol(c"xkb_state_unref")?,
                mod_index: library.symbol(c"xkb_keymap_mod_get_index")?,
                update_mask: library.symbol(c"xkb_state_update_mask")?,
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

#[cfg(test)]
mod tests {
    use super::*;
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
            vec![10, 11, 12]
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
