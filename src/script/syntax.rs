//! Bounded syntax for scoped mouse scripts.
//!
//! Parsing has no desktop or filesystem effects. The executor must resolve
//! names, validate operation arguments and bound expanded functions/repeats
//! before creating devices or submitting input.

use crate::language::{DecodeError, Value};
use std::{collections::BTreeMap, fmt};

pub use crate::language::Span;

pub const MAX_SCRIPT_BYTES: usize = crate::language::MAX_SCRIPT_BYTES;
pub const MAX_NAME_BYTES: usize = crate::language::MAX_NAME_BYTES;
pub const MAX_STRING_BYTES: usize = crate::language::MAX_STRING_BYTES;
pub const MAX_ARGS: usize = crate::language::MAX_ARGS;
pub const MAX_TOKENS: usize = 65_536;
pub const MAX_STATEMENTS: usize = 4096;
pub const MAX_DEPTH: usize = 32;
pub const MAX_REPEAT: u32 = 4096;

#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    pub statements: Vec<Statement>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Statement {
    pub kind: StatementKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum StatementKind {
    Let {
        name: String,
        value: Expr,
    },
    Call(Call),
    Block(Vec<Statement>),
    Parallel(Vec<Statement>),
    Sequence(Vec<Statement>),
    Function {
        name: String,
        params: Vec<String>,
        body: Vec<Statement>,
    },
    Repeat {
        count: u32,
        body: Vec<Statement>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub receiver: Option<String>,
    pub name: String,
    pub args: Vec<Argument>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Argument {
    pub name: Option<String>,
    pub value: Expr,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    /// The validated decimal spelling, retained for exact integer conversion.
    Number(String),
    /// Decoded UTF-8 text, with the same escapes as the original call language.
    String(String),
    Bool(bool),
    Identifier(String),
    Call(Call),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    ScriptTooLarge,
    TooManyTokens,
    TooManyStatements,
    TooDeep,
    NameTooLong,
    StringTooLong,
    InvalidNumber,
    InvalidEscape,
    ControlCharacter,
    UnterminatedString,
    UnterminatedComment,
    UnexpectedCharacter,
    ExpectedName,
    ReservedName,
    ExpectedValue,
    ExpectedCall,
    ExpectedEquals,
    ExpectedOpenParen,
    ExpectedCloseParen,
    ExpectedOpenBrace,
    ExpectedCloseBrace,
    ExpectedSeparator,
    ExpectedCommaOrCloseParen,
    DuplicateArgument,
    PositionalAfterNamed,
    TooManyArguments,
    DuplicateParameter,
    InvalidRepeatCount,
    UnexpectedCloseBrace,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub kind: ErrorKind,
    pub span: Span,
}

impl ParseError {
    pub const fn message(&self) -> &'static str {
        match self.kind {
            ErrorKind::ScriptTooLarge => "script exceeds the source size limit",
            ErrorKind::TooManyTokens => "script exceeds the token limit",
            ErrorKind::TooManyStatements => "script exceeds the statement limit",
            ErrorKind::TooDeep => "script nesting exceeds the depth limit",
            ErrorKind::NameTooLong => "identifier exceeds the byte limit",
            ErrorKind::StringTooLong => "string exceeds the byte limit",
            ErrorKind::InvalidNumber => "invalid or nonfinite decimal number",
            ErrorKind::InvalidEscape => "invalid string escape",
            ErrorKind::ControlCharacter => "unexpected control character",
            ErrorKind::UnterminatedString => "unterminated string",
            ErrorKind::UnterminatedComment => "unterminated block comment",
            ErrorKind::UnexpectedCharacter => "unexpected character",
            ErrorKind::ExpectedName => "expected an identifier",
            ErrorKind::ReservedName => "a keyword cannot be used as a binding name",
            ErrorKind::ExpectedValue => "expected a literal, identifier or call",
            ErrorKind::ExpectedCall => "expected a function or mouse method call",
            ErrorKind::ExpectedEquals => "expected =",
            ErrorKind::ExpectedOpenParen => "expected (",
            ErrorKind::ExpectedCloseParen => "expected )",
            ErrorKind::ExpectedOpenBrace => "expected {",
            ErrorKind::ExpectedCloseBrace => "expected }",
            ErrorKind::ExpectedSeparator => "expected a newline or semicolon",
            ErrorKind::ExpectedCommaOrCloseParen => "expected , or )",
            ErrorKind::DuplicateArgument => "duplicate named argument",
            ErrorKind::PositionalAfterNamed => "positional argument follows a named argument",
            ErrorKind::TooManyArguments => "too many arguments or parameters",
            ErrorKind::DuplicateParameter => "duplicate function parameter",
            ErrorKind::InvalidRepeatCount => "repeat requires an integer from 0 through 4096",
            ErrorKind::UnexpectedCloseBrace => "unexpected }",
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} at {}:{} (byte {})",
            self.message(),
            self.span.line,
            self.span.column,
            self.span.start
        )
    }
}

