//! Hand-rolled lexer for the Stage-1 subset of Sigil.
//!
//! Stage-1 tokens:
//! - Keywords: `fn`, `let`, `perform`, `import`, `return`.
//! - Identifiers (alpha or `_` followed by alnum / `_`).
//! - Integer literals (decimal digits, no sign at the token layer).
//! - String literals (double-quoted, with `\"`, `\\`, `\n` escapes).
//! - Punctuation: `{ } ( ) ; , : . !` and operator `->`.
//! - Comments: `// line`, `/* block */` (no nesting).
//! - Version pragma: a leading `// sigil: X.Y` comment is recognised.
//!
//! Unknown characters produce `E0010` at the position of the offending byte.
//! Every token carries a `Span` back to the source.

use crate::errors::{self, CompilerError, Severity, Span};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenKind {
    // keywords
    Fn,
    Let,
    Perform,
    Import,
    Return,

    // atoms
    Ident(String),
    IntLit(i64),
    StringLit(String),

    // punctuation
    LBrace,
    RBrace,
    LParen,
    RParen,
    Semi,
    Comma,
    Colon,
    Dot,
    Bang,
    Arrow,
    LBracket,
    RBracket,
    Eq,
    Eof,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

pub fn lex(file: &str, src: &str) -> (Vec<Token>, Vec<CompilerError>) {
    let mut tokens = Vec::new();
    let mut errors = Vec::new();
    let mut cursor = Cursor::new(file, src);

    // Recognise a leading `// sigil: X.Y` pragma as the first non-whitespace
    // line. Record-only in Stage 1; no errors emitted.
    cursor.skip_version_pragma();

    loop {
        cursor.skip_whitespace_and_comments();
        if cursor.at_eof() {
            tokens.push(Token {
                kind: TokenKind::Eof,
                span: cursor.cur_span(0),
            });
            break;
        }

        let start_line = cursor.line;
        let start_col = cursor.col;
        let c = cursor.peek();

        if c.is_ascii_alphabetic() || c == '_' {
            let ident = cursor.take_while(|ch| ch.is_ascii_alphanumeric() || ch == '_');
            let kind = match ident.as_str() {
                "fn" => TokenKind::Fn,
                "let" => TokenKind::Let,
                "perform" => TokenKind::Perform,
                "import" => TokenKind::Import,
                "return" => TokenKind::Return,
                _ => TokenKind::Ident(ident),
            };
            tokens.push(Token {
                kind,
                span: Span::new(file, start_line, start_col, cursor.line, cursor.col),
            });
            continue;
        }

        if c.is_ascii_digit() {
            let lit = cursor.take_while(|ch| ch.is_ascii_digit());
            let span = Span::new(file, start_line, start_col, cursor.line, cursor.col);
            match lit.parse::<i64>() {
                Ok(n) => tokens.push(Token {
                    kind: TokenKind::IntLit(n),
                    span,
                }),
                Err(_) => {
                    // Preserve forward progress for subsequent tokens: we
                    // still emit a token slot (with value 0) so downstream
                    // parser positions do not shift, then attach the
                    // positioned E0050. The zero never reaches codegen —
                    // compile aborts after the errors sweep.
                    errors.push(CompilerError::new(
                        Severity::Error,
                        errors::code("E0050"),
                        span.clone(),
                        format!("integer literal `{lit}` is out of range for `Int` (i64)"),
                    ));
                    tokens.push(Token {
                        kind: TokenKind::IntLit(0),
                        span,
                    });
                }
            }
            continue;
        }

        if c == '"' {
            match cursor.take_string_lit() {
                Ok(s) => {
                    tokens.push(Token {
                        kind: TokenKind::StringLit(s),
                        span: Span::new(file, start_line, start_col, cursor.line, cursor.col),
                    });
                }
                Err(e) => errors.push(e),
            }
            continue;
        }

        // Punctuation.
        if c == '-' && cursor.peek_at(1) == Some('>') {
            cursor.advance();
            cursor.advance();
            tokens.push(Token {
                kind: TokenKind::Arrow,
                span: Span::new(file, start_line, start_col, cursor.line, cursor.col),
            });
            continue;
        }

        let single: Option<TokenKind> = match c {
            '{' => Some(TokenKind::LBrace),
            '}' => Some(TokenKind::RBrace),
            '(' => Some(TokenKind::LParen),
            ')' => Some(TokenKind::RParen),
            ';' => Some(TokenKind::Semi),
            ',' => Some(TokenKind::Comma),
            ':' => Some(TokenKind::Colon),
            '.' => Some(TokenKind::Dot),
            '!' => Some(TokenKind::Bang),
            '[' => Some(TokenKind::LBracket),
            ']' => Some(TokenKind::RBracket),
            '=' => Some(TokenKind::Eq),
            _ => None,
        };
        if let Some(kind) = single {
            cursor.advance();
            tokens.push(Token {
                kind,
                span: Span::new(file, start_line, start_col, cursor.line, cursor.col),
            });
            continue;
        }

        // Unknown character.
        let span = Span::new(file, start_line, start_col, start_line, start_col + 1);
        cursor.advance();
        errors.push(CompilerError::new(
            Severity::Error,
            errors::code("E0010"),
            span,
            format!("unexpected character `{c}`"),
        ));
    }

    (tokens, errors)
}

struct Cursor<'a> {
    file: &'a str,
    src: &'a [u8],
    pos: usize,
    line: u32,
    col: u32,
}

