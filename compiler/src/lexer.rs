//! Hand-rolled lexer for the Stage-1 + Stage-2 subset of Sigil.
//!
//! Stage-1 tokens: keywords `fn`, `let`, `perform`, `import`, `return`;
//! identifiers; integer literals; string literals; punctuation
//! `{ } ( ) ; , : . !`; operator `->`; comments; version pragma.
//!
//! Stage-2 additions (plan A2 task 20):
//! - Keywords: `true`, `false`, `if`, `else`, `match`.
//! - Operators: `+ - * / % == != < > <= >= && || ! =>`. The lexer does
//!   not distinguish unary from binary `-`; the parser does.
//! - Character literals: `'x'`, with `\n`, `\t`, `\r`, `\\`, `\'` escapes.
//!
//! Stage-4 additions (plan A3 task 36):
//! - Keyword: `type`.
//! - Single-char token: `|` (bare `Pipe`). Two-char `||` still wins
//!   under longest-match; bare `|` only lexes when the next byte is
//!   not `|`. Used by `type` decls to separate constructors in
//!   `type Foo = | Ctor | Ctor(T)`.
//!
//! Stage-6 additions (plan B task 53):
//! - Keywords: `effect`, `handle`, `with`. Surface forms `effect E[T]
//!   { op: (T) -> R, ... }` (declaration) and `handle expr with { ... }`
//!   (handler expression). The attributes `resumes` and `many` from
//!   `effect E resumes: many { ... }` stay as plain identifiers and are
//!   matched contextually by `parse_effect_decl` — reserving them would
//!   break user code that legitimately wants `let resumes = 5` etc.
//!
//! Unknown characters produce `E0010` at the position of the offending byte.
//! Every token carries a `Span` back to the source.

use crate::errors::{self, CompilerError, Severity, Span};