impl std::error::Error for ParseError {}

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Name(String),
    Number(String),
    Text(String),
    OpenParen,
    CloseParen,
    OpenBrace,
    CloseBrace,
    Comma,
    Dot,
    Equals,
    Semicolon,
    Newline,
    End,
}

#[derive(Clone, Debug)]
struct Token {
    kind: Kind,
    span: Span,
}

struct Lexer<'a> {
    source: &'a str,
    offset: usize,
    line: usize,
    column: usize,
    module_syntax: bool,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            offset: usize::from(source.starts_with('\u{feff}')) * '\u{feff}'.len_utf8(),
            line: 1,
            column: 1,
            module_syntax: false,
        }
    }

    fn span(&self) -> Span {
        Span {
            start: self.offset,
            end: self.offset,
            line: self.line,
            column: self.column,
        }
    }

    fn error(&self, kind: ErrorKind) -> ParseError {
        ParseError {
            kind,
            span: self.span(),
        }
    }

    fn peek(&self) -> Option<char> {
        self.source[self.offset..].chars().next()
    }

    fn starts(&self, text: &str) -> bool {
        self.source[self.offset..].starts_with(text)
    }

    fn advance(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.offset += ch.len_utf8();
        if ch == '\r' {
            if self.peek() == Some('\n') {
                self.offset += 1;
            }
            self.line += 1;
            self.column = 1;
        } else if ch == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(ch)
    }

    fn at(&self, kind: ErrorKind, offset: usize) -> ParseError {
        let mut cursor = Self::new(self.source);
        while cursor.offset < offset {
            cursor.advance();
        }
        cursor.error(kind)
    }

    fn name(&mut self) -> Result<Kind, ParseError> {
        let start = self.offset;
        while self.peek().is_some_and(|c| {
            c.is_ascii_alphanumeric() || c == '_' || (self.module_syntax && c == '-')
        }) {
            self.advance();
        }
        if self.offset - start > MAX_NAME_BYTES {
            return Err(self.at(ErrorKind::NameTooLong, start));
        }
        Ok(Kind::Name(self.source[start..self.offset].to_owned()))
    }

    fn number(&mut self) -> Result<Kind, ParseError> {
        let start = self.offset;
        if self.peek() == Some('-') {
            self.advance();
        }
        match self.peek() {
            Some('0') => {
                self.advance();
            }
            Some('1'..='9') => {
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.advance();
                }
            }
            _ => return Err(self.error(ErrorKind::InvalidNumber)),
        }
        if self.peek() == Some('.') {
            self.advance();
            if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
                return Err(self.error(ErrorKind::InvalidNumber));
            }
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.advance();
            }
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            self.advance();
            if matches!(self.peek(), Some('+' | '-')) {
                self.advance();
            }
            if !self.peek().is_some_and(|c| c.is_ascii_digit()) {
                return Err(self.error(ErrorKind::InvalidNumber));
            }
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.advance();
            }
        }
        if self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.'))
        {
            return Err(self.error(ErrorKind::InvalidNumber));
        }
        let raw = &self.source[start..self.offset];
        if !raw.parse::<f64>().is_ok_and(|v| v.is_finite()) {
            return Err(self.at(ErrorKind::InvalidNumber, start));
        }
        Ok(Kind::Number(raw.to_owned()))
    }

    fn quoted(&mut self) -> Result<Kind, ParseError> {
        let quote = self
            .advance()
            .ok_or_else(|| self.error(ErrorKind::ExpectedValue))?;
        let start = self.offset;
        loop {
            let current = self
                .peek()
                .ok_or_else(|| self.error(ErrorKind::UnterminatedString))?;
            if current == quote {
                let raw = &self.source[start..self.offset];
                let value = Value::String(raw);
                let decoded = value.decoded_len().map_err(|error| match error {
                    DecodeError::InvalidEscape { offset } => {
                        self.at(ErrorKind::InvalidEscape, start + offset)
                    }
                    _ => self.at(ErrorKind::InvalidEscape, start),
                })?;
                if raw.len() > MAX_STRING_BYTES || decoded > MAX_STRING_BYTES {
                    return Err(self.at(ErrorKind::StringTooLong, start));
                }
                let mut bytes = vec![0; decoded];
                value
                    .decode_text(&mut bytes)
                    .map_err(|_| self.at(ErrorKind::InvalidEscape, start))?;
                let text = String::from_utf8(bytes)
                    .map_err(|_| self.at(ErrorKind::InvalidEscape, start))?;
                self.advance();
                return Ok(Kind::Text(text));
            }
            if current < ' ' {
                return Err(self.error(ErrorKind::ControlCharacter));
            }
            if self.offset - start >= MAX_STRING_BYTES {
                return Err(self.at(ErrorKind::StringTooLong, start));
            }
            self.advance();
            if current == '\\' {
                let escaped = self
                    .peek()
                    .ok_or_else(|| self.error(ErrorKind::UnterminatedString))?;
                if escaped < ' ' {
                    return Err(self.error(ErrorKind::ControlCharacter));
                }
                self.advance();
            }
        }
    }

    fn block_comment(&mut self) -> Result<bool, ParseError> {
        let start_line = self.line;
        self.advance();
        self.advance();
        let mut depth = 1;
        while depth > 0 {
            if self.starts("/*") {
                if depth == MAX_DEPTH {
                    return Err(self.error(ErrorKind::TooDeep));
                }
                self.advance();
                self.advance();
                depth += 1;
            } else if self.starts("*/") {
                self.advance();
                self.advance();
                depth -= 1;
            } else if self.advance().is_none() {
                return Err(self.error(ErrorKind::UnterminatedComment));
            }
        }
        Ok(self.line != start_line)
    }

    fn tokens(mut self) -> Result<(Vec<Token>, BTreeMap<usize, String>), ParseError> {
        let mut tokens = Vec::new();
        let mut descriptions = BTreeMap::new();
        let mut description = String::new();
        let mut description_line = None;
        let mut line_has_code = false;
        while let Some(ch) = self.peek() {
            let mut span = self.span();
            let kind = match ch {
                ' ' | '\t' => {
                    self.advance();
                    continue;
                }
                '\r' | '\n' => {
                    if description_line != Some(self.line) {
                        description.clear();
                        description_line = None;
                    }
                    self.advance();
                    line_has_code = false;
                    Kind::Newline
                }
                '#' => {
                    description.clear();
                    description_line = None;
                    line_has_code = true;
                    while self.peek().is_some_and(|c| !matches!(c, '\r' | '\n')) {
                        self.advance();
                    }
                    continue;
                }
                '/' if self.starts("//") => {
                    let documented = self.module_syntax
                        && !line_has_code
                        && self.starts("///")
                        && !self.starts("////");
                    let start = self.offset + 3;
                    while self.peek().is_some_and(|c| !matches!(c, '\r' | '\n')) {
                        self.advance();
                    }
                    if documented {
                        if description_line.is_some() {
                            description.push('\n');
                        }
                        description.push_str(self.source[start..self.offset].trim());
                        description_line = Some(self.line);
                    } else {
                        description.clear();
                        description_line = None;
                    }
                    line_has_code = true;
                    continue;
                }
                '/' if self.starts("/*") => {
                    description.clear();
                    description_line = None;
                    line_has_code = true;
                    if self.block_comment()? {
                        Kind::Newline
                    } else {
                        continue;
                    }
                }
                'a'..='z' | 'A'..='Z' | '_' => self.name()?,
                '-' | '0'..='9' => self.number()?,
                '"' | '\'' => self.quoted()?,
                _ => {
                    let kind = match ch {
                        '(' => Kind::OpenParen,
                        ')' => Kind::CloseParen,
                        '{' => Kind::OpenBrace,
                        '}' => Kind::CloseBrace,
                        ',' => Kind::Comma,
                        '.' => Kind::Dot,
                        '=' => Kind::Equals,
                        ';' => Kind::Semicolon,
                        _ if ch.is_control() => {
                            return Err(self.error(ErrorKind::ControlCharacter));
                        }
                        _ => return Err(self.error(ErrorKind::UnexpectedCharacter)),
                    };
                    self.advance();
                    kind
                }
            };
            if tokens.len() == MAX_TOKENS {
                return Err(ParseError {
                    kind: ErrorKind::TooManyTokens,
                    span,
                });
            }
            if !matches!(kind, Kind::Newline) {
                if matches!(&kind, Kind::Name(name) if name == "fn")
                    && description_line.is_some_and(|line| span.line == line + 1)
                    && !line_has_code
                {
                    descriptions.insert(span.start, std::mem::take(&mut description));
                } else {
                    description.clear();
                }
                description_line = None;
                line_has_code = true;
            }
            span.end = self.offset;
            tokens.push(Token { kind, span });
        }
        tokens.push(Token {
            kind: Kind::End,
            span: self.span(),
        });
        Ok((tokens, descriptions))
    }
}

