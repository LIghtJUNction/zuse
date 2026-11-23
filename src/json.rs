//! Small owned conveniences over Project Z's bounded JSON parser.
use zmcpio::json::{Document, Limits, Token, Value};

pub(crate) const MAX_TOKENS: usize = 65_536;

pub fn quote(text: &str) -> String {
    use zconv::json::{ControlEscapes, Writer};
    let mut sizing = Writer::new(&mut []);
    sizing.string_with_control_escapes(text.as_bytes(), ControlEscapes::Unicode);
    let needed = match sizing.finish() {
        Err(zconv::Error::OutputTooSmall { needed }) => needed,
        result => panic!("valid JSON string sizing failed: {result:?}"),
    };
    let mut output = vec![0; needed];
    let mut writer = Writer::new(&mut output);
    writer.string_with_control_escapes(text.as_bytes(), ControlEscapes::Unicode);
    writer.finish().expect("sized JSON string output");
    String::from_utf8(output).expect("validated UTF-8 JSON string")
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
    with_document(input, |document| f(document.root()))
}

pub(crate) fn token_count(input: &str) -> Result<usize, String> {
    with_document(input, |document| Ok(document.tokens_used()))
}

fn with_document<T>(
    input: &str,
    f: impl FnOnce(Document<'_>) -> Result<T, String>,
) -> Result<T, String> {
    let mut tokens = vec![Token::EMPTY; (input.len() / 2 + 8).min(MAX_TOKENS)];
    let doc = Document::parse(
        input.as_bytes(),
        &mut tokens,
        Limits {
            max_bytes: 2 * 1024 * 1024,
            max_depth: 64,
            max_members: MAX_TOKENS,
            max_string_bytes: 1024 * 1024,
        },
    )
    .map_err(|e| e.to_string())?;
    f(doc)
}

pub fn field(input: &str, key: &str) -> Result<String, String> {
    with_value(input, |v| {
        text(v.get(key).ok_or_else(|| format!("missing {key}"))?)
    })
}

pub fn error(message: &str) -> String {
    format!("{{\"error\":{}}}", quote(message))
}

#[cfg(test)]
mod shared_escape_contract {
    #[test]
    fn serialized_control_bytes_keep_the_consumer_contract() {
        let controls: String = (0u8..32).map(char::from).collect();
        let expected = r#""\u0000\u0001\u0002\u0003\u0004\u0005\u0006\u0007\u0008\t\n\u000b\u000c\r\u000e\u000f\u0010\u0011\u0012\u0013\u0014\u0015\u0016\u0017\u0018\u0019\u001a\u001b\u001c\u001d\u001e\u001f""#;
        assert_eq!(super::quote(&controls), expected);
        assert_eq!(
            super::quote("literal \\b \\f \\u0008 \" 模型🚀"),
            r#""literal \\b \\f \\u0008 \" 模型🚀""#
        );
    }
}