#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    // keywords
    Fn,
    Let,
    Perform,
    Import,
    Return,
    True,
    False,
    If,
    Else,
    Match,
    // Stage-4 (plan A3 task 36).
    Type,
    // Stage-6 (plan B task 53).
    Effect,
    Handle,
    With,

    // atoms
    Ident(String),
    IntLit(i64),
    FloatLit(f64),
    StringLit(String),
    CharLit(char),

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
    // Stage-2 operators. Ordering matters for two-char lookahead:
    // longest-match wins, so `==` is recognised before bare `=`, `=>`
    // before `=`, `!=` before `!`, etc.
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,
    NotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    AndAnd,
    OrOr,
    FatArrow,
    // Stage-4 (plan A3 task 36). Single `|`; `||` lexes as `OrOr`
    // under the two-char-lookahead path above.
    Pipe,

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
                "true" => TokenKind::True,
                "false" => TokenKind::False,
                "if" => TokenKind::If,
                "else" => TokenKind::Else,
                "match" => TokenKind::Match,
                "type" => TokenKind::Type,
                "effect" => TokenKind::Effect,
                "handle" => TokenKind::Handle,
                "with" => TokenKind::With,
                _ => TokenKind::Ident(ident),
            };
            tokens.push(Token {
                kind,
                span: Span::new(file, start_line, start_col, cursor.line, cursor.col),
            });
            continue;
        }

        if c.is_ascii_digit() {
            let mut lit = cursor.take_while(|ch| ch.is_ascii_digit());
            let mut is_float = false;
            // Check for fractional part: `.` followed by a digit.
            if cursor.peek() == '.' && cursor.peek_at(1).is_some_and(|ch| ch.is_ascii_digit()) {
                is_float = true;
                lit.push('.');
                cursor.advance();
                lit.push_str(&cursor.take_while(|ch| ch.is_ascii_digit()));
            }
            // Check for exponent: `e`/`E`, optional `+`/`-`, then digit(s).
            // Only commit if at least one digit follows the exponent marker
            // (with optional sign). Otherwise leave `e` unconsumed so `1e`
            // lexes as integer `1` followed by identifier `e`.
            if cursor.peek() == 'e' || cursor.peek() == 'E' {
                let skip_sign = if cursor.peek_at(1).is_some_and(|ch| ch == '+' || ch == '-') {
                    2
                } else {
                    1
                };
                if cursor
                    .peek_at(skip_sign)
                    .is_some_and(|ch| ch.is_ascii_digit())
                {
                    is_float = true;
                    lit.push(cursor.peek());
                    cursor.advance();
                    if cursor.peek() == '+' || cursor.peek() == '-' {
                        lit.push(cursor.peek());
                        cursor.advance();
                    }
                    lit.push_str(&cursor.take_while(|ch| ch.is_ascii_digit()));
                }
            }
            let span = Span::new(file, start_line, start_col, cursor.line, cursor.col);
            if is_float {
                match lit.parse::<f64>() {
                    Ok(f) => tokens.push(Token {
                        kind: TokenKind::FloatLit(f),
                        span,
                    }),
                    Err(_) => {
                        errors.push(CompilerError::new(
                            Severity::Error,
                            errors::code("E0050"),
                            span.clone(),
                            format!("float literal `{lit}` is out of range"),
                        ));
                        tokens.push(Token {
                            kind: TokenKind::FloatLit(0.0),
                            span,
                        });
                    }
                }
            } else {
                match lit.parse::<i64>() {
                    Ok(n) => tokens.push(Token {
                        kind: TokenKind::IntLit(n),
                        span,
                    }),
                    Err(_) => {
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

        if c == '\'' {
            match cursor.take_char_lit() {
                Ok(ch) => {
                    tokens.push(Token {
                        kind: TokenKind::CharLit(ch),
                        span: Span::new(file, start_line, start_col, cursor.line, cursor.col),
                    });
                }
                Err(e) => errors.push(e),
            }
            continue;
        }

        // Two-char operator lookahead first (longest match wins). The
        // ordering below matters: `==` must be recognised before `=`,
        // `->` before `-`, etc.
        if c == '-' && cursor.peek_at(1) == Some('>') {
            cursor.advance();
            cursor.advance();
            tokens.push(Token {
                kind: TokenKind::Arrow,
                span: Span::new(file, start_line, start_col, cursor.line, cursor.col),
            });
            continue;
        }
        if c == '=' && cursor.peek_at(1) == Some('=') {
            cursor.advance();
            cursor.advance();
            tokens.push(Token {
                kind: TokenKind::EqEq,
                span: Span::new(file, start_line, start_col, cursor.line, cursor.col),
            });
            continue;
        }
        if c == '=' && cursor.peek_at(1) == Some('>') {
            cursor.advance();
            cursor.advance();
            tokens.push(Token {
                kind: TokenKind::FatArrow,
                span: Span::new(file, start_line, start_col, cursor.line, cursor.col),
            });
            continue;
        }
        if c == '!' && cursor.peek_at(1) == Some('=') {
            cursor.advance();
            cursor.advance();
            tokens.push(Token {
                kind: TokenKind::NotEq,
                span: Span::new(file, start_line, start_col, cursor.line, cursor.col),
            });
            continue;
        }
        if c == '<' && cursor.peek_at(1) == Some('=') {
            cursor.advance();
            cursor.advance();
            tokens.push(Token {
                kind: TokenKind::LtEq,
                span: Span::new(file, start_line, start_col, cursor.line, cursor.col),
            });
            continue;
        }
        if c == '>' && cursor.peek_at(1) == Some('=') {
            cursor.advance();
            cursor.advance();
            tokens.push(Token {
                kind: TokenKind::GtEq,
                span: Span::new(file, start_line, start_col, cursor.line, cursor.col),
            });
            continue;
        }
        if c == '&' && cursor.peek_at(1) == Some('&') {
            cursor.advance();
            cursor.advance();
            tokens.push(Token {
                kind: TokenKind::AndAnd,
                span: Span::new(file, start_line, start_col, cursor.line, cursor.col),
            });
            continue;
        }
        if c == '|' && cursor.peek_at(1) == Some('|') {
            cursor.advance();
            cursor.advance();
            tokens.push(Token {
                kind: TokenKind::OrOr,
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
            '+' => Some(TokenKind::Plus),
            '-' => Some(TokenKind::Minus),
            '*' => Some(TokenKind::Star),
            '/' => Some(TokenKind::Slash),
            '%' => Some(TokenKind::Percent),
            '<' => Some(TokenKind::Lt),
            '>' => Some(TokenKind::Gt),
            '|' => Some(TokenKind::Pipe),
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

    /// Consume a single-quoted character literal: one character or one
    /// recognised escape sequence. Escapes supported: `\n \t \r \\ \'`.
    /// Anything else (including empty `''`, multi-char, or unknown
    /// escape) produces an `E0010` with a span pointing at the offending
    /// bytes. Returns the closing quote position on success so the outer
    /// loop can keep advancing.
    fn take_char_lit(&mut self) -> Result<char, CompilerError> {
        let start_line = self.line;
        let start_col = self.col;
        // Consume the opening quote.
        self.advance();
        if self.at_eof() {
            let span = Span::new(self.file, start_line, start_col, self.line, self.col);
            return Err(CompilerError::new(
                Severity::Error,
                errors::code("E0010"),
                span,
                "unterminated character literal",
            ));
        }
        let ch = self.peek();
        let value = if ch == '\\' {
            // Escape sequence.
            self.advance();
            if self.at_eof() {
                let span = Span::new(self.file, start_line, start_col, self.line, self.col);
                return Err(CompilerError::new(
                    Severity::Error,
                    errors::code("E0010"),
                    span,
                    "unterminated character escape",
                ));
            }
            let esc = self.peek();
            self.advance();
            match esc {
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                '\\' => '\\',
                '\'' => '\'',
                other => {
                    let span = Span::new(self.file, start_line, start_col, self.line, self.col);
                    return Err(CompilerError::new(
                        Severity::Error,
                        errors::code("E0010"),
                        span,
                        format!("unknown character escape `\\{other}`"),
                    ));
                }
            }
        } else if ch == '\'' {
            // Empty `''` — not a valid literal.
            let span = Span::new(self.file, start_line, start_col, self.line, self.col + 1);
            self.advance();
            return Err(CompilerError::new(
                Severity::Error,
                errors::code("E0010"),
                span,
                "empty character literal",
            ));
        } else {
            self.advance();
            ch
        };
        // Require the closing quote. Multi-char literals like `'ab'` are
        // rejected here.
        if self.at_eof() || self.peek() != '\'' {
            let span = Span::new(self.file, start_line, start_col, self.line, self.col);
            return Err(CompilerError::new(
                Severity::Error,
                errors::code("E0010"),
                span,
                "expected closing `'` in character literal",
            ));
        }
        self.advance();
        Ok(value)
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

    // Plan A2 task 20 — Stage-2 token coverage.

    #[test]
    fn stage2_keywords_lex_as_keywords() {
        let (toks, errs) = lex("x.sigil", "true false if else match");
        assert!(errs.is_empty(), "{errs:?}");
        use TokenKind::*;
        let ks: Vec<TokenKind> = toks.iter().map(|t| t.kind.clone()).collect();
        assert!(ks.starts_with(&[True, False, If, Else, Match]));
    }

    #[test]
    fn arithmetic_and_comparison_operators_lex() {
        // Order matters: lookahead must pick `==` over `=`, `=>` over `=`,
        // `!=` over `!`, `<=` over `<`, `>=` over `>`, `&&`/`||`, and so on.
        let (toks, errs) = lex("x.sigil", "+ - * / % == != < > <= >= && || ! => =");
        assert!(errs.is_empty(), "{errs:?}");
        use TokenKind::*;
        let ks: Vec<TokenKind> = toks.iter().map(|t| t.kind.clone()).collect();
        let expected = [
            Plus, Minus, Star, Slash, Percent, EqEq, NotEq, Lt, Gt, LtEq, GtEq, AndAnd, OrOr, Bang,
            FatArrow, Eq, Eof,
        ];
        assert_eq!(ks, expected, "operator sequence mismatch");
    }

    #[test]
    fn arrow_still_lexes_as_single_token() {
        // `->` regression: after adding bare `-` as Minus, the Arrow
        // lookahead must still win.
        let (toks, errs) = lex("x.sigil", "fn f() -> Int ![] { 0 }");
        assert!(errs.is_empty(), "{errs:?}");
        assert!(toks.iter().any(|t| matches!(t.kind, TokenKind::Arrow)));
        assert!(!toks.iter().any(|t| matches!(t.kind, TokenKind::Minus)));
    }

    #[test]
    fn char_literal_basic() {
        let (toks, errs) = lex("x.sigil", "'a'");
        assert!(errs.is_empty(), "{errs:?}");
        assert!(matches!(toks[0].kind, TokenKind::CharLit('a')));
    }

    #[test]
    fn char_literal_escapes() {
        let (toks, errs) = lex("x.sigil", r"'\n' '\t' '\r' '\\' '\''");
        assert!(errs.is_empty(), "{errs:?}");
        let chars: Vec<char> = toks
            .iter()
            .filter_map(|t| match t.kind {
                TokenKind::CharLit(c) => Some(c),
                _ => None,
            })
            .collect();
        assert_eq!(chars, vec!['\n', '\t', '\r', '\\', '\'']);
    }

    #[test]
    fn empty_char_literal_is_e0010() {
        let (_toks, errs) = lex("x.sigil", "''");
        assert!(!errs.is_empty());
        assert_eq!(errs[0].code.as_str(), "E0010");
    }

    #[test]
    fn unterminated_char_literal_is_e0010() {
        let (_toks, errs) = lex("x.sigil", "'a");
        assert!(!errs.is_empty());
        assert_eq!(errs[0].code.as_str(), "E0010");
    }

    #[test]
    fn unknown_char_escape_is_e0010() {
        let (_toks, errs) = lex("x.sigil", r"'\q'");
        assert!(!errs.is_empty());
        assert_eq!(errs[0].code.as_str(), "E0010");
    }

    #[test]
    fn multi_char_literal_is_e0010() {
        // `'ab'` — two chars between the quotes; closing quote missing
        // at the expected position.
        let (_toks, errs) = lex("x.sigil", "'ab'");
        assert!(!errs.is_empty());
        assert_eq!(errs[0].code.as_str(), "E0010");
    }

    // ===== Plan A3 task 36 — Stage-4 tokens =========================

    #[test]
    fn type_keyword_lexes_as_keyword() {
        let (toks, errs) = lex("x.sigil", "type Foo = Bar");
        assert!(errs.is_empty(), "{errs:?}");
        use TokenKind::*;
        let ks: Vec<TokenKind> = toks.iter().map(|t| t.kind.clone()).collect();
        assert!(matches!(ks.first(), Some(Type)));
        // `Foo` is still an Ident; only the keyword is reserved.
        assert!(ks.contains(&Ident("Foo".into())));
    }

    #[test]
    fn bare_pipe_lexes_as_pipe() {
        // `|` alone, not `||`. Used to separate sum-type variants in
        // `type Name = | Ctor | Ctor(T)`.
        let (toks, errs) = lex("x.sigil", "| a | b");
        assert!(errs.is_empty(), "{errs:?}");
        use TokenKind::*;
        let ks: Vec<TokenKind> = toks.iter().map(|t| t.kind.clone()).collect();
        assert_eq!(
            ks,
            vec![Pipe, Ident("a".into()), Pipe, Ident("b".into()), Eof],
        );
    }

    #[test]
    fn double_pipe_still_lexes_as_oror() {
        // Regression: after adding bare `|` as Pipe, the OrOr
        // two-char lookahead must still win for `||`.
        let (toks, errs) = lex("x.sigil", "a || b");
        assert!(errs.is_empty(), "{errs:?}");
        use TokenKind::*;
        let ks: Vec<TokenKind> = toks.iter().map(|t| t.kind.clone()).collect();
        assert!(ks.contains(&OrOr));
        assert!(!ks.contains(&Pipe));
    }

    // ===== Plan B task 53 — Stage-6 tokens ==========================

    #[test]
    fn effect_handle_with_lex_as_keywords() {
        let (toks, errs) = lex("x.sigil", "effect handle with");
        assert!(errs.is_empty(), "{errs:?}");
        use TokenKind::*;
        let ks: Vec<TokenKind> = toks.iter().map(|t| t.kind.clone()).collect();
        assert!(ks.starts_with(&[Effect, Handle, With]));
    }

    #[test]
    fn resumes_and_many_remain_idents() {
        // Context keywords inside `effect E resumes: many { ... }`.
        // Reserving them would break user variables; the parser handles
        // them positionally via Ident-string match.
        let (toks, errs) = lex("x.sigil", "let resumes = many;");
        assert!(errs.is_empty(), "{errs:?}");
        use TokenKind::*;
        let ks: Vec<TokenKind> = toks.iter().map(|t| t.kind.clone()).collect();
        assert!(ks.contains(&Ident("resumes".into())));
        assert!(ks.contains(&Ident("many".into())));
    }

    #[test]
    fn handle_expr_skeleton_tokenises_cleanly() {
        let src = "handle e with { return(v) => v, IO.read(k) => 0 }";
        let (toks, errs) = lex("x.sigil", src);
        assert!(errs.is_empty(), "{errs:?}");
        use TokenKind::*;
        let ks: Vec<TokenKind> = toks.iter().map(|t| t.kind.clone()).collect();
        assert!(ks.contains(&Handle));
        assert!(ks.contains(&With));
        assert!(ks.contains(&Return));
    }

    #[test]
    fn effect_decl_skeleton_tokenises_cleanly() {
        let src = "effect Raise[T] { fail: (String) -> T }";
        let (toks, errs) = lex("x.sigil", src);
        assert!(errs.is_empty(), "{errs:?}");
        use TokenKind::*;
        let ks: Vec<TokenKind> = toks.iter().map(|t| t.kind.clone()).collect();
        assert!(ks.contains(&Effect));
        assert!(ks.contains(&Ident("Raise".into())));
        assert!(ks.contains(&Ident("fail".into())));
    }

    #[test]
    fn type_decl_skeleton_tokenises_cleanly() {
        // Full `type` decl skeleton: keyword, ident, `=`, pipes, idents,
        // parens, commas. No errors.
        let (toks, errs) = lex("x.sigil", "type Option = | None | Some(Int)");
        assert!(errs.is_empty(), "{errs:?}");
        use TokenKind::*;
        let ks: Vec<TokenKind> = toks.iter().map(|t| t.kind.clone()).collect();
        let expected = vec![
            Type,
            Ident("Option".into()),
            Eq,
            Pipe,
            Ident("None".into()),
            Pipe,
            Ident("Some".into()),
            LParen,
            Ident("Int".into()),
            RParen,
            Eof,
        ];
        assert_eq!(ks, expected);
    }

    #[test]
    fn float_exponent_requires_digits() {
        use TokenKind::*;
        let (toks, errs) = lex("<test>", "1e");
        assert!(errs.is_empty());
        assert_eq!(kinds(&toks), vec![IntLit(1), Ident("e".into()), Eof]);
    }

    #[test]
    fn float_exponent_sign_requires_digits() {
        use TokenKind::*;
        let (toks, errs) = lex("<test>", "1e+");
        assert!(errs.is_empty());
        assert_eq!(kinds(&toks), vec![IntLit(1), Ident("e".into()), Plus, Eof]);
    }

    #[test]
    fn float_valid_exponent_parses() {
        use TokenKind::*;
        let (toks, errs) = lex("<test>", "2e10");
        assert!(errs.is_empty());
        assert_eq!(kinds(&toks), vec![FloatLit(2e10), Eof]);
    }
}
