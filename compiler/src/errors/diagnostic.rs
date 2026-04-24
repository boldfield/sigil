//! Diagnostics emitter — JSON Lines (default) or human-readable on stderr.
//!
//! Every user-visible error flows through `CompilerError` and out through a
//! `DiagnosticEmitter`. Construction requires an `ErrorCode` registered in
//! the catalog (`CompilerError::new` takes `ErrorCode`, not `&str`), so it
//! is structurally impossible to emit an error without a stable code.

use std::io::{self, Write};

use super::catalog::ErrorCode;

/// Byte-offset source span inside a single file. Line/column pairs are
/// 1-based, matching editor conventions. Both ends are inclusive at the
/// start and exclusive at the end.
///
/// `Ord`/`PartialOrd` are derived so `Span` can key a `BTreeMap` —
/// Plan A3 task 41.2 keys the `CheckedProgram.match_scrut_tys` map by
/// match-expression span to thread scrutinee types from typecheck into
/// codegen. Ordering is lexicographic over the derived field order
/// (`file`, then line/column quadruple); stability is incidental, the
/// map usage does not rely on any particular order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Span {
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

impl Span {
    pub fn new(
        file: impl Into<String>,
        line: u32,
        column: u32,
        end_line: u32,
        end_column: u32,
    ) -> Self {
        Self {
            file: file.into(),
            line,
            column,
            end_line,
            end_column,
        }
    }

    pub fn synthetic(file: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            line: 1,
            column: 1,
            end_line: 1,
            end_column: 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

#[derive(Clone, Debug)]
pub struct CompilerError {
    pub severity: Severity,
    pub code: ErrorCode,
    pub span: Span,
    pub message: String,
    pub hint: Option<String>,
}

impl CompilerError {
    pub fn new(
        severity: Severity,
        code: ErrorCode,
        span: Span,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity,
            code,
            span,
            message: message.into(),
            hint: None,
        }
    }

    pub fn error(code: ErrorCode, span: Span, message: impl Into<String>) -> Self {
        Self::new(Severity::Error, code, span, message)
    }

    pub fn warning(code: ErrorCode, span: Span, message: impl Into<String>) -> Self {
        Self::new(Severity::Warning, code, span, message)
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorFormat {
    JsonLines,
    Human,
}

pub struct DiagnosticEmitter<W: Write> {
    writer: W,
    format: ErrorFormat,
}

impl<W: Write> DiagnosticEmitter<W> {
    pub fn new(writer: W, format: ErrorFormat) -> Self {
        Self { writer, format }
    }

    pub fn emit(&mut self, err: &CompilerError) -> io::Result<()> {
        match self.format {
            ErrorFormat::JsonLines => self.emit_json(err),
            ErrorFormat::Human => self.emit_human(err),
        }
    }

    fn emit_json(&mut self, err: &CompilerError) -> io::Result<()> {
        let hint_field = match &err.hint {
            Some(h) => format!(",\"hint\":\"{}\"", json_escape(h)),
            None => String::from(",\"hint\":null"),
        };
        writeln!(
            self.writer,
            "{{\"level\":\"{}\",\"code\":\"{}\",\"file\":\"{}\",\"line\":{},\"column\":{},\"end_line\":{},\"end_column\":{},\"message\":\"{}\"{}}}",
            err.severity.as_str(),
            err.code,
            json_escape(&err.span.file),
            err.span.line,
            err.span.column,
            err.span.end_line,
            err.span.end_column,
            json_escape(&err.message),
            hint_field,
        )
    }

    fn emit_human(&mut self, err: &CompilerError) -> io::Result<()> {
        writeln!(
            self.writer,
            "{}[{}]: {}",
            err.severity.as_str(),
            err.code,
            err.message,
        )?;
        writeln!(
            self.writer,
            "  --> {}:{}:{}",
            err.span.file, err.span.line, err.span.column,
        )?;
        if let Some(hint) = &err.hint {
            writeln!(self.writer, "  = hint: {}", hint)?;
        }
        Ok(())
    }
}

/// Minimal JSON string escape — we emit JSON Lines by hand rather than pull
/// in serde_json. The input set is the small set of fields we emit and they
/// are strings of compiler-controlled origin (codes, filenames, messages).
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;

    fn code(s: &str) -> ErrorCode {
        match ErrorCode::new(s) {
            Some(c) => c,
            None => panic!("test setup: unknown code {s}"),
        }
    }

    #[test]
    fn json_emit_shape() {
        let err = CompilerError::error(
            code("E0010"),
            Span::new("hello.sigil", 3, 5, 3, 9),
            "unexpected token `@`",
        );
        let mut buf = Vec::new();
        let mut em = DiagnosticEmitter::new(&mut buf, ErrorFormat::JsonLines);
        em.emit(&err).expect("emit");
        let line = String::from_utf8(buf).expect("utf8");
        assert_eq!(
            line,
            "{\"level\":\"error\",\"code\":\"E0010\",\"file\":\"hello.sigil\",\"line\":3,\"column\":5,\"end_line\":3,\"end_column\":9,\"message\":\"unexpected token `@`\",\"hint\":null}\n"
        );
    }

    #[test]
    fn json_emit_escapes_and_hints() {
        let err = CompilerError::error(
            code("E0020"),
            Span::new("ok.sigil", 1, 1, 1, 2),
            "redefinition of `x`\"\\\n",
        )
        .with_hint("rename to `x_next`");
        let mut buf = Vec::new();
        let mut em = DiagnosticEmitter::new(&mut buf, ErrorFormat::JsonLines);
        em.emit(&err).expect("emit");
        let line = String::from_utf8(buf).expect("utf8");
        assert!(line.contains("\"message\":\"redefinition of `x`\\\"\\\\\\n\""));
        assert!(line.contains("\"hint\":\"rename to `x_next`\""));
        assert!(line.ends_with('\n'));
    }

    #[test]
    fn human_emit_shape() {
        let err = CompilerError::error(code("E0010"), Span::new("f.sigil", 2, 3, 2, 4), "oops")
            .with_hint("try something");
        let mut buf = Vec::new();
        let mut em = DiagnosticEmitter::new(&mut buf, ErrorFormat::Human);
        em.emit(&err).expect("emit");
        let s = String::from_utf8(buf).expect("utf8");
        assert!(s.contains("error[E0010]: oops"));
        assert!(s.contains("--> f.sigil:2:3"));
        assert!(s.contains("hint: try something"));
    }
}
