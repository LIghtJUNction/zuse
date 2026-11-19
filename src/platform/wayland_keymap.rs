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
const MAX_DISTINCT_CHARACTERS: usize = 248;

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
    /// XKB codes in 8..=255; send code - 8 through virtual-keyboard-v1.
    pub codes: Vec<u32>,
}

/// Streams an already validated UTF-8 string without retaining prior batches.
///
/// A batch holds at most 1024 input characters and 248 distinct characters.
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
            codes.push(slot as u32 + 8);
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
    map.push_str("xkb_keymap {\nxkb_keycodes \"zuse\" {\nminimum = 8;\nmaximum = 255;\n");
    for index in 0..characters.len() {
        // Formatting into a String cannot return a formatting error.
        let _ = writeln!(map, "<K{index:03}> = {};", index + 8);
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
        assert_eq!(batch.codes, [8, 9, 8, 10, 11, 12, 13, 14, 15]);
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
        assert_eq!(batches[0].codes, vec![8; MAX_BATCH_CHARACTERS]);
        assert_eq!(batches[1].codes, [8]);
    }

    #[test]
    fn distinct_limit_uses_entire_xkb_code_range_without_dropping_next_char() {
        let source: String = (0..249)
            .map(|offset| char::from_u32(0x4e00 + offset).unwrap())
            .collect();
        let batches: Vec<_> = Batches::new(&source).collect();
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].codes, (8..=255).collect::<Vec<_>>());
        assert_eq!(batches[1].codes, [8]);
        assert!(batches[0].keymap.contains("<K247> = 255;"));
        assert!(batches[1].keymap.contains("U4EF8"));
    }

    #[test]
    fn repeats_after_distinct_limit_still_fit_until_the_next_new_character() {
        let mut source: String = (0..248)
            .map(|offset| char::from_u32(0x4e00 + offset).unwrap())
            .collect();
        source.push_str(&"一".repeat(7));
        source.push('\u{4ef8}');
        let batches: Vec<_> = Batches::new(&source).collect();
        assert_eq!(batches.len(), 2);
        assert_eq!(&batches[0].codes[248..], &[8; 7]);
        assert_eq!(batches[1].codes, [8]);
        assert_eq!(
            batches.iter().map(|batch| batch.codes.len()).sum::<usize>(),
            256
        );
    }
}
