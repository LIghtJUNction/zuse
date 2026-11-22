//! Bounded, self-contained XKB plans for literal Unicode text.
//!
//! Call validate on the complete input before creating any virtual keyboard or
//! injecting any batch. Each batch is a temporary map: its codes are XKB codes;
//! the Wayland virtual-keyboard wire protocol uses code - 8.

use std::fmt::Write;
use std::iter::{FusedIterator, Peekable};
use std::str::Chars;

const MAX_TEXT_BYTES: usize = 1024 * 1024;
const MAX_BATCH_CHARACTERS: usize = 1024;

// XKB 8 becomes evdev KEY_RESERVED (0). Older Chromium Wayland clients
// discard that code, and also discard codes without a DomCode mapping before
// consulting the custom keymap. Use the nonzero XKB entries at or below 255 in
// Chromium's common mapping table; its gaps must not become text slots.
// https://chromium.googlesource.com/chromium/src/+/78141ba63b3e66e6c934c3a7e95cdb19b53a41c7/ui/events/keycodes/dom/dom_code_data.inc
const TEXT_KEYCODES: &[u32] = &[
    9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32,
    33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56,
    57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80,
    81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 93, 94, 95, 96, 97, 98, 99, 100, 101, 102, 104,
    105, 106, 107, 108, 110, 111, 112, 113, 114, 115, 116, 117, 118, 119, 121, 122, 123, 124, 125,
    126, 127, 128, 129, 130, 131, 132, 133, 134, 135, 136, 137, 139, 140, 141, 142, 143, 144, 145,
    146, 148, 150, 151, 152, 158, 160, 163, 164, 166, 167, 169, 171, 172, 173, 174, 175, 176, 177,
    179, 180, 181, 182, 187, 188, 189, 190, 191, 192, 193, 194, 195, 196, 197, 198, 199, 200, 201,
    202, 209, 212, 214, 215, 216, 217, 218, 225, 232, 233, 235, 236, 237, 238, 239, 240, 241, 242,
    243, 252,
];
const MAX_DISTINCT_CHARACTERS: usize = TEXT_KEYCODES.len();

/// Validate the entire input before the first externally visible effect.
///
/// Tab, LF, and CR are accepted and become Tab/Return keysyms. Other Unicode
/// control characters (the Cc category recognized by char::is_control) are
/// rejected. Unicode formatting characters are ordinary scalar text here.
pub(super) fn validate(text: &str) -> Result<(), String> {
    if text.len() > MAX_TEXT_BYTES {
        return Err("Wayland text exceeds the 1 MiB UTF-8 limit".into());
    }
    for (offset, character) in text.char_indices() {
        if character.is_control() && !matches!(character, '\t' | '\n' | '\r') {
            return Err(format!(
                "unsupported control U+{:04X} at byte {offset}; text permits only Tab, LF and CR",
                character as u32
            ));
        }
    }
    Ok(())
}

/// One independent temporary keymap and the XKB codes to send in source order.
#[derive(Debug)]
pub(super) struct Batch {
    /// Self-contained XKB text. Append a NUL byte when transferring it by FD.
    pub keymap: String,
    /// Enumerated XKB codes in 9..=255; send code - 8 through virtual-keyboard-v1.
    pub codes: Vec<u32>,
}

/// Streams an already validated UTF-8 string without retaining prior batches.
///
/// A batch holds at most 1024 input characters and 191 distinct characters.
/// The caller must call validate once on the full source before execution;
/// iteration alone is a planner and deliberately has no external effects.
pub(super) struct Batches<'a> {
    remaining: Peekable<Chars<'a>>,
}

impl<'a> Batches<'a> {
    pub(super) fn new(text: &'a str) -> Self {
        Self {
            remaining: text.chars().peekable(),
        }
    }
}

