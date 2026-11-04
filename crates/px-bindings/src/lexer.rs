//! The WebIDL lexical grammar (https://webidl.spec.whatwg.org/#idl-grammar,
//! "Lexical grammar" in the appendix).
//!
//! Keywords are not a token kind: the spec's terminals such as `"interface"`
//! are spelled like identifiers, so the lexer produces `Identifier` and the
//! parser decides by text. A leading underscore escapes a keyword
//! (`_interface` is the identifier `interface`) and is stripped here, with
//! `Token::escaped` recording that it was there.

use crate::span::{Span, SyntaxError};

/// Largest source the lexer accepts. The token vector costs roughly forty
/// bytes per token, so this bounds the worst case (one token per byte) to
/// about eighty megabytes. The real DOM IDL is a few hundred kilobytes at most.
pub const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;
/// Longest identifier or number.
pub const MAX_WORD_BYTES: usize = 4096;
/// Longest string literal body.
pub const MAX_STRING_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Integer,
    Decimal,
    Identifier,
    /// A `"..."` literal; `text` excludes the quotes.
    String,
    /// Any other single character, or the three-character `...`.
    Other,
    Eof,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token<'a> {
    pub kind: TokenKind,
    pub text: &'a str,
    /// An identifier written with a leading underscore.
    pub escaped: bool,
    pub span: Span,
}

struct Lexer<'a> {
    src: &'a str,
    file: u32,
    file_name: &'a str,
    pos: usize,
    line: u32,
    col: u32,
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic()
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

impl<'a> Lexer<'a> {
    fn byte(&self, off: usize) -> Option<u8> {
        self.src.as_bytes().get(self.pos.checked_add(off)?).copied()
    }

    fn here(&self) -> Span {
        let at = u32::try_from(self.pos).unwrap_or(u32::MAX);
        Span {
            file: self.file,
            start: at,
            end: at,
            line: self.line,
            col: self.col,
        }
    }

    fn err_at(&self, span: Span, message: &str) -> SyntaxError {
        SyntaxError {
            file: self.file_name.to_owned(),
            line: span.line,
            col: span.col,
            message: message.to_owned(),
            expected: Vec::new(),
            found: String::new(),
        }
    }

    /// Consume one character, keeping line and column current.
    fn bump(&mut self) -> Option<char> {
        let c = self.src.get(self.pos..)?.chars().next()?;
        self.pos += c.len_utf8();
        if c == '\n' {
            self.line = self.line.saturating_add(1);
            self.col = 1;
        } else {
            self.col = self.col.saturating_add(1);
        }
        Some(c)
    }

    fn bump_n_ascii(&mut self, n: usize) {
        for _ in 0..n {
            self.bump();
        }
    }