struct Parser {
    tokens: Vec<Token>,
    cursor: usize,
    statements: usize,
    module_syntax: bool,
}

impl Parser {
    fn current(&self) -> &Token {
        &self.tokens[self.cursor]
    }

    fn error(&self, kind: ErrorKind) -> ParseError {
        ParseError {
            kind,
            span: self.current().span,
        }
    }

    fn is(&self, kind: &Kind) -> bool {
        &self.current().kind == kind
    }

    fn take(&mut self) -> Token {
        let token = self.current().clone();
        if token.kind != Kind::End {
            self.cursor += 1;
        }
        token
    }

    fn expect(&mut self, kind: Kind, error: ErrorKind) -> Result<Token, ParseError> {
        if self.is(&kind) {
            Ok(self.take())
        } else {
            Err(self.error(error))
        }
    }

    fn newlines(&mut self) {
        while self.is(&Kind::Newline) {
            self.take();
        }
    }

    fn after_newlines(&self, mut cursor: usize) -> Option<&Kind> {
        while self
            .tokens
            .get(cursor)
            .is_some_and(|token| token.kind == Kind::Newline)
        {
            cursor += 1;
        }
        self.tokens.get(cursor).map(|token| &token.kind)
    }

    fn separators(&mut self) {
        while matches!(self.current().kind, Kind::Newline | Kind::Semicolon) {
            self.take();
        }
    }

