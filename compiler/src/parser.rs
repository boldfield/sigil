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
//!
//! # Stage 6 grammar additions (plan B task 53).
//! program   := (import | fn_decl | type_decl | effect_decl)*
//! effect_decl  := 'effect' ident generic_params? resumes_attr? '{' effect_op (',' effect_op)* ','? '}'
//! resumes_attr := 'resumes' ':' 'many'   # context idents matched by string
//! effect_op    := ident ':' '(' (type (',' type)* ','?)? ')' '->' type
//! primary   := ... | 'handle' expr 'with' '{' handle_arm (',' handle_arm)* ','? '}'
//! handle_arm   := 'return' '(' ident ')' '=>' expr     # return arm
//!              | ident '.' ident '(' (ident (',' ident)* ','?) ')' '=>' expr
//!                # operation arm: trailing ident is the continuation
//!                # binding `k`. At least one parameter is required.
//!
//! # Stage 4 grammar additions (plan A3 task 37).
//! program   := (import | fn_decl | type_decl)*
//! type_decl := 'type' ident '=' ('|' variant)+
//!           | 'type' ident '=' '{' record_fields '}'    # single-ctor shorthand
//! variant   := ident ('(' type (',' type)* ','? ')')?
//!           | ident '{' record_fields '}'
//! record_fields := (ident ':' type) (',' (ident ':' type))* ','?
//! primary   := ... | ident '{' record_field_lit (',' record_field_lit)* ','? '}'
//!   # (record literal; disabled in if-cond / match-scrutinee positions to avoid
//!   # ambiguity with the block that follows. Parens `(Ctor { .. })` re-enable.)
//! record_field_lit := ident ':' expr
//! pattern   := literal | bool_lit | char_lit | '_' | ident      # Var or nullary Ctor
//!           | ident '(' pattern (',' pattern)* ','? ')'         # positional Ctor
//!           | ident '{' pattern_field (',' pattern_field)* ','? '}'  # record Ctor
//!           | '(' pattern (',' pattern)* ','? ')'               # tuple (or paren)
//! pattern_field := ident             # field-pun: binds same name
//!               | ident ':' pattern  # rename
//! # Plan A3 explicitly rejects pattern guards, or-patterns, and as-bindings
//! # with E0110 at the parser level. See the error catalog entry for the
//! # rationale: these forms are anti-ergonomic under fight-the-priors.
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
        no_record_lits: false,
    };
    let items = p.parse_program();
    (
        Program {
            items,
            file: file.to_string(),
            stdlib_files: std::collections::BTreeSet::new(),
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
    /// Plan A3 task 37 — disables recognition of `Ident { ... }` as a
    /// record literal inside `if` conditions and `match` scrutinees,
    /// where the immediately-following `{` would otherwise be ambiguous
    /// with the block that starts the arm / branch. Parenthesised
    /// sub-expressions restore the default (record literals allowed),
    /// so `if (Foo { x: 1 }).some_bool_field { .. }` still parses.
    no_record_lits: bool,
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

    /// Plan A3 task 37 — recover from a malformed match arm by skipping
    /// tokens until either a `=>` (end of the attempted arm's LHS),
    /// a `,` (end of the arm), or a `}` (end of the match). Leaves the
    /// parser positioned AT the terminator, not past it, so the outer
    /// arm loop can decide how to continue.
    fn recover_to_arm_terminator(&mut self) {
        while !self.at_eof() {
            match self.peek().kind {
                TokenKind::FatArrow => {
                    // Consume through the `=>` and the following body
                    // expression so the caller can test for `,` / `}`.
                    self.advance();
                    let _ = self.parse_expr();
                    return;
                }
                TokenKind::Comma | TokenKind::RBrace => return,
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
                TokenKind::Use => match self.parse_use_decl() {
                    Some(u) => items.push(Item::Use(Box::new(u))),
                    None => self.synchronise_to_semi_or_brace(),
                },
                TokenKind::Fn => match self.parse_fn_decl() {
                    Some(f) => items.push(Item::Fn(Box::new(f))),
                    None => self.synchronise_to_semi_or_brace(),
                },
                TokenKind::Type => match self.parse_type_decl() {
                    Some(t) => items.push(Item::Type(Box::new(t))),
                    None => self.synchronise_to_semi_or_brace(),
                },
                TokenKind::Effect => match self.parse_effect_decl() {
                    Some(e) => items.push(Item::Effect(Box::new(e))),
                    None => self.synchronise_to_semi_or_brace(),
                },
                _ => {
                    let span = self.peek().span.clone();
                    self.err(
                        span,
                        "expected `import`, `use`, `fn`, `type`, or `effect` at top level",
                    );
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
        // Plan F1 — optional `as <alias>` clause. Aliases the module
        // (not individual symbols): `import std.option as O;` makes
        // `O.x` a synonym for `std.option.x` at qualified call sites.
        // The alias must be a single identifier — `as foo.bar` is
        // rejected.
        let mut alias = None;
        if matches!(self.peek().kind, TokenKind::As) {
            self.advance();
            let alias_name = self.parse_ident("module alias after `as`")?;
            alias = Some(alias_name);
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
        Some(ImportDecl {
            path,
            alias,
            span: start,
        })
    }

    /// Plan F1 — selective bare-name opt-in for an imported module.
    ///
    /// Grammar:
    /// ```text
    /// use_decl    := 'use' qualified_path '.' '{' use_binding (',' use_binding)* '}' ';'
    /// use_binding := IDENT ('as' IDENT)?
    /// qualified_path := IDENT ('.' IDENT)*
    /// ```
    ///
    /// Wildcard `use mod.path.*;` is rejected with a dedicated error
    /// (we'd otherwise re-introduce the cross-module bare-name
    /// ambiguity class). Empty binding lists `use mod.path.{};` are
    /// also rejected — a `use` with zero symbols is dead code.
    fn parse_use_decl(&mut self) -> Option<UseDecl> {
        let start = self.peek().span.clone();
        self.expect(&TokenKind::Use, "`use`")?;
        // Parse the qualified module path: at least two segments
        // separated by `.`. The trailing `.` before `{...}` is part
        // of the path lexically, but the LAST segment is the module's
        // tail name, not a symbol. We accumulate dotted idents until
        // we see `{` (the binding-list opener).
        let mut module_path = Vec::new();
        let head = self.parse_ident("module name in `use` path")?;
        module_path.push(head);
        loop {
            if !matches!(self.peek().kind, TokenKind::Dot) {
                let span = self.peek().span.clone();
                self.err(span, "expected `.` between module-path segments in `use`");
                return None;
            }
            self.advance(); // consume `.`
                            // Wildcard form: `use mod.*;`. Reject with a clear message.
            if matches!(self.peek().kind, TokenKind::Star) {
                let span = self.peek().span.clone();
                self.advance(); // consume the `*` for parity
                if matches!(self.peek().kind, TokenKind::Semi) {
                    self.advance();
                }
                self.errors.push(CompilerError::new(
                    Severity::Error,
                    errors::code("E0034"),
                    span,
                    "wildcard `use` is not supported; list names explicitly: \
                     `use mod.path.{name1, name2}`."
                        .to_string(),
                ));
                return None;
            }
            // Brace-enclosed binding list ends the path.
            if matches!(self.peek().kind, TokenKind::LBrace) {
                break;
            }
            module_path.push(self.parse_ident("module-path segment in `use`")?);
        }
        self.expect(&TokenKind::LBrace, "`{` opening `use` binding list")?;
        let mut bindings: Vec<UseBinding> = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RBrace | TokenKind::Eof) {
            let bind_start = self.peek().span.clone();
            let source_name = self.parse_ident("imported symbol name")?;
            // Reject nested-path bindings: `use mod.{a.b}` is a structural
            // mistake (callers usually meant `use mod.a.{b}`).
            if matches!(self.peek().kind, TokenKind::Dot) {
                let span = self.peek().span.clone();
                self.err(
                    span,
                    "expected `,`, `}`, or `as` after symbol name (nested \
                     paths like `{a.b}` are not allowed — write \
                     `use mod.a.{b}` instead)",
                );
                return None;
            }
            let local_name = if matches!(self.peek().kind, TokenKind::As) {
                self.advance();
                self.parse_ident("local alias after `as`")?
            } else {
                source_name.clone()
            };
            bindings.push(UseBinding {
                source_name,
                local_name,
                span: bind_start,
            });
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.advance();
                continue;
            }
            break;
        }
        let close = self.peek().span.clone();
        self.expect(&TokenKind::RBrace, "`}` closing `use` binding list")?;
        if matches!(self.peek().kind, TokenKind::Semi) {
            self.advance();
        }
        if bindings.is_empty() {
            self.errors.push(CompilerError::new(
                Severity::Error,
                errors::code("E0035"),
                close,
                "`use` declaration must name at least one symbol".to_string(),
            ));
            return None;
        }
        // v1 imports gate (E0031 mirror): the `use` source must be a
        // stdlib path. User-code modules are not addressable in v1.
        if module_path.first().map(String::as_str) != Some("std") {
            self.errors.push(CompilerError::new(
                Severity::Error,
                errors::code("E0031"),
                start.clone(),
                format!(
                    "user-code modules are not addressable in v1 (saw `use {} ...`)",
                    module_path.join(".")
                ),
            ));
        }
        Some(UseDecl {
            module_path,
            bindings,
            span: start,
        })
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

        // Plan B Task 47 — optional `[A, B]` generic-parameter list.
        let generic_params = self.parse_generic_params()?;

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
        let (effects, effect_row_var) = self.parse_effect_row()?;
        let body = self.parse_block()?;
        Some(FnDecl {
            name,
            name_span,
            generic_params,
            params,
            return_type,
            effects,
            effect_row_var,
            body,
            span: start,
        })
    }

    /// Plan B Task 47 — parse the body of an effect row:
    /// `IO, Foo, Bar` or `IO | e` (row-variable). Caller already
    /// consumed the opening `[`; this method consumes through the
    /// closing `]`. Returns (effects, optional row variable).
    fn parse_effect_row(&mut self) -> Option<(Vec<EffectRef>, Option<RowVar>)> {
        let mut effects: Vec<EffectRef> = Vec::new();
        let mut row_var = None;
        while !matches!(
            self.peek().kind,
            TokenKind::RBracket | TokenKind::Pipe | TokenKind::Eof
        ) {
            let name_tok = self.peek().clone();
            let name = self.parse_ident("effect name")?;
            // Plan D Task 114 — accept type-parameterized effect
            // references in row position: `![Raise[E]]`. The parser
            // here only captures the syntactic form; the typechecker
            // checks the args' arity against the declared
            // `EffectDecl::generic_params` and substitutes them at
            // the row site. Bare-name refs (the pre-Task-114 surface)
            // produce `args: vec![]` and remain the dominant shape
            // for non-generic effects (`IO`, `Mem`, `ArithError`).
            let mut args: Vec<TypeExpr> = Vec::new();
            let mut end_span = name_tok.span.clone();
            if matches!(self.peek().kind, TokenKind::LBracket) {
                self.advance();
                while !matches!(self.peek().kind, TokenKind::RBracket | TokenKind::Eof) {
                    let arg = self.parse_type()?;
                    args.push(arg);
                    if matches!(self.peek().kind, TokenKind::Comma) {
                        self.advance();
                    } else {
                        break;
                    }
                }
                let close = self.peek().span.clone();
                self.expect(&TokenKind::RBracket, "`]` closing effect type-arg list")?;
                end_span = close;
            }
            let span = Span {
                file: name_tok.span.file.clone(),
                line: name_tok.span.line,
                column: name_tok.span.column,
                end_line: end_span.end_line,
                end_column: end_span.end_column,
            };
            effects.push(EffectRef { name, args, span });
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        if matches!(self.peek().kind, TokenKind::Pipe) {
            self.advance();
            let tok = self.peek().clone();
            match tok.kind {
                TokenKind::Ident(name) => {
                    self.advance();
                    row_var = Some(RowVar {
                        name,
                        span: tok.span,
                    });
                }
                _ => {
                    self.err(tok.span, "expected row-variable name after `|`");
                }
            }
        }
        self.expect(&TokenKind::RBracket, "`]` closing effect row")?;
        Some((effects, row_var))
    }

    /// Plan B Task 47 — optional `[A, B, ...]` generic-param header
    /// preceding a fn or type's main body. Returns `Some(Vec::new())`
    /// when the next token isn't `[` (a non-generic decl); returns
    /// `None` if a malformed list aborts parsing of the enclosing
    /// item. Mirrors `parse_effect_row`'s shape so callers propagate
    /// failures via `?` consistently — bounds and defaults extensions
    /// in a future plan can repurpose the same shape.
    fn parse_generic_params(&mut self) -> Option<Vec<GenericParam>> {
        if !matches!(self.peek().kind, TokenKind::LBracket) {
            return Some(Vec::new());
        }
        // Lookahead: `[` could open a non-generic `[A, B]` decl or
        // start a value-level expression in a different context. Here
        // (between a fn/type name and its parameter list / `=`), the
        // only valid `[...]` is a generic-param list.
        self.advance();
        let mut params = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RBracket | TokenKind::Eof) {
            let tok = self.peek().clone();
            match tok.kind {
                TokenKind::Ident(name) => {
                    self.advance();
                    params.push(GenericParam {
                        name,
                        span: tok.span,
                    });
                }
                _ => {
                    self.err(tok.span, "expected generic-parameter name");
                    return None;
                }
            }
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        self.expect(&TokenKind::RBracket, "`]` closing generic-parameter list")?;
        Some(params)
    }

    fn parse_type(&mut self) -> Option<TypeExpr> {
        let tok = self.peek().clone();
        // Plan B' Stage 6.8 Task 102 — first-class function type:
        // `(T1, ..., Tn) -> R ![E1, ..., En]`.
        // Plan D Task 113 — tuple type: `(T1, T2, ...)` (arity ≥ 2)
        // when NOT followed by `->`. Arity-1 `(T)` is paren-grouping
        // (returns the inner T directly). Discriminated by the
        // trailing `->` or `Comma` — the `(` opens both shapes, and
        // we look ahead at `)` to decide.
        if matches!(tok.kind, TokenKind::LParen) {
            self.advance();
            let mut params = Vec::new();
            while !matches!(self.peek().kind, TokenKind::RParen | TokenKind::Eof) {
                let p = self.parse_type()?;
                params.push(p);
                if matches!(self.peek().kind, TokenKind::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
            self.expect(&TokenKind::RParen, "`)` closing parenthesised type")?;
            // Plan D Task 113 — tuple-vs-fn-type discrimination: if
            // the next token is NOT `->`, this is either a tuple type
            // (arity ≥ 2) or paren-grouping (arity 1). Empty parens
            // `()` are reserved for a future Unit-type spelling and
            // are rejected here.
            if !matches!(self.peek().kind, TokenKind::Arrow) {
                if params.is_empty() {
                    self.err(
                        tok.span.clone(),
                        "empty `()` is not a valid type — use `Unit` for the unit type, \
                         or `(T1, T2, ...)` for a tuple type with arity ≥ 2",
                    );
                    return None;
                }
                if params.len() == 1 {
                    // Paren-grouping over a single type: return inner.
                    return params.into_iter().next();
                }
                return Some(TypeExpr::Tuple {
                    elems: params,
                    span: tok.span,
                });
            }
            self.expect(&TokenKind::Arrow, "`->` fn-type return arrow")?;
            let ret = self.parse_type()?;
            // Plan B' Stage 6.8 R5 Finding 4 — per-arrow `![..]`
            // diagnostic. Sigil's per-arrow effect-row syntax
            // (PLAN_B_PRIME_DEVIATIONS [DEVIATION Task 103]) requires
            // every fn-type to carry its own `![..]`. The default
            // `expect("`!`...")` error message is opaque on first
            // encounter — many users (and the implementer at
            // `986a8b4`) reach for ML-style outermost-only effects.
            // When the next token is `{` (likely a fn-decl body) or
            // an arrow-y token, attach a hint pointing at the
            // per-arrow requirement.
            if !matches!(self.peek().kind, TokenKind::Bang) {
                let here = self.peek().span.clone();
                let msg = format!(
                    "expected `![..]` for this fn-type's effect row \
                     (Sigil v1 requires every fn-type to carry its own \
                     effect row, including inner returns — `(A) -> B \
                     ![]` not `(A) -> B`); got {:?}",
                    self.peek().kind
                );
                self.err(here, &msg);
                return None;
            }
            self.expect(&TokenKind::Bang, "`!` before fn-type effect row")?;
            self.expect(&TokenKind::LBracket, "`[` opening fn-type effect row")?;
            let (effects, effect_row_var) = self.parse_effect_row()?;
            return Some(TypeExpr::Fn(Box::new(crate::ast::FnTypeExpr {
                params,
                ret,
                effects,
                effect_row_var,
                span: tok.span,
            })));
        }
        let TokenKind::Ident(n) = tok.kind else {
            self.err(tok.span.clone(), "expected type name");
            return None;
        };
        self.advance();
        // Plan B Task 47 — optional generic application
        // `Name[T1, T2, ...]`. The arguments are themselves
        // TypeExprs, so generic application nests:
        // `Map[String, List[Int]]` parses recursively.
        if matches!(self.peek().kind, TokenKind::LBracket) {
            self.advance();
            let mut args = Vec::new();
            while !matches!(self.peek().kind, TokenKind::RBracket | TokenKind::Eof) {
                let arg = self.parse_type()?;
                args.push(arg);
                if matches!(self.peek().kind, TokenKind::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
            self.expect(&TokenKind::RBracket, "`]` closing generic-argument list")?;
            return Some(TypeExpr::Apply {
                name: n,
                args,
                span: tok.span,
            });
        }
        Some(TypeExpr::Named(n, tok.span))
    }

    /// Plan A3 task 37 — parse a top-level `type` declaration.
    ///
    /// Grammar:
    /// ```text
    /// type Name = | Ctor | Ctor(T1, T2) | Ctor { f: T, ... }
    /// type Name = { f: T, ... }   # single-ctor record shorthand
    /// ```
    ///
    /// The record-shorthand form desugars to a single `Variant` whose
    /// name equals the type name and whose fields are the listed record
    /// fields. The sum form requires at least one leading `|` and at
    /// least one variant.
    fn parse_type_decl(&mut self) -> Option<TypeDecl> {
        let start = self.peek().span.clone();
        self.expect(&TokenKind::Type, "`type`")?;
        let name_tok = self.peek().clone();
        let name = match name_tok.kind {
            TokenKind::Ident(ref n) => {
                self.advance();
                n.clone()
            }
            _ => {
                self.err(name_tok.span.clone(), "expected type name after `type`");
                return None;
            }
        };
        let name_span = name_tok.span;

        // Plan B Task 47 — optional `[A, B]` generic-parameter list
        // between the type name and `=`.
        let generic_params = self.parse_generic_params()?;

        self.expect(&TokenKind::Eq, "`=`")?;

        // Single-constructor record shorthand `type Name = { f: T, ... }`.
        if matches!(self.peek().kind, TokenKind::LBrace) {
            self.advance();
            let fields = self.parse_record_field_decls()?;
            self.expect(&TokenKind::RBrace, "`}` closing record fields")?;
            let variant = Variant {
                name: name.clone(),
                name_span: name_span.clone(),
                fields: VariantFields::Record(fields),
                span: name_span.clone(),
            };
            return Some(TypeDecl {
                name,
                name_span,
                generic_params,
                variants: vec![variant],
                span: start,
            });
        }

        // Sum form: `= | Ctor | Ctor(T, ...) | Ctor { ... } ...`.
        let mut variants = Vec::new();
        while matches!(self.peek().kind, TokenKind::Pipe) {
            self.advance();
            let variant = self.parse_variant()?;
            variants.push(variant);
        }
        if variants.is_empty() {
            let span = self.peek().span.clone();
            self.err(
                span,
                "expected at least one `| Ctor` variant after `=`, or `{` for record shorthand",
            );
            return None;
        }
        Some(TypeDecl {
            name,
            name_span,
            generic_params,
            variants,
            span: start,
        })
    }

    /// Parse one constructor in a sum-type decl:
    /// `Ctor`, `Ctor(T1, T2)`, or `Ctor { f: T, ... }`.
    fn parse_variant(&mut self) -> Option<Variant> {
        let name_tok = self.peek().clone();
        let (name, name_span) = match name_tok.kind {
            TokenKind::Ident(ref n) => {
                self.advance();
                (n.clone(), name_tok.span.clone())
            }
            _ => {
                self.err(name_tok.span.clone(), "expected constructor name");
                return None;
            }
        };
        let span = name_span.clone();
        let fields = match self.peek().kind {
            TokenKind::LParen => {
                self.advance();
                let mut tys = Vec::new();
                while !matches!(self.peek().kind, TokenKind::RParen | TokenKind::Eof) {
                    tys.push(self.parse_type()?);
                    if matches!(self.peek().kind, TokenKind::Comma) {
                        self.advance();
                    } else {
                        break;
                    }
                }
                self.expect(&TokenKind::RParen, "`)` closing positional fields")?;
                VariantFields::Positional(tys)
            }
            TokenKind::LBrace => {
                self.advance();
                let fields = self.parse_record_field_decls()?;
                self.expect(&TokenKind::RBrace, "`}` closing record fields")?;
                VariantFields::Record(fields)
            }
            _ => VariantFields::Unit,
        };
        Some(Variant {
            name,
            name_span,
            fields,
            span,
        })
    }

    /// Plan B task 53 — parse a top-level `effect` declaration.
    ///
    /// Grammar:
    /// ```text
    /// effect Name [GenericParams] (resumes : many)? { op_decl, op_decl, ... }
    /// op_decl := name : ( type, ... ) -> type
    /// ```
    ///
    /// `resumes` and `many` are matched as plain Idents (not lexer
    /// keywords) so the surface declaration is the only place those
    /// names carry semantic meaning. A trailing comma after the last
    /// op is allowed, mirroring the convention in record field
    /// declarations and match arms.
    fn parse_effect_decl(&mut self) -> Option<EffectDecl> {
        let start = self.peek().span.clone();
        self.expect(&TokenKind::Effect, "`effect`")?;
        let name_tok = self.peek().clone();
        let name = match name_tok.kind {
            TokenKind::Ident(ref n) => {
                self.advance();
                n.clone()
            }
            _ => {
                self.err(name_tok.span.clone(), "expected effect name after `effect`");
                return None;
            }
        };
        let name_span = name_tok.span;

        // Optional `[A, B]` generic-parameter list between the effect
        // name and the optional `resumes: many` attribute / opening `{`.
        let generic_params = self.parse_generic_params()?;

        // Optional `resumes : many` attribute. Matched on Ident strings
        // so `resumes` and `many` are reusable as plain user identifiers
        // outside this position.
        let resumes_many = self.eat_resumes_many_attr()?;

        self.expect(&TokenKind::LBrace, "`{` opening effect body")?;
        let mut ops: Vec<EffectOp> = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RBrace | TokenKind::Eof) {
            let op = self.parse_effect_op()?;
            ops.push(op);
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        self.expect(&TokenKind::RBrace, "`}` closing effect body")?;
        if ops.is_empty() {
            // An effect must declare at least one operation. This is a
            // hard parser-level rule rather than a typecheck one
            // because empty `effect E { }` is unambiguously useless and
            // catching it here gives a clearer source-position
            // diagnostic than a downstream "no ops in registry" error.
            self.err(
                start.clone(),
                "effect declaration must contain at least one operation `name: (...) -> T`",
            );
            return None;
        }
        Some(EffectDecl {
            name,
            name_span,
            generic_params,
            resumes_many,
            ops,
            span: start,
        })
    }

    /// Plan B task 53 — recognise the optional `resumes : many`
    /// attribute. Returns `Some(true)` when the three-token sequence
    /// is present and consumed; `Some(false)` when the next token is
    /// `{` (no attribute); `None` on a malformed partial match
    /// (`resumes` followed by something other than `: many`) so the
    /// caller can synchronise.
    fn eat_resumes_many_attr(&mut self) -> Option<bool> {
        let TokenKind::Ident(ref n) = self.peek().kind else {
            return Some(false);
        };
        if n != "resumes" {
            return Some(false);
        }
        let resumes_span = self.peek().span.clone();
        self.advance();
        self.expect(&TokenKind::Colon, "`:` after `resumes`")?;
        let tok = self.peek().clone();
        match tok.kind {
            TokenKind::Ident(ref n) if n == "many" => {
                self.advance();
                Some(true)
            }
            _ => {
                self.err(
                    resumes_span,
                    "expected `many` after `resumes:`; v1 supports `resumes: many` (multi-shot) only",
                );
                None
            }
        }
    }

    /// Plan B task 53 — parse a single operation inside an effect
    /// body: `name : ( T1, T2, ... ) -> R`. The parameter list is
    /// always parenthesised; an empty list `()` is permitted.
    fn parse_effect_op(&mut self) -> Option<EffectOp> {
        let name_tok = self.peek().clone();
        let (name, name_span) = match name_tok.kind {
            TokenKind::Ident(ref n) => {
                self.advance();
                (n.clone(), name_tok.span.clone())
            }
            _ => {
                self.err(name_tok.span.clone(), "expected operation name");
                return None;
            }
        };
        // Plan D Task 115 — per-op generic params after the op name.
        // `parse_generic_params` short-circuits to `Vec::new()` when
        // the next token isn't `[`, so the bare-name surface
        // (`fail: (E) -> Int`) parses unchanged.
        let generic_params = self.parse_generic_params()?;
        self.expect(&TokenKind::Colon, "`:` after operation name")?;
        self.expect(&TokenKind::LParen, "`(` opening operation parameter list")?;
        let mut params: Vec<TypeExpr> = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RParen | TokenKind::Eof) {
            params.push(self.parse_type()?);
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        self.expect(&TokenKind::RParen, "`)` closing operation parameter list")?;
        self.expect(&TokenKind::Arrow, "`->` before operation return type")?;
        let return_type = self.parse_type()?;
        Some(EffectOp {
            name,
            name_span: name_span.clone(),
            generic_params,
            params,
            return_type,
            span: name_span,
        })
    }

    /// Parse `ident ':' type (',' ident ':' type)* ','?` inside `{ ... }`.
    fn parse_record_field_decls(&mut self) -> Option<Vec<RecordFieldDecl>> {
        let mut fields = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RBrace | TokenKind::Eof) {
            let tok = self.peek().clone();
            let fname = match tok.kind {
                TokenKind::Ident(ref n) => {
                    self.advance();
                    n.clone()
                }
                _ => {
                    self.err(tok.span.clone(), "expected field name");
                    return None;
                }
            };
            self.expect(&TokenKind::Colon, "`:` in record field declaration")?;
            let fty = self.parse_type()?;
            fields.push(RecordFieldDecl {
                name: fname,
                ty: fty,
                span: tok.span,
            });
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        Some(fields)
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
                // Constant-fold `-<int-literal>` and `-<float-literal>`.
                if let Expr::IntLit(n, _) = &operand {
                    return Some(Expr::IntLit(n.wrapping_neg(), tok.span));
                }
                if let Expr::FloatLit(f, _) = &operand {
                    return Some(Expr::FloatLit(-f, tok.span));
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
            // Plan A3 task 37: call-argument parsing is a fresh
            // expression context — `if cond_fn(Foo { x: 1 }) { ... }`
            // should parse the record literal normally. Save/restore
            // `no_record_lits` around the argument list.
            let saved = self.no_record_lits;
            self.no_record_lits = false;
            let mut args = Vec::new();
            while !matches!(self.peek().kind, TokenKind::RParen | TokenKind::Eof) {
                args.push(self.parse_expr()?);
                if matches!(self.peek().kind, TokenKind::Comma) {
                    self.advance();
                } else {
                    break;
                }
            }
            self.no_record_lits = saved;
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
            TokenKind::FloatLit(f) => {
                self.advance();
                Some(Expr::FloatLit(f, tok.span))
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
                let head = n.clone();
                let name_span = tok.span.clone();
                self.advance();
                // Plan A3 task 37: record literal `Ctor { f: v, ... }`.
                // Disabled in if-cond / match-scrutinee positions
                // (`no_record_lits`) to avoid ambiguity with the block
                // that follows; parens reset the flag. Note the check
                // is `LBrace` not a broader "compound follow" — a
                // lambda body after `=>` also uses `parse_expr` but the
                // `=>` token is already consumed, so the lambda arm
                // does not enter this path under the wrong flag state.
                if !self.no_record_lits && matches!(self.peek().kind, TokenKind::LBrace) {
                    return self.parse_record_lit(head, name_span);
                }
                // Plan F1 — accumulate dotted-identifier chains as a
                // single dotted Ident name. Used for qualified-name
                // references (`std.list.map`, `Option.Some`, etc.).
                // The typechecker's `Expr::Ident` resolver splits on
                // `.` to classify the form (bare name vs qualified-
                // path).
                //
                // We only walk the chain while the lookahead is `Dot`
                // FOLLOWED BY an `Ident` token. A trailing `.` with
                // no ident (e.g. `e.`) leaves the bare ident in place
                // so downstream parser-recovery sees the same token
                // stream it did pre-Plan-F1.
                //
                // E0151 (field-access) used to fire here at parse
                // time. Under qualified imports, `a.b` is potentially
                // a valid qualified-name reference — we can't tell
                // syntactically. The check moves to resolve-time,
                // which has the import context to decide.
                let mut full = head;
                while matches!(self.peek().kind, TokenKind::Dot) {
                    let after_dot = self.peek_at(1).cloned();
                    let is_ident_next = matches!(
                        after_dot.as_ref().map(|t| &t.kind),
                        Some(TokenKind::Ident(_))
                    );
                    if !is_ident_next {
                        break;
                    }
                    self.advance(); // consume `.`
                    let seg = self.parse_ident("path segment after `.`")?;
                    full.push('.');
                    full.push_str(&seg);
                }
                Some(Expr::Ident(full, name_span))
            }
            TokenKind::Perform => self.parse_perform_expr().map(Expr::Perform),
            TokenKind::LParen => {
                // Parenthesised expression, tuple value, or Unit literal.
                // `()` → UnitLit, `(e)` → paren-grouping, `(e1, e2, …)` → tuple.
                let lparen_span = self.peek().span.clone();
                self.advance();
                let saved = self.no_record_lits;
                self.no_record_lits = false;
                if matches!(self.peek().kind, TokenKind::RParen) {
                    let rparen_span = self.peek().span.clone();
                    self.no_record_lits = saved;
                    self.advance();
                    let span = Span::new(
                        &lparen_span.file,
                        lparen_span.line,
                        lparen_span.column,
                        rparen_span.end_line,
                        rparen_span.end_column,
                    );
                    return Some(Expr::UnitLit(span));
                }
                let first = self.parse_expr()?;
                if matches!(self.peek().kind, TokenKind::Comma) {
                    // Tuple value (arity ≥ 2). `(e,)` with a trailing
                    // comma but no second element is rejected to
                    // preserve the symmetry with type-side parsing
                    // (where arity-1 falls through to paren-grouping
                    // and `(T,)` is currently not a recognised
                    // syntax). The R1 reviewer surfaced that the
                    // initial implementation silently produced an
                    // arity-1 `Expr::Tuple` here; we now diagnose.
                    let mut elems = vec![first];
                    while matches!(self.peek().kind, TokenKind::Comma) {
                        self.advance();
                        // Allow trailing comma before `)`.
                        if matches!(self.peek().kind, TokenKind::RParen) {
                            break;
                        }
                        let e = self.parse_expr()?;
                        elems.push(e);
                    }
                    self.no_record_lits = saved;
                    self.expect(&TokenKind::RParen, "`)` closing tuple value")?;
                    if elems.len() < 2 {
                        self.err(
                            lparen_span.clone(),
                            "tuple values require arity ≥ 2 — `(e,)` with a trailing \
                             comma is not a valid tuple. Use `(e1, e2, ...)` for a \
                             tuple, or remove the trailing comma to write a \
                             parenthesised expression `(e)`",
                        );
                        return None;
                    }
                    Some(Expr::Tuple {
                        elems,
                        span: lparen_span,
                    })
                } else {
                    self.no_record_lits = saved;
                    self.expect(&TokenKind::RParen, "`)` closing parenthesised expression")?;
                    Some(first)
                }
            }
            TokenKind::If => self.parse_if_expr(),
            TokenKind::Match => self.parse_match_expr(),
            TokenKind::Fn => self.parse_lambda_expr(),
            TokenKind::Handle => self.parse_handle_expr(),
            // Plan B Task 55, Phase 4e captures+ Slice B — a brace-
            // delimited block at expression position parses as
            // `Expr::Block(parse_block())`. Required for arm bodies
            // of shape `Effect.op(k) => { let r: Ty = k(arg);
            // pure_tail }` where the lambda-lifted post-arm-k
            // continuation needs the `let` form to bind the k-call's
            // result. Block expressions also parse cleanly in any
            // other expression position (let-binding RHS, match-arm
            // body, lambda body, parenthesised contexts) because
            // `parse_expr` consistently dispatches through here.
            // Fn bodies and if/else branches continue to call
            // `parse_block` directly via dedicated parsers
            // (`parse_fn_decl` / `parse_if_expr`), unaffected by
            // this addition.
            TokenKind::LBrace => self.parse_block().map(|b| Expr::Block(Box::new(b))),
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
        let (effects, effect_row_var) = self.parse_effect_row()?;
        self.expect(&TokenKind::FatArrow, "`=>` before lambda body")?;
        let body = self.parse_expr()?;
        Some(Expr::Lambda {
            params,
            return_type,
            effects,
            effect_row_var,
            body: Box::new(body),
            span: start,
        })
    }

    /// Plan B task 53 — parse a handler expression of the form
    ///
    /// ```text
    /// handle <body-expr> with {
    ///   return(<binding>) => <expr>,           # optional, at most one
    ///   <Effect>.<op>(<param>, ..., <k>) => <expr>,
    ///   ...
    /// }
    /// ```
    ///
    /// The body expression is parsed via the standard expression
    /// grammar but with `no_record_lits` enabled so `handle Foo { ... }
    /// with { ... }` parses as `handle (Ident Foo) with { ... }` rather
    /// than ambiguously trying to consume the brace block as a record
    /// literal. Parenthesising the body restores record-literal parsing.
    ///
    /// At least one operation arm is required. Duplicate `return`
    /// arms are rejected at parse time with an error spanning the
    /// duplicate; the AST keeps the first arm under "first wins"
    /// semantics so a future cross-span diagnostic in Task 54 can
    /// reference both positions through the recorded error and the
    /// preserved AST.
    ///
    /// Each operation arm carries the discharged effect's name, the
    /// op's name, an op-parameter binding list (matching the op's
    /// declared parameters), and a trailing continuation binding `k`
    /// (always present at the surface). Task 54 attaches op-parameter
    /// types from the registered `EffectDecl::ops`.
    fn parse_handle_expr(&mut self) -> Option<Expr> {
        let start = self.peek().span.clone();
        self.expect(&TokenKind::Handle, "`handle`")?;
        // Disable record-literal recognition while parsing the body
        // expression so `handle Foo with { ... }` doesn't try to read
        // `Foo {` as the start of a record literal. Parens restore.
        let saved = self.no_record_lits;
        self.no_record_lits = true;
        let body = self.parse_expr()?;
        self.no_record_lits = saved;
        self.expect(&TokenKind::With, "`with` between handle body and arms")?;
        self.expect(&TokenKind::LBrace, "`{` opening handler arms")?;
        let mut return_arm: Option<Box<HandleReturnArm>> = None;
        let mut op_arms: Vec<HandleOpArm> = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RBrace | TokenKind::Eof) {
            let arm_start = self.peek().span.clone();
            match self.peek().kind {
                TokenKind::Return => {
                    let ra = self.parse_handle_return_arm(arm_start)?;
                    if return_arm.is_some() {
                        // Surface as a parser-level error so the user
                        // sees the duplicate immediately. **First-wins
                        // semantics**: keep the original return arm in
                        // the AST so downstream passes (Task 54+
                        // typechecker, future cross-span diagnostics)
                        // see a stable target. The duplicate is
                        // dropped on the floor; the error span points
                        // at the *second* (offending) arm so the
                        // user's eye lands on the line they need to
                        // remove. Reviewer feedback PR #19 item 2.
                        self.err(
                            ra.span.clone(),
                            "duplicate `return` arm in handler; only one is allowed",
                        );
                    } else {
                        return_arm = Some(Box::new(ra));
                    }
                }
                TokenKind::Ident(_) => {
                    let oa = self.parse_handle_op_arm(arm_start)?;
                    op_arms.push(oa);
                }
                _ => {
                    let span = self.peek().span.clone();
                    self.err(
                        span,
                        "expected `return(<binding>) => ...` or `<Effect>.<op>(...) => ...` in handler arm",
                    );
                    return None;
                }
            }
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        self.expect(&TokenKind::RBrace, "`}` closing handler arms")?;
        if op_arms.is_empty() {
            // A handler that discharges no operations is useless: it
            // cannot interrupt any `perform`. Parser-level rejection
            // gives a clearer message than waiting for the (Task 54)
            // typechecker to flag an empty discharge set.
            self.err(
                start.clone(),
                "handle expression must have at least one operation arm `<Effect>.<op>(..., k) => ...`",
            );
            return None;
        }
        Some(Expr::Handle {
            body: Box::new(body),
            return_arm,
            op_arms,
            span: start,
        })
    }

    /// Parse `return ( <binding> ) => <expr>`, with the leading
    /// `return` already at the cursor (consumed inside this fn).
    fn parse_handle_return_arm(&mut self, arm_start: Span) -> Option<HandleReturnArm> {
        self.expect(&TokenKind::Return, "`return`")?;
        self.expect(&TokenKind::LParen, "`(` opening `return` arm binding")?;
        let bind_tok = self.peek().clone();
        let binding = match bind_tok.kind {
            TokenKind::Ident(ref n) => {
                self.advance();
                n.clone()
            }
            _ => {
                self.err(
                    bind_tok.span.clone(),
                    "expected binding name inside `return(...)` arm",
                );
                return None;
            }
        };
        self.expect(&TokenKind::RParen, "`)` closing `return` arm binding")?;
        self.expect(&TokenKind::FatArrow, "`=>` after `return(<binding>)`")?;
        let body = self.parse_expr()?;
        Some(HandleReturnArm {
            binding,
            binding_span: bind_tok.span,
            body,
            span: arm_start,
        })
    }

    /// Parse `<Effect> '.' <op> '(' <p1>, ..., <k> ')' '=>' <expr>` —
    /// a single operation arm of a `handle`. The trailing parameter is
    /// the continuation binding `k`; at least one parameter is required.
    fn parse_handle_op_arm(&mut self, arm_start: Span) -> Option<HandleOpArm> {
        let eff_tok = self.peek().clone();
        let (effect, effect_span) = match eff_tok.kind {
            TokenKind::Ident(ref n) => {
                self.advance();
                (n.clone(), eff_tok.span.clone())
            }
            _ => {
                self.err(eff_tok.span.clone(), "expected effect name in handler arm");
                return None;
            }
        };
        self.expect(&TokenKind::Dot, "`.` between effect name and operation")?;
        let op_tok = self.peek().clone();
        let (op, op_span) = match op_tok.kind {
            TokenKind::Ident(ref n) => {
                self.advance();
                (n.clone(), op_tok.span.clone())
            }
            _ => {
                self.err(op_tok.span.clone(), "expected operation name after `.`");
                return None;
            }
        };
        self.expect(
            &TokenKind::LParen,
            "`(` opening operation-arm parameter list",
        )?;
        // Collect parameter idents in source order. The trailing one
        // becomes `k`; everything before it is an op parameter binding.
        let mut idents: Vec<HandleArmParam> = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RParen | TokenKind::Eof) {
            let tok = self.peek().clone();
            match tok.kind {
                TokenKind::Ident(ref n) => {
                    self.advance();
                    idents.push(HandleArmParam {
                        name: n.clone(),
                        span: tok.span,
                    });
                }
                _ => {
                    self.err(
                        tok.span.clone(),
                        "expected binding name in handler-arm parameter list",
                    );
                    return None;
                }
            }
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        self.expect(
            &TokenKind::RParen,
            "`)` closing operation-arm parameter list",
        )?;
        // The trailing continuation `k` is always present. Even an
        // op declared with `()` -> R needs a continuation in its arm:
        // `Effect.op(k) => body`. Reject empty arms here so the
        // diagnostic spans the parameter list, not the body. Routing
        // the structural pop through a single `match` keeps the empty
        // case as a real `None` return without going through the
        // `disallowed-methods` `Option::expect` lint.
        let Some(k) = idents.pop() else {
            self.err(
                arm_start.clone(),
                "handler operation arm requires a trailing continuation binding `k`",
            );
            return None;
        };
        self.expect(
            &TokenKind::FatArrow,
            "`=>` after operation-arm parameter list",
        )?;
        let body = self.parse_expr()?;
        Some(HandleOpArm {
            effect,
            effect_span,
            op,
            op_span,
            params: idents,
            k_name: k.name,
            k_span: k.span,
            body,
            span: arm_start,
        })
    }

    fn parse_if_expr(&mut self) -> Option<Expr> {
        let start = self.peek().span.clone();
        self.expect(&TokenKind::If, "`if`")?;
        // Plan A3 task 37: disable record-literal recognition inside the
        // condition so `if Foo { ... } else { ... }` parses as an `if`
        // with an Ident cond and a block body, not an `if` with a
        // record-literal cond. Parens restore the default.
        let saved = self.no_record_lits;
        self.no_record_lits = true;
        let cond = self.parse_expr()?;
        self.no_record_lits = saved;
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
        // Plan A3 task 37: same reasoning as parse_if_expr — disable
        // record-literal recognition while parsing the scrutinee so
        // `match Foo { a => ... }` is the arm form, not a record lit.
        let saved = self.no_record_lits;
        self.no_record_lits = true;
        let scrutinee = self.parse_expr()?;
        self.no_record_lits = saved;
        self.expect(&TokenKind::LBrace, "`{` opening match arms")?;
        let mut arms = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RBrace | TokenKind::Eof) {
            let arm_start = self.peek().span.clone();
            let pattern = self.parse_pattern()?;
            // Plan A3 task 37: reject or-patterns / guards / as-bindings
            // immediately after the first pattern of an arm. The
            // patterns themselves parse cleanly, so the ambiguous
            // tokens only surface here (where `=>` is expected).
            // Fire E0110 for each, then recover by consuming until
            // the next `=>` / `,` / `}`.
            match self.peek().kind {
                TokenKind::Pipe => {
                    let span = self.peek().span.clone();
                    self.errors.push(CompilerError::new(
                        Severity::Error,
                        errors::code("E0110"),
                        span,
                        "or-patterns `p1 | p2` are not supported in v1; write each variant as a separate `match` arm",
                    ));
                    self.recover_to_arm_terminator();
                    if matches!(self.peek().kind, TokenKind::Comma) {
                        self.advance();
                    }
                    continue;
                }
                TokenKind::If => {
                    let span = self.peek().span.clone();
                    self.errors.push(CompilerError::new(
                        Severity::Error,
                        errors::code("E0110"),
                        span,
                        "pattern guards `pat if cond` are not supported in v1; move the condition into the arm body",
                    ));
                    self.recover_to_arm_terminator();
                    if matches!(self.peek().kind, TokenKind::Comma) {
                        self.advance();
                    }
                    continue;
                }
                TokenKind::As => {
                    let span = self.peek().span.clone();
                    self.errors.push(CompilerError::new(
                        Severity::Error,
                        errors::code("E0110"),
                        span,
                        "as-bindings `pat as name` are not supported in v1; introduce bindings via constructor / tuple patterns",
                    ));
                    self.recover_to_arm_terminator();
                    if matches!(self.peek().kind, TokenKind::Comma) {
                        self.advance();
                    }
                    continue;
                }
                _ => {}
            }
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
        // Plan A3 task 37: `|` / `as` / `if` in pattern position are
        // explicitly rejected with E0110 — or-patterns, as-bindings,
        // and guards are deliberate restrictions under fight-the-
        // priors (see the E0110 catalog entry). We reject at the
        // pattern entry point so any syntactic misstep in any pattern
        // position gets a clear message; if we recovered and parsed
        // the LHS first, the error would appear in a surprising
        // position.
        match tok.kind {
            TokenKind::Pipe => {
                self.errors.push(CompilerError::new(
                    Severity::Error,
                    errors::code("E0110"),
                    tok.span.clone(),
                    "or-patterns `p1 | p2` are not supported in v1; write each variant as a separate `match` arm",
                ));
                self.advance();
                return None;
            }
            TokenKind::If => {
                self.errors.push(CompilerError::new(
                    Severity::Error,
                    errors::code("E0110"),
                    tok.span.clone(),
                    "pattern guards `pat if cond` are not supported in v1; move the condition into the arm body",
                ));
                self.advance();
                return None;
            }
            // `as` is a reserved keyword (Plan F1 — import / use
            // aliasing). At a pattern entry point an `as` token is
            // ill-formed regardless of what precedes it; we reject
            // with E0110 here so the diagnostic is "as-bindings are
            // not supported," not the generic "expected pattern"
            // that an unmatched keyword would otherwise produce.
            TokenKind::As => {
                self.errors.push(CompilerError::new(
                    Severity::Error,
                    errors::code("E0110"),
                    tok.span.clone(),
                    "as-bindings `pat as name` are not supported in v1; introduce bindings via constructor / tuple patterns instead",
                ));
                self.advance();
                return None;
            }
            _ => {}
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
            TokenKind::Ident(ref n) => {
                let name = n.clone();
                let name_span = tok.span.clone();
                self.advance();
                // Constructor pattern with positional fields: `Ctor(pat, ...)`.
                if matches!(self.peek().kind, TokenKind::LParen) {
                    self.advance();
                    let mut inner = Vec::new();
                    while !matches!(self.peek().kind, TokenKind::RParen | TokenKind::Eof) {
                        inner.push(self.parse_pattern()?);
                        if matches!(self.peek().kind, TokenKind::Comma) {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    self.expect(&TokenKind::RParen, "`)` closing constructor pattern")?;
                    return Some(Pattern::Ctor {
                        name,
                        fields: CtorPatternFields::Positional(inner),
                        span: name_span,
                    });
                }
                // Constructor pattern with record fields: `Ctor { f, f: p, ... }`.
                if matches!(self.peek().kind, TokenKind::LBrace) {
                    self.advance();
                    let fields = self.parse_ctor_pattern_record_fields()?;
                    self.expect(&TokenKind::RBrace, "`}` closing record pattern")?;
                    return Some(Pattern::Ctor {
                        name,
                        fields: CtorPatternFields::Record(fields),
                        span: name_span,
                    });
                }
                // Bare identifier: `Pattern::Var`. Task 38's typechecker
                // may reinterpret this as a nullary `Ctor` if the name
                // resolves to a constructor for the scrutinee's type.
                Some(Pattern::Var(name, name_span))
            }
            TokenKind::LParen => {
                // `(pat)` is a parenthesised pattern (returns inner);
                // `(pat, pat, ...)` is a tuple pattern.
                self.advance();
                let mut pats = Vec::new();
                let mut saw_comma = false;
                while !matches!(self.peek().kind, TokenKind::RParen | TokenKind::Eof) {
                    pats.push(self.parse_pattern()?);
                    if matches!(self.peek().kind, TokenKind::Comma) {
                        saw_comma = true;
                        self.advance();
                    } else {
                        break;
                    }
                }
                self.expect(&TokenKind::RParen, "`)` closing pattern group")?;
                // clippy::disallowed-methods forbids `Option::unwrap`.
                // We know `pats.len() == 1` here, but destructure
                // defensively so the lint passes.
                if pats.len() == 1 && !saw_comma {
                    let mut it = pats.into_iter();
                    match it.next() {
                        Some(p) => Some(p),
                        None => unreachable!(
                            "parse_pattern: pats.len() == 1 contradicts iter().next() == None"
                        ),
                    }
                } else {
                    Some(Pattern::Tuple(pats, tok.span))
                }
            }
            _ => {
                self.err(
                    tok.span.clone(),
                    "expected pattern (literal, `_`, identifier, constructor, or tuple)",
                );
                None
            }
        }
    }

    /// Parse the body of a record-shaped constructor pattern:
    /// `{ f, f: p, g: q, ... }`. Field-pun `f` is equivalent to
    /// `f: f` (binds a fresh variable with the same name as the field).
    fn parse_ctor_pattern_record_fields(&mut self) -> Option<Vec<CtorPatternField>> {
        let mut fields = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RBrace | TokenKind::Eof) {
            let tok = self.peek().clone();
            let fname = match tok.kind {
                TokenKind::Ident(ref n) => {
                    self.advance();
                    n.clone()
                }
                _ => {
                    self.err(tok.span.clone(), "expected field name in record pattern");
                    return None;
                }
            };
            let pattern = if matches!(self.peek().kind, TokenKind::Colon) {
                self.advance();
                self.parse_pattern()?
            } else {
                // Field-pun: binds a variable of the same name.
                Pattern::Var(fname.clone(), tok.span.clone())
            };
            fields.push(CtorPatternField {
                name: fname,
                pattern,
                span: tok.span,
            });
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        Some(fields)
    }

    /// Plan A3 task 37 — parse a record literal `Ctor { f: v, ... }`.
    /// Invoked from `parse_primary` after having already consumed the
    /// `Ctor` identifier and verified the following `{`. Allows
    /// trailing comma in the field list.
    fn parse_record_lit(&mut self, name: String, name_span: Span) -> Option<Expr> {
        self.expect(&TokenKind::LBrace, "`{` opening record literal")?;
        let mut fields = Vec::new();
        while !matches!(self.peek().kind, TokenKind::RBrace | TokenKind::Eof) {
            let tok = self.peek().clone();
            let fname = match tok.kind {
                TokenKind::Ident(ref n) => {
                    self.advance();
                    n.clone()
                }
                _ => {
                    self.err(tok.span.clone(), "expected field name in record literal");
                    return None;
                }
            };
            self.expect(&TokenKind::Colon, "`:` between field name and value")?;
            // Record literals inside record literals are unambiguous
            // — the `no_record_lits` flag only matters at if-cond /
            // match-scrutinee positions. We explicitly restore the
            // default (allow) for nested field values so an `if`-cond
            // containing a record literal via `(...)` still works.
            let saved = self.no_record_lits;
            self.no_record_lits = false;
            let value = self.parse_expr()?;
            self.no_record_lits = saved;
            fields.push(RecordFieldLit {
                name: fname,
                value,
                span: tok.span,
            });
            if matches!(self.peek().kind, TokenKind::Comma) {
                self.advance();
            } else {
                break;
            }
        }
        self.expect(&TokenKind::RBrace, "`}` closing record literal")?;
        Some(Expr::RecordLit {
            name,
            fields,
            span: name_span,
        })
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
        assert_eq!(
            f.effects
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            vec!["IO"]
        );
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

    // --- Plan F1 — qualified imports + use declarations -------------------

    fn parse_only(src: &str) -> (Program, Vec<CompilerError>) {
        let (toks, lex_errs) = lex("x.sigil", src);
        assert!(lex_errs.is_empty(), "lex errs: {lex_errs:?}");
        parse("x.sigil", &toks)
    }

    #[test]
    fn parses_use_single_binding() {
        let (prog, errs) = parse_only("use std.list.{map};\nfn main() -> Int ![] { 0 }\n");
        assert!(errs.is_empty(), "errs: {errs:?}");
        let Item::Use(u) = &prog.items[0] else {
            panic!("expected Item::Use first, got {:?}", prog.items[0]);
        };
        assert_eq!(u.module_path, vec!["std".to_string(), "list".to_string()]);
        assert_eq!(u.bindings.len(), 1);
        assert_eq!(u.bindings[0].source_name, "map");
        assert_eq!(u.bindings[0].local_name, "map");
    }

    #[test]
    fn parses_use_with_alias() {
        let (prog, errs) =
            parse_only("use std.list.{map as list_map};\nfn main() -> Int ![] { 0 }\n");
        assert!(errs.is_empty(), "errs: {errs:?}");
        let Item::Use(u) = &prog.items[0] else {
            panic!();
        };
        assert_eq!(u.bindings.len(), 1);
        assert_eq!(u.bindings[0].source_name, "map");
        assert_eq!(u.bindings[0].local_name, "list_map");
    }

    #[test]
    fn parses_use_multiple_bindings_with_mixed_aliases() {
        let src = "use std.list.{map, fold as list_fold, filter};\n\
                   fn main() -> Int ![] { 0 }\n";
        let (prog, errs) = parse_only(src);
        assert!(errs.is_empty(), "errs: {errs:?}");
        let Item::Use(u) = &prog.items[0] else {
            panic!();
        };
        let pairs: Vec<(&str, &str)> = u
            .bindings
            .iter()
            .map(|b| (b.source_name.as_str(), b.local_name.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![("map", "map"), ("fold", "list_fold"), ("filter", "filter")]
        );
    }

    #[test]
    fn use_wildcard_is_e0034() {
        let (_prog, errs) = parse_only("use std.list.*;\nfn main() -> Int ![] { 0 }\n");
        assert!(
            errs.iter().any(|e| e.code.as_str() == "E0034"),
            "expected E0034 for wildcard `use`, got {errs:?}"
        );
    }

    #[test]
    fn use_empty_binding_list_is_e0035() {
        let (_prog, errs) = parse_only("use std.list.{};\nfn main() -> Int ![] { 0 }\n");
        assert!(
            errs.iter().any(|e| e.code.as_str() == "E0035"),
            "expected E0035 for empty `use` list, got {errs:?}"
        );
    }

    #[test]
    fn use_nested_binding_path_is_rejected() {
        let (_prog, errs) = parse_only("use std.list.{map.fold};\nfn main() -> Int ![] { 0 }\n");
        // Doesn't pin a specific code — recovery may surface E0010 or
        // similar. The point is that the binding-list parser refuses
        // dotted names; assert at least one error fires.
        assert!(!errs.is_empty(), "expected at least one parse error");
    }

    #[test]
    fn user_use_is_e0031() {
        let (_prog, errs) = parse_only("use mylib.foo.{x};\nfn main() -> Int ![] { 0 }\n");
        assert!(
            errs.iter().any(|e| e.code.as_str() == "E0031"),
            "expected E0031 for non-std `use`, got {errs:?}"
        );
    }

    #[test]
    fn parses_import_with_alias() {
        let (prog, errs) = parse_only("import std.option as O;\nfn main() -> Int ![] { 0 }\n");
        assert!(errs.is_empty(), "errs: {errs:?}");
        let Item::Import(imp) = &prog.items[0] else {
            panic!("expected Item::Import first");
        };
        assert_eq!(imp.path, vec!["std".to_string(), "option".to_string()]);
        assert_eq!(imp.alias.as_deref(), Some("O"));
    }

    #[test]
    fn parses_import_without_alias_still_works() {
        let (prog, errs) = parse_only("import std.list\nfn main() -> Int ![] { 0 }\n");
        assert!(errs.is_empty(), "errs: {errs:?}");
        let Item::Import(imp) = &prog.items[0] else {
            panic!();
        };
        assert!(imp.alias.is_none());
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
                assert_eq!(return_type.head_name(), "Int");
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
                assert_eq!(params[0].ty.head_name(), "Int");
                assert_eq!(return_type.head_name(), "Int");
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
                assert_eq!(
                    effects.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
                    vec!["IO"]
                );
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

    /// Plan F1 — `IDENT.IDENT` in expression position now parses as
    /// a qualified-path reference (`Expr::Ident("e.score", _)`) and
    /// no longer fires E0151 at parse time. The diagnostic for an
    /// unresolved qualified path moves to typecheck (E0046 / unknown
    /// identifier with the dotted name), where the import context is
    /// known.
    #[test]
    fn ident_dot_ident_parses_as_dotted_ident() {
        let errs = parse_errs(
            "fn main() -> Int ![IO] {\n  \
                 let x: Int = e.score;\n  \
                 0\n\
               }\n",
        );
        assert!(
            !errs.iter().any(|e| e.code.as_str() == "E0151"),
            "Plan F1: parser should accept `e.score` as a qualified-path \
             ident — typecheck emits the unresolved diagnostic. got: {errs:?}"
        );
    }

    /// `import std.list` — module-path dots in imports MUST NOT fire
    /// E0151. Imports are parsed by `parse_import` (separate path),
    /// so this is a sanity check that the parse_primary E0151 doesn't
    /// leak into import handling.
    #[test]
    fn import_dot_does_not_fire_e0151() {
        let errs = parse_errs(
            "import std.list\n\n\
               fn main() -> Int ![] { 0 }\n",
        );
        assert!(
            !errs.iter().any(|e| e.code.as_str() == "E0151"),
            "import dot should not fire E0151: {errs:?}"
        );
    }

    /// `perform IO.println(...)` — effect-op syntax uses a dot
    /// between the effect name and the operation. This goes through
    /// `parse_perform_expr`, NOT `parse_primary`'s Ident arm, so
    /// E0151 must not fire.
    #[test]
    fn perform_effect_dot_does_not_fire_e0151() {
        let errs = parse_errs(
            "fn main() -> Int ![IO] {\n  \
                 perform IO.println(\"hi\");\n  \
                 0\n\
               }\n",
        );
        assert!(
            !errs.iter().any(|e| e.code.as_str() == "E0151"),
            "effect-op dot should not fire E0151: {errs:?}"
        );
    }

    // ===== Plan A3 Task 37 — Stage 4 grammar ============================

    fn parse_ok(src: &str) -> Program {
        let (toks, lex_errs) = lex("t.sigil", src);
        assert!(lex_errs.is_empty(), "lex errs: {lex_errs:?}");
        let (prog, parse_errs) = parse("t.sigil", &toks);
        assert!(parse_errs.is_empty(), "parse errs: {parse_errs:?}");
        prog
    }

    fn parse_tail_pattern(src: &str) -> Pattern {
        // Wraps `src` as the pattern of a single-arm match. Easier than
        // standing up a full program for pattern experiments.
        let full = format!("fn main() -> Int ![] {{ match 0 {{ {src} => 0 }} }}");
        let prog = parse_ok(&full);
        let Item::Fn(ref f) = prog.items[0] else {
            panic!("expected fn decl")
        };
        let Expr::Match { ref arms, .. } = f.body.tail.clone().expect("tail expr") else {
            panic!("expected match expr")
        };
        arms[0].pattern.clone()
    }

    #[test]
    fn type_decl_unit_and_positional_variants() {
        // The canonical Option shape. Two variants: one unit, one
        // positional with a single Int.
        let prog = parse_ok("type Option = | None | Some(Int)\nfn main() -> Int ![] { 0 }\n");
        let Item::Type(ref t) = prog.items[0] else {
            panic!("expected type decl, got {:?}", prog.items[0])
        };
        assert_eq!(t.name, "Option");
        assert_eq!(t.variants.len(), 2);
        assert_eq!(t.variants[0].name, "None");
        assert!(matches!(t.variants[0].fields, VariantFields::Unit));
        assert_eq!(t.variants[1].name, "Some");
        match &t.variants[1].fields {
            VariantFields::Positional(tys) => {
                assert_eq!(tys.len(), 1);
                assert!(matches!(&tys[0], TypeExpr::Named(n, _) if n == "Int"));
            }
            other => panic!("expected Positional, got {other:?}"),
        }
    }

    #[test]
    fn type_decl_record_variant() {
        // A sum with a single record-shaped variant.
        let prog = parse_ok(
            "type Shape = | Circle { radius: Int } | Rect { w: Int, h: Int }\n\
             fn main() -> Int ![] { 0 }\n",
        );
        let Item::Type(ref t) = prog.items[0] else {
            panic!()
        };
        assert_eq!(t.variants.len(), 2);
        match &t.variants[0].fields {
            VariantFields::Record(fields) => {
                assert_eq!(fields.len(), 1);
                assert_eq!(fields[0].name, "radius");
            }
            other => panic!("expected Record, got {other:?}"),
        }
        match &t.variants[1].fields {
            VariantFields::Record(fields) => {
                assert_eq!(fields.len(), 2);
                assert_eq!(fields[0].name, "w");
                assert_eq!(fields[1].name, "h");
            }
            other => panic!("expected Record, got {other:?}"),
        }
    }

    #[test]
    fn type_decl_single_ctor_record_shorthand() {
        // `type Name = { fields }` desugars to one variant named Name.
        let prog = parse_ok("type Point = { x: Int, y: Int }\nfn main() -> Int ![] { 0 }\n");
        let Item::Type(ref t) = prog.items[0] else {
            panic!()
        };
        assert_eq!(t.name, "Point");
        assert_eq!(t.variants.len(), 1);
        assert_eq!(t.variants[0].name, "Point");
        match &t.variants[0].fields {
            VariantFields::Record(fields) => {
                assert_eq!(fields.len(), 2);
                assert_eq!(fields[0].name, "x");
                assert_eq!(fields[1].name, "y");
            }
            other => panic!("expected Record shorthand, got {other:?}"),
        }
    }

    #[test]
    fn type_decl_trailing_comma_in_fields() {
        // Trailing commas tolerated in both positional and record fields.
        let prog = parse_ok(
            "type Pair = | P(Int, Int,)\n\
             type Loc = { line: Int, col: Int, }\n\
             fn main() -> Int ![] { 0 }\n",
        );
        assert_eq!(
            prog.items
                .iter()
                .filter(|i| matches!(i, Item::Type(_)))
                .count(),
            2
        );
    }

    #[test]
    fn type_decl_without_variants_errors() {
        // `type Empty =` alone is a parse error — need either `| Ctor` or `{ .. }`.
        let errs = parse_errs("type Empty =\nfn main() -> Int ![] { 0 }\n");
        assert!(!errs.is_empty(), "empty type decl should parse-error");
    }

    #[test]
    fn record_literal_basic() {
        // `Point { x: 1, y: 2 }` in a let RHS. Unambiguous — no if/match around.
        let prog = parse_ok(
            "type Point = { x: Int, y: Int }\n\
             fn main() -> Int ![] { let p: Point = Point { x: 1, y: 2 }; 0 }\n",
        );
        let Item::Fn(ref f) = prog.items[1] else {
            panic!()
        };
        let Stmt::Let(ref l) = f.body.stmts[0] else {
            panic!()
        };
        match &l.value {
            Expr::RecordLit { name, fields, .. } => {
                assert_eq!(name, "Point");
                assert_eq!(fields.len(), 2);
                assert_eq!(fields[0].name, "x");
                assert_eq!(fields[1].name, "y");
            }
            other => panic!("expected RecordLit, got {other:?}"),
        }
    }

    #[test]
    fn record_literal_disabled_in_if_cond() {
        // `if Foo { ... } else { ... }` — the `{` starts the then-block,
        // not a record literal. The cond resolves to `Ident("Foo")`
        // (which typecheck will later reject if it isn't a Bool —
        // but the parser must accept this shape).
        let prog = parse_ok("fn main() -> Int ![] { if Foo { 1 } else { 2 } }\n");
        let Item::Fn(ref f) = prog.items[0] else {
            panic!()
        };
        match f.body.tail.as_ref().expect("tail expr") {
            Expr::If { cond, .. } => match cond.as_ref() {
                Expr::Ident(n, _) => assert_eq!(n, "Foo"),
                other => panic!("cond should be Ident, got {other:?}"),
            },
            other => panic!("expected If, got {other:?}"),
        }
    }

    #[test]
    fn record_literal_disabled_in_match_scrutinee() {
        // Same reasoning as if-cond: `match Foo { ... }` — scrutinee
        // is `Ident("Foo")`.
        let prog = parse_ok("fn main() -> Int ![] { match Foo { _ => 0 } }\n");
        let Item::Fn(ref f) = prog.items[0] else {
            panic!()
        };
        match f.body.tail.as_ref().expect("tail expr") {
            Expr::Match { scrutinee, .. } => match scrutinee.as_ref() {
                Expr::Ident(n, _) => assert_eq!(n, "Foo"),
                other => panic!("scrutinee should be Ident, got {other:?}"),
            },
            other => panic!("expected Match, got {other:?}"),
        }
    }

    #[test]
    fn record_literal_in_parens_inside_if_cond() {
        // `if (Foo { x: 1 }).flag { ... }` — the record lit is
        // inside parens, so `no_record_lits` resets and the lit
        // parses. We can't directly probe a field access (no field
        // access syntax in A3), so test the shape with a call that
        // carries the record lit as an arg instead.
        let prog = parse_ok(
            "type Foo = { x: Int }\n\
             fn is_ok(f: Foo) -> Bool ![] { true }\n\
             fn main() -> Int ![] { if is_ok(Foo { x: 1 }) { 1 } else { 2 } }\n",
        );
        let Item::Fn(ref main_fn) = prog.items[2] else {
            panic!()
        };
        match main_fn.body.tail.as_ref().expect("tail") {
            Expr::If { cond, .. } => match cond.as_ref() {
                Expr::Call { args, .. } => match &args[0] {
                    Expr::RecordLit { name, .. } => assert_eq!(name, "Foo"),
                    other => panic!("expected RecordLit arg, got {other:?}"),
                },
                other => panic!("expected Call cond, got {other:?}"),
            },
            other => panic!("expected If, got {other:?}"),
        }
    }

    #[test]
    fn record_literal_in_call_arg_of_match_scrutinee() {
        // `match f(Foo { x: 1 }) { ... }` — the match scrutinee sets
        // `no_record_lits = true`, but the call's arg list restores
        // the flag so the record literal parses. Pins the
        // flag-reset-through-call-arg-list path in the compound
        // match-scrutinee position (twin of the if-cond case above).
        let prog = parse_ok(
            "type Foo = { x: Int }\n\
             fn tag(f: Foo) -> Int ![] { 1 }\n\
             fn main() -> Int ![] { match tag(Foo { x: 1 }) { 1 => 10, _ => 20 } }\n",
        );
        let Item::Fn(ref main_fn) = prog.items[2] else {
            panic!()
        };
        match main_fn.body.tail.as_ref().expect("tail") {
            Expr::Match { scrutinee, .. } => match scrutinee.as_ref() {
                Expr::Call { args, .. } => match &args[0] {
                    Expr::RecordLit { name, .. } => assert_eq!(name, "Foo"),
                    other => panic!("expected RecordLit arg, got {other:?}"),
                },
                other => panic!("expected Call scrutinee, got {other:?}"),
            },
            other => panic!("expected Match, got {other:?}"),
        }
    }

    #[test]
    fn pattern_var_binds_identifier() {
        // A bare identifier in pattern position becomes `Pattern::Var`.
        let p = parse_tail_pattern("x");
        assert!(matches!(p, Pattern::Var(ref n, _) if n == "x"));
    }

    #[test]
    fn pattern_positional_ctor() {
        // `Some(n)` — positional constructor pattern with a var inside.
        let p = parse_tail_pattern("Some(n)");
        match p {
            Pattern::Ctor { name, fields, .. } => {
                assert_eq!(name, "Some");
                match fields {
                    CtorPatternFields::Positional(inner) => {
                        assert_eq!(inner.len(), 1);
                        assert!(matches!(inner[0], Pattern::Var(ref n, _) if n == "n"));
                    }
                    other => panic!("expected Positional, got {other:?}"),
                }
            }
            other => panic!("expected Ctor, got {other:?}"),
        }
    }

    #[test]
    fn pattern_record_ctor_with_pun_and_rename() {
        // `Point { x, y: py }` — x puns, y renames to py.
        let p = parse_tail_pattern("Point { x, y: py }");
        match p {
            Pattern::Ctor { name, fields, .. } => {
                assert_eq!(name, "Point");
                match fields {
                    CtorPatternFields::Record(rf) => {
                        assert_eq!(rf.len(), 2);
                        assert_eq!(rf[0].name, "x");
                        assert!(matches!(rf[0].pattern, Pattern::Var(ref n, _) if n == "x"));
                        assert_eq!(rf[1].name, "y");
                        assert!(matches!(rf[1].pattern, Pattern::Var(ref n, _) if n == "py"));
                    }
                    other => panic!("expected Record, got {other:?}"),
                }
            }
            other => panic!("expected Ctor, got {other:?}"),
        }
    }

    #[test]
    fn pattern_tuple() {
        let p = parse_tail_pattern("(a, b)");
        match p {
            Pattern::Tuple(pats, _) => {
                assert_eq!(pats.len(), 2);
                assert!(matches!(pats[0], Pattern::Var(ref n, _) if n == "a"));
                assert!(matches!(pats[1], Pattern::Var(ref n, _) if n == "b"));
            }
            other => panic!("expected Tuple, got {other:?}"),
        }
    }

    #[test]
    fn pattern_parenthesised_single_is_inner() {
        // `(a)` is a parenthesised pattern, not a 1-tuple.
        let p = parse_tail_pattern("(a)");
        assert!(matches!(p, Pattern::Var(ref n, _) if n == "a"));
    }

    #[test]
    fn pattern_or_pattern_is_e0110() {
        // `Foo | Bar => ...` — or-pattern rejected at parse time.
        let errs = parse_errs("fn main() -> Int ![] { match 0 { Foo | Bar => 0 } }\n");
        assert!(
            errs.iter().any(|e| e.code.as_str() == "E0110"),
            "expected E0110 for or-pattern, got: {errs:?}"
        );
    }

    #[test]
    fn pattern_guard_is_e0110() {
        // `Some(n) if n > 0 => ...` — guard rejected at parse time.
        let errs = parse_errs("fn main() -> Int ![] { match 0 { Some(n) if n > 0 => 0 } }\n");
        // Guards land at the arm-body parser entry after the first
        // pattern, not at the pattern itself — we can still see E0110
        // because `if` in any pattern position errors. (The second
        // pattern slot — after `|` — is what actually triggers; here
        // the grammar gets confused by `if` showing up where a `=>` is
        // expected, but E0110 still fires from a nested parse_pattern.)
        assert!(
            errs.iter().any(|e| e.code.as_str() == "E0110"),
            "expected E0110 for pattern guard, got: {errs:?}"
        );
    }

    #[test]
    fn pattern_as_binding_is_e0110() {
        // `Pair(a, b) as whole => ...` — as-binding rejected. `as` is
        // an Ident token, but our parse_pattern catches it
        // specifically.
        let errs = parse_errs("fn main() -> Int ![] { match 0 { x as whole => 0 } }\n");
        assert!(
            errs.iter().any(|e| e.code.as_str() == "E0110"),
            "expected E0110 for as-binding, got: {errs:?}"
        );
    }

    // ===== Plan B Task 47 — generic params + row variables =====

    fn parse_str(src: &str) -> Program {
        let (toks, lex_errs) = lex("t.sigil", src);
        assert!(lex_errs.is_empty(), "lex errors: {lex_errs:?}");
        let (prog, parse_errs) = parse("t.sigil", &toks);
        assert!(parse_errs.is_empty(), "parse errors: {parse_errs:?}");
        prog
    }

    fn first_fn(p: &Program) -> &FnDecl {
        for it in &p.items {
            if let Item::Fn(f) = it {
                return f;
            }
        }
        panic!("no fn in program");
    }

    fn first_type(p: &Program) -> &TypeDecl {
        for it in &p.items {
            if let Item::Type(td) = it {
                return td;
            }
        }
        panic!("no type decl in program");
    }

    #[test]
    fn fn_with_no_generic_params_has_empty_list() {
        // Backward-compat guard: existing non-generic fn signatures
        // continue to parse with `generic_params: []`.
        let p = parse_str("fn id(x: Int) -> Int ![] { x }\n");
        let f = first_fn(&p);
        assert!(f.generic_params.is_empty());
        assert!(f.effect_row_var.is_none());
    }

    #[test]
    fn fn_with_single_generic_param_parses() {
        let p = parse_str("fn id[A](x: A) -> A ![] { x }\n");
        let f = first_fn(&p);
        assert_eq!(f.generic_params.len(), 1);
        assert_eq!(f.generic_params[0].name, "A");
        assert_eq!(f.params[0].ty.head_name(), "A");
        assert_eq!(f.return_type.head_name(), "A");
    }

    #[test]
    fn fn_with_two_generic_params_parses() {
        let p = parse_str("fn pair[A, B](a: A, b: B) -> Int ![] { 0 }\n");
        let f = first_fn(&p);
        assert_eq!(f.generic_params.len(), 2);
        assert_eq!(f.generic_params[0].name, "A");
        assert_eq!(f.generic_params[1].name, "B");
    }

    #[test]
    fn type_application_in_param_position_parses_as_apply() {
        let p = parse_str("fn f(xs: List[Int]) -> Int ![] { 0 }\n");
        let f = first_fn(&p);
        match &f.params[0].ty {
            TypeExpr::Apply { name, args, .. } => {
                assert_eq!(name, "List");
                assert_eq!(args.len(), 1);
                assert_eq!(args[0].head_name(), "Int");
            }
            other => panic!("expected Apply, got {other:?}"),
        }
    }

    #[test]
    fn nested_type_application_parses_recursively() {
        let p = parse_str("fn f(m: Map[String, List[Int]]) -> Int ![] { 0 }\n");
        let f = first_fn(&p);
        match &f.params[0].ty {
            TypeExpr::Apply { name, args, .. } => {
                assert_eq!(name, "Map");
                assert_eq!(args.len(), 2);
                assert_eq!(args[0].head_name(), "String");
                match &args[1] {
                    TypeExpr::Apply { name, args, .. } => {
                        assert_eq!(name, "List");
                        assert_eq!(args.len(), 1);
                        assert_eq!(args[0].head_name(), "Int");
                    }
                    other => panic!("expected nested Apply, got {other:?}"),
                }
            }
            other => panic!("expected outer Apply, got {other:?}"),
        }
    }

    #[test]
    fn explicit_row_variable_parses() {
        let p = parse_str("fn f(x: Int) -> Int ![IO | e] { x }\n");
        let f = first_fn(&p);
        assert_eq!(
            f.effects
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            vec!["IO"]
        );
        let rv = f
            .effect_row_var
            .as_ref()
            .expect("expected row variable `e`");
        assert_eq!(rv.name, "e");
    }

    #[test]
    fn row_variable_alone_with_no_effects_parses() {
        // `![| r]` introduces only a row variable, no concrete effects.
        let p = parse_str("fn f(x: Int) -> Int ![| r] { x }\n");
        let f = first_fn(&p);
        assert!(f.effects.is_empty());
        assert_eq!(
            f.effect_row_var.as_ref().map(|r| r.name.as_str()),
            Some("r")
        );
    }

    #[test]
    fn type_decl_with_generic_param_parses() {
        let p = parse_str("type List[A] = | Nil | Cons(A, List[A])\n");
        let td = first_type(&p);
        assert_eq!(td.generic_params.len(), 1);
        assert_eq!(td.generic_params[0].name, "A");
        assert_eq!(td.variants.len(), 2);
        // The Cons variant's first field is `A`, second is `List[A]`.
        match &td.variants[1].fields {
            VariantFields::Positional(field_tes) => {
                assert_eq!(field_tes.len(), 2);
                assert_eq!(field_tes[0].head_name(), "A");
                match &field_tes[1] {
                    TypeExpr::Apply { name, args, .. } => {
                        assert_eq!(name, "List");
                        assert_eq!(args[0].head_name(), "A");
                    }
                    other => panic!("expected Apply, got {other:?}"),
                }
            }
            other => panic!("expected Positional, got {other:?}"),
        }
    }

    #[test]
    fn type_decl_with_two_generic_params_parses() {
        let p = parse_str("type Pair[A, B] = | P(A, B)\n");
        let td = first_type(&p);
        assert_eq!(td.generic_params.len(), 2);
        assert_eq!(td.generic_params[0].name, "A");
        assert_eq!(td.generic_params[1].name, "B");
    }

    #[test]
    fn record_type_decl_with_generic_params_parses() {
        let p = parse_str("type Wrapper[A] = { value: A }\n");
        let td = first_type(&p);
        assert_eq!(td.generic_params.len(), 1);
        assert_eq!(td.generic_params[0].name, "A");
    }

    #[test]
    fn lambda_with_row_variable_parses() {
        // Lambdas accept `![IO | e]` row syntax too.
        let e = parse_tail_expr("fn (x: Int) -> Int ![IO | e] => x");
        match e {
            Expr::Lambda {
                effects,
                effect_row_var,
                ..
            } => {
                assert_eq!(
                    effects.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
                    vec!["IO"]
                );
                assert_eq!(effect_row_var.map(|r| r.name), Some("e".to_string()));
            }
            other => panic!("expected Lambda, got {other:?}"),
        }
    }

    #[test]
    fn closed_row_has_no_row_variable() {
        // Regression-guard: pre-Plan-B `![IO]` parses as closed row.
        let p = parse_str("fn f(x: Int) -> Int ![IO] { x }\n");
        let f = first_fn(&p);
        assert_eq!(
            f.effects
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            vec!["IO"]
        );
        assert!(f.effect_row_var.is_none());
    }

    // ===== Plan B Task 47 — negative-path / malformed-syntax pinning =====

    #[test]
    fn generic_params_trailing_comma_is_accepted() {
        // `[A,]` — pin trailing comma as accepted, matching the
        // param-list / argument-list convention elsewhere in the
        // grammar. If a future plan adds bounds (`[A: Foo, B,]`) the
        // trailing-comma rule still applies.
        let p = parse_str("fn f[A,](x: A) -> A ![] { x }\n");
        let f = first_fn(&p);
        assert_eq!(f.generic_params.len(), 1);
        assert_eq!(f.generic_params[0].name, "A");
    }

    #[test]
    fn generic_params_missing_comma_errors() {
        // `[A B]` — no comma between parameters. The expect(`]`) at
        // the end of `parse_generic_params` fails on the second ident
        // and the entire fn-decl parse aborts with at least one error.
        let errs = parse_errs("fn f[A B](x: A) -> A ![] { x }\n");
        assert!(
            !errs.is_empty(),
            "missing comma between generic params should parse-error"
        );
    }

    #[test]
    fn row_pipe_with_no_row_var_errors() {
        // `![| ]` — pipe present but no row-variable identifier
        // follows. Should produce a parser error pointing at the
        // missing name.
        let errs = parse_errs("fn f(x: Int) -> Int ![| ] { x }\n");
        assert!(
            errs.iter().any(|e| e.message.contains("row-variable")),
            "pipe with absent row-var name should parse-error; got {errs:?}"
        );
    }

    #[test]
    fn row_pipe_after_effects_with_no_var_errors() {
        // `![IO |]` — effects followed by pipe followed by `]`. The
        // pipe branch fires the same "expected row-variable name"
        // diagnostic as the no-effect case.
        let errs = parse_errs("fn f(x: Int) -> Int ![IO |] { x }\n");
        assert!(
            errs.iter().any(|e| e.message.contains("row-variable")),
            "pipe with no row-var after effects should parse-error; got {errs:?}"
        );
    }

    // ===== Plan B task 53 — effect declarations and handle expressions ====

    fn parse_first_item(src: &str) -> Item {
        let (toks, lex_errs) = lex("t.sigil", src);
        assert!(lex_errs.is_empty(), "lex errs: {lex_errs:?}");
        let (prog, parse_errs) = parse("t.sigil", &toks);
        assert!(parse_errs.is_empty(), "parse errs: {parse_errs:?}");
        prog.items
            .into_iter()
            .next()
            .expect("expected at least one top-level item")
    }

    fn parse_clean(src: &str) -> Program {
        let (toks, lex_errs) = lex("t.sigil", src);
        assert!(lex_errs.is_empty(), "lex errs: {lex_errs:?}");
        let (prog, parse_errs) = parse("t.sigil", &toks);
        assert!(parse_errs.is_empty(), "parse errs: {parse_errs:?}");
        prog
    }

    #[test]
    fn effect_decl_one_op_default_one_shot() {
        // Default form: no `resumes: many`. resumes_many should be false.
        let item = parse_first_item("effect Raise { fail: (String) -> Int }\n");
        let Item::Effect(ed) = item else {
            panic!("expected Item::Effect, got {item:?}")
        };
        assert_eq!(ed.name, "Raise");
        assert!(ed.generic_params.is_empty());
        assert!(!ed.resumes_many);
        assert_eq!(ed.ops.len(), 1);
        assert_eq!(ed.ops[0].name, "fail");
        assert_eq!(ed.ops[0].params.len(), 1);
        assert_eq!(ed.ops[0].params[0].head_name(), "String");
        assert_eq!(ed.ops[0].return_type.head_name(), "Int");
    }

    #[test]
    fn effect_decl_with_generic_params() {
        // `effect Raise[T] { fail: (String) -> T }` — generic effect.
        let item = parse_first_item("effect Raise[T] { fail: (String) -> T }\n");
        let Item::Effect(ed) = item else {
            panic!("expected Item::Effect")
        };
        assert_eq!(ed.generic_params.len(), 1);
        assert_eq!(ed.generic_params[0].name, "T");
        assert_eq!(ed.ops[0].return_type.head_name(), "T");
    }

    #[test]
    fn effect_decl_resumes_many_attr() {
        // Multi-shot form via `resumes: many`.
        let item = parse_first_item("effect Choose resumes: many { choose: (Int) -> Int }\n");
        let Item::Effect(ed) = item else {
            panic!("expected Item::Effect")
        };
        assert!(ed.resumes_many);
        assert_eq!(ed.name, "Choose");
        assert_eq!(ed.ops.len(), 1);
    }

    #[test]
    fn effect_decl_resumes_many_with_generics() {
        // `effect Choose[T] resumes: many { ... }` — both attributes.
        let item = parse_first_item("effect Choose[T] resumes: many { pick: (T, T) -> T }\n");
        let Item::Effect(ed) = item else {
            panic!("expected Item::Effect")
        };
        assert_eq!(ed.generic_params.len(), 1);
        assert!(ed.resumes_many);
        assert_eq!(ed.ops[0].params.len(), 2);
    }

    #[test]
    fn effect_decl_multiple_ops_with_trailing_comma() {
        let item = parse_first_item("effect State[T] { get: () -> T, put: (T) -> Unit, }\n");
        let Item::Effect(ed) = item else {
            panic!("expected Item::Effect")
        };
        assert_eq!(ed.ops.len(), 2);
        assert_eq!(ed.ops[0].name, "get");
        assert!(ed.ops[0].params.is_empty());
        assert_eq!(ed.ops[1].name, "put");
        assert_eq!(ed.ops[1].params.len(), 1);
    }

    #[test]
    fn effect_decl_empty_body_errors() {
        // `effect E { }` — no ops; rejected at parser level.
        let errs = parse_errs("effect E { }\n");
        assert!(
            errs.iter()
                .any(|e| e.message.contains("at least one operation")),
            "empty effect body should parse-error; got {errs:?}"
        );
    }

    #[test]
    fn effect_decl_resumes_without_many_errors() {
        // `effect E resumes: noway { ... }` — only `many` is accepted
        // after `resumes:` in v1. The diagnostic spans the `resumes`
        // keyword to anchor the user-visible position.
        let errs = parse_errs("effect E resumes: noway { fail: () -> Int }\n");
        assert!(
            errs.iter().any(|e| e.message.contains("`many`")),
            "non-`many` resumes value should parse-error; got {errs:?}"
        );
    }

    #[test]
    fn resumes_outside_effect_decl_remains_ident() {
        // `resumes` is a context keyword. Outside `effect ... resumes :
        // many`, it must lex+parse as a plain Ident — no regression for
        // user code that wants `let resumes = 5`.
        let prog = parse_clean(
            "fn main() -> Int ![] {\n  let resumes: Int = 5;\n  let many: Int = 9;\n  resumes\n}\n",
        );
        let Item::Fn(ref f) = prog.items[0] else {
            panic!()
        };
        assert_eq!(f.body.stmts.len(), 2);
        match &f.body.tail {
            Some(Expr::Ident(n, _)) => assert_eq!(n, "resumes"),
            other => panic!("expected tail Ident `resumes`, got {other:?}"),
        }
    }

    #[test]
    fn effect_as_user_ident_in_let_is_rejected() {
        // Reviewer feedback PR #19 item 9 — symmetric keyword-side
        // adversarial. `effect` IS a lexer keyword as of Task 53, so
        // `let effect: Int = 1;` cannot parse: the `let` arm expects
        // an Ident binding name and gets a `TokenKind::Effect`
        // instead, producing a parser-level error. This is the
        // counterpart to `resumes_outside_effect_decl_remains_ident`,
        // which pins the *non*-reserved attribute words to the Ident
        // path; together they cover both halves of the keyword
        // matrix.
        let errs = parse_errs("fn main() -> Int ![] { let effect: Int = 1; 0 }\n");
        assert!(
            !errs.is_empty(),
            "`let effect: Int = 1` must parse-error because `effect` is reserved; got no errors",
        );
    }

    #[test]
    fn handle_with_keywords_rejected_in_user_ident_position() {
        // `handle` and `with` are reserved too. Same argument as
        // above: a `let handle = 1` shape must not silently rebind
        // them as user identifiers. Together with the `effect` test,
        // this pins all three Stage-6 keywords.
        let h_errs = parse_errs("fn main() -> Int ![] { let handle: Int = 1; 0 }\n");
        assert!(
            !h_errs.is_empty(),
            "`let handle: Int = 1` must parse-error because `handle` is reserved; got no errors",
        );
        let w_errs = parse_errs("fn main() -> Int ![] { let with: Int = 1; 0 }\n");
        assert!(
            !w_errs.is_empty(),
            "`let with: Int = 1` must parse-error because `with` is reserved; got no errors",
        );
    }

    #[test]
    fn handle_expr_minimal_form() {
        // Smallest possible handle: one op arm, no return arm.
        let e = parse_tail_expr("handle 0 with { Raise.fail(msg, k) => 0 }");
        let Expr::Handle {
            return_arm,
            op_arms,
            ..
        } = e
        else {
            panic!("expected Expr::Handle")
        };
        assert!(return_arm.is_none());
        assert_eq!(op_arms.len(), 1);
        let arm = &op_arms[0];
        assert_eq!(arm.effect, "Raise");
        assert_eq!(arm.op, "fail");
        assert_eq!(arm.params.len(), 1);
        assert_eq!(arm.params[0].name, "msg");
        assert_eq!(arm.k_name, "k");
    }

    #[test]
    fn handle_expr_with_return_arm() {
        let e = parse_tail_expr("handle 42 with { return(v) => v, Raise.fail(msg, k) => 0 }");
        let Expr::Handle {
            return_arm,
            op_arms,
            ..
        } = e
        else {
            panic!("expected Expr::Handle")
        };
        let ra = return_arm.expect("expected return arm");
        assert_eq!(ra.binding, "v");
        assert!(matches!(&ra.body, Expr::Ident(n, _) if n == "v"));
        assert_eq!(op_arms.len(), 1);
    }

    #[test]
    fn handle_expr_multiple_op_arms() {
        // Multi-effect handle: arms from different effects share one
        // handler. This form is the parser-level rationale for the
        // qualified `Effect.op(...)` arm shape.
        let e = parse_tail_expr(
            "handle 0 with { Raise.fail(msg, k) => 0, State.get(k) => 1, State.put(v, k) => 2 }",
        );
        let Expr::Handle { op_arms, .. } = e else {
            panic!()
        };
        assert_eq!(op_arms.len(), 3);
        assert_eq!(op_arms[0].effect, "Raise");
        assert_eq!(op_arms[1].effect, "State");
        assert_eq!(op_arms[1].op, "get");
        assert!(op_arms[1].params.is_empty()); // `get` has only `k`
        assert_eq!(op_arms[1].k_name, "k");
        assert_eq!(op_arms[2].effect, "State");
        assert_eq!(op_arms[2].op, "put");
        assert_eq!(op_arms[2].params.len(), 1);
    }

    #[test]
    fn handle_expr_op_arm_continuation_is_last_param() {
        // Verify the trailing-param-becomes-k convention: in
        // `Effect.op(a, b, c, k) => body`, `c` is an op param and `k`
        // is the continuation.
        let e = parse_tail_expr("handle 0 with { E.op(a, b, c, k) => 0 }");
        let Expr::Handle { op_arms, .. } = e else {
            panic!()
        };
        let arm = &op_arms[0];
        assert_eq!(arm.params.len(), 3);
        assert_eq!(arm.params[0].name, "a");
        assert_eq!(arm.params[1].name, "b");
        assert_eq!(arm.params[2].name, "c");
        assert_eq!(arm.k_name, "k");
    }

    #[test]
    fn handle_expr_empty_arms_errors() {
        // `handle 0 with { }` — at least one operation arm is required.
        let errs = parse_errs("fn main() -> Int ![] { handle 0 with { } }\n");
        assert!(
            errs.iter()
                .any(|e| e.message.contains("at least one operation arm")),
            "empty handler arms should parse-error; got {errs:?}"
        );
    }

    #[test]
    fn handle_expr_op_arm_without_k_errors() {
        // `handle 0 with { Raise.fail() => 0 }` — empty arm parameter
        // list. The trailing `k` is mandatory at the surface level.
        let errs = parse_errs("fn main() -> Int ![] { handle 0 with { Raise.fail() => 0 } }\n");
        assert!(
            errs.iter()
                .any(|e| e.message.contains("trailing continuation binding")),
            "op arm without `k` should parse-error; got {errs:?}"
        );
    }

    #[test]
    fn handle_expr_missing_with_errors() {
        // `handle 0 { ... }` — `with` keyword missing.
        let errs = parse_errs("fn main() -> Int ![] { handle 0 { Raise.fail(k) => 0 } }\n");
        assert!(
            errs.iter().any(|e| e.message.contains("`with`")),
            "handle without `with` should parse-error; got {errs:?}"
        );
    }

    #[test]
    fn handle_expr_duplicate_return_arm_errors() {
        // Two `return` arms: parser rejects with a "duplicate" message.
        let errs = parse_errs(
            "fn main() -> Int ![] { handle 0 with { return(a) => a, return(b) => b, Raise.fail(k) => 0 } }\n",
        );
        assert!(
            errs.iter()
                .any(|e| e.message.contains("duplicate `return`")),
            "duplicate return arms should parse-error; got {errs:?}"
        );
    }

    #[test]
    fn handle_expr_duplicate_return_arm_first_wins_in_ast() {
        // Reviewer feedback PR #19 item 2 — pin first-wins AST
        // semantics: the parser drops the SECOND arm on the floor and
        // keeps the first in the AST so downstream passes see a
        // stable target. The error still fires (covered by the
        // sibling test above); this one pins the AST shape.
        let src = "fn main() -> Int ![] { handle 0 with { return(first) => first, return(second) => second, Raise.fail(k) => 0 } }\n";
        let (toks, lex_errs) = lex("t.sigil", src);
        assert!(lex_errs.is_empty(), "lex errs: {lex_errs:?}");
        let (prog, _) = parse("t.sigil", &toks);
        let Item::Fn(ref f) = prog.items[0] else {
            panic!()
        };
        let Some(Expr::Handle { ref return_arm, .. }) = f.body.tail else {
            panic!("expected tail Expr::Handle, got {:?}", f.body.tail);
        };
        let ra = return_arm
            .as_ref()
            .expect("return_arm should be populated by first-wins semantics");
        assert_eq!(
            ra.binding, "first",
            "first-wins: return arm binding must be `first`, not `second`",
        );
    }

    #[test]
    fn handle_expr_body_does_not_consume_brace_as_record_literal() {
        // Regression: `handle Foo with { ... }` must not try to read
        // `Foo {` as a record literal start. The `no_record_lits` flag
        // is enabled while parsing the handle body. (We use a synth
        // ident `x` since we don't have an actual record type
        // accessible here, but the structural property holds.)
        let e = parse_tail_expr("handle x with { E.op(k) => 0 }");
        let Expr::Handle { body, .. } = e else {
            panic!()
        };
        assert!(matches!(*body, Expr::Ident(ref n, _) if n == "x"));
    }

    #[test]
    fn effect_decl_in_program_alongside_fn_decl() {
        // Both an `effect` decl and a `fn` decl in the same program.
        // This is the integration-shape unit test — Task 54 will
        // exercise the full pipeline; here we just want to make sure
        // both items round-trip through the parser.
        let prog =
            parse_clean("effect Raise { fail: (String) -> Int }\nfn main() -> Int ![] { 0 }\n");
        assert_eq!(prog.items.len(), 2);
        assert!(matches!(prog.items[0], Item::Effect(_)));
        assert!(matches!(prog.items[1], Item::Fn(_)));
    }

    #[test]
    fn block_parses_as_primary_expression_at_let_binding_rhs() {
        // Plan B Task 55, Phase 4e captures+ Slice B — `parse_primary`
        // accepts `TokenKind::LBrace` and produces `Expr::Block`. The
        // change is broader than its motivating use case (arm body
        // shape `Effect.op(k) => { let r = k(arg); pure_tail }`):
        // block expressions parse cleanly in any expression position.
        // This test pins one such non-arm-body context — a let-binding
        // RHS — so a future grammar change that silently regresses
        // block-as-expression in other positions fires here.
        //
        // Pre-Slice-B, this would error with "expected an expression"
        // because `parse_primary` had no `LBrace` arm.
        let prog = parse_clean(
            "fn test() -> Int ![] {\n  \
               let r: Int = { let y: Int = 1; y + 1 };\n  \
               r\n\
             }\n",
        );
        assert_eq!(prog.items.len(), 1);
        let f = match &prog.items[0] {
            Item::Fn(f) => f,
            other => panic!("expected fn, got {other:?}"),
        };
        // The let RHS is the Block expression `{ let y = 1; y + 1 }`.
        let r_let = match &f.body.stmts[0] {
            crate::ast::Stmt::Let(l) => l,
            other => panic!("expected Stmt::Let, got {other:?}"),
        };
        match &r_let.value {
            Expr::Block(_) => {}
            other => panic!("expected Expr::Block at let RHS, got {other:?}"),
        }
    }

    #[test]
    fn block_parses_as_primary_expression_at_arm_body() {
        // Slice B's motivating use case — `Effect.op(k) => { let r =
        // k(arg); tail }` shape. Pin that the arm body's `Expr` is a
        // Block carrying a Stmt::Let with a k-call value.
        let prog = parse_clean(
            "effect Raise { fail: () -> Int }\n\
             fn main() -> Int ![IO] {\n  \
               let n: Int = handle 42 with {\n    \
                 Raise.fail(k) => { let r: Int = k(99); r + 1 },\n  \
               };\n  \
               perform IO.println(int_to_string(n));\n  \
               0\n\
             }\n",
        );
        // Items: effect Raise + fn main. fn main's body has `let n: Int
        // = handle ... with { Raise.fail(k) => { ... } }; ...`. We just
        // need to confirm the program parsed cleanly with the arm body
        // shape we expect — full-AST navigation is overkill for the
        // pin's purpose (catch grammar regression).
        assert_eq!(prog.items.len(), 2);
    }

    // ----------------------------------------------------------------
    // Plan B' Stage 6.8 Task 102 — `TypeExpr::Fn` parser surface.
    // ----------------------------------------------------------------

    /// Helper: parse `src` as a stand-alone `TypeExpr` via a fn-decl
    /// position. Returns the param's type for inspection. Panics on any
    /// parse / lex error so test assertions read cleanly.
    fn parse_type_in_param_position(src_param_ty: &str) -> TypeExpr {
        let src = format!("fn f(x: {src_param_ty}) -> Int ![] {{ 0 }}\n");
        let (toks, lex_errs) = lex("t.sigil", &src);
        assert!(lex_errs.is_empty(), "lex errs: {lex_errs:?}");
        let (prog, errs) = parse("t.sigil", &toks);
        assert!(errs.is_empty(), "parse errs for `{src_param_ty}`: {errs:?}");
        let Item::Fn(f) = &prog.items[0] else {
            panic!("expected fn item")
        };
        f.params[0].ty.clone()
    }

    #[test]
    fn fn_type_zero_params_no_effects_parses() {
        let te = parse_type_in_param_position("() -> Int ![]");
        let TypeExpr::Fn(fty) = te else {
            panic!("expected TypeExpr::Fn, got {te:?}")
        };
        assert!(fty.params.is_empty(), "zero-param fn type has empty params");
        assert_eq!(fty.ret.head_name(), "Int");
        assert!(fty.effects.is_empty());
        assert!(fty.effect_row_var.is_none());
    }

    #[test]
    fn fn_type_one_param_one_effect_parses() {
        let te = parse_type_in_param_position("(Int) -> String ![IO]");
        let TypeExpr::Fn(fty) = te else {
            panic!("expected TypeExpr::Fn, got {te:?}")
        };
        assert_eq!(fty.params.len(), 1);
        assert_eq!(fty.params[0].head_name(), "Int");
        assert_eq!(fty.ret.head_name(), "String");
        assert_eq!(
            fty.effects
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            vec!["IO"]
        );
    }

    #[test]
    fn fn_type_two_params_two_effects_parses() {
        let te = parse_type_in_param_position("(Int, String) -> Bool ![IO, Choose]");
        let TypeExpr::Fn(fty) = te else {
            panic!("expected TypeExpr::Fn, got {te:?}")
        };
        assert_eq!(fty.params.len(), 2);
        assert_eq!(fty.params[0].head_name(), "Int");
        assert_eq!(fty.params[1].head_name(), "String");
        assert_eq!(fty.ret.head_name(), "Bool");
        assert_eq!(
            fty.effects
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            vec!["IO", "Choose"]
        );
    }

    #[test]
    fn fn_type_with_row_variable_parses() {
        let te = parse_type_in_param_position("(Int) -> Int ![IO | r]");
        let TypeExpr::Fn(fty) = te else {
            panic!("expected TypeExpr::Fn, got {te:?}")
        };
        assert_eq!(
            fty.effects
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            vec!["IO"]
        );
        let rv = fty.effect_row_var.as_ref().expect("row var present");
        assert_eq!(rv.name, "r");
    }

    #[test]
    fn fn_type_nested_fn_in_param_parses() {
        // `((Int) -> Int ![]) -> Int ![]` — a fn that takes another
        // fn-typed value as its only parameter.
        let te = parse_type_in_param_position("((Int) -> Int ![]) -> Int ![]");
        let TypeExpr::Fn(fty) = te else {
            panic!("expected outer TypeExpr::Fn, got {te:?}")
        };
        assert_eq!(fty.params.len(), 1);
        let TypeExpr::Fn(inner) = &fty.params[0] else {
            panic!("expected inner TypeExpr::Fn, got {:?}", fty.params[0])
        };
        assert_eq!(inner.params.len(), 1);
        assert_eq!(inner.params[0].head_name(), "Int");
        assert_eq!(inner.ret.head_name(), "Int");
        assert_eq!(fty.ret.head_name(), "Int");
    }

    #[test]
    fn fn_type_returning_fn_parses() {
        // `(Int) -> (Int) -> Int ![] ![]` — a fn that returns a fn-
        // typed value. Right-associative reading of two `->`s with
        // explicit `![..]` per arrow.
        let te = parse_type_in_param_position("(Int) -> (Int) -> Int ![] ![]");
        let TypeExpr::Fn(fty) = te else {
            panic!("expected outer TypeExpr::Fn, got {te:?}")
        };
        assert_eq!(fty.params.len(), 1);
        let TypeExpr::Fn(inner) = &fty.ret else {
            panic!("expected inner TypeExpr::Fn, got {:?}", fty.ret)
        };
        assert_eq!(inner.params.len(), 1);
        assert_eq!(inner.params[0].head_name(), "Int");
        assert_eq!(inner.ret.head_name(), "Int");
    }

    #[test]
    fn fn_type_with_generic_param_in_signature_parses() {
        let src = "fn f(x: (A) -> A ![]) -> Int ![] { 0 }\n";
        let (toks, _) = lex("t.sigil", src);
        let (prog, errs) = parse("t.sigil", &toks);
        assert!(errs.is_empty(), "errs: {errs:?}");
        let Item::Fn(f) = &prog.items[0] else {
            panic!("expected fn item")
        };
        let TypeExpr::Fn(fty) = &f.params[0].ty else {
            panic!("expected TypeExpr::Fn, got {:?}", f.params[0].ty)
        };
        assert_eq!(fty.params[0].head_name(), "A");
        assert_eq!(fty.ret.head_name(), "A");
    }

    #[test]
    fn fn_type_in_let_binding_position_parses() {
        let src = "fn main() -> Int ![] { let g: (Int) -> Int ![] = id_fn; 0 }\n";
        let (toks, _) = lex("t.sigil", src);
        let (prog, errs) = parse("t.sigil", &toks);
        assert!(errs.is_empty(), "errs: {errs:?}");
        let Item::Fn(f) = &prog.items[0] else {
            panic!("expected fn item")
        };
        let Stmt::Let(l) = &f.body.stmts[0] else {
            panic!("expected let stmt")
        };
        let TypeExpr::Fn(fty) = &l.ty else {
            panic!("expected TypeExpr::Fn in let binding, got {:?}", l.ty)
        };
        assert_eq!(fty.params.len(), 1);
        assert_eq!(fty.params[0].head_name(), "Int");
        assert_eq!(fty.ret.head_name(), "Int");
    }

    #[test]
    fn fn_type_missing_arrow_errors_cleanly() {
        let src = "fn f(x: (Int) Int ![]) -> Int ![] { 0 }\n";
        let (toks, _) = lex("t.sigil", src);
        let (_prog, errs) = parse("t.sigil", &toks);
        assert!(
            !errs.is_empty(),
            "expected at least one parse error for missing `->`"
        );
    }

    #[test]
    fn fn_type_missing_effect_row_errors_cleanly() {
        let src = "fn f(x: (Int) -> Int) -> Int ![] { 0 }\n";
        let (toks, _) = lex("t.sigil", src);
        let (_prog, errs) = parse("t.sigil", &toks);
        assert!(
            !errs.is_empty(),
            "expected at least one parse error for missing `![..]`"
        );
    }

    // ----------------------------------------------------------------
    // R1 finding 5 — parser edge cases (trailing comma, missing
    // separator, row-var-only effect row).
    // ----------------------------------------------------------------

    #[test]
    fn fn_type_trailing_comma_in_param_list_parses() {
        // `(Int,) -> Int ![]` — trailing comma after the last param.
        // The parameter loop sees the comma, advances; the next peek
        // is `)`, which exits the loop via the `RParen | Eof` guard.
        // Sigil follows Rust's discipline (accept trailing comma).
        let te = parse_type_in_param_position("(Int,) -> Int ![]");
        let TypeExpr::Fn(fty) = te else {
            panic!("expected TypeExpr::Fn, got {te:?}")
        };
        assert_eq!(fty.params.len(), 1);
        assert_eq!(fty.params[0].head_name(), "Int");
    }

    #[test]
    fn fn_type_missing_comma_between_params_errors_cleanly() {
        // `(Int Int) -> Int ![]` — missing comma between two params.
        // The first `parse_type` consumes `Int`; the next peek is
        // `Int` (an Ident), not `Comma` — loop's `else { break }`
        // exits, then `expect(RParen)` fails, surfacing a clean
        // parse error.
        let src = "fn f(x: (Int Int) -> Int ![]) -> Int ![] { 0 }\n";
        let (toks, _) = lex("t.sigil", src);
        let (_prog, errs) = parse("t.sigil", &toks);
        assert!(
            !errs.is_empty(),
            "expected parse error for missing comma between fn-type params"
        );
    }

    #[test]
    fn fn_type_row_var_only_effect_row_parses() {
        // `(Int) -> Int ![| r]` — effect row with only a row variable
        // (no leading effect names). `parse_effect_row` expects the
        // initial loop body to handle effect names; with `|` as the
        // first token, the loop's `Pipe | RBracket | Eof` guard
        // exits immediately, then the row-var branch fires. Tests
        // that this corner case parses without a synthetic empty
        // effect.
        let te = parse_type_in_param_position("(Int) -> Int ![| r]");
        let TypeExpr::Fn(fty) = te else {
            panic!("expected TypeExpr::Fn, got {te:?}")
        };
        assert!(fty.effects.is_empty(), "no leading effects");
        let rv = fty.effect_row_var.as_ref().expect("row var present");
        assert_eq!(rv.name, "r");
    }
}