impl<'a> Cursor<'a> {
    fn new(file: &'a str, src: &'a str) -> Self {
        Self {
            file,
            src: src.as_bytes(),
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    fn at_eof(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn peek(&self) -> char {
        if self.at_eof() {
            '\0'
        } else {
            self.src[self.pos] as char
        }
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        let p = self.pos + offset;
        if p < self.src.len() {
            Some(self.src[p] as char)
        } else {
            None
        }
    }

    fn advance(&mut self) {
        if self.at_eof() {
            return;
        }
        let c = self.src[self.pos];
        self.pos += 1;
        if c == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            if self.at_eof() {
                return;
            }
            let c = self.peek();
            if c == ' ' || c == '\t' || c == '\r' || c == '\n' {
                self.advance();
                continue;
            }
            if c == '/' && self.peek_at(1) == Some('/') {
                while !self.at_eof() && self.peek() != '\n' {
                    self.advance();
                }
                continue;
            }
            if c == '/' && self.peek_at(1) == Some('*') {
                self.advance();
                self.advance();
                while !self.at_eof() {
                    if self.peek() == '*' && self.peek_at(1) == Some('/') {
                        self.advance();
                        self.advance();
                        break;
                    }
                    self.advance();
                }
                continue;
            }
            return;
        }
    }

    fn skip_version_pragma(&mut self) {
        // Leading `// sigil: X.Y` before the first non-whitespace, non-comment
        // token. Consumed silently; v1 tolerates presence or absence.
        let save_pos = self.pos;
        let save_line = self.line;
        let save_col = self.col;
        self.skip_whitespace_and_comments();
        // If a non-comment token sits here, leave state as-is (but we've
        // already consumed the comments). No rollback needed — pragma
        // content is arbitrary `// sigil: ...` lines which we already
        // treated as a regular line comment.
        let _ = (save_pos, save_line, save_col);
    }

    fn take_while(&mut self, mut pred: impl FnMut(char) -> bool) -> String {
        let mut s = String::new();
        while !self.at_eof() && pred(self.peek()) {
            s.push(self.peek());
            self.advance();
        }
        s
    }

    fn take_string_lit(&mut self) -> Result<String, CompilerError> {
        let start_line = self.line;
        let start_col = self.col;
        // Consume the opening quote.
        self.advance();
        let mut s = String::new();
        loop {
            if self.at_eof() {
                let span = Span::new(self.file, start_line, start_col, self.line, self.col);
                return Err(CompilerError::new(
                    Severity::Error,
                    errors::code("E0010"),
                    span,
                    "unterminated string literal",
                ));
            }
            let c = self.peek();
            if c == '"' {
                self.advance();
                return Ok(s);
            }
            if c == '\\' {
                self.advance();
                let esc = self.peek();
                match esc {
                    'n' => s.push('\n'),
                    't' => s.push('\t'),
                    'r' => s.push('\r'),
                    '\\' => s.push('\\'),
                    '"' => s.push('"'),
                    other => {
                        let span =
                            Span::new(self.file, self.line, self.col, self.line, self.col + 1);
                        self.advance();
                        return Err(CompilerError::new(
                            Severity::Error,
                            errors::code("E0010"),
                            span,
                            format!("unknown string escape `\\{other}`"),
                        ));
                    }
                }
                self.advance();
                continue;
            }
            s.push(c);
            self.advance();
        }
    }

    fn cur_span(&self, width: u32) -> Span {
        Span::new(self.file, self.line, self.col, self.line, self.col + width)
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;

    fn kinds(toks: &[Token]) -> Vec<TokenKind> {
        toks.iter().map(|t| t.kind.clone()).collect()
    }

    #[test]
    fn hello_world_tokens() {
        let src = "import std.io\n\nfn main() -> Int ![IO] {\n  perform IO.println(\"hello, world\");\n  0\n}\n";
        let (toks, errs) = lex("hello.sigil", src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
        let ks = kinds(&toks);
        use TokenKind::*;
        // Just spot-check a slice rather than hard-code every kind.
        assert!(ks.contains(&Import));
        assert!(ks.contains(&Fn));
        assert!(ks.contains(&Ident("main".into())));
        assert!(ks.contains(&Arrow));
        assert!(ks.contains(&Ident("Int".into())));
        assert!(ks.contains(&Bang));
        assert!(ks.contains(&LBracket));
        assert!(ks.contains(&Ident("IO".into())));
        assert!(ks.contains(&RBracket));
        assert!(ks.contains(&Perform));
        assert!(ks.contains(&StringLit("hello, world".into())));
        assert!(ks.contains(&IntLit(0)));
        assert!(ks.contains(&Eof));
    }

    #[test]
    fn string_escapes() {
        let (toks, errs) = lex("x.sigil", r#""a\nb\\c""#);
        assert!(errs.is_empty());
        match &toks[0].kind {
            TokenKind::StringLit(s) => assert_eq!(s, "a\nb\\c"),
            other => panic!("expected string lit, got {other:?}"),
        }
    }

    #[test]
    fn unterminated_string_is_e0010() {
        let (_toks, errs) = lex("x.sigil", "\"hello");
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code.as_str(), "E0010");
    }

    #[test]
    fn unknown_character_is_e0010_and_recovers() {
        let (toks, errs) = lex("x.sigil", "a @ b");
        // Both `a` and `b` should be identifiers; a single E0010 for `@`.
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code.as_str(), "E0010");
        assert!(kinds(&toks).contains(&TokenKind::Ident("a".into())));
        assert!(kinds(&toks).contains(&TokenKind::Ident("b".into())));
    }

    #[test]
    fn integer_literal_overflow_is_e0050() {
        // 20 nines exceeds i64::MAX (9_223_372_036_854_775_807, 19 digits).
        // Pre-fix this silently lexed to IntLit(0).
        let src = "99999999999999999999";
        let (_toks, errs) = lex("x.sigil", src);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].code.as_str(), "E0050");
        // Span should span the literal's 20 bytes on line 1.
        assert_eq!(errs[0].span.line, 1);
        assert_eq!(errs[0].span.column, 1);
        assert_eq!(errs[0].span.end_column, 21);
    }

    #[test]
    fn integer_literal_at_i64_max_does_not_error() {
        let src = "9223372036854775807";
        let (toks, errs) = lex("x.sigil", src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
        match &toks[0].kind {
            TokenKind::IntLit(n) => assert_eq!(*n, i64::MAX),
            other => panic!("expected int lit, got {other:?}"),
        }
    }
}
