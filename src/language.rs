//! A bounded, allocation-free parser for zuse call scripts.
//!
//! A script is a sequence of named calls separated by a newline or `;`:
//! `mouse.move(x=120, y=80, curve="smooth"); wait(ms=40)`.
//! Arguments are named; `#` and `//` introduce line comments. The parser borrows
//! its source, validates every quoted escape, and stops after its first error.
//! Consumers should parse and validate the whole script before applying effects.

use core::fmt;

/// Largest accepted UTF-8 script (one MiB).
pub const MAX_SCRIPT_BYTES: usize = 1_048_576;
/// Maximum arguments in one call.
pub const MAX_ARGS: usize = 32;
/// Maximum identifier length in bytes.
pub const MAX_NAME_BYTES: usize = 128;
/// Maximum quoted value length before escape decoding.
pub const MAX_STRING_BYTES: usize = 65_536;

/// A location in the original script. Lines and character columns start at one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub column: usize,
}

/// A value borrowed from a validated script.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value<'a> {
    /// A decimal number; keeping the spelling avoids integer precision loss.
    Number(&'a str),
    /// Quoted contents without the delimiters. Use `decode_text` for escapes.
    String(&'a str),
    Bool(bool),
    /// An unquoted name, for example `left` or `smooth`.
    Identifier(&'a str),
}

impl<'a> Value<'a> {
    pub fn as_i64(self) -> Option<i64> {
        match self {
            Self::Number(s) => s.parse().ok(),
            _ => None,
        }
    }

    pub fn as_u64(self) -> Option<u64> {
        match self {
            Self::Number(s) => s.parse().ok(),
            _ => None,
        }
    }

    /// Convert a number only if it has a finite floating-point representation.
    pub fn as_f64(self) -> Option<f64> {
        match self {
            Self::Number(s) => s.parse::<f64>().ok().filter(|v| v.is_finite()),
            _ => None,
        }
    }

    pub const fn as_bool(self) -> Option<bool> {
        match self {
            Self::Bool(v) => Some(v),
            _ => None,
        }
    }

    /// Borrow the original text. For strings this still contains escape spelling.
    /// For user-visible input, use `decode_text` instead.
    pub const fn as_text(self) -> Option<&'a str> {
        match self {
            Self::String(s) | Self::Identifier(s) => Some(s),
            _ => None,
        }
    }

    /// Decode text into a caller-owned buffer. Insufficient capacity leaves the
    /// buffer unchanged. The result borrows the buffer, not the source script.
    pub fn decode_text(self, output: &mut [u8]) -> Result<&str, DecodeError> {
        let (raw, escapes) = match self {
            Self::String(s) => (s, true),
            Self::Identifier(s) => (s, false),
            _ => return Err(DecodeError::NotText),
        };
        let needed = if escapes {
            decoded_len(raw)?
        } else {
            raw.len()
        };
        if output.len() < needed {
            return Err(DecodeError::BufferTooSmall { needed });
        }
        if !escapes {
            output[..needed].copy_from_slice(raw.as_bytes());
        } else {
            let mut offset = 0;
            let mut written = 0;
            while offset < raw.len() {
                let (ch, consumed) = decoded_char(raw, offset)?;
                let n = ch.encode_utf8(&mut output[written..]).len();
                written += n;
                offset += consumed;
            }
        }
        core::str::from_utf8(&output[..needed])
            .map_err(|_| DecodeError::InvalidEscape { offset: 0 })
    }

    pub fn decoded_len(self) -> Result<usize, DecodeError> {
        match self {
            Self::String(s) => decoded_len(s),
            Self::Identifier(s) => Ok(s.len()),
            _ => Err(DecodeError::NotText),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    NotText,
    BufferTooSmall { needed: usize },
    InvalidEscape { offset: usize },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotText => f.write_str("value is not text"),
            Self::BufferTooSmall { needed } => write!(f, "text buffer needs {needed} bytes"),
            Self::InvalidEscape { offset } => write!(f, "invalid string escape at byte {offset}"),
        }
    }
}
impl core::error::Error for DecodeError {}

/// One named argument.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Argument<'a> {
    pub name: &'a str,
    pub value: Value<'a>,
}
impl Argument<'_> {
    const EMPTY: Self = Self {
        name: "",
        value: Value::Identifier(""),
    };
}

