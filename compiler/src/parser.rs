//! Recursive-descent parser for the Stage-1 + Stage-2 subset.
//!
//! Grammar handled:
//!
//! ```text
//! program   := (import | fn_decl)*
//! import    := 'import' ident ('.' ident)* ';'?
//! fn_decl   := 'fn' ident '(' param_list? ')' '->' type '!' '[' effect_list ']' block
//! param     := ident ':' type
//! type      := ident
//! block     := '{' stmt* tail? '}'
//! stmt      := let_stmt ';' | expr ';' | perform_expr ';'
//! let_stmt  := 'let' ident ':' type '=' expr
//!
//! # Stage 2 expression grammar (plan A2 task 21). Precedence is encoded
//! # via the Pratt-style `parse_expr_prec` loop; higher binds tighter.
//! # All binary operators are left-associative.
//! expr         := or_expr
//! or_expr      := and_expr ('||' and_expr)*           # prec 1
//! and_expr     := eq_expr  ('&&' eq_expr)*            # prec 2
//! eq_expr      := cmp_expr (('==' | '!=') cmp_expr)*  # prec 3
//! cmp_expr     := add_expr (('<' | '>' | '<=' | '>=') add_expr)*  # prec 4
//! add_expr     := mul_expr (('+' | '-') mul_expr)*    # prec 5
//! mul_expr     := unary    (('*' | '/' | '%') unary)* # prec 6
//! unary        := ('-' | '!') unary | postfix
//! postfix      := primary ('(' arg_list? ')')*
//! primary      := int_lit | string_lit | char_lit | bool_lit | ident
//!               | perform_expr | '(' expr ')' | if_expr | match_expr
//!               | lambda_expr
//! if_expr      := 'if' expr block 'else' block
//! match_expr   := 'match' expr '{' match_arm (',' match_arm)* ','? '}'
//! match_arm    := pattern '=>' expr
//! pattern      := '-'? int_lit | bool_lit | char_lit | '_'
//! perform_expr := 'perform' ident '.' ident '(' arg_list? ')'
//!
//! # Stage 3 grammar additions (plan A2 task 29).
//! lambda_expr  := 'fn' '(' param_list? ')' '->' type '!' '[' effect_list ']' '=>' expr
//! # (Multi-arg function declarations and function-call expressions
//! # with arguments were already admissible under the Plan-A1 grammar
//! # above: `fn_decl` uses `param_list?` and `postfix` accepts
//! # `'(' arg_list? ')'` chains. Task 29 only introduces `lambda_expr`
//! # as a new grammar form.)
//! ```
//!
//! `-<integer-literal>` is constant-folded at parse time into a single
//! `Expr::IntLit(-n, ..)` node rather than `Unary(Neg, IntLit(n, ..))`.
//!
//! Error recovery: on a syntax error the parser synchronises to the next
//! `;` or matching `}` and continues. All errors from one run surface in
//! the same compile invocation.

use crate::ast::*;
use crate::errors::{self, CompilerError, Severity, Span};
use crate::lexer::{Token, TokenKind};

pub fn parse(file: &str, tokens: &[Token]) -> (Program, Vec<CompilerError>) {
    let mut p = Parser {
        file: file.to_string(),
        toks: tokens,
        pos: 0,
        errors: Vec::new(),
    };
    let items = p.parse_program();
    (
        Program {
            items,
            file: file.to_string(),
        },
        p.errors,
    )
}

