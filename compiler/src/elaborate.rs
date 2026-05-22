//! Elaboration — plan A1 stage 1 task 8, extended in plan A2 task 23.
//!
//! Plan A1 was identity: hello-world already fits ANF shape. Plan A2
//! task 23 does two transformations:
//!
//! 1. **`if/else` desugared into `match` on `Bool`.** Every `Expr::If`
//!    becomes an `Expr::Match` whose scrutinee is the original condition
//!    and whose arms are `true => then_body, false => else_body`. When
//!    a branch block has statements (not just a tail expression), the
//!    block is wrapped in `Expr::Block` so it survives as a
//!    statement-sequence inside the match arm body (see `ast.rs`).
//!
//! 2. **Arithmetic flattened into A-normal form.** Non-trivial operands
//!    of binary and unary operators are hoisted into synthetic `let`
//!    bindings in the enclosing block. After this pass every `Expr::
//!    Binary` and `Expr::Unary` operand is a *trivial* expression:
//!    `IntLit`, `StringLit`, `BoolLit`, `CharLit`, or `Ident`. The
//!    result of a hoisted expression is bound to a fresh synthetic name
//!    (`$elab_t{N}`), and the enclosing expression references it by
//!    that name. Synthetic names use `$` as a prefix character; the
//!    surface lexer rejects `$`, so the names cannot collide with any
//!    user-visible identifier.
//!
//! Scope for task 23:
//!
//! - **In scope**: `Expr::If` → `Expr::Match` desugar; `Expr::Binary`
//!   and `Expr::Unary` operand flattening into ANF.
//! - **Out of scope**: `match` scrutinee flattening, `perform` arg
//!   flattening, `call` arg flattening. The plan's acceptance for
//!   Stage 2 is carried by tasks 24-26 (codegen + examples); task 23
//!   only establishes the arithmetic-shape invariant codegen consumes.
//!
//! # Typing the synthetic bindings
//!
//! Elaborate introduces `let $elab_tN: <TypeExpr> = <expr>;` bindings
//! for each hoisted compound. The declared type is inferred directly
//! from the operator: `BinOp::Add/Sub/Mul/Div/Mod` and `UnOp::Neg`
//! produce `Int`; every other binary operator (comparison and logic)
//! and `UnOp::Not` produce `Bool`. This matches plan A2 task 22's
//! typing rules exactly — typecheck already ran and ensured every
//! operator's operand types; the result type of each op is
//! deterministic from the op alone.
//!
//! Elaborate does **not** re-run typecheck. Downstream passes
//! (monomorphize/color/cps/closure_convert/codegen) currently only
//! inspect top-level `Item::Fn` items and do not recurse into
//! expressions in plan A2, so the synthetic bindings are safe to
//! introduce without an additional resolve/typecheck pass. Task 24's
//! codegen extension reads operator types directly from the `BinOp` /
//! `UnOp` tag, so the `TypeExpr` annotation on the synthetic `let` is
//! a belt-and-braces signal rather than the only source of truth.
//!
//! # Synthetic name stability
//!
//! `$elab_tN` numbering is monotonic across the whole program. Every
//! call to `fresh_name` bumps the counter; reordering functions in the
//! input program reorders the names. For task 23 this is fine because
//! no test pins specific synthetic names — tests assert *shape* (the
//! existence of hoisted let bindings, the desugared match form) rather
//! than names.

use crate::ast::*;
use crate::errors::Span;
use crate::typecheck::CheckedProgram;

#[derive(Clone, Debug)]
pub struct AnfProgram {
    pub checked: CheckedProgram,
}

pub fn elaborate(mut checked: CheckedProgram) -> AnfProgram {
    let mut elab = Elaborator { fresh_counter: 0 };
    for item in &mut checked.program.items {
        if let Item::Fn(f) = item {
            let body_span = f.body.span.clone();
            let body = std::mem::replace(
                &mut f.body,
                Block {
                    stmts: Vec::new(),
                    tail: None,
                    span: body_span,
                },
            );
            f.body = elab.elab_block(body);
        }
    }
    AnfProgram { checked }
}

