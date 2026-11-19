//! Small owned conveniences over Project Z's bounded JSON parser.
use std::fmt::Write;
use zmcpio::json::{Document, Limits, Token, Value};

pub fn quote(text: &str) -> String {
    let mut s = String::with_capacity(text.len() + 2);
    s.push('"');
    for c in text.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            '\r' => s.push_str("\\r"),
            '\t' => s.push_str("\\t"),
            c if c < ' ' => {
                let _ = write!(s, "\\u{:04x}", c as u32);
            }
            c => s.push(c),
        }
    }
    s.push('"');
    s
}

pub fn text(value: Value<'_>) -> Result<String, String> {
    value
        .string()
        .map(|s| s.chars().collect())
        .ok_or_else(|| "expected a string".into())
}

pub fn with_value<T>(
    input: &str,
    f: impl FnOnce(Value<'_>) -> Result<T, String>,
) -> Result<T, String> {
    let mut tokens = vec![Token::EMPTY; (input.len() / 2 + 8).min(65536)];
    let doc = Document::parse(
        input.as_bytes(),
        &mut tokens,
        Limits {
            max_bytes: 2 * 1024 * 1024,
            max_depth: 64,
            max_members: 65536,
            max_string_bytes: 1024 * 1024,
        },
    )
    .map_err(|e| e.to_string())?;
    f(doc.root())
}

pub fn field(input: &str, key: &str) -> Result<String, String> {
    with_value(input, |v| {
        text(v.get(key).ok_or_else(|| format!("missing {key}"))?)
    })
}

pub fn error(message: &str) -> String {
    format!("{{\"error\":{}}}", quote(message))
}