#[allow(dead_code)]
struct Parser<'a> {
    file: String,
    toks: &'a [Token],
    pos: usize,
    errors: Vec<CompilerError>,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> &Token {
        &self.toks[self.pos]
    }

    #[allow(dead_code)]
    fn peek_at(&self, offset: usize) -> Option<&Token> {
        self.toks.get(self.pos + offset)
    }

    fn advance(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        if !matches!(t.kind, TokenKind::Eof) {
            self.pos += 1;
        }
        t
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek().kind, TokenKind::Eof)
    }

    fn err(&mut self, span: Span, message: impl Into<String>) {
        self.errors.push(CompilerError::new(
            Severity::Error,
            errors::code("E0010"),
            span,
            message,
        ));
    }

    fn expect(&mut self, kind: &TokenKind, what: &str) -> Option<Token> {
        if std::mem::discriminant(&self.peek().kind) == std::mem::discriminant(kind) {
            Some(self.advance())
        } else {
            let span = self.peek().span.clone();
            self.err(span, format!("expected {what}"));
            None
        }
    }

    fn synchronise_to_semi_or_brace(&mut self) {
        while !self.at_eof() {
            match self.peek().kind {
                TokenKind::Semi => {
                    self.advance();
                    return;
                }
                TokenKind::RBrace => return,
                _ => {
                    self.advance();
                }
            }
        }
    }

    fn parse_program(&mut self) -> Vec<Item> {
        let mut items = Vec::new();
        while !self.at_eof() {
            let saved_pos = self.pos;
            match self.peek().kind {
                TokenKind::Import => match self.parse_import() {
                    Some(i) => items.push(Item::Import(i)),
                    None => self.synchronise_to_semi_or_brace(),
                },
                TokenKind::Fn => match self.parse_fn_decl() {
                    Some(f) => items.push(Item::Fn(Box::new(f))),
                    None => self.synchronise_to_semi_or_brace(),
                },
                _ => {
                    let span = self.peek().span.clone();
                    self.err(span, "expected `import` or `fn` at top level");
                    self.synchronise_to_semi_or_brace();
                }
            }
            // Forward-progress guarantee. synchronise_to_semi_or_brace
            // stops *at* a `}` without consuming it (correct inside a
            // block), so a stray `}` at top level would re-enter this
            // loop at the same position and accumulate errors forever.
            // Force an advance if recovery left us stuck.
            if self.pos == saved_pos {
                self.advance();
            }
        }
        items
    }

    fn parse_import(&mut self) -> Option<ImportDecl> {
        let start = self.peek().span.clone();
        self.expect(&TokenKind::Import, "`import`")?;
        let mut path = Vec::new();
        let head = self.parse_ident("module name")?;
        path.push(head);
        while matches!(self.peek().kind, TokenKind::Dot) {
            self.advance();
            path.push(self.parse_ident("module component")?);
        }
        if matches!(self.peek().kind, TokenKind::Semi) {
            self.advance();
        }
        // v1 restricts user imports to std.*.
        if path.first().map(String::as_str) != Some("std") {
            self.errors.push(CompilerError::new(
                Severity::Error,
                errors::code("E0031"),
                start.clone(),
                format!(
                    "user-code imports are not supported in v1 (saw `{}`)",
                    path.join(".")
                ),
            ));
        }
        Some(ImportDecl { path, span: start })
    }

    fn parse_ident(&mut self, what: &str) -> Option<String> {
        let t = self.peek().clone();
        if let TokenKind::Ident(name) = t.kind {
            self.advance();
            Some(name)
        } else {
            let span = t.span;
            self.err(span, format!("expected {what}"));
            None
        }
    }

    fn parse_fn_decl(&mut self) -> Option<FnDecl> {
        let start = self.peek().span.clone();
        self.expect(&TokenKind::Fn, "`fn`")?;
        let name_tok = self.peek().clone();
        let name = match name_tok.kind {
            TokenKind::Ident(ref n) => {
                self.advance();
                n.clone()
            }
            _ => {
                self.err(name_tok.span.clone(), "expected function name");
                return None;
            }
        };
        let name_span = name_tok.span;

        self.expect(&TokenKind::LParen, "`(`")?;
        let mut params = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RParen | TokenKind::Eof) {
            let p_start = self.peek().span.clone();
            let pname = self.parse_ident("parameter name")?;
            self.expect(&TokenKind::Colon, "`:`")?;
            let pty = self.parse_type()?;
            params.push(Param {
                name: pname,
                ty: pty,
                span: p_start,
            });
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        self.expect(&TokenKind::RParen, "`)`")?;
        self.expect(&TokenKind::Arrow, "`->` return-type arrow")?;
        let return_type = self.parse_type()?;
        self.expect(&TokenKind::Bang, "`!` before effect row")?;
        self.expect(&TokenKind::LBracket, "`[` opening effect row")?;
        let mut effects = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RBracket | TokenKind::Eof) {
            let e = self.parse_ident("effect name")?;
            effects.push(e);
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        self.expect(&TokenKind::RBracket, "`]` closing effect row")?;
        let body = self.parse_block()?;
        Some(FnDecl {
            name,
            name_span,
            params,
            return_type,
            effects,
            body,
            span: start,
        })
    }

    fn parse_type(&mut self) -> Option<TypeExpr> {
        let tok = self.peek().clone();
        let TokenKind::Ident(n) = tok.kind else {
            self.err(tok.span.clone(), "expected type name");
            return None;
        };
        self.advance();
        Some(TypeExpr::Named(n, tok.span))
    }

    fn parse_block(&mut self) -> Option<Block> {
        let start = self.peek().span.clone();
        self.expect(&TokenKind::LBrace, "`{` opening block")?;
        let mut stmts = Vec::new();
        let mut tail: Option<Expr> = None;
        while !matches!(self.peek().kind, TokenKind::RBrace | TokenKind::Eof) {
            // Try statement forms first.
            match self.peek().kind {
                TokenKind::Let => {
                    if let Some(l) = self.parse_let_stmt() {
                        stmts.push(Stmt::Let(l));
                    } else {
                        self.synchronise_to_semi_or_brace();
                    }
                }
                TokenKind::Perform => {
                    // A perform can be either a statement (expr;) or the tail
                    // expression (no trailing `;`). Peek past the call to
                    // decide.
                    if let Some(pe) = self.parse_perform_expr() {
                        if matches!(self.peek().kind, TokenKind::Semi) {
                            self.advance();
                            stmts.push(Stmt::Perform(pe));
                        } else {
                            tail = Some(Expr::Perform(pe));
                            break;
                        }
                    } else {
                        self.synchronise_to_semi_or_brace();
                    }
                }
                _ => {
                    let expr = self.parse_expr()?;
                    if matches!(self.peek().kind, TokenKind::Semi) {
                        self.advance();
                        stmts.push(Stmt::Expr(expr));
                    } else {
                        tail = Some(expr);
                        break;
                    }
                }
            }
        }
        self.expect(&TokenKind::RBrace, "`}` closing block")?;
        Some(Block {
            stmts,
            tail,
            span: start,
        })
    }

    fn parse_let_stmt(&mut self) -> Option<LetStmt> {
        let start = self.peek().span.clone();
        self.expect(&TokenKind::Let, "`let`")?;
        let name = self.parse_ident("binding name")?;
        self.expect(&TokenKind::Colon, "`:`")?;
        let ty = self.parse_type()?;
        self.expect(&TokenKind::Eq, "`=`")?;
        let value = self.parse_expr()?;
        self.expect(&TokenKind::Semi, "`;`")?;
        Some(LetStmt {
            name,
            ty,
            value,
            span: start,
        })
    }

    fn parse_perform_expr(&mut self) -> Option<PerformExpr> {
        let start = self.peek().span.clone();
        self.expect(&TokenKind::Perform, "`perform`")?;
        let effect = self.parse_ident("effect name")?;
        self.expect(&TokenKind::Dot, "`.`")?;
        let op = self.parse_ident("operation name")?;
        self.expect(&TokenKind::LParen, "`(`")?;
        let mut args = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RParen | TokenKind::Eof) {
            args.push(self.parse_expr()?);
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        self.expect(&TokenKind::RParen, "`)`")?;
        Some(PerformExpr {
            effect,
            op,
            args,
            span: start,
        })
    }

    fn parse_expr(&mut self) -> Option<Expr> {
        self.parse_expr_prec(0)
    }

    /// Precedence-climbing parser for binary operators. Precedences
    /// (higher binds tighter; all left-associative):
    /// 1 — `||`
    /// 2 — `&&`
    /// 3 — `==` `!=`
    /// 4 — `<` `>` `<=` `>=`
    /// 5 — `+` `-` (binary)
    /// 6 — `*` `/` `%`
    /// Unary prefix (`-`, `!`) is parsed inside `parse_unary` and binds
    /// tighter than any binary operator.
    fn parse_expr_prec(&mut self, min_prec: u8) -> Option<Expr> {
        let mut lhs = self.parse_unary()?;
        while let Some((op, prec)) = Self::binop_for(&self.peek().kind) {
            if prec < min_prec {
                break;
            }
            self.advance();
            // Left-associative: raise min_prec by one for the right side.
            let rhs = self.parse_expr_prec(prec + 1)?;
            let span = lhs.span();
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                span,
            };
        }
        Some(lhs)
    }

    fn binop_for(kind: &TokenKind) -> Option<(BinOp, u8)> {
        Some(match kind {
            TokenKind::OrOr => (BinOp::Or, 1),
            TokenKind::AndAnd => (BinOp::And, 2),
            TokenKind::EqEq => (BinOp::Eq, 3),
            TokenKind::NotEq => (BinOp::NotEq, 3),
            TokenKind::Lt => (BinOp::Lt, 4),
            TokenKind::Gt => (BinOp::Gt, 4),
            TokenKind::LtEq => (BinOp::LtEq, 4),
            TokenKind::GtEq => (BinOp::GtEq, 4),
            TokenKind::Plus => (BinOp::Add, 5),
            TokenKind::Minus => (BinOp::Sub, 5),
            TokenKind::Star => (BinOp::Mul, 6),
            TokenKind::Slash => (BinOp::Div, 6),
            TokenKind::Percent => (BinOp::Mod, 6),
            _ => return None,
        })
    }

    /// Unary prefix operators: `-` (negation) and `!` (boolean not).
    /// Constant-folds `-<integer-literal>` into a negative literal at
    /// parse time, per plan A2 task 21 — the downstream IRs never see
    /// a `Unary(Neg, IntLit(n))` node for a literal operand.
    fn parse_unary(&mut self) -> Option<Expr> {
        let tok = self.peek().clone();
        match tok.kind {
            TokenKind::Minus => {
                self.advance();
                let operand = self.parse_unary()?;
                // Constant-fold `-<int-literal>`.
                if let Expr::IntLit(n, _) = &operand {
                    return Some(Expr::IntLit(n.wrapping_neg(), tok.span));
                }
                Some(Expr::Unary {
                    op: UnOp::Neg,
                    operand: Box::new(operand),
                    span: tok.span,
                })
            }
            TokenKind::Bang => {
                self.advance();
                let operand = self.parse_unary()?;
                Some(Expr::Unary {
                    op: UnOp::Not,
                    operand: Box::new(operand),
                    span: tok.span,
                })
            }
            _ => self.parse_postfix(),
        }
    }

    /// Postfix forms that bind on a primary: function-call chains.
    fn parse_postfix(&mut self) -> Option<Expr> {
        let mut expr = self.parse_primary()?;
        while matches!(self.peek().kind, TokenKind::LParen) {
            let call_start = expr.span();
            self.advance();
            let mut args = Vec::new();
            while !matches!(self.peek().kind, TokenKind::RParen | TokenKind::Eof) {
                args.push(self.parse_expr()?);
                if matches!(self.peek().kind, TokenKind::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
            self.expect(&TokenKind::RParen, "`)`")?;
            expr = Expr::Call {
                callee: Box::new(expr),
                args,
                span: call_start,
            };
        }
        Some(expr)
    }

    fn parse_primary(&mut self) -> Option<Expr> {
        let tok = self.peek().clone();
        match tok.kind {
            TokenKind::IntLit(n) => {
                self.advance();
                Some(Expr::IntLit(n, tok.span))
            }
            TokenKind::StringLit(ref s) => {
                self.advance();
                Some(Expr::StringLit(s.clone(), tok.span))
            }
            TokenKind::CharLit(c) => {
                self.advance();
                Some(Expr::CharLit(c, tok.span))
            }
            TokenKind::True => {
                self.advance();
                Some(Expr::BoolLit(true, tok.span))
            }
            TokenKind::False => {
                self.advance();
                Some(Expr::BoolLit(false, tok.span))
            }
            TokenKind::Ident(ref n) => {
                self.advance();
                Some(Expr::Ident(n.clone(), tok.span))
            }
            TokenKind::Perform => self.parse_perform_expr().map(Expr::Perform),
            TokenKind::LParen => {
                // Parenthesized expression for precedence override.
                self.advance();
                let inner = self.parse_expr()?;
                self.expect(&TokenKind::RParen, "`)` closing parenthesized expression")?;
                Some(inner)
            }
            TokenKind::If => self.parse_if_expr(),
            TokenKind::Match => self.parse_match_expr(),
            TokenKind::Fn => self.parse_lambda_expr(),
            _ => {
                self.err(tok.span.clone(), "expected an expression");
                None
            }
        }
    }

    /// Parse a lambda expression of the form
    /// `fn (x: T, ...) -> U ![E, ...] => body`. Body is a single
    /// expression, not a block. The surrounding `parse_primary`
    /// dispatches on `TokenKind::Fn`; no `fn_decl`/`lambda`
    /// disambiguation is needed because `parse_fn_decl` only runs
    /// from `parse_program`, not from expression-position parsing.
    ///
    /// # Body precedence note
    ///
    /// The body is parsed via `parse_expr`, which recurses all the
    /// way down to `postfix`. That means `fn () -> Int ![] => 1()`
    /// parses as `Lambda { body: Call(IntLit 1, []) }`, *not*
    /// `Call(Lambda { body: IntLit 1 }, [])`. Callers who want the
    /// latter parse must parenthesise the lambda. This mirrors the
    /// ML/Haskell convention — the lambda body extends as far to the
    /// right as possible. The `lambda_body_swallows_postfix` test
    /// pins the behaviour so a future precedence tweak is deliberate.
    fn parse_lambda_expr(&mut self) -> Option<Expr> {
        let start = self.peek().span.clone();
        self.expect(&TokenKind::Fn, "`fn`")?;

        self.expect(&TokenKind::LParen, "`(`")?;
        let mut params = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RParen | TokenKind::Eof) {
            let p_start = self.peek().span.clone();
            let pname = self.parse_ident("lambda parameter name")?;
            self.expect(&TokenKind::Colon, "`:`")?;
            let pty = self.parse_type()?;
            params.push(Param {
                name: pname,
                ty: pty,
                span: p_start,
            });
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        self.expect(&TokenKind::RParen, "`)`")?;
        self.expect(&TokenKind::Arrow, "`->` before lambda return type")?;
        let return_type = self.parse_type()?;
        self.expect(&TokenKind::Bang, "`!` before lambda effect row")?;
        self.expect(&TokenKind::LBracket, "`[` opening lambda effect row")?;
        let mut effects = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RBracket | TokenKind::Eof) {
            let e = self.parse_ident("effect name")?;
            effects.push(e);
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        self.expect(&TokenKind::RBracket, "`]` closing lambda effect row")?;
        self.expect(&TokenKind::FatArrow, "`=>` before lambda body")?;
        let body = self.parse_expr()?;
        Some(Expr::Lambda {
            params,
            return_type,
            effects,
            body: Box::new(body),
            span: start,
        })
    }

    fn parse_if_expr(&mut self) -> Option<Expr> {
        let start = self.peek().span.clone();
        self.expect(&TokenKind::If, "`if`")?;
        let cond = self.parse_expr()?;
        let then_block = self.parse_block()?;
        self.expect(
            &TokenKind::Else,
            "`else` (every `if` requires an `else` branch)",
        )?;
        let else_block = self.parse_block()?;
        Some(Expr::If {
            cond: Box::new(cond),
            then_block: Box::new(then_block),
            else_block: Box::new(else_block),
            span: start,
        })
    }

    fn parse_match_expr(&mut self) -> Option<Expr> {
        let start = self.peek().span.clone();
        self.expect(&TokenKind::Match, "`match`")?;
        let scrutinee = self.parse_expr()?;
        self.expect(&TokenKind::LBrace, "`{` opening match arms")?;
        let mut arms = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RBrace | TokenKind::Eof) {
            let arm_start = self.peek().span.clone();
            let pattern = self.parse_pattern()?;
            self.expect(&TokenKind::FatArrow, "`=>`")?;
            let body = self.parse_expr()?;
            arms.push(MatchArm {
                pattern,
                body,
                span: arm_start,
            });
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        self.expect(&TokenKind::RBrace, "`}` closing match arms")?;
        Some(Expr::Match {
            scrutinee: Box::new(scrutinee),
            arms,
            span: start,
        })
    }

    fn parse_pattern(&mut self) -> Option<Pattern> {
        let tok = self.peek().clone();
        // `-<int-lit>` as a pattern: accept the negation and fold.
        if matches!(tok.kind, TokenKind::Minus) {
            self.advance();
            let next = self.peek().clone();
            if let TokenKind::IntLit(n) = next.kind {
                self.advance();
                return Some(Pattern::IntLit(n.wrapping_neg(), tok.span));
            }
            self.err(next.span, "expected integer literal after `-` in pattern");
            return None;
        }
        match tok.kind {
            TokenKind::IntLit(n) => {
                self.advance();
                Some(Pattern::IntLit(n, tok.span))
            }
            TokenKind::True => {
                self.advance();
                Some(Pattern::BoolLit(true, tok.span))
            }
            TokenKind::False => {
                self.advance();
                Some(Pattern::BoolLit(false, tok.span))
            }
            TokenKind::CharLit(c) => {
                self.advance();
                Some(Pattern::CharLit(c, tok.span))
            }
            TokenKind::Ident(ref n) if n == "_" => {
                self.advance();
                Some(Pattern::Wildcard(tok.span))
            }
            _ => {
                self.err(tok.span.clone(), "expected pattern (literal or `_`)");
                None
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::lexer::lex;

    #[test]
    fn parses_hello_world() {
        let src = "import std.io\n\nfn main() -> Int ![IO] {\n  perform IO.println(\"hello, world\");\n  0\n}\n";
        let (toks, lex_errs) = lex("hello.sigil", src);
        assert!(lex_errs.is_empty(), "{lex_errs:?}");
        let (prog, errs) = parse("hello.sigil", &toks);
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(prog.items.len(), 2);
        let Item::Fn(ref f) = prog.items[1] else {
            panic!()
        };
        assert_eq!(f.name, "main");
        assert_eq!(f.effects, vec!["IO".to_string()]);
        assert_eq!(f.body.stmts.len(), 1);
        let Some(Expr::IntLit(0, _)) = f.body.tail else {
            panic!()
        };
    }

    #[test]
    fn user_import_is_e0031() {
        let src = "import mylib.foo\nfn main() -> Int ![] { 0 }\n";
        let (toks, _) = lex("x.sigil", src);
        let (_prog, errs) = parse("x.sigil", &toks);
        assert!(errs.iter().any(|e| e.code.as_str() == "E0031"));
    }

    #[test]
    fn two_syntax_errors_in_one_run() {
        // Two distinct syntax errors: stray `@` and a missing effect row.
        let src = "fn a() @ Int ![] { 0 }\nfn b() -> Int { 0 }\n";
        let (toks, lex_errs) = lex("x.sigil", src);
        // `@` triggers a lexer E0010; the parser then recovers. `fn b` has
        // no `!` before `{` which triggers a parser E0010.
        let (_prog, parse_errs) = parse("x.sigil", &toks);
        let total = lex_errs.len() + parse_errs.len();
        assert!(
            total >= 2,
            "expected >=2 errors, got {lex_errs:?} + {parse_errs:?}"
        );
    }

    // Plan A2 task 21 — Stage-2 expression parsing.

    fn parse_tail_expr(src: &str) -> Expr {
        let full = format!("fn main() -> Int ![] {{ {src} }}");
        let (toks, lex_errs) = lex("x.sigil", &full);
        assert!(lex_errs.is_empty(), "lex errors: {lex_errs:?}");
        let (prog, parse_errs) = parse("x.sigil", &toks);
        assert!(parse_errs.is_empty(), "parse errors: {parse_errs:?}");
        let Item::Fn(ref f) = prog.items[0] else {
            panic!("expected fn decl")
        };
        f.body.tail.clone().expect("expected a tail expression")
    }

    #[test]
    fn arith_precedence_mul_over_add() {
        // 1 + 2 * 3 should parse as 1 + (2 * 3), not (1+2)*3.
        let e = parse_tail_expr("1 + 2 * 3");
        match e {
            Expr::Binary {
                op: BinOp::Add,
                lhs,
                rhs,
                ..
            } => {
                assert!(matches!(*lhs, Expr::IntLit(1, _)));
                match *rhs {
                    Expr::Binary { op: BinOp::Mul, .. } => {}
                    other => panic!("expected Mul on RHS, got {other:?}"),
                }
            }
            other => panic!("expected top-level Add, got {other:?}"),
        }
    }

    #[test]
    fn arith_left_assoc() {
        // 1 - 2 - 3 should parse as (1 - 2) - 3, not 1 - (2 - 3).
        let e = parse_tail_expr("1 - 2 - 3");
        match e {
            Expr::Binary {
                op: BinOp::Sub,
                lhs,
                rhs,
                ..
            } => {
                match *lhs {
                    Expr::Binary { op: BinOp::Sub, .. } => {}
                    other => panic!("expected nested Sub on LHS, got {other:?}"),
                }
                assert!(matches!(*rhs, Expr::IntLit(3, _)));
            }
            other => panic!("expected top-level Sub, got {other:?}"),
        }
    }

    #[test]
    fn comparison_below_arith() {
        // 1 + 2 < 3 * 4 should parse as (1+2) < (3*4).
        let e = parse_tail_expr("1 + 2 < 3 * 4");
        match e {
            Expr::Binary { op: BinOp::Lt, .. } => {}
            other => panic!("expected top-level Lt, got {other:?}"),
        }
    }

    #[test]
    fn equality_below_comparison() {
        // a < b == c > d groups as (a < b) == (c > d).
        let e = parse_tail_expr("1 < 2 == 3 > 4");
        match e {
            Expr::Binary { op: BinOp::Eq, .. } => {}
            other => panic!("expected top-level Eq, got {other:?}"),
        }
    }

    #[test]
    fn and_below_or() {
        // true || false && true groups as true || (false && true).
        let e = parse_tail_expr("true || false && true");
        match e {
            Expr::Binary {
                op: BinOp::Or,
                lhs,
                rhs,
                ..
            } => {
                assert!(matches!(*lhs, Expr::BoolLit(true, _)));
                assert!(matches!(*rhs, Expr::Binary { op: BinOp::And, .. }));
            }
            other => panic!("expected top-level Or, got {other:?}"),
        }
    }

    #[test]
    fn paren_overrides_precedence() {
        // (1 + 2) * 3 should parse as (1 + 2) * 3.
        let e = parse_tail_expr("(1 + 2) * 3");
        match e {
            Expr::Binary {
                op: BinOp::Mul,
                lhs,
                rhs,
                ..
            } => {
                assert!(matches!(*lhs, Expr::Binary { op: BinOp::Add, .. }));
                assert!(matches!(*rhs, Expr::IntLit(3, _)));
            }
            other => panic!("expected top-level Mul, got {other:?}"),
        }
    }

    #[test]
    fn negative_int_literal_is_constant_folded() {
        // `-5` should parse as Expr::IntLit(-5, _), not Unary(Neg, IntLit(5)).
        let e = parse_tail_expr("-5");
        assert!(matches!(e, Expr::IntLit(-5, _)));
    }

    #[test]
    fn unary_neg_on_non_literal() {
        // `-x` where x is an ident still becomes Unary(Neg, Ident).
        let e = parse_tail_expr("-x");
        match e {
            Expr::Unary {
                op: UnOp::Neg,
                operand,
                ..
            } => {
                assert!(matches!(*operand, Expr::Ident(ref name, _) if name == "x"));
            }
            other => panic!("expected Unary Neg, got {other:?}"),
        }
    }

    #[test]
    fn unary_not_on_bool_lit() {
        let e = parse_tail_expr("!true");
        match e {
            Expr::Unary {
                op: UnOp::Not,
                operand,
                ..
            } => {
                assert!(matches!(*operand, Expr::BoolLit(true, _)));
            }
            other => panic!("expected Unary Not, got {other:?}"),
        }
    }

    #[test]
    fn if_expression_with_else() {
        let e = parse_tail_expr("if true { 1 } else { 2 }");
        match e {
            Expr::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                assert!(matches!(*cond, Expr::BoolLit(true, _)));
                assert!(matches!(then_block.tail, Some(Expr::IntLit(1, _))));
                assert!(matches!(else_block.tail, Some(Expr::IntLit(2, _))));
            }
            other => panic!("expected If, got {other:?}"),
        }
    }

    #[test]
    fn match_expression_literal_arms() {
        let e = parse_tail_expr("match n { 0 => 10, 1 => 20, _ => 30 }");
        match e {
            Expr::Match { arms, .. } => {
                assert_eq!(arms.len(), 3);
                assert!(matches!(arms[0].pattern, Pattern::IntLit(0, _)));
                assert!(matches!(arms[1].pattern, Pattern::IntLit(1, _)));
                assert!(matches!(arms[2].pattern, Pattern::Wildcard(_)));
            }
            other => panic!("expected Match, got {other:?}"),
        }
    }

    #[test]
    fn char_literal_expression() {
        let e = parse_tail_expr("'a'");
        assert!(matches!(e, Expr::CharLit('a', _)));
    }

    // ===== Plan A2 Task 29 — Stage 3 grammar =======================

    #[test]
    fn multi_arg_fn_decl_parses() {
        // Mix types across params to exercise `parse_type()` re-entry
        // between commas. Per PR #5 reviewer follow-up.
        let src = "fn mix(a: Int, b: String, c: Bool) -> Int ![] { 0 }\n\
                   fn main() -> Int ![] { 0 }\n";
        let (toks, _) = lex("t.sigil", src);
        let (prog, errs) = parse("t.sigil", &toks);
        assert!(errs.is_empty(), "parse errs: {errs:?}");
        let mix = prog
            .items
            .iter()
            .find_map(|i| match i {
                Item::Fn(f) if f.name == "mix" => Some(f),
                _ => None,
            })
            .expect("mix not parsed");
        assert_eq!(mix.params.len(), 3);
        assert_eq!(mix.params[0].name, "a");
        assert_eq!(mix.params[1].name, "b");
        assert_eq!(mix.params[2].name, "c");
        assert!(matches!(&mix.params[0].ty, TypeExpr::Named(n, _) if n == "Int"));
        assert!(matches!(&mix.params[1].ty, TypeExpr::Named(n, _) if n == "String"));
        assert!(matches!(&mix.params[2].ty, TypeExpr::Named(n, _) if n == "Bool"));
    }

    #[test]
    fn trailing_comma_in_param_list_is_tolerated() {
        // The `parse_fn_decl` loop peeks for `Comma` after each
        // param, advances past it, then loops; if the next token is
        // `RParen`, the while-condition exits cleanly. Net effect:
        // trailing commas in parameter lists are accepted. This
        // mirrors the `arg_list` behaviour in `parse_postfix`.
        let src = "fn ok(a: Int,) -> Int ![] { 0 }\n\
                   fn main() -> Int ![] { 0 }\n";
        let (toks, _) = lex("t.sigil", src);
        let (prog, errs) = parse("t.sigil", &toks);
        assert!(errs.is_empty(), "expected clean parse, got: {errs:?}");
        let ok = prog
            .items
            .iter()
            .find_map(|i| match i {
                Item::Fn(f) if f.name == "ok" => Some(f),
                _ => None,
            })
            .expect("ok not parsed");
        assert_eq!(ok.params.len(), 1);
    }

    #[test]
    fn call_expression_with_args_parses() {
        // The grammar already admits `postfix := primary ('(' arg_list? ')')*`
        // since Plan A1. Pin the shape: a call with multiple args
        // produces `Expr::Call { callee: Ident, args: [...] }`.
        let e = parse_tail_expr("f(1, 2, 3)");
        match e {
            Expr::Call { callee, args, .. } => {
                assert!(matches!(*callee, Expr::Ident(ref n, _) if n == "f"));
                assert_eq!(args.len(), 3);
                assert!(matches!(args[0], Expr::IntLit(1, _)));
                assert!(matches!(args[1], Expr::IntLit(2, _)));
                assert!(matches!(args[2], Expr::IntLit(3, _)));
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn call_with_no_args_parses() {
        let e = parse_tail_expr("f()");
        match e {
            Expr::Call { args, .. } => {
                assert!(args.is_empty());
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn chained_calls_parse_left_assoc() {
        // `f()()` → Call(Call(Ident f, []), [])
        let e = parse_tail_expr("f()()");
        match e {
            Expr::Call { callee, args, .. } => {
                assert!(args.is_empty());
                match *callee {
                    Expr::Call { args: inner, .. } => assert!(inner.is_empty()),
                    other => panic!("expected inner Call, got {other:?}"),
                }
            }
            other => panic!("expected outer Call, got {other:?}"),
        }
    }

    #[test]
    fn lambda_no_params_no_effects() {
        let e = parse_tail_expr("fn () -> Int ![] => 42");
        match e {
            Expr::Lambda {
                params,
                return_type,
                effects,
                body,
                ..
            } => {
                assert!(params.is_empty());
                match return_type {
                    TypeExpr::Named(n, _) => assert_eq!(n, "Int"),
                }
                assert!(effects.is_empty());
                assert!(matches!(*body, Expr::IntLit(42, _)));
            }
            other => panic!("expected Lambda, got {other:?}"),
        }
    }

    #[test]
    fn lambda_one_param() {
        let e = parse_tail_expr("fn (x: Int) -> Int ![] => x");
        match e {
            Expr::Lambda {
                params,
                return_type,
                effects,
                body,
                ..
            } => {
                assert_eq!(params.len(), 1);
                assert_eq!(params[0].name, "x");
                match &params[0].ty {
                    TypeExpr::Named(n, _) => assert_eq!(n, "Int"),
                }
                match return_type {
                    TypeExpr::Named(n, _) => assert_eq!(n, "Int"),
                }
                assert!(effects.is_empty());
                match *body {
                    Expr::Ident(ref n, _) => assert_eq!(n, "x"),
                    other => panic!("body not Ident, got {other:?}"),
                }
            }
            other => panic!("expected Lambda, got {other:?}"),
        }
    }

    #[test]
    fn lambda_two_params_with_effect() {
        let e = parse_tail_expr("fn (a: Int, b: Int) -> Int ![IO] => a + b");
        match e {
            Expr::Lambda {
                params,
                effects,
                body,
                ..
            } => {
                assert_eq!(params.len(), 2);
                assert_eq!(params[0].name, "a");
                assert_eq!(params[1].name, "b");
                assert_eq!(effects, vec!["IO".to_string()]);
                match *body {
                    Expr::Binary { op, .. } => assert_eq!(op, BinOp::Add),
                    other => panic!("body not Binary, got {other:?}"),
                }
            }
            other => panic!("expected Lambda, got {other:?}"),
        }
    }

    #[test]
    fn lambda_body_can_be_another_lambda() {
        // `fn (x: Int) -> ... => fn (y: Int) -> Int ![] => x + y`.
        // Nested lambda parsing — currying by hand. Pin the shape so
        // a future precedence tweak doesn't silently break it.
        let e = parse_tail_expr("fn (x: Int) -> Int ![] => fn (y: Int) -> Int ![] => x + y");
        match e {
            Expr::Lambda { body, .. } => match *body {
                Expr::Lambda { params, body, .. } => {
                    assert_eq!(params.len(), 1);
                    assert_eq!(params[0].name, "y");
                    assert!(matches!(*body, Expr::Binary { .. }));
                }
                other => panic!("inner not Lambda, got {other:?}"),
            },
            other => panic!("outer not Lambda, got {other:?}"),
        }
    }

    #[test]
    fn lambda_applied_immediately_with_parens() {
        // `(fn (x: Int) -> Int ![] => x + 1)(41)` — parenthesised
        // lambda as callee, applied to an argument. Exercises the
        // interaction between `primary := '(' expr ')'` and
        // `postfix := primary '(' args ')'`.
        let e = parse_tail_expr("(fn (x: Int) -> Int ![] => x + 1)(41)");
        match e {
            Expr::Call { callee, args, .. } => {
                assert_eq!(args.len(), 1);
                assert!(matches!(args[0], Expr::IntLit(41, _)));
                assert!(matches!(*callee, Expr::Lambda { .. }));
            }
            other => panic!("expected Call on Lambda, got {other:?}"),
        }
    }

    // ===== Plan A2 Task 30 — reviewer follow-ups on parser surface ======

    #[test]
    fn lambda_body_swallows_postfix() {
        // `parse_lambda_expr`'s body is `parse_expr`, which recurses
        // down to postfix; that means `fn () -> Int ![] => 1()`
        // parses as `Lambda { body: Call(IntLit 1, []) }`, **not**
        // `Call(Lambda { body: IntLit 1 }, [])`. Callers who want the
        // other parse must parenthesise the lambda. This matches ML
        // and Haskell conventions — pinned here so a future grammar
        // tweak to lambda-body precedence is a deliberate decision.
        let e = parse_tail_expr("fn () -> Int ![] => 1()");
        match e {
            Expr::Lambda { body, .. } => match *body {
                Expr::Call { callee, args, .. } => {
                    assert!(args.is_empty());
                    assert!(matches!(*callee, Expr::IntLit(1, _)));
                }
                other => panic!("body should be Call, got {other:?}"),
            },
            other => panic!("outer should be Lambda, got {other:?}"),
        }
    }

    #[test]
    fn lambda_in_call_arg_parses() {
        // `f(fn () -> Int ![] => 1)` — lambda as a call argument.
        // Should compose via `parse_postfix`'s arg-list calling
        // `parse_expr`.
        let e = parse_tail_expr("f(fn () -> Int ![] => 1)");
        match e {
            Expr::Call { callee, args, .. } => {
                assert_eq!(args.len(), 1);
                assert!(matches!(args[0], Expr::Lambda { .. }));
                assert!(matches!(*callee, Expr::Ident(ref n, _) if n == "f"));
            }
            other => panic!("expected Call with Lambda arg, got {other:?}"),
        }
    }

    fn parse_errs(src: &str) -> Vec<CompilerError> {
        let (toks, _) = lex("t.sigil", src);
        let (_prog, errs) = parse("t.sigil", &toks);
        errs
    }

    #[test]
    fn lambda_missing_arrow_errors() {
        // `fn () Int ![] => 0` — missing `->` before return type.
        let errs = parse_errs("fn main() -> Int ![] { fn () Int ![] => 0 }\n");
        assert!(
            !errs.is_empty(),
            "missing `->` in lambda should parse-error"
        );
    }

    #[test]
    fn lambda_missing_bang_errors() {
        // `fn () -> Int [] => 0` — missing `!` before the effect row.
        let errs = parse_errs("fn main() -> Int ![] { fn () -> Int [] => 0 }\n");
        assert!(
            !errs.is_empty(),
            "missing `!` before effect row should parse-error"
        );
    }

    #[test]
    fn lambda_missing_fat_arrow_errors() {
        // `fn () -> Int ![] 0` — missing `=>` between the effect row
        // and the body.
        let errs = parse_errs("fn main() -> Int ![] { fn () -> Int ![] 0 }\n");
        assert!(
            !errs.is_empty(),
            "missing `=>` before lambda body should parse-error"
        );
    }

    #[test]
    fn lambda_missing_body_errors() {
        // `fn () -> Int ![] =>` — no body after `=>`.
        let errs = parse_errs("fn main() -> Int ![] { fn () -> Int ![] => }\n");
        assert!(!errs.is_empty(), "missing lambda body should parse-error");
    }
}
