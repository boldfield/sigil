//! Recursive-descent parser for the Stage-1 subset.
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
//! expr      := int_lit | string_lit | ident | call_expr | perform_expr
//! call_expr := expr '(' arg_list? ')'
//! perform_expr := 'perform' ident '.' ident '(' arg_list? ')'
//! ```
//!
//! Error recovery: on a syntax error the parser synchronises to the next
//! `;` or matching `}` and continues. All errors from one run surface in
//! the same compile invocation.

use crate::ast::*;
use crate::errors::{catalog::ErrorCode, CompilerError, Severity, Span};
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
        let code = match ErrorCode::new("E0010") {
            Some(c) => c,
            None => panic!("catalog is missing E0010"),
        };
        self.errors
            .push(CompilerError::new(Severity::Error, code, span, message));
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
            let code = match ErrorCode::new("E0031") {
                Some(c) => c,
                None => panic!("catalog is missing E0031"),
            };
            self.errors.push(CompilerError::new(
                Severity::Error,
                code,
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
        let tok = self.peek().clone();
        let mut expr = match tok.kind {
            TokenKind::IntLit(n) => {
                self.advance();
                Expr::IntLit(n, tok.span)
            }
            TokenKind::StringLit(ref s) => {
                self.advance();
                Expr::StringLit(s.clone(), tok.span)
            }
            TokenKind::Ident(ref n) => {
                self.advance();
                Expr::Ident(n.clone(), tok.span)
            }
            TokenKind::Perform => {
                return self.parse_perform_expr().map(Expr::Perform);
            }
            _ => {
                self.err(tok.span.clone(), "expected an expression");
                return None;
            }
        };
        // Postfix call parsing: support f(a, b) chains.
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
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
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
}