/// A parsed call. Storage is bounded and lives in this value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Call<'a> {
    pub name: &'a str,
    pub span: Span,
    arguments: [Argument<'a>; MAX_ARGS],
    length: usize,
}

impl<'a> Call<'a> {
    pub fn args(&self) -> &[Argument<'a>] {
        &self.arguments[..self.length]
    }
    pub fn get(&self, name: &str) -> Option<Value<'a>> {
        self.args()
            .iter()
            .find(|arg| arg.name == name)
            .map(|arg| arg.value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    ScriptTooLarge,
    NameTooLong,
    ExpectedName,
    ExpectedOpenParen,
    ExpectedArgumentName,
    ExpectedEquals,
    ExpectedValue,
    InvalidNumber,
    UnterminatedString,
    InvalidEscape,
    ControlCharacter,
    StringTooLong,
    TooManyArguments,
    DuplicateArgument,
    ExpectedCommaOrCloseParen,
    ExpectedSeparator,
}

/// A deterministic syntax error with zero-based byte offset and one-based line
/// and Unicode character column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub kind: ErrorKind,
    pub offset: usize,
    pub line: usize,
    pub column: usize,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} at {}:{} (byte {})",
            self.kind, self.line, self.column, self.offset
        )
    }
}
impl core::error::Error for ParseError {}

/// Streaming syntax parser. It performs no allocation, I/O, or command execution.
pub struct Parser<'a> {
    source: &'a str,
    offset: usize,
    line: usize,
    column: usize,
    failed: bool,
}

impl<'a> Parser<'a> {
    pub const fn new(source: &'a str) -> Self {
        Self {
            source,
            offset: 0,
            line: 1,
            column: 1,
            failed: false,
        }
    }

    fn byte(&self) -> Option<u8> {
        self.source.as_bytes().get(self.offset).copied()
    }

    fn advance(&mut self) {
        if let Some(ch) = self.source[self.offset..].chars().next() {
            self.offset += ch.len_utf8();
            if ch == '\n' {
                self.line += 1;
                self.column = 1;
            } else {
                self.column += 1;
            }
        }
    }

    fn error(&self, kind: ErrorKind) -> ParseError {
        ParseError {
            kind,
            offset: self.offset,
            line: self.line,
            column: self.column,
        }
    }

    fn at(&self, kind: ErrorKind, offset: usize) -> ParseError {
        let before = &self.source[..offset];
        let line = before.bytes().filter(|b| *b == b'\n').count() + 1;
        let start = before.rfind('\n').map_or(0, |i| i + 1);
        let column = before[start..].chars().count() + 1;
        ParseError {
            kind,
            offset,
            line,
            column,
        }
    }

    fn starts_comment(&self) -> bool {
        self.byte() == Some(b'#') || self.source[self.offset..].starts_with("//")
    }

    fn comment(&mut self) {
        while !matches!(self.byte(), None | Some(b'\n')) {
            self.advance();
        }
    }

    fn whitespace(&mut self) {
        loop {
            while matches!(self.byte(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
                self.advance();
            }
            if self.starts_comment() {
                self.comment();
            } else {
                break;
            }
        }
    }

    fn identifier(&mut self, dotted: bool, kind: ErrorKind) -> Result<&'a str, ParseError> {
        let start = self.offset;
        if !self.byte().is_some_and(is_name_start) {
            return Err(self.error(kind));
        }
        self.advance();
        loop {
            while self.byte().is_some_and(is_name_continue) {
                self.advance();
            }
            if dotted && self.byte() == Some(b'.') {
                self.advance();
                if !self.byte().is_some_and(is_name_start) {
                    return Err(self.error(kind));
                }
                self.advance();
            } else {
                break;
            }
        }
        if self.offset - start > MAX_NAME_BYTES {
            return Err(self.at(ErrorKind::NameTooLong, start));
        }
        Ok(&self.source[start..self.offset])
    }