    fn binding_name(&mut self) -> Result<String, ParseError> {
        match &self.current().kind {
            Kind::Name(name) if reserved(name) => Err(self.error(ErrorKind::ReservedName)),
            Kind::Name(_) => match self.take().kind {
                Kind::Name(name) => Ok(name),
                _ => unreachable!(),
            },
            _ => Err(self.error(ErrorKind::ExpectedName)),
        }
    }

    fn member_name(&mut self) -> Result<String, ParseError> {
        if self.module_syntax
            && let Kind::Name(name) = &self.current().kind
        {
            let name = name.clone();
            self.take();
            Ok(name)
        } else {
            self.binding_name()
        }
    }

    fn end_span(&self, mut start: Span) -> Span {
        if self.cursor > 0 {
            start.end = self.tokens[self.cursor - 1].span.end;
        }
        start
    }

    fn check_depth(&self, depth: usize) -> Result<(), ParseError> {
        if depth > MAX_DEPTH {
            Err(self.error(ErrorKind::TooDeep))
        } else {
            Ok(())
        }
    }

    fn body(&mut self, depth: usize) -> Result<Vec<Statement>, ParseError> {
        self.check_depth(depth)?;
        self.newlines();
        self.expect(Kind::OpenBrace, ErrorKind::ExpectedOpenBrace)?;
        let statements = self.list(depth, true)?;
        self.expect(Kind::CloseBrace, ErrorKind::ExpectedCloseBrace)?;
        Ok(statements)
    }

    fn list(&mut self, depth: usize, braced: bool) -> Result<Vec<Statement>, ParseError> {
        let mut result = Vec::new();
        self.separators();
        loop {
            if self.is(&Kind::End) {
                if braced {
                    return Err(self.error(ErrorKind::ExpectedCloseBrace));
                }
                break;
            }
            if self.is(&Kind::CloseBrace) {
                if !braced {
                    return Err(self.error(ErrorKind::UnexpectedCloseBrace));
                }
                break;
            }
            result.push(self.statement(depth)?);
            self.separators();
        }
        Ok(result)
    }