struct Elaborator {
    fresh_counter: u32,
}

impl Elaborator {
    fn fresh_name(&mut self) -> String {
        let n = self.fresh_counter;
        self.fresh_counter += 1;
        format!("$elab_t{n}")
    }

    fn elab_block(&mut self, b: Block) -> Block {
        let mut new_stmts: Vec<Stmt> = Vec::with_capacity(b.stmts.len());
        for s in b.stmts {
            let (new_s, hoisted) = self.elab_stmt(s);
            new_stmts.extend(hoisted);
            new_stmts.push(new_s);
        }
        let new_tail = b.tail.map(|t| {
            let (e, hoisted) = self.elab_expr(t, false);
            new_stmts.extend(hoisted);
            e
        });
        Block {
            stmts: new_stmts,
            tail: new_tail,
            span: b.span,
        }
    }

    fn elab_stmt(&mut self, s: Stmt) -> (Stmt, Vec<Stmt>) {
        match s {
            Stmt::Let(l) => {
                let (value, hoisted) = self.elab_expr(l.value, false);
                (Stmt::Let(LetStmt { value, ..l }), hoisted)
            }
            Stmt::Expr(e) => {
                let (e, hoisted) = self.elab_expr(e, false);
                (Stmt::Expr(e), hoisted)
            }
            Stmt::Perform(p) => {
                // Plan A2 task 23 scope: do not flatten `perform` args.
                // The only `perform` recognised in Plan A2 is
                // `IO.println`, whose argument is already a String-
                // producing expression (typically a literal). A future
                // plan (B) that supports arbitrary effects may need to
                // flatten here.
                let (p, hoisted) = self.elab_perform(p);
                (Stmt::Perform(p), hoisted)
            }
        }
    }

    fn elab_perform(&mut self, p: PerformExpr) -> (PerformExpr, Vec<Stmt>) {
        let mut hoisted: Vec<Stmt> = Vec::new();
        let new_args = p
            .args
            .into_iter()
            .map(|a| {
                let (e, h) = self.elab_expr(a, false);
                hoisted.extend(h);
                e
            })
            .collect();
        (
            PerformExpr {
                args: new_args,
                ..p
            },
            hoisted,
        )
    }