impl Iterator for Batches<'_> {
    type Item = Batch;

    fn next(&mut self) -> Option<Self::Item> {
        self.remaining.peek()?;
        let mut characters = ['\0'; MAX_DISTINCT_CHARACTERS];
        let mut distinct = 0;
        let mut codes = Vec::with_capacity(MAX_BATCH_CHARACTERS);

        while codes.len() < MAX_BATCH_CHARACTERS {
            let Some(&character) = self.remaining.peek() else {
                break;
            };
            let slot = match characters[..distinct]
                .iter()
                .position(|&existing| existing == character)
            {
                Some(slot) => slot,
                None if distinct < MAX_DISTINCT_CHARACTERS => {
                    let slot = distinct;
                    characters[slot] = character;
                    distinct += 1;
                    slot
                }
                None => break,
            };
            self.remaining.next();
            codes.push(TEXT_KEYCODES[slot]);
        }

        Some(Batch {
            keymap: keymap(&characters[..distinct]),
            codes,
        })
    }
}

impl FusedIterator for Batches<'_> {}

fn keymap(characters: &[char]) -> String {
    let mut map = String::with_capacity(400 + characters.len() * 110);
    map.push_str("xkb_keymap {\nxkb_keycodes \"zuse\" {\nminimum = 9;\nmaximum = 255;\n");
    for (index, code) in TEXT_KEYCODES.iter().take(characters.len()).enumerate() {
        // Formatting into a String cannot return a formatting error.
        let _ = writeln!(map, "<K{index:03}> = {code};");
    }
    map.push_str(
        "};\nxkb_types \"zuse\" {\n\
         type \"ONE_LEVEL\" {\n\
         modifiers = None;\n\
         map[None] = Level1;\n\
         level_name[Level1] = \"Any\";\n\
         };\n\
         };\nxkb_compatibility \"zuse\" {};\n\
         xkb_symbols \"zuse\" {\n\
         name[Group1] = \"zuse\";\n",
    );
    for (index, &character) in characters.iter().enumerate() {
        let _ = write!(
            map,
            "key <K{index:03}> {{ type[Group1] = \"ONE_LEVEL\", symbols[Group1] = [ "
        );
        match character {
            '\n' | '\r' => map.push_str("Return"),
            '\t' => map.push_str("Tab"),
            _ => {
                let _ = write!(map, "U{:04X}", character as u32);
            }
        }
        map.push_str(" ] };\n");
    }
    map.push_str("};\n};\n");
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_unicode_and_the_three_text_controls() {
        assert!(validate("Zuse 42! 中文 😀✓\n\r\t\u{200b}\u{10ffff}").is_ok());
    }

    #[test]
    fn rejects_every_other_c0_c1_control_and_reports_late_errors() {
        for number in 0..=0x9f {
            let character = char::from_u32(number).unwrap();
            if character.is_control() && !matches!(character, '\t' | '\n' | '\r') {
                assert!(validate(&character.to_string()).is_err(), "U+{number:04X}");
            }
        }
        let source = "中".repeat(2048) + "\u{1b}";
        let error = validate(&source).unwrap_err();
        assert!(error.contains("U+001B"));
        assert!(error.contains("byte 6144"));
    }

    #[test]
    fn byte_limit_is_checked_before_any_batches_are_executed() {
        assert!(validate(&"x".repeat(MAX_TEXT_BYTES)).is_ok());
        assert!(validate(&"x".repeat(MAX_TEXT_BYTES + 1)).is_err());
        assert!(validate(&"😀".repeat(MAX_TEXT_BYTES / 4)).is_ok());
        assert!(validate(&"😀".repeat(MAX_TEXT_BYTES / 4 + 1)).is_err());
    }

    #[test]
    fn empty_input_has_no_batches_and_iteration_is_fused() {
        let mut batches = Batches::new("");
        assert!(batches.next().is_none());
        assert!(batches.next().is_none());
    }

    #[test]
    fn repeats_keep_codes_and_source_order() {
        let batch = Batches::new("中😀中\n\t\r!\"\\").next().unwrap();
        assert_eq!(batch.codes, [9, 10, 9, 11, 12, 13, 14, 15, 16]);
        assert!(batch.keymap.contains("U4E2D"));
        assert!(batch.keymap.contains("U1F600"));
        assert!(batch.keymap.contains("U0022"));
        assert!(batch.keymap.contains("U005C"));
        assert_eq!(batch.keymap.matches("[ Return ]").count(), 2);
        assert_eq!(batch.keymap.matches("[ Tab ]").count(), 1);
        assert!(!batch.keymap.contains("include"));
    }

    #[test]
    fn batch_limit_counts_unicode_characters_not_bytes() {
        let source = "😀".repeat(MAX_BATCH_CHARACTERS + 1);
        let batches: Vec<_> = Batches::new(&source).collect();
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].codes, vec![9; MAX_BATCH_CHARACTERS]);
        assert_eq!(batches[1].codes, [9]);
    }

    #[test]
    fn reserved_and_unmapped_codes_are_never_allocated_to_text() {
        assert_eq!(TEXT_KEYCODES.first(), Some(&9));
        assert_eq!(TEXT_KEYCODES.len(), 191);
        assert!(TEXT_KEYCODES.windows(2).all(|codes| codes[0] < codes[1]));
        assert!(TEXT_KEYCODES.iter().all(|code| (9..=255).contains(code)));
        // These are XKB codes, not evdev codes: the first omitted holes are
        // evdev 84, 95, 101 and 112. No custom symbol fixes a missing DomCode.
        for code in [8, 92, 103, 109, 120, 138, 147, 255] {
            assert!(!TEXT_KEYCODES.contains(&code));
        }
        let batch = Batches::new("zuse").next().unwrap();
        assert_eq!(batch.codes, [9, 10, 11, 12]);
        assert!(batch.keymap.contains("<K000> = 9;"));
        assert!(!batch.keymap.contains(" = 8;"));
    }

    #[test]
    fn distinct_limit_preserves_order_across_sparse_code_pool_and_next_batch() {
        let source: String = (0..MAX_DISTINCT_CHARACTERS + 1)
            .map(|offset| char::from_u32(0x4e00 + offset as u32).unwrap())
            .collect();
        let batches: Vec<_> = Batches::new(&source).collect();
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].codes.as_slice(), TEXT_KEYCODES);
        assert_eq!(batches[1].codes, [9]);
        for (index, code) in TEXT_KEYCODES.iter().enumerate() {
            assert!(
                batches[0]
                    .keymap
                    .contains(&format!("<K{index:03}> = {code};"))
            );
            assert!(batches[0].keymap.contains(&format!(
                "key <K{index:03}> {{ type[Group1] = \"ONE_LEVEL\", symbols[Group1] = [ U{:04X} ] }};",
                0x4e00 + index as u32,
            )));
        }
        assert!(
            batches[1]
                .keymap
                .contains(&format!("U{:04X}", 0x4e00 + MAX_DISTINCT_CHARACTERS as u32))
        );
    }

    #[test]
    fn repeats_after_distinct_limit_still_fit_until_the_next_new_character() {
        let mut source: String = (0..MAX_DISTINCT_CHARACTERS)
            .map(|offset| char::from_u32(0x4e00 + offset as u32).unwrap())
            .collect();
        source.push_str(&"一".repeat(7));
        source.push(char::from_u32(0x4e00 + MAX_DISTINCT_CHARACTERS as u32).unwrap());
        source.push_str("一一");
        let batches: Vec<_> = Batches::new(&source).collect();
        assert_eq!(batches.len(), 2);
        assert_eq!(&batches[0].codes[MAX_DISTINCT_CHARACTERS..], &[9; 7]);
        assert_eq!(batches[1].codes, [9, 10, 10]);
        assert_eq!(
            batches.iter().map(|batch| batch.codes.len()).sum::<usize>(),
            MAX_DISTINCT_CHARACTERS + 10
        );
    }
}
