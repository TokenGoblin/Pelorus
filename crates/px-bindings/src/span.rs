//! Source positions and the one error type the front end reports.

use std::fmt;

/// A half-open byte range in one source file plus the line/column of its start.
///
/// Lines and columns are 1-based; a column counts Unicode scalar values, not
/// bytes, so an error under a non-ASCII character points where an editor does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    /// Index of the file in the order the caller supplied the sources.
    pub file: u32,
    pub start: u32,
    pub end: u32,
    pub line: u32,
    pub col: u32,
}

impl Span {
    /// The span covering `self` through `other` (which must come later).
    pub fn to(self, other: Span) -> Span {
        Span {
            end: other.end.max(self.end),
            ..self
        }
    }
}

/// A lexing or parsing failure, positioned.
///
/// `expected` is the list of things the grammar would have accepted at this
/// point (token spellings or production names); it is empty when the failure
/// is not an "unexpected token" (an unterminated comment, a limit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxError {
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub message: String,
    pub expected: Vec<String>,
    pub found: String,
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}: error: {}",
            self.file, self.line, self.col, self.message
        )?;
        if !self.expected.is_empty() {
            write!(f, " (expected {}", self.expected.join(" or "))?;
            if !self.found.is_empty() {
                write!(f, ", found {}", self.found)?;
            }
            write!(f, ")")?;
        }
        Ok(())
    }
}

impl std::error::Error for SyntaxError {}