    /// Elaborate an expression. If `need_trivial` is true, guarantee
    /// the returned `Expr` is a trivial form (IntLit / StringLit /
    /// BoolLit / CharLit / Ident); any compound result is bound to a
    /// fresh synthetic `let` and the `Expr` returned is the `Ident`
    /// referring to it.
    fn elab_expr(&mut self, e: Expr, need_trivial: bool) -> (Expr, Vec<Stmt>) {
        match e {
            // Trivial forms: no transformation.
            Expr::IntLit(..)
            | Expr::FloatLit(..)
            | Expr::StringLit(..)
            | Expr::BoolLit(..)
            | Expr::CharLit(..)
            | Expr::UnitLit(..)
            | Expr::Ident(..) => (e, Vec::new()),

            Expr::Binary { op, lhs, rhs, span } => {
                let mut hoisted = Vec::new();
                let (lhs_e, h1) = self.elab_expr(*lhs, true);
                hoisted.extend(h1);
                let (rhs_e, h2) = self.elab_expr(*rhs, true);
                hoisted.extend(h2);
                let new = Expr::Binary {
                    op,
                    lhs: Box::new(lhs_e),
                    rhs: Box::new(rhs_e),
                    span: span.clone(),
                };
                if need_trivial {
                    let ty = binop_result_type(op, span.clone());
                    let ident = self.bind(&mut hoisted, ty, new, span);
                    (ident, hoisted)
                } else {
                    (new, hoisted)
                }
            }

            Expr::Unary { op, operand, span } => {
                let (operand_e, mut hoisted) = self.elab_expr(*operand, true);
                let new = Expr::Unary {
                    op,
                    operand: Box::new(operand_e),
                    span: span.clone(),
                };
                if need_trivial {
                    let ty = unop_result_type(op, span.clone());
                    let ident = self.bind(&mut hoisted, ty, new, span);
                    (ident, hoisted)
                } else {
                    (new, hoisted)
                }
            }

            // Desugar if/else into match on Bool.
            Expr::If {
                cond,
                then_block,
                else_block,
                span,
            } => {
                let (cond_e, hoisted) = self.elab_expr(*cond, false);
                let then_elab = self.elab_block(*then_block);
                let else_elab = self.elab_block(*else_block);
                let match_expr = Expr::Match {
                    scrutinee: Box::new(cond_e),
                    arms: vec![
                        MatchArm {
                            pattern: Pattern::BoolLit(true, span.clone()),
                            body: block_to_expr(then_elab),
                            span: span.clone(),
                        },
                        MatchArm {
                            pattern: Pattern::BoolLit(false, span.clone()),
                            body: block_to_expr(else_elab),
                            span: span.clone(),
                        },
                    ],
                    span,
                };
                (match_expr, hoisted)
            }

            Expr::Match {
                scrutinee,
                arms,
                span,
            } => {
                // Task 23 scope: don't trivialize the scrutinee. Codegen
                // (task 24) lowers a non-trivial scrutinee by emitting
                // its code inline then using the result value. If a
                // future task needs a trivial scrutinee, add `true` to
                // the recursion here.
                let (scrutinee_e, hoisted) = self.elab_expr(*scrutinee, false);
                let new_arms = arms
                    .into_iter()
                    .map(|arm| {
                        let (body, arm_hoisted) = self.elab_expr(arm.body, false);
                        // Arm-body hoisted bindings cannot leak out of
                        // the arm scope. If we got any, wrap the arm
                        // body in an `Expr::Block` whose stmts carry
                        // the hoisted lets and whose tail is `body`.
                        let final_body = if arm_hoisted.is_empty() {
                            body
                        } else {
                            let span = arm.span.clone();
                            Expr::Block(Box::new(Block {
                                stmts: arm_hoisted,
                                tail: Some(body),
                                span,
                            }))
                        };
                        MatchArm {
                            pattern: arm.pattern,
                            body: final_body,
                            span: arm.span,
                        }
                    })
                    .collect();
                let new = Expr::Match {
                    scrutinee: Box::new(scrutinee_e),
                    arms: new_arms,
                    span,
                };
                (new, hoisted)
            }

            Expr::Call { callee, args, span } => {
                // Recurse into callee + args without trivializing
                // (call-arg ANF flattening is still deferred to a
                // future task; the recursion only handles desugars
                // like `Expr::If` → `Expr::Match` so they don't
                // reach codegen unelaborated). Hoisted bindings
                // bubble up to the caller — they cannot leak across
                // a lambda/handle boundary, but a Call is not such a
                // boundary, so this is safe.
                let (callee_e, mut hoisted) = self.elab_expr(*callee, false);
                let new_args = args
                    .into_iter()
                    .map(|a| {
                        let (a_e, h) = self.elab_expr(a, false);
                        hoisted.extend(h);
                        a_e
                    })
                    .collect();
                (
                    Expr::Call {
                        callee: Box::new(callee_e),
                        args: new_args,
                        span,
                    },
                    hoisted,
                )
            }

            Expr::Perform(p) => {
                let (p, hoisted) = self.elab_perform(p);
                (Expr::Perform(p), hoisted)
            }

            // An `Expr::Block` in the *input* to elaborate shouldn't
            // happen — the surface parser never produces one. If a
            // future caller does produce one, treat it defensively:
            // elaborate its contents and return it as-is.
            Expr::Block(b) => (Expr::Block(Box::new(self.elab_block(*b))), Vec::new()),

            // Lambda expressions parse in Task 29 but don't yet
            // typecheck (Task 30) or reach codegen (Tasks 31/32).
            // Typecheck rejects them with E0043, which exits the
            // pipeline before elaborate runs, so in practice this
            // arm is never hit in Stage 3's first PR. Defensive
            // handling: elaborate the body in place (no hoisting,
            // no ANF flattening across a lambda boundary — that
            // rewriting lands in Task 31's closure conversion).
            Expr::Lambda {
                params,
                return_type,
                effects,
                effect_row_var,
                body,
                span,
            } => {
                let (body, hoisted) = self.elab_expr(*body, false);
                // Hoisted bindings inside a lambda body cannot leak
                // out of its scope. If elaborate produced any (it
                // shouldn't in PR 5 since the pre-typecheck rejection
                // ensures Lambda never reaches elaborate on a well-
                // formed program), wrap the body in `Expr::Block`.
                let final_body = if hoisted.is_empty() {
                    body
                } else {
                    let b_span = span.clone();
                    Expr::Block(Box::new(Block {
                        stmts: hoisted,
                        tail: Some(body),
                        span: b_span,
                    }))
                };
                (
                    Expr::Lambda {
                        params,
                        return_type,
                        effects,
                        effect_row_var,
                        body: Box::new(final_body),
                        span,
                    },
                    Vec::new(),
                )
            }
            // `ClosureRecord` / `ClosureEnvLoad` are synthesized by
            // plan A2 task 31's closure conversion, which runs strictly
            // after elaborate. They cannot appear in the AST elaborate
            // sees.
            Expr::ClosureRecord { .. } | Expr::ClosureEnvLoad { .. } => {
                unreachable!("elaborate: closure-conversion nodes should not appear pre-CC")
            }
            // Plan A3 task 37: record literal. Field values are not
            // ANF-flattened (the codegen allocator in task 41
            // accepts compound field values), but they DO need to
            // be elaborated so desugars like `Expr::If` →
            // `Expr::Match` reach codegen. Hoisted bindings from
            // field values bubble up to the caller.
            Expr::RecordLit { name, fields, span } => {
                let mut hoisted: Vec<Stmt> = Vec::new();
                let new_fields = fields
                    .into_iter()
                    .map(|f| {
                        let (v, h) = self.elab_expr(f.value, false);
                        hoisted.extend(h);
                        RecordFieldLit {
                            name: f.name,
                            value: v,
                            span: f.span,
                        }
                    })
                    .collect();
                (
                    Expr::RecordLit {
                        name,
                        fields: new_fields,
                        span,
                    },
                    hoisted,
                )
            }
            // Plan B task 53 — `handle <body> with { ... }` shape
            // is preserved (Task 55's codegen consumes it directly),
            // but the body, the optional `return` arm body, and each
            // op-arm's body must be elaborated so desugars like
            // `Expr::If` → `Expr::Match` reach codegen. Each of those
            // sub-expressions is its own scope: hoisted bindings from
            // them MUST NOT leak past the handle (the body is inside
            // the handle's effect-discharge boundary; arm bodies are
            // inside their own arm scope). Wrap each in `Expr::Block`
            // when its elaboration produced hoisted bindings, mirroring
            // the lambda-body / match-arm pattern above.
            //
            // E0134 was lifted in Task 55 Phase 2 (`2d69b52`), so a
            // handle reaching this code is now a well-formed shape
            // and the recursion is load-bearing for downstream
            // codegen correctness.
            Expr::Handle {
                body,
                return_arm,
                op_arms,
                span,
            } => {
                let body_span = body.span();
                let (body_e, body_hoisted) = self.elab_expr(*body, false);
                let body_final = if body_hoisted.is_empty() {
                    body_e
                } else {
                    Expr::Block(Box::new(Block {
                        stmts: body_hoisted,
                        tail: Some(body_e),
                        span: body_span,
                    }))
                };
                let return_arm_e = return_arm.map(|ra| {
                    let arm_span = ra.span.clone();
                    let (b, h) = self.elab_expr(ra.body, false);
                    let final_body = if h.is_empty() {
                        b
                    } else {
                        Expr::Block(Box::new(Block {
                            stmts: h,
                            tail: Some(b),
                            span: arm_span.clone(),
                        }))
                    };
                    Box::new(HandleReturnArm {
                        binding: ra.binding,
                        binding_span: ra.binding_span,
                        body: final_body,
                        span: ra.span,
                    })
                });
                let new_op_arms = op_arms
                    .into_iter()
                    .map(|arm| {
                        let arm_span = arm.span.clone();
                        let (b, h) = self.elab_expr(arm.body, false);
                        let final_body = if h.is_empty() {
                            b
                        } else {
                            Expr::Block(Box::new(Block {
                                stmts: h,
                                tail: Some(b),
                                span: arm_span,
                            }))
                        };
                        HandleOpArm {
                            effect: arm.effect,
                            effect_span: arm.effect_span,
                            op: arm.op,
                            op_span: arm.op_span,
                            params: arm.params,
                            k_name: arm.k_name,
                            k_span: arm.k_span,
                            body: final_body,
                            span: arm.span,
                        }
                    })
                    .collect();
                (
                    Expr::Handle {
                        body: Box::new(body_final),
                        return_arm: return_arm_e,
                        op_arms: new_op_arms,
                        span,
                    },
                    Vec::new(),
                )
            }
            // Plan D Task 113 — tuple values: elaborate each element
            // without trivializing (mirrors Call's path; Tuple's
            // element types depend on inference, so the surface
            // doesn't synthesize a let-binding here even when
            // `need_trivial`). Compound elaboration of inner exprs
            // (e.g., a nested Binary) still happens via the recursive
            // elab_expr.
            Expr::Tuple { elems, span } => {
                let mut hoisted = Vec::new();
                let new_elems: Vec<Expr> = elems
                    .into_iter()
                    .map(|e| {
                        let (e_elab, h) = self.elab_expr(e, false);
                        hoisted.extend(h);
                        e_elab
                    })
                    .collect();
                (
                    Expr::Tuple {
                        elems: new_elems,
                        span,
                    },
                    hoisted,
                )
            }
        }
    }