    fn statement(&mut self, depth: usize) -> Result<Statement, ParseError> {
        self.check_depth(depth)?;
        if self.statements == MAX_STATEMENTS {
            return Err(self.error(ErrorKind::TooManyStatements));
        }
        self.statements += 1;
        let span = self.current().span;
        let mut compound = false;
        let kind = match &self.current().kind {
            Kind::Name(name) if name == "let" => {
                self.take();
                self.newlines();
                let name = self.binding_name()?;
                self.newlines();
                self.expect(Kind::Equals, ErrorKind::ExpectedEquals)?;
                self.newlines();
                let value = self.expression(depth + 1)?;
                StatementKind::Let { name, value }
            }
            Kind::Name(name) if name == "fn" => {
                self.take();
                self.newlines();
                let name = self.binding_name()?;
                self.newlines();
                self.expect(Kind::OpenParen, ErrorKind::ExpectedOpenParen)?;
                self.newlines();
                let mut params = Vec::new();
                while !self.is(&Kind::CloseParen) {
                    if params.len() == MAX_ARGS {
                        return Err(self.error(ErrorKind::TooManyArguments));
                    }
                    let location = self.current().span;
                    let name = self.binding_name()?;
                    if params.contains(&name) {
                        return Err(ParseError {
                            kind: ErrorKind::DuplicateParameter,
                            span: location,
                        });
                    }
                    params.push(name);
                    self.newlines();
                    if self.is(&Kind::Comma) {
                        self.take();
                        self.newlines();
                    } else if !self.is(&Kind::CloseParen) {
                        return Err(self.error(ErrorKind::ExpectedCommaOrCloseParen));
                    }
                }
                self.expect(Kind::CloseParen, ErrorKind::ExpectedCloseParen)?;
                let body = self.body(depth + 1)?;
                compound = true;
                StatementKind::Function { name, params, body }
            }
            Kind::Name(name)
                if matches!(name.as_str(), "parallel" | "sequence" | "serial")
                    && (!self.module_syntax
                        || self.after_newlines(self.cursor + 1) == Some(&Kind::OpenBrace)) =>
            {
                let keyword = match self.take().kind {
                    Kind::Name(name) => name,
                    _ => unreachable!(),
                };
                let body = self.body(depth + 1)?;
                compound = true;
                if keyword == "parallel" {
                    StatementKind::Parallel(body)
                } else {
                    StatementKind::Sequence(body)
                }
            }
            Kind::Name(name) if name == "repeat" => {
                self.take();
                self.newlines();
                let count = match &self.current().kind {
                    Kind::Number(raw) => raw
                        .parse::<u32>()
                        .ok()
                        .filter(|count| *count <= MAX_REPEAT)
                        .ok_or_else(|| self.error(ErrorKind::InvalidRepeatCount))?,
                    _ => return Err(self.error(ErrorKind::InvalidRepeatCount)),
                };
                self.take();
                let body = self.body(depth + 1)?;
                compound = true;
                StatementKind::Repeat { count, body }
            }
            Kind::OpenBrace => {
                let body = self.body(depth + 1)?;
                compound = true;
                StatementKind::Block(body)
            }
            _ => match self.expression(depth + 1)? {
                Expr::Call(call) => StatementKind::Call(call),
                _ => return Err(self.error(ErrorKind::ExpectedCall)),
            },
        };
        let span = self.end_span(span);
        if !compound
            && !matches!(
                self.current().kind,
                Kind::Semicolon | Kind::Newline | Kind::CloseBrace | Kind::End
            )
        {
            return Err(self.error(ErrorKind::ExpectedSeparator));
        }
        Ok(Statement { kind, span })
    }