    fn quoted(&mut self) -> Result<Value<'a>, ParseError> {
        let quote = self
            .byte()
            .ok_or_else(|| self.error(ErrorKind::ExpectedValue))?;
        self.advance();
        let start = self.offset;
        loop {
            let current = match self.byte() {
                None => return Err(self.error(ErrorKind::UnterminatedString)),
                Some(b) => b,
            };
            if current == quote {
                let raw = &self.source[start..self.offset];
                if let Err(DecodeError::InvalidEscape { offset }) = decoded_len(raw) {
                    return Err(self.at(ErrorKind::InvalidEscape, start + offset));
                }
                self.advance();
                return Ok(Value::String(raw));
            }
            if current < 0x20 {
                return Err(self.error(ErrorKind::ControlCharacter));
            }
            if self.offset - start >= MAX_STRING_BYTES {
                return Err(self.error(ErrorKind::StringTooLong));
            }
            if current == b'\\' {
                self.advance();
                match self.byte() {
                    None => return Err(self.error(ErrorKind::UnterminatedString)),
                    Some(0..=31) => return Err(self.error(ErrorKind::ControlCharacter)),
                    _ => self.advance(),
                }
            } else {
                self.advance();
            }
            if self.offset - start > MAX_STRING_BYTES {
                return Err(self.error(ErrorKind::StringTooLong));
            }
        }
    }

    fn number(&mut self) -> Result<Value<'a>, ParseError> {
        let start = self.offset;
        if self.byte() == Some(b'-') {
            self.advance();
        }
        match self.byte() {
            Some(b'0') => self.advance(),
            Some(b'1'..=b'9') => {
                self.advance();
                while self.byte().is_some_and(|b| b.is_ascii_digit()) {
                    self.advance();
                }
            }
            _ => return Err(self.error(ErrorKind::InvalidNumber)),
        }
        if self.byte() == Some(b'.') {
            self.advance();
            if !self.byte().is_some_and(|b| b.is_ascii_digit()) {
                return Err(self.error(ErrorKind::InvalidNumber));
            }
            while self.byte().is_some_and(|b| b.is_ascii_digit()) {
                self.advance();
            }
        }
        if matches!(self.byte(), Some(b'e' | b'E')) {
            self.advance();
            if matches!(self.byte(), Some(b'+' | b'-')) {
                self.advance();
            }
            if !self.byte().is_some_and(|b| b.is_ascii_digit()) {
                return Err(self.error(ErrorKind::InvalidNumber));
            }
            while self.byte().is_some_and(|b| b.is_ascii_digit()) {
                self.advance();
            }
        }
        if self
            .byte()
            .is_some_and(|b| is_name_continue(b) || b == b'.')
        {
            return Err(self.error(ErrorKind::InvalidNumber));
        }
        let raw = &self.source[start..self.offset];
        if !raw.parse::<f64>().is_ok_and(|v| v.is_finite()) {
            return Err(self.at(ErrorKind::InvalidNumber, start));
        }
        Ok(Value::Number(raw))
    }

    fn value(&mut self) -> Result<Value<'a>, ParseError> {
        match self.byte() {
            Some(b'"' | b'\'') => self.quoted(),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(b) if is_name_start(b) => {
                let value = self.identifier(true, ErrorKind::ExpectedValue)?;
                Ok(match value {
                    "true" => Value::Bool(true),
                    "false" => Value::Bool(false),
                    _ => Value::Identifier(value),
                })
            }
            _ => Err(self.error(ErrorKind::ExpectedValue)),
        }
    }

    fn call(&mut self) -> Result<Call<'a>, ParseError> {
        let mut span = Span {
            start: self.offset,
            end: self.offset,
            line: self.line,
            column: self.column,
        };
        let name = self.identifier(true, ErrorKind::ExpectedName)?;
        self.whitespace();
        if self.byte() != Some(b'(') {
            return Err(self.error(ErrorKind::ExpectedOpenParen));
        }
        self.advance();
        let mut arguments = [Argument::EMPTY; MAX_ARGS];
        let mut length = 0;
        self.whitespace();
        while self.byte() != Some(b')') {
            if length == MAX_ARGS {
                return Err(self.error(ErrorKind::TooManyArguments));
            }
            let start = self.offset;
            let arg_name = self.identifier(false, ErrorKind::ExpectedArgumentName)?;
            if arguments[..length].iter().any(|a| a.name == arg_name) {
                return Err(self.at(ErrorKind::DuplicateArgument, start));
            }
            self.whitespace();
            if self.byte() != Some(b'=') {
                return Err(self.error(ErrorKind::ExpectedEquals));
            }
            self.advance();
            self.whitespace();
            let value = self.value()?;
            arguments[length] = Argument {
                name: arg_name,
                value,
            };
            length += 1;
            self.whitespace();
            match self.byte() {
                Some(b',') => {
                    self.advance();
                    self.whitespace();
                }
                Some(b')') => break,
                _ => return Err(self.error(ErrorKind::ExpectedCommaOrCloseParen)),
            }
        }
        self.advance();
        span.end = self.offset;
        while matches!(self.byte(), Some(b' ' | b'\t' | b'\r')) {
            self.advance();
        }
        if self.starts_comment() {
            self.comment();
        }
        match self.byte() {
            Some(b';' | b'\n') => self.advance(),
            None => {}
            _ => return Err(self.error(ErrorKind::ExpectedSeparator)),
        }
        Ok(Call {
            name,
            span,
            arguments,
            length,
        })
    }
}