    /// Emit a fresh `let $elab_tN: ty = value;` into `hoisted` and
    /// return an `Expr::Ident` referencing the bound name.
    fn bind(&mut self, hoisted: &mut Vec<Stmt>, ty: TypeExpr, value: Expr, span: Span) -> Expr {
        let name = self.fresh_name();
        hoisted.push(Stmt::Let(LetStmt {
            name: name.clone(),
            ty,
            value,
            span: span.clone(),
        }));
        Expr::Ident(name, span)
    }
}

/// Convert a block into an `Expr` suitable for a `MatchArm::body`. A
/// block with no statements and a single tail expression is its tail
/// (unwrap). Otherwise it wraps in `Expr::Block`.
fn block_to_expr(b: Block) -> Expr {
    if b.stmts.is_empty() {
        if let Some(tail) = b.tail {
            return tail;
        }
    }
    Expr::Block(Box::new(b))
}

fn binop_result_type(op: BinOp, span: Span) -> TypeExpr {
    let name = match op {
        BinOp::Add
        | BinOp::Sub
        | BinOp::Mul
        | BinOp::Div
        | BinOp::Mod
        | BinOp::SdivUnchecked
        | BinOp::SremUnchecked => "Int",
        BinOp::Eq
        | BinOp::NotEq
        | BinOp::Lt
        | BinOp::Gt
        | BinOp::LtEq
        | BinOp::GtEq
        | BinOp::And
        | BinOp::Or => "Bool",
    };
    TypeExpr::Named(name.to_string(), span)
}