    fn expression(&mut self, depth: usize) -> Result<Expr, ParseError> {
        self.check_depth(depth)?;
        let token = self.take();
        let name = match token.kind {
            Kind::Number(value) => return Ok(Expr::Number(value)),
            Kind::Text(value) => return Ok(Expr::String(value)),
            Kind::Name(name) if name == "true" => return Ok(Expr::Bool(true)),
            Kind::Name(name) if name == "false" => return Ok(Expr::Bool(false)),
            Kind::Name(name) if reserved(&name) && !self.module_syntax => {
                return Err(ParseError {
                    kind: ErrorKind::ReservedName,
                    span: token.span,
                });
            }
            Kind::Name(name) => name,
            _ => {
                return Err(ParseError {
                    kind: ErrorKind::ExpectedValue,
                    span: token.span,
                });
            }
        };
        let mut receiver = None;
        let mut name = name;
        while self.is(&Kind::Dot) {
            self.take();
            self.newlines();
            receiver = Some(match receiver {
                None => name,
                Some(prefix) => format!("{prefix}.{name}"),
            });
            name = self.member_name()?;
            if !self.module_syntax {
                break;
            }
        }
        if self.after_newlines(self.cursor) == Some(&Kind::OpenParen) {
            self.newlines();
        }
        if !self.is(&Kind::OpenParen) && (receiver.is_none() || self.module_syntax) {
            return Ok(Expr::Identifier(match receiver {
                Some(prefix) => format!("{prefix}.{name}"),
                None => name,
            }));
        }
        self.expect(Kind::OpenParen, ErrorKind::ExpectedOpenParen)?;
        let mut args: Vec<Argument> = Vec::new();
        let mut named = false;
        self.newlines();
        while !self.is(&Kind::CloseParen) {
            if args.len() == MAX_ARGS {
                return Err(self.error(ErrorKind::TooManyArguments));
            }
            let span = self.current().span;
            let argument_name = if matches!(self.current().kind, Kind::Name(_))
                && self.after_newlines(self.cursor + 1) == Some(&Kind::Equals)
            {
                let name = self.member_name()?;
                self.newlines();
                self.take();
                self.newlines();
                if args.iter().any(|arg| arg.name.as_ref() == Some(&name)) {
                    return Err(ParseError {
                        kind: ErrorKind::DuplicateArgument,
                        span,
                    });
                }
                named = true;
                Some(name)
            } else {
                if named {
                    return Err(self.error(ErrorKind::PositionalAfterNamed));
                }
                None
            };
            let value = self.expression(depth + 1)?;
            args.push(Argument {
                name: argument_name,
                value,
                span: self.end_span(span),
            });
            self.newlines();
            if self.is(&Kind::Comma) {
                self.take();
                self.newlines();
            } else if !self.is(&Kind::CloseParen) {
                return Err(self.error(ErrorKind::ExpectedCommaOrCloseParen));
            }
        }
        self.expect(Kind::CloseParen, ErrorKind::ExpectedCloseParen)?;
        Ok(Expr::Call(Call {
            receiver,
            name,
            args,
            span: self.end_span(token.span),
        }))
    }
}

fn reserved(name: &str) -> bool {
    matches!(
        name,
        "let" | "fn" | "parallel" | "sequence" | "serial" | "repeat" | "true" | "false"
    )
}

/// Parse the complete script without resolving names or performing effects.
///
/// Simple statements end at a semicolon, newline, closing brace or EOF.
/// Compound statements can be adjacent. Parallel children are separate branches;
/// an ordinary braced child groups several statements into one sequential branch.
/// ASCII binding names and decoded UTF-8 string values are supported.
pub fn parse(source: &str) -> Result<Program, ParseError> {
    parse_with_modules(source, false).map(|(program, _)| program)
}

/// Parse module declarations and flat command names without changing the
/// original scoped parser's identifier or borrowed-handle grammar.
pub fn parse_module(source: &str) -> Result<Program, ParseError> {
    parse_module_documented(source).map(|(program, _)| program)
}

/// Preserve lexical documentation without changing the public syntax tree.
pub(super) fn parse_module_documented(
    source: &str,
) -> Result<(Program, BTreeMap<usize, String>), ParseError> {
    parse_with_modules(source, true)
}

fn parse_with_modules(
    source: &str,
    module_syntax: bool,
) -> Result<(Program, BTreeMap<usize, String>), ParseError> {
    if source.len() > MAX_SCRIPT_BYTES {
        return Err(ParseError {
            kind: ErrorKind::ScriptTooLarge,
            span: Span {
                start: 0,
                end: 0,
                line: 1,
                column: 1,
            },
        });
    }
    let mut lexer = Lexer::new(source);
    lexer.module_syntax = module_syntax;
    let (tokens, descriptions) = lexer.tokens()?;
    let mut parser = Parser {
        tokens,
        cursor: 0,
        statements: 0,
        module_syntax,
    };
    Ok((
        Program {
            statements: parser.list(0, false)?,
        },
        descriptions,
    ))
}