impl<'a> Iterator for Parser<'a> {
    type Item = Result<Call<'a>, ParseError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        if self.source.len() > MAX_SCRIPT_BYTES {
            self.failed = true;
            return Some(Err(self.error(ErrorKind::ScriptTooLarge)));
        }
        self.whitespace();
        while self.byte() == Some(b';') {
            self.advance();
            self.whitespace();
        }
        self.byte()?;
        let result = self.call();
        self.failed = result.is_err();
        Some(result)
    }
}
impl core::iter::FusedIterator for Parser<'_> {}

const fn is_name_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}
const fn is_name_continue(b: u8) -> bool {
    is_name_start(b) || b.is_ascii_digit() || b == b'-'
}

fn hex4(raw: &[u8], at: usize) -> Option<u16> {
    let digits = raw.get(at..at.checked_add(4)?)?;
    let mut result = 0_u16;
    for &digit in digits {
        result = result * 16
            + match digit {
                b'0'..=b'9' => (digit - b'0') as u16,
                b'a'..=b'f' => (digit - b'a' + 10) as u16,
                b'A'..=b'F' => (digit - b'A' + 10) as u16,
                _ => return None,
            };
    }
    Some(result)
}

fn decoded_char(raw: &str, offset: usize) -> Result<(char, usize), DecodeError> {
    let bad = || DecodeError::InvalidEscape { offset };
    let bytes = raw.as_bytes();
    let b = *bytes.get(offset).ok_or_else(bad)?;
    if b != b'\\' {
        let ch = raw
            .get(offset..)
            .and_then(|s| s.chars().next())
            .ok_or_else(bad)?;
        if ch < ' ' {
            return Err(bad());
        }
        return Ok((ch, ch.len_utf8()));
    }
    let escape = *bytes.get(offset + 1).ok_or_else(bad)?;
    let ch = match escape {
        b'"' => '"',
        b'\'' => '\'',
        b'\\' => '\\',
        b'/' => '/',
        b'n' => '\n',
        b'r' => '\r',
        b't' => '\t',
        b'b' => '\u{08}',
        b'f' => '\u{0c}',
        b'u' => {
            let high = hex4(bytes, offset + 2).ok_or_else(bad)?;
            if (0xd800..=0xdbff).contains(&high) {
                if bytes.get(offset + 6..offset + 8) != Some(b"\\u") {
                    return Err(bad());
                }
                let low = hex4(bytes, offset + 8).ok_or_else(bad)?;
                if !(0xdc00..=0xdfff).contains(&low) {
                    return Err(bad());
                }
                let value = 0x10000 + (((high as u32) - 0xd800) << 10) + (low as u32 - 0xdc00);
                return Ok((char::from_u32(value).ok_or_else(bad)?, 12));
            }
            return Ok((char::from_u32(high as u32).ok_or_else(bad)?, 6));
        }
        _ => return Err(bad()),
    };
    Ok((ch, 2))
}

fn decoded_len(raw: &str) -> Result<usize, DecodeError> {
    let mut offset = 0;
    let mut needed = 0;
    while offset < raw.len() {
        let (ch, consumed) = decoded_char(raw, offset)?;
        needed += ch.len_utf8();
        offset += consumed;
    }
    Ok(needed)
}