fn unop_result_type(op: UnOp, span: Span) -> TypeExpr {
    let name = match op {
        UnOp::Neg => "Int",
        UnOp::Not => "Bool",
    };
    TypeExpr::Named(name.to_string(), span)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::lexer::lex;
    use crate::parser::parse;
    use crate::resolve::resolve;
    use crate::typecheck::typecheck;

    fn elab(src: &str) -> AnfProgram {
        let (toks, lex_errs) = lex("t.sigil", src);
        assert!(lex_errs.is_empty(), "lex errs: {lex_errs:?}");
        let (prog, parse_errs) = parse("t.sigil", &toks);
        assert!(parse_errs.is_empty(), "parse errs: {parse_errs:?}");
        let (rp, res_errs) = resolve(prog);
        assert!(res_errs.is_empty(), "resolve errs: {res_errs:?}");
        let (checked, tc_errs) = typecheck(rp.program);
        assert!(tc_errs.is_empty(), "typecheck errs: {tc_errs:?}");
        elaborate(checked)
    }

    fn main_body(p: &AnfProgram) -> &Block {
        for item in &p.checked.program.items {
            if let Item::Fn(f) = item {
                if f.name == "main" {
                    return &f.body;
                }
            }
        }
        panic!("no main");
    }

    /// Count `Stmt::Let` bindings whose name starts with `$elab_t`.
    fn count_synthetic_lets(b: &Block) -> usize {
        b.stmts
            .iter()
            .filter(|s| matches!(s, Stmt::Let(l) if l.name.starts_with("$elab_t")))
            .count()
    }

    #[test]
    fn hello_world_is_identity() {
        // Hello-world has no Stage-2 shapes; elaborate should produce
        // an AST indistinguishable in shape from its input.
        let src = "import std.io\n\
               use std.io.{IO};\n\
               fn main() -> Int ![IO] { perform IO.println(\"hi\"); 0 }\n\
               ";
        let p = elab(src);
        let body = main_body(&p);
        assert_eq!(count_synthetic_lets(body), 0, "unexpected hoisting");
        // Stmts: the single perform. Tail: IntLit(0).
        assert_eq!(body.stmts.len(), 1);
        assert!(matches!(body.stmts[0], Stmt::Perform(_)));
        assert!(matches!(body.tail, Some(Expr::IntLit(0, _))));
    }

    #[test]
    fn trivial_binary_not_hoisted_at_tail() {
        // `let n: Int = 1 + 2;` — binary with trivial operands stays
        // as the RHS of the user let; no hoisting happens.
        let src = "fn main() -> Int ![] { let n: Int = 1 + 2; n }\n";
        let p = elab(src);
        let body = main_body(&p);
        assert_eq!(count_synthetic_lets(body), 0);
        assert_eq!(body.stmts.len(), 1);
        match &body.stmts[0] {
            Stmt::Let(l) => {
                assert_eq!(l.name, "n");
                match &l.value {
                    Expr::Binary { op, lhs, rhs, .. } => {
                        assert_eq!(*op, BinOp::Add);
                        assert!(matches!(**lhs, Expr::IntLit(1, _)));
                        assert!(matches!(**rhs, Expr::IntLit(2, _)));
                    }
                    other => panic!("expected Binary, got {other:?}"),
                }
            }
            other => panic!("expected Let, got {other:?}"),
        }
    }

    #[test]
    fn nested_binary_hoists_inner() {
        // `1 + 2 * 3` parses as `1 + (2 * 3)`. Inner `2 * 3` is the
        // RHS of `+`, and `+` needs trivial operands, so the inner
        // binary hoists. Outer binary's operands are now both trivial.
        let src = "fn main() -> Int ![] { let n: Int = 1 + 2 * 3; n }\n";
        let p = elab(src);
        let body = main_body(&p);
        assert_eq!(
            count_synthetic_lets(body),
            1,
            "expected exactly one hoisted binding for `2 * 3`"
        );
        // Shape: [Let $elab_t0 = 2 * 3, Let n = 1 + $elab_t0, tail n]
        assert_eq!(body.stmts.len(), 2);
        match &body.stmts[0] {
            Stmt::Let(l) => {
                assert!(l.name.starts_with("$elab_t"));
                match &l.value {
                    Expr::Binary { op, .. } => assert_eq!(*op, BinOp::Mul),
                    other => panic!("expected Binary(Mul), got {other:?}"),
                }
            }
            other => panic!("expected synthetic Let, got {other:?}"),
        }
        match &body.stmts[1] {
            Stmt::Let(l) => {
                assert_eq!(l.name, "n");
                match &l.value {
                    Expr::Binary { op, lhs, rhs, .. } => {
                        assert_eq!(*op, BinOp::Add);
                        assert!(matches!(**lhs, Expr::IntLit(1, _)));
                        // RHS is the synthetic Ident.
                        match &**rhs {
                            Expr::Ident(name, _) => assert!(name.starts_with("$elab_t")),
                            other => panic!("expected Ident, got {other:?}"),
                        }
                    }
                    other => panic!("expected Binary, got {other:?}"),
                }
            }
            other => panic!("expected Let, got {other:?}"),
        }
    }

    #[test]
    fn unary_nested_hoists() {
        // `-(x + 1)` where x: Int. The inner `x + 1` is a non-trivial
        // operand to `-`, so it hoists. Outer unary now has trivial
        // operand.
        let src = "fn main() -> Int ![] {\n\
                     let x: Int = 5;\n\
                     let y: Int = -(x + 1);\n\
                     y\n\
                   }\n";
        let p = elab(src);
        let body = main_body(&p);
        assert_eq!(count_synthetic_lets(body), 1);
    }

    #[test]
    fn if_desugars_to_match_with_bool_arms() {
        // Pure-expression branches: both blocks have only tails.
        let src = "fn main() -> Int ![] { let n: Int = if true { 1 } else { 2 }; n }\n";
        let p = elab(src);
        let body = main_body(&p);
        match &body.stmts[0] {
            Stmt::Let(l) => {
                let e = &l.value;
                match e {
                    Expr::Match {
                        scrutinee, arms, ..
                    } => {
                        assert!(matches!(**scrutinee, Expr::BoolLit(true, _)));
                        assert_eq!(arms.len(), 2);
                        assert!(matches!(arms[0].pattern, Pattern::BoolLit(true, _)));
                        assert!(matches!(arms[1].pattern, Pattern::BoolLit(false, _)));
                        // Pure-expr branches unwrap their blocks: the
                        // arm body is the block's tail directly, not an
                        // Expr::Block wrapper.
                        assert!(matches!(arms[0].body, Expr::IntLit(1, _)));
                        assert!(matches!(arms[1].body, Expr::IntLit(2, _)));
                    }
                    other => panic!("expected Match, got {other:?}"),
                }
            }
            other => panic!("expected Let, got {other:?}"),
        }
    }

    #[test]
    fn if_with_stmt_branch_wraps_in_block_expr() {
        // `then` branch has a `let`; survives as an `Expr::Block` arm
        // body. `else` branch is pure-expr and unwraps.
        let src = "fn main() -> Int ![] {\n\
                     let n: Int = if true { let t: Int = 1; t } else { 0 };\n\
                     n\n\
                   }\n";
        let p = elab(src);
        let body = main_body(&p);
        match &body.stmts[0] {
            Stmt::Let(l) => match &l.value {
                Expr::Match { arms, .. } => {
                    assert!(matches!(&arms[0].body, Expr::Block(b) if b.stmts.len() == 1));
                    assert!(matches!(arms[1].body, Expr::IntLit(0, _)));
                }
                other => panic!("expected Match, got {other:?}"),
            },
            other => panic!("expected Let, got {other:?}"),
        }
    }

    #[test]
    fn if_nested_in_arithmetic_hoists_its_own_let() {
        // `let n: Int = 1 + if true { 2 } else { 3 };` — the if
        // desugars to match; the outer + needs trivial operands, so
        // the match gets hoisted into a synthetic let.
        //
        // Actually: task 23 doesn't hoist Match results (scope
        // limitation — see module doc). The inner desugared `Match`
        // stays inline as the RHS of `+`. This test pins the current
        // shape so a future scope expansion notices the change.
        let src = "fn main() -> Int ![] {\n\
                     let n: Int = 1 + if true { 2 } else { 3 };\n\
                     n\n\
                   }\n";
        let p = elab(src);
        let body = main_body(&p);
        match &body.stmts[0] {
            Stmt::Let(l) => match &l.value {
                Expr::Binary { op, rhs, .. } => {
                    assert_eq!(*op, BinOp::Add);
                    // Post-task-23 scope: RHS is a Match, not an
                    // Ident. Codegen (task 24) handles this directly.
                    assert!(matches!(**rhs, Expr::Match { .. }));
                }
                other => panic!("expected Binary, got {other:?}"),
            },
            other => panic!("expected Let, got {other:?}"),
        }
    }

    #[test]
    fn synthetic_let_has_inferred_type() {
        // `1 + 2 * 3` hoists `2 * 3` into a synthetic let. Its declared
        // TypeExpr should be `Int` (derived from `*`).
        let src = "fn main() -> Int ![] { let n: Int = 1 + 2 * 3; n }\n";
        let p = elab(src);
        let body = main_body(&p);
        let syn = body
            .stmts
            .iter()
            .find_map(|s| match s {
                Stmt::Let(l) if l.name.starts_with("$elab_t") => Some(l),
                _ => None,
            })
            .expect("synthetic let not found");
        assert_eq!(syn.ty.head_name(), "Int");
    }

    #[test]
    fn fresh_names_are_unique_within_program() {
        // Every hoisted let gets its own name; none collide.
        let src = "fn main() -> Int ![] {\n\
                     let a: Int = 1 + 2 * 3;\n\
                     let b: Int = 4 + 5 * 6;\n\
                     a + b\n\
                   }\n";
        let p = elab(src);
        let body = main_body(&p);
        let names: Vec<&str> = body
            .stmts
            .iter()
            .filter_map(|s| match s {
                Stmt::Let(l) if l.name.starts_with("$elab_t") => Some(l.name.as_str()),
                _ => None,
            })
            .collect();
        // The tail `a + b` has trivial operands so no hoisting there.
        // The two RHS binaries each hoist their inner `*`, so 2 names.
        assert_eq!(names.len(), 2, "names={names:?}");
        assert_ne!(names[0], names[1]);
    }

    #[test]
    fn if_inside_call_args_is_desugared_to_match() {
        // Regression for the elaborate-doesn't-recurse-into-Call-args
        // bug exposed by Plan B Task 55's MF2 property test. Before
        // the fix, `Expr::If` nested inside a Call's args reached
        // codegen un-desugared and tripped the
        // `unreachable!("Expr::If should have been desugared by elaborate")`
        // arm in `lower_expr`.
        let src = "import std.int\n\
               import std.io\n\
               use std.int.{int_to_string};\n\
               use std.io.{IO};\n\
               fn main() -> Int ![IO] {\n\
               perform IO.println(int_to_string(if true { 1 } else { 2 }));\n\
                 0\n\
               }\n\
               ";
        let p = elab(src);
        let body = main_body(&p);
        let mut found_match = false;
        for s in &body.stmts {
            if let Stmt::Perform(p) = s {
                for a in &p.args {
                    if let Expr::Call { args, .. } = a {
                        for inner in args {
                            if matches!(inner, Expr::Match { .. }) {
                                found_match = true;
                            }
                            assert!(
                                !matches!(inner, Expr::If { .. }),
                                "Expr::If under Call args must be desugared",
                            );
                        }
                    }
                }
            }
        }
        assert!(found_match, "expected Expr::Match inside the call's args");
    }

    #[test]
    fn if_inside_handle_body_is_desugared_to_match() {
        // Regression for the elaborate-doesn't-recurse-into-Handle-children
        // bug. Before E0134 lifted, `Expr::Handle` short-circuited in
        // elaborate; with the gate lifted in Phase 2, body+arm bodies
        // need real elaboration.
        let src = "import std.int\n\
               import std.io\n\
               use std.int.{int_to_string};\n\
               use std.io.{IO};\n\
               effect E_eff { op: () -> Int }\n\
               fn main() -> Int ![IO] {\n\
                 let n: Int = handle (if true { 1 } else { 2 }) with { E_eff.op(k) => 0 };\n\
                 perform IO.println(int_to_string(n));\n\
                 0\n\
               }\n\
               ";
        let p = elab(src);
        let body = main_body(&p);
        // Find the let n: ... = handle ... binding and check that the
        // handle body is now an Expr::Match (post-desugar) rather
        // than an Expr::If.
        match &body.stmts[0] {
            Stmt::Let(l) => match &l.value {
                Expr::Handle { body, .. } => {
                    assert!(
                        !matches!(**body, Expr::If { .. }),
                        "handle body should not contain Expr::If post-elaborate",
                    );
                    assert!(
                        matches!(**body, Expr::Match { .. }),
                        "handle body should be desugared to Expr::Match",
                    );
                }
                other => panic!("expected Expr::Handle, got {other:?}"),
            },
            other => panic!("expected Let, got {other:?}"),
        }
    }
}