    fn slice(&self, start: usize, end: usize) -> &'a str {
        self.src.get(start..end).unwrap_or("")
    }

    fn skip_trivia(&mut self) -> Result<(), SyntaxError> {
        loop {
            match (self.byte(0), self.byte(1)) {
                (Some(b' ' | b'\t' | b'\n' | b'\r'), _) => {
                    self.bump();
                }
                (Some(b'/'), Some(b'/')) => {
                    while let Some(b) = self.byte(0) {
                        if b == b'\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                (Some(b'/'), Some(b'*')) => {
                    let start = self.here();
                    self.bump_n_ascii(2);
                    loop {
                        match (self.byte(0), self.byte(1)) {
                            (Some(b'*'), Some(b'/')) => {
                                self.bump_n_ascii(2);
                                break;
                            }
                            (Some(_), _) => {
                                self.bump();
                            }
                            (None, _) => {
                                return Err(self.err_at(start, "unterminated block comment"));
                            }
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    /// First offset at or after `from` whose byte fails `pred`.
    fn scan(&self, from: usize, pred: fn(u8) -> bool) -> usize {
        let mut n = from;
        while self.byte(n).is_some_and(pred) {
            n += 1;
        }
        n
    }

    /// Length in bytes of the number at the cursor (including `lead`, the
    /// length of an optional leading minus) and whether it is a decimal.
    /// `None` when no number starts here.
    fn number(&self, lead: usize) -> Option<(usize, bool)> {
        let b0 = self.byte(lead)?;
        let int_end = self.scan(lead, |b| b.is_ascii_digit());
        let has_int = int_end > lead;
        if !has_int && !(b0 == b'.' && self.byte(lead + 1).is_some_and(|b| b.is_ascii_digit())) {
            return None;
        }
        // Hexadecimal: 0x / 0X followed by at least one hex digit.
        if b0 == b'0'
            && matches!(self.byte(lead + 1), Some(b'x' | b'X'))
            && self.byte(lead + 2).is_some_and(|b| b.is_ascii_hexdigit())
        {
            return Some((self.scan(lead + 2, |b| b.is_ascii_hexdigit()), false));
        }
        let mut end = int_end;
        let mut decimal = false;
        if self.byte(end) == Some(b'.') {
            decimal = true;
            end = self.scan(end + 1, |b| b.is_ascii_digit());
        }
        if matches!(self.byte(end), Some(b'e' | b'E')) {
            let mut e = end + 1;
            if matches!(self.byte(e), Some(b'+' | b'-')) {
                e += 1;
            }
            let e_end = self.scan(e, |b| b.is_ascii_digit());
            if e_end > e {
                decimal = true;
                end = e_end;
            }
        }
        if decimal {
            return Some((end, true));
        }
        // Integer: 0[0-7]* for a leading zero (so `08` is `0` then `8`),
        // [1-9][0-9]* otherwise.
        if b0 == b'0' {
            Some((self.scan(lead + 1, |b| (b'0'..=b'7').contains(&b)), false))
        } else {
            Some((int_end, false))
        }
    }

    fn next_token(&mut self) -> Result<Token<'a>, SyntaxError> {
        let start_span = self.here();
        let start = self.pos;
        let Some(b0) = self.byte(0) else {
            return Ok(Token {
                kind: TokenKind::Eof,
                text: "",
                escaped: false,
                span: start_span,
            });
        };
        let finish = |lx: &Self, kind, text, escaped| Token {
            kind,
            text,
            escaped,
            span: Span {
                end: u32::try_from(lx.pos).unwrap_or(u32::MAX),
                ..start_span
            },
        };

        // Numbers, with an optional leading minus.
        let lead = usize::from(b0 == b'-');
        if (b0 == b'-' || b0.is_ascii_digit() || b0 == b'.')
            && let Some((len, decimal)) = self.number(lead)
        {
            if len > MAX_WORD_BYTES {
                return Err(self.err_at(start_span, "numeric literal is too long"));
            }
            self.bump_n_ascii(len);
            let text = self.slice(start, self.pos);
            let kind = if decimal {
                TokenKind::Decimal
            } else {
                TokenKind::Integer
            };
            return Ok(finish(self, kind, text, false));
        }

        // Identifiers: [_-]?[A-Za-z][0-9A-Z_a-z-]*
        let ident_lead = usize::from(b0 == b'_' || b0 == b'-');
        if self.byte(ident_lead).is_some_and(is_ident_start) {
            let end = self.scan(ident_lead + 1, is_ident_continue);
            if end > MAX_WORD_BYTES {
                return Err(self.err_at(start_span, "identifier is too long"));
            }
            self.bump_n_ascii(end);
            let raw = self.slice(start, self.pos);
            return Ok(if b0 == b'_' {
                finish(
                    self,
                    TokenKind::Identifier,
                    raw.get(1..).unwrap_or(""),
                    true,
                )
            } else {
                finish(self, TokenKind::Identifier, raw, false)
            });
        }

        match b0 {
            b'"' => {
                self.bump();
                let body = self.pos;
                loop {
                    match self.byte(0) {
                        Some(b'"') => break,
                        Some(_) => {
                            self.bump();
                            if self.pos - body > MAX_STRING_BYTES {
                                return Err(self.err_at(start_span, "string literal is too long"));
                            }
                        }
                        None => {
                            return Err(self.err_at(start_span, "unterminated string literal"));
                        }
                    }
                }
                let text = self.slice(body, self.pos);
                self.bump();
                Ok(finish(self, TokenKind::String, text, false))
            }
            0 => Err(self.err_at(start_span, "NUL character in source")),
            b'.' if self.byte(1) == Some(b'.') && self.byte(2) == Some(b'.') => {
                self.bump_n_ascii(3);
                Ok(finish(self, TokenKind::Other, "...", false))
            }
            _ => {
                self.bump();
                let text = self.slice(start, self.pos);
                Ok(finish(self, TokenKind::Other, text, false))
            }
        }
    }
}

/// Lex `source` completely, ending with one `Eof` token.
pub fn lex<'a>(
    file: u32,
    file_name: &'a str,
    source: &'a str,
) -> Result<Vec<Token<'a>>, SyntaxError> {
    let mut lx = Lexer {
        src: source,
        file,
        file_name,
        pos: 0,
        line: 1,
        col: 1,
    };
    if source.len() > MAX_SOURCE_BYTES {
        return Err(lx.err_at(
            lx.here(),
            &format!(
                "source is {} bytes; the limit is {MAX_SOURCE_BYTES}",
                source.len()
            ),
        ));
    }
    let mut out = Vec::new();
    loop {
        lx.skip_trivia()?;
        let t = lx.next_token()?;
        let eof = t.kind == TokenKind::Eof;
        out.push(t);
        if eof {
            return Ok(out);
        }
    }
}
