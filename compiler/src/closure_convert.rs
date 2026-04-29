//! Closure conversion — plan A1 Stage 1 task 11, extended in plan A2 task 31.
//!
//! Rewrites every `Expr::Lambda` in the post-elaboration AST into a pair of
//! artefacts:
//!
//! - A `ClosureRecord` expression at the lambda's original position, naming
//!   a synthetic top-level function and carrying the values of its captures
//!   (evaluated in the outer scope where the lambda was written).
//! - A new `Item::Fn` appended to the program's items list. Its parameter
//!   list, return type, and effect row are copied verbatim from the lambda;
//!   its body is the lambda's body with every reference to a captured outer
//!   variable rewritten into an `Expr::ClosureEnvLoad { index, kind, .. }`.
//!   Synthetic names use the `$lambda_N` form; `$` is not a lexer-legal
//!   identifier character, so these never collide with user names.
//!
//! Top-level `fn`s remain as-is in the item list and retain their original
//! name; they are conceptually closures with an empty environment and are
//! compiled under the same calling convention (`closure_ptr, args...`) by
//! codegen (task 32). No synthetic wrapper is materialised for them.
//!
//! # Nested lambdas and transitive captures
//!
//! Typecheck's `collect_free_vars` (see `typecheck::collect_free_vars`) walks
//! through inner lambdas when computing an outer lambda's capture set, so a
//! nested lambda's reference to a variable from the outermost scope appears
//! in *every* enclosing lambda's capture list. This pass relies on that
//! invariant: when the inner lambda's env is populated, the values for its
//! captures are built in the enclosing lambda's scope — which means an
//! enclosing lambda's capture surfaces as a `ClosureEnvLoad` on the inner
//! `env_exprs`, threading the value from the outer closure through.
//!
//! # Pass order and invariants
//!
//! Runs after typecheck, elaborate, monomorphize, color, and CPS (all
//! identity or near-identity in Plan A2). The input AST is well-typed and
//! contains `Expr::Lambda` nodes in their original positions. The output
//! AST contains no `Expr::Lambda` nodes; every one has been replaced by an
//! `Expr::ClosureRecord`. The two post-CC variants (`ClosureRecord` and
//! `ClosureEnvLoad`) are rejected by typecheck and elaborate with
//! `unreachable!` arms — a belt-and-braces check that they only ever
//! appear downstream of this pass.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{
    Block, EnvSlotKind, Expr, FnDecl, Item, LetStmt, MatchArm, PerformExpr, RecordFieldLit, Stmt,
};
use crate::color::ColoredProgram;
use crate::errors::Span;
use crate::typecheck::Ty;

#[derive(Clone, Debug)]
pub struct ClosureConvertedProgram {
    pub colored: ColoredProgram,
    /// Per-fn capture summary. Populated for both original and synthetic
    /// items; each entry is `(fn_name, [captured_name, ...])`. The
    /// authoritative per-slot metadata (slot kind, load/store width) lives
    /// on the `ClosureRecord` / `ClosureEnvLoad` AST nodes — this summary
    /// is a flat back-reference kept for Plan B tooling that expects a
    /// program-level captures index, and for tests.
    pub captures: Vec<(String, Vec<String>)>,
    /// Plan B' Stage 6.8 Phase C+ Part 2 — typed capture metadata per
    /// synth lambda fn, keyed by synth fn name (`$lambda_N`). Each
    /// entry is the lambda's capture list with full `Ty` info from
    /// typecheck's `lambda_captures` side-table. Codegen consumes
    /// this when entering a synth fn's `Lowerer` to populate
    /// `local_fn_types` / `captured_fn_sigs` for fn-typed captures
    /// — required for `lower_call`'s `ClosureEnvLoad`-callee
    /// dispatch (compose-style: lambda body invokes a captured
    /// fn-typed value).
    pub captures_typed: BTreeMap<String, Vec<(String, Ty)>>,
}

pub fn convert(mut colored: ColoredProgram) -> ClosureConvertedProgram {
    // Move the per-lambda capture side-table out of the checked program so
    // the rewriter can consume it without tangling borrows with the item
    // list below. The field is not read by any downstream pass.
    let all_captures = std::mem::take(&mut colored.mono.anf.checked.lambda_captures);
    let original_items = std::mem::take(&mut colored.mono.anf.checked.program.items);

    // Pre-scan for user fn names that would collide with `$lambda_N`
    // AFTER codegen's `$` → `__` mangling. A user fn named
    // `__lambda_3` mangles to `sigil_user___lambda_3`, the same linker
    // symbol that a synthetic `$lambda_3` would produce. Cranelift
    // surfaces such collisions as a duplicate-symbol error at compile
    // time (not a silent miscompile), but the user-facing diagnostic is
    // opaque. Collect the reserved `N` values here and skip them in the
    // counter so `$lambda_N` always maps to a fresh linker symbol. The
    // `$` character is itself rejected by the lexer, so the reverse
    // direction (user name collides with a synthetic) is the only
    // collision worth guarding against.
    let reserved_counters: BTreeSet<usize> = original_items
        .iter()
        .filter_map(|it| match it {
            Item::Fn(f) => f
                .name
                .strip_prefix("__lambda_")
                .and_then(|rest| rest.parse::<usize>().ok()),
            _ => None,
        })
        .collect();

    // Plan B' Stage 6.8 Task 104 — collect user-defined top-level fn
    // names so `rewrite_expr` can materialize fn-as-value uses as
    // ClosureRecords. Built before the rewrite loop so the ordering
    // matches: forward references in lower fns to higher fns work.
    let top_level_fn_names: BTreeSet<String> = original_items
        .iter()
        .filter_map(|it| match it {
            Item::Fn(f) => Some(f.name.clone()),
            _ => None,
        })
        .collect();

    let mut conv = Converter {
        all_captures,
        counter: 0,
        hoisted: Vec::new(),
        hoisted_captures: BTreeMap::new(),
        reserved_counters,
        top_level_fn_names,
    };

    // Rewrite every user fn's body in its own param scope with no enclosing
    // closure captures. Synthesized lambda fns appended to `hoisted` inherit
    // their captures from each lambda's typecheck entry.
    let mut new_items: Vec<Item> = Vec::with_capacity(original_items.len());
    for item in original_items {
        match item {
            Item::Import(_) | Item::Type(_) | Item::Effect(_) => new_items.push(item),
            Item::Fn(mut f) => {
                let param_names: BTreeSet<String> =
                    f.params.iter().map(|p| p.name.clone()).collect();
                f.body = conv.rewrite_block(f.body, &param_names, &[]);
                new_items.push(Item::Fn(f));
            }
        }
    }
    let Converter {
        hoisted,
        hoisted_captures,
        ..
    } = conv;
    new_items.extend(hoisted);

    // Build the flat per-fn captures summary from the final item list.
    // Original user fns have empty capture lists at this layer (lambda
    // captures are attached to the `ClosureRecord` nodes inside their
    // bodies); synthetic `$lambda_N` fns report their captured names,
    // looked up by name directly in `hoisted_captures` (Phase C+ Part 2
    // R4 fixup: previously reverse-parsed `$lambda_N` → counter; the
    // direct cross-reference avoids a silent failure mode if synth-fn
    // naming changes).
    let captures: Vec<(String, Vec<String>)> = new_items
        .iter()
        .filter_map(|it| match it {
            Item::Fn(f) => {
                let caps_names = hoisted_captures
                    .get(&f.name)
                    .map(|v| v.iter().map(|(s, _)| s.clone()).collect())
                    .unwrap_or_default();
                Some((f.name.clone(), caps_names))
            }
            _ => None,
        })
        .collect();

    // Plan B' Stage 6.8 Phase C+ Part 2 — typed captures map for
    // codegen consumption. The `hoisted_captures` map is already
    // keyed by synth fn name (R4 fixup), so this is a direct
    // ownership transfer rather than a rebuild — original user fns
    // aren't in the map (no synth entry was inserted for them).
    let captures_typed: BTreeMap<String, Vec<(String, Ty)>> = hoisted_captures;

    colored.mono.anf.checked.program.items = new_items;

    ClosureConvertedProgram {
        colored,
        captures,
        captures_typed,
    }
}

struct Converter {
    all_captures: Vec<(Span, Vec<(String, Ty)>)>,
    counter: usize,
    hoisted: Vec<Item>,
    /// Per-synthetic-lambda capture list, keyed by the synth fn's
    /// name (`$lambda_<N>`). A `BTreeMap` keyed by name rather than
    /// counter so the program-level summary at the end of `convert`
    /// (and Phase C+ Part 2's typed-captures map) can look up by
    /// fn name directly without reverse-parsing `$lambda_<N>` —
    /// avoids a silent failure mode if synth-fn naming changes.
    hoisted_captures: BTreeMap<String, Vec<(String, Ty)>>,
    /// Counter values that mangle to the same linker symbol as a
    /// user-defined top-level fn (`__lambda_N`). `allocate_counter`
    /// skips past any value in this set so synthetic names stay unique
    /// at the symbol-table level.
    reserved_counters: BTreeSet<usize>,
    /// Plan B' Stage 6.8 Task 104 — set of user-defined top-level fn
    /// names. When `rewrite_expr` sees `Expr::Ident(name)` outside a
    /// callee position, and `name` is in this set, it rewrites to
    /// `Expr::ClosureRecord { code_fn_name: name, env_exprs: [], .. }`.
    /// The caller's `Expr::Call { callee: Ident(name), .. }` arm
    /// short-circuits the rewrite for callees in this set so direct
    /// dispatch is preserved.
    top_level_fn_names: BTreeSet<String>,
}

impl Converter {
    fn capture_at(&self, span: &Span) -> Vec<(String, Ty)> {
        self.all_captures
            .iter()
            .find(|(s, _)| s == span)
            .map(|(_, c)| c.clone())
            .unwrap_or_default()
    }

    /// Allocate the next synthetic `$lambda_N` counter, skipping any
    /// value that would mangle to the same linker symbol as a
    /// user-defined top-level fn named `__lambda_N`. Returns the
    /// chosen counter; callers build `format!("$lambda_{}", N)` and
    /// use `counter` as the `hoisted_captures` map key so the program-
    /// level summary can find the right capture list for each `N`
    /// without walking the AST.
    fn allocate_counter(&mut self) -> usize {
        while self.reserved_counters.contains(&self.counter) {
            self.counter += 1;
        }
        let n = self.counter;
        self.counter += 1;
        n
    }

    fn rewrite_block(
        &mut self,
        b: Block,
        enclosing_locals: &BTreeSet<String>,
        captures: &[(String, Ty)],
    ) -> Block {
        // Clone enclosing locals so we can accumulate let bindings within
        // this block without leaking them up-scope.
        let mut locals = enclosing_locals.clone();
        let mut new_stmts: Vec<Stmt> = Vec::with_capacity(b.stmts.len());
        for s in b.stmts {
            match s {
                Stmt::Let(l) => {
                    let value = self.rewrite_expr(l.value, &locals, captures);
                    locals.insert(l.name.clone());
                    new_stmts.push(Stmt::Let(LetStmt { value, ..l }));
                }
                Stmt::Expr(e) => {
                    new_stmts.push(Stmt::Expr(self.rewrite_expr(e, &locals, captures)));
                }
                Stmt::Perform(p) => {
                    new_stmts.push(Stmt::Perform(self.rewrite_perform(p, &locals, captures)));
                }
            }
        }
        let tail = b.tail.map(|t| self.rewrite_expr(t, &locals, captures));
        Block {
            stmts: new_stmts,
            tail,
            span: b.span,
        }
    }

    fn rewrite_perform(
        &mut self,
        p: PerformExpr,
        locals: &BTreeSet<String>,
        captures: &[(String, Ty)],
    ) -> PerformExpr {
        let args = p
            .args
            .into_iter()
            .map(|a| self.rewrite_expr(a, locals, captures))
            .collect();
        PerformExpr { args, ..p }
    }

    fn rewrite_expr(
        &mut self,
        e: Expr,
        locals: &BTreeSet<String>,
        captures: &[(String, Ty)],
    ) -> Expr {
        match e {
            Expr::IntLit(..) | Expr::StringLit(..) | Expr::BoolLit(..) | Expr::CharLit(..) => e,
            Expr::Ident(name, span) => {
                if locals.contains(&name) {
                    // Local param / let — passes through unchanged.
                    Expr::Ident(name, span)
                } else if let Some((idx, (_, ty))) =
                    captures.iter().enumerate().find(|(_, (n, _))| *n == name)
                {
                    Expr::ClosureEnvLoad {
                        kind: slot_kind_for_ty(ty),
                        index: idx,
                        name,
                        span,
                    }
                } else if self.top_level_fn_names.contains(&name) {
                    // Plan B' Stage 6.8 Task 104 — fn-as-value
                    // materialization. A bare `Ident(top_level_fn)`
                    // outside callee position represents using the fn
                    // as a Ty::Fn value (e.g., `let f = id_fn`,
                    // `apply(my_fn, 42)`). Rewrite to a captureless
                    // ClosureRecord so codegen allocates a record
                    // {header, code_ptr@8} on the GC heap. Codegen's
                    // existing `lower_closure_record` (env_len = 0)
                    // handles the empty-env case. The caller's
                    // `Expr::Call { callee: Ident(name), .. }` arm
                    // short-circuits this rewrite for callee names so
                    // direct dispatch via `user_fn_refs` is preserved.
                    Expr::ClosureRecord {
                        code_fn_name: name,
                        env_exprs: Vec::new(),
                        env_slot_kinds: Vec::new(),
                        span,
                    }
                } else {
                    // Builtin fn reference (e.g., `int_to_string`) or a
                    // legitimately-free name that resolve/typecheck
                    // already accepted. Passes through unchanged.
                    Expr::Ident(name, span)
                }
            }
            Expr::Binary { op, lhs, rhs, span } => Expr::Binary {
                op,
                lhs: Box::new(self.rewrite_expr(*lhs, locals, captures)),
                rhs: Box::new(self.rewrite_expr(*rhs, locals, captures)),
                span,
            },
            Expr::Unary { op, operand, span } => Expr::Unary {
                op,
                operand: Box::new(self.rewrite_expr(*operand, locals, captures)),
                span,
            },
            Expr::If {
                cond,
                then_block,
                else_block,
                span,
            } => Expr::If {
                cond: Box::new(self.rewrite_expr(*cond, locals, captures)),
                then_block: Box::new(self.rewrite_block(*then_block, locals, captures)),
                else_block: Box::new(self.rewrite_block(*else_block, locals, captures)),
                span,
            },
            Expr::Match {
                scrutinee,
                arms,
                span,
            } => {
                let scrutinee = Box::new(self.rewrite_expr(*scrutinee, locals, captures));
                let arms: Vec<MatchArm> = arms
                    .into_iter()
                    .map(|a| {
                        // Plan A3 task 39: Pattern::Var bindings (top-
                        // level or nested inside Ctor/Tuple patterns)
                        // are arm-local. Extend `locals` for this arm's
                        // body rewrite so capture-vs-local detection
                        // treats them correctly.
                        let mut arm_locals = locals.clone();
                        crate::typecheck::pattern_bindings(&a.pattern, &mut arm_locals);
                        MatchArm {
                            body: self.rewrite_expr(a.body, &arm_locals, captures),
                            ..a
                        }
                    })
                    .collect();
                Expr::Match {
                    scrutinee,
                    arms,
                    span,
                }
            }
            Expr::Block(b) => Expr::Block(Box::new(self.rewrite_block(*b, locals, captures))),
            Expr::Call { callee, args, span } => {
                // Plan B' Stage 6.8 Task 104 — preserve direct dispatch
                // for `Call { callee: Ident(top_level_fn), .. }`. The
                // Ident arm above would otherwise rewrite the callee to
                // a ClosureRecord, forcing every direct call to allocate
                // a closure record. Short-circuit here: if the callee is
                // a bare Ident naming a top-level fn (and not shadowed
                // by a local or capture), keep the Ident so codegen's
                // direct-dispatch path matches.
                let callee = match *callee {
                    Expr::Ident(ref name, _)
                        if !locals.contains(name)
                            && !captures.iter().any(|(n, _)| n == name)
                            && self.top_level_fn_names.contains(name) =>
                    {
                        *callee
                    }
                    other => self.rewrite_expr(other, locals, captures),
                };
                Expr::Call {
                    callee: Box::new(callee),
                    args: args
                        .into_iter()
                        .map(|a| self.rewrite_expr(a, locals, captures))
                        .collect(),
                    span,
                }
            }
            Expr::Perform(p) => Expr::Perform(self.rewrite_perform(p, locals, captures)),
            Expr::Lambda {
                params,
                return_type,
                effects,
                effect_row_var,
                body,
                span,
            } => {
                // Allocate the synthetic name up front so outer lambdas
                // get lower numbers than the lambdas nested inside them.
                // `allocate_counter` skips values reserved by
                // user-defined `__lambda_N` top-level fns.
                let counter = self.allocate_counter();
                let fn_name = format!("$lambda_{counter}");

                let caps: Vec<(String, Ty)> = self.capture_at(&span);

                // env_exprs evaluate in the scope where the original lambda
                // was written. A capture that is itself a capture of the
                // enclosing scope rewrites into a `ClosureEnvLoad` on the
                // enclosing closure's env — threading the value down.
                let env_exprs: Vec<Expr> = caps
                    .iter()
                    .map(|(n, _)| {
                        self.rewrite_expr(Expr::Ident(n.clone(), span.clone()), locals, captures)
                    })
                    .collect();
                let env_slot_kinds: Vec<EnvSlotKind> =
                    caps.iter().map(|(_, t)| slot_kind_for_ty(t)).collect();

                // Rewrite the lambda body in its own scope: only its
                // params are locals, and its own captures become the
                // env-load source.
                let inner_locals: BTreeSet<String> =
                    params.iter().map(|p| p.name.clone()).collect();
                let rewritten_body = self.rewrite_expr(*body, &inner_locals, &caps);

                let body_block = Block {
                    stmts: Vec::new(),
                    tail: Some(rewritten_body),
                    span: span.clone(),
                };
                let synthetic = FnDecl {
                    name: fn_name.clone(),
                    name_span: span.clone(),
                    // Plan B Task 47: synthetic fn-from-lambda inherits
                    // the lambda's row variable (if any) and has no
                    // generic parameters of its own — closure
                    // conversion never introduces new type parameters.
                    generic_params: Vec::new(),
                    params,
                    return_type,
                    effects,
                    effect_row_var,
                    body: body_block,
                    span: span.clone(),
                };
                self.hoisted.push(Item::Fn(Box::new(synthetic)));
                self.hoisted_captures.insert(fn_name.clone(), caps);

                Expr::ClosureRecord {
                    code_fn_name: fn_name,
                    env_exprs,
                    env_slot_kinds,
                    span,
                }
            }
            Expr::ClosureRecord { .. } | Expr::ClosureEnvLoad { .. } => {
                unreachable!("closure_convert: input AST contains post-CC nodes (pass run twice?)")
            }
            // Plan A3 task 37: record literal. Captures in the field
            // values must be rewritten just like any other expression
            // position — a record literal can close over locals or
            // outer captures.
            Expr::RecordLit { name, fields, span } => {
                let rewritten_fields = fields
                    .into_iter()
                    .map(|f| RecordFieldLit {
                        name: f.name,
                        value: self.rewrite_expr(f.value, locals, captures),
                        span: f.span,
                    })
                    .collect();
                Expr::RecordLit {
                    name,
                    fields: rewritten_fields,
                    span,
                }
            }
            // Plan B task 53 — handler expressions participate in
            // closure conversion like any compound expression: each
            // arm body may close over outer-scope captures, and the
            // arms themselves introduce new locals that take
            // precedence inside their bodies.
            //
            // The `return` arm binds a single value; each operation
            // arm binds its parameter list plus the trailing
            // continuation `k`. We recurse into all arm bodies under
            // an extended locals set, then restore.
            //
            // (Closure conversion runs strictly after typecheck; a
            // `handle` reaching this code is fine — the typecheck
            // E0134 error is non-fatal and downstream passes still
            // need a structurally correct walk so the workspace
            // compiles cleanly.)
            Expr::Handle {
                body,
                return_arm,
                op_arms,
                span,
            } => {
                let new_body = self.rewrite_expr(*body, locals, captures);
                let new_return_arm = return_arm.map(|ra| {
                    let mut arm_locals = locals.clone();
                    arm_locals.insert(ra.binding.clone());
                    let new_body = self.rewrite_expr(ra.body, &arm_locals, captures);
                    Box::new(crate::ast::HandleReturnArm {
                        binding: ra.binding,
                        binding_span: ra.binding_span,
                        body: new_body,
                        span: ra.span,
                    })
                });
                let new_op_arms = op_arms
                    .into_iter()
                    .map(|arm| {
                        let mut arm_locals = locals.clone();
                        for p in &arm.params {
                            arm_locals.insert(p.name.clone());
                        }
                        arm_locals.insert(arm.k_name.clone());
                        let new_body = self.rewrite_expr(arm.body, &arm_locals, captures);
                        crate::ast::HandleOpArm {
                            body: new_body,
                            ..arm
                        }
                    })
                    .collect();
                Expr::Handle {
                    body: Box::new(new_body),
                    return_arm: new_return_arm,
                    op_arms: new_op_arms,
                    span,
                }
            }
        }
    }
}

pub(crate) fn slot_kind_for_ty(ty: &Ty) -> EnvSlotKind {
    match ty {
        Ty::Int => EnvSlotKind::Int,
        Ty::Bool => EnvSlotKind::Bool,
        Ty::Char => EnvSlotKind::Char,
        Ty::Byte => EnvSlotKind::Byte,
        Ty::Unit => EnvSlotKind::Unit,
        Ty::String => EnvSlotKind::String,
        Ty::Fn(_) => EnvSlotKind::Closure,
        Ty::User(_, _) => EnvSlotKind::User,
        // Plan B task 48 — post-typecheck IR shouldn't have unbound
        // type variables in capture types: the codegen-entry walker
        // (`contains_apply_or_generic_ref`) rejects programs whose
        // AST has surface generic syntax, and closure conversion
        // runs after that gate.
        Ty::Var(_) => unreachable!(
            "closure conversion: Ty::Var is impossible after Plan B task 48 codegen-entry guard"
        ),
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::ast::{EnvSlotKind, Expr, Item};

    fn run(src: &str) -> ClosureConvertedProgram {
        let (toks, _) = crate::lexer::lex("t.sigil", src);
        let (prog, _) = crate::parser::parse("t.sigil", &toks);
        let (rp, _) = crate::resolve::resolve(prog);
        let (checked, errs) = crate::typecheck::typecheck(rp.program);
        assert!(errs.is_empty(), "expected clean typecheck, got: {errs:?}");
        let anf = crate::elaborate::elaborate(checked);
        let mono = crate::monomorphize::monomorphize(anf);
        let colored = crate::color::infer_colors(mono);
        convert(colored)
    }

    fn items(cc: &ClosureConvertedProgram) -> &[Item] {
        &cc.colored.mono.anf.checked.program.items
    }

    fn fn_by_name<'a>(cc: &'a ClosureConvertedProgram, name: &str) -> &'a FnDecl {
        items(cc)
            .iter()
            .find_map(|it| match it {
                Item::Fn(f) if f.name == name => Some(f.as_ref()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("fn `{name}` not found"))
    }

    #[test]
    fn no_lambda_no_hoist() {
        // Pure Stage-1 program with zero lambdas: items list is
        // unchanged, no synthetic fns, no captures summary beyond the
        // original fn's entry.
        let src = "fn main() -> Int ![] { 42 }\n";
        let cc = run(src);
        let names: Vec<&str> = items(&cc)
            .iter()
            .filter_map(|it| match it {
                Item::Fn(f) => Some(f.name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["main"]);
    }

    #[test]
    fn iife_with_no_captures_hoists_one_synthetic() {
        // (fn (x: Int) -> Int ![] => x + 1)(41) — single lambda, no
        // captures. Expect: `main` and `$lambda_0` in items; the site
        // of the lambda becomes a `ClosureRecord { code_fn_name:
        // "$lambda_0", env_*: [] }`.
        let src = "fn main() -> Int ![] { (fn (x: Int) -> Int ![] => x + 1)(41) }\n";
        let cc = run(src);
        let names: Vec<&str> = items(&cc)
            .iter()
            .filter_map(|it| match it {
                Item::Fn(f) => Some(f.name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["main", "$lambda_0"]);

        // Find the ClosureRecord in main's body.
        let main = fn_by_name(&cc, "main");
        let tail = main.body.tail.as_ref().expect("main has a tail");
        match tail {
            Expr::Call { callee, .. } => match callee.as_ref() {
                Expr::ClosureRecord {
                    code_fn_name,
                    env_exprs,
                    env_slot_kinds,
                    ..
                } => {
                    assert_eq!(code_fn_name, "$lambda_0");
                    assert!(env_exprs.is_empty(), "no captures expected");
                    assert!(env_slot_kinds.is_empty(), "no kinds expected");
                }
                other => panic!("expected ClosureRecord as callee, got {other:?}"),
            },
            other => panic!("expected Call tail, got {other:?}"),
        }

        // $lambda_0's body still references the param `x`; no env loads.
        let lam = fn_by_name(&cc, "$lambda_0");
        let body_tail = lam.body.tail.as_ref().expect("lambda has a tail");
        // The body is `x + 1`, which is a `Binary` with `Ident("x")` as lhs.
        // No ClosureEnvLoad should appear anywhere in the body.
        assert!(
            !contains_env_load(body_tail),
            "lambda with no captures should have no ClosureEnvLoad"
        );
    }

    #[test]
    fn iife_with_int_capture() {
        // `let x: Int = 10; (fn (y: Int) -> Int ![] => x + y)(5)`
        // Expect: main + $lambda_0; lambda captures `x: Int` → env slot 0
        // with kind Int; inside the lambda, `x` reads become ClosureEnvLoad.
        let src = "fn main() -> Int ![] {\n\
                     let x: Int = 10;\n\
                     (fn (y: Int) -> Int ![] => x + y)(5)\n\
                   }\n";
        let cc = run(src);

        let main = fn_by_name(&cc, "main");
        let tail = main.body.tail.as_ref().expect("main has a tail");
        match tail {
            Expr::Call { callee, .. } => match callee.as_ref() {
                Expr::ClosureRecord {
                    code_fn_name,
                    env_exprs,
                    env_slot_kinds,
                    ..
                } => {
                    assert_eq!(code_fn_name, "$lambda_0");
                    assert_eq!(env_exprs.len(), 1, "one capture");
                    assert_eq!(env_slot_kinds, &[EnvSlotKind::Int]);
                    // env_exprs[0] is an Ident("x") — it evaluates in main's scope.
                    match &env_exprs[0] {
                        Expr::Ident(n, _) => assert_eq!(n, "x"),
                        other => panic!("expected Ident(\"x\"), got {other:?}"),
                    }
                }
                other => panic!("expected ClosureRecord, got {other:?}"),
            },
            other => panic!("expected Call tail, got {other:?}"),
        }

        // Inside $lambda_0, the reference to `x` is a ClosureEnvLoad(0, Int).
        let lam = fn_by_name(&cc, "$lambda_0");
        let body_tail = lam.body.tail.as_ref().expect("lambda has a tail");
        assert!(
            find_env_load(body_tail, "x").is_some(),
            "lambda body should have ClosureEnvLoad for `x`, got {body_tail:?}"
        );
        // The param `y` must stay as a plain Ident.
        assert!(
            find_ident(body_tail, "y").is_some(),
            "lambda body should still reference param `y`"
        );
    }

    #[test]
    fn nested_lambda_threads_capture_through_outer() {
        // Outer captures `x` from main; inner captures `x` too (via
        // transitive propagation in typecheck). In the rewritten tree:
        //  * outer's env_exprs = [Ident("x")] (main scope)
        //  * inner's env_exprs = [ClosureEnvLoad(0, "x", Int)] (outer scope)
        //  * inner's body loads `x` and `y`: ClosureEnvLoad(0, "x") + Ident("y").
        let src = "fn main() -> Int ![] {\n\
                     let x: Int = 10;\n\
                     ((fn (_p: Int) -> Int ![] => (fn (y: Int) -> Int ![] => x + y)(1))(0))\n\
                   }\n";
        let cc = run(src);

        let names: Vec<&str> = items(&cc)
            .iter()
            .filter_map(|it| match it {
                Item::Fn(f) => Some(f.name.as_str()),
                _ => None,
            })
            .collect();
        // main + two synthetic lambdas. Outer is processed first (gets
        // counter 0), then inner (counter 1). Inner is pushed onto
        // `hoisted` before outer because the outer's body rewrite
        // completes only after the inner's rewrite returns.
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"main"));
        assert!(names.contains(&"$lambda_0"));
        assert!(names.contains(&"$lambda_1"));

        // Outer lambda (the one appearing at the lambda-nesting root in
        // source text) received the lower counter → `$lambda_0`.
        let outer = fn_by_name(&cc, "$lambda_0");
        let outer_tail = outer.body.tail.as_ref().unwrap();
        // Outer's body is `inner_lambda(1)` — a Call whose callee is a
        // ClosureRecord for `$lambda_1`.
        match outer_tail {
            Expr::Call { callee, .. } => match callee.as_ref() {
                Expr::ClosureRecord {
                    code_fn_name,
                    env_exprs,
                    env_slot_kinds,
                    ..
                } => {
                    assert_eq!(code_fn_name, "$lambda_1");
                    // The inner's capture `x` is threaded through outer's
                    // own env — so outer's construction of the inner's
                    // env_exprs rewrites `Ident(x)` into a ClosureEnvLoad
                    // against outer's env.
                    assert_eq!(env_slot_kinds, &[EnvSlotKind::Int]);
                    assert_eq!(env_exprs.len(), 1);
                    match &env_exprs[0] {
                        Expr::ClosureEnvLoad {
                            name, index, kind, ..
                        } => {
                            assert_eq!(name, "x");
                            assert_eq!(*index, 0);
                            assert_eq!(*kind, EnvSlotKind::Int);
                        }
                        other => {
                            panic!("expected ClosureEnvLoad in outer's env_exprs[0], got {other:?}")
                        }
                    }
                }
                other => panic!("expected ClosureRecord callee, got {other:?}"),
            },
            other => panic!("expected Call, got {other:?}"),
        }
    }

    /// Regression test for PR #7 review item 1: a user-defined fn
    /// named `__lambda_0` mangles to `sigil_user___lambda_0` — the
    /// same linker symbol a synthetic `$lambda_0` would produce.
    /// Closure conversion must detect the collision and allocate the
    /// synthetic name from the next free counter (here, `$lambda_1`
    /// whose mangled form `sigil_user___lambda_1` doesn't collide).
    /// The alternative — letting Cranelift surface a duplicate-symbol
    /// error at compile time — is correct but opaque; this test pins
    /// the preferred behaviour (skip past reserved counters).
    #[test]
    fn user_fn_named_like_synthetic_forces_counter_skip() {
        let src = "fn __lambda_0() -> Int ![] { 100 }\n\
                   fn main() -> Int ![] {\n\
                     (fn (x: Int) -> Int ![] => x + 1)(41)\n\
                   }\n";
        let cc = run(src);
        let names: Vec<&str> = items(&cc)
            .iter()
            .filter_map(|it| match it {
                Item::Fn(f) => Some(f.name.as_str()),
                _ => None,
            })
            .collect();
        // `__lambda_0` stays as a user fn; the synthetic gets counter
        // 1, not 0, so `$lambda_1` is the hoisted name.
        assert!(names.contains(&"__lambda_0"), "user fn lost: {names:?}");
        assert!(
            names.contains(&"$lambda_1"),
            "synthetic should skip reserved 0 and use counter 1: {names:?}"
        );
        assert!(
            !names.contains(&"$lambda_0"),
            "synthetic must not collide with user `__lambda_0`: {names:?}"
        );

        // The call site in main references `$lambda_1`.
        let main = fn_by_name(&cc, "main");
        let tail = main.body.tail.as_ref().expect("main has a tail");
        match tail {
            Expr::Call { callee, .. } => match callee.as_ref() {
                Expr::ClosureRecord { code_fn_name, .. } => {
                    assert_eq!(code_fn_name, "$lambda_1");
                }
                other => panic!("expected ClosureRecord, got {other:?}"),
            },
            other => panic!("expected Call, got {other:?}"),
        }
    }

    /// Reserved counters are arbitrary positive integers; a user fn
    /// named `__lambda_5` reserves `5` but `0..=4` and `6..` remain
    /// free. This test pins the skip behaviour when the reserved
    /// counter is above the first synthetic's natural counter.
    #[test]
    fn non_zero_reserved_counter_is_skipped() {
        // Reserve counter 0 with `__lambda_0`; three lambdas should
        // take `$lambda_1`, `$lambda_2`, `$lambda_3`.
        let src = "fn __lambda_0() -> Int ![] { 0 }\n\
                   fn main() -> Int ![] {\n\
                     let a: Int = (fn (x: Int) -> Int ![] => x)(1);\n\
                     let b: Int = (fn (x: Int) -> Int ![] => x)(2);\n\
                     let c: Int = (fn (x: Int) -> Int ![] => x)(3);\n\
                     a + b + c\n\
                   }\n";
        let cc = run(src);
        let names: std::collections::BTreeSet<&str> = items(&cc)
            .iter()
            .filter_map(|it| match it {
                Item::Fn(f) => Some(f.name.as_str()),
                _ => None,
            })
            .collect();
        for expected in ["__lambda_0", "main", "$lambda_1", "$lambda_2", "$lambda_3"] {
            assert!(
                names.contains(expected),
                "missing `{expected}` in {names:?}"
            );
        }
        assert!(
            !names.contains("$lambda_0"),
            "synthetic must not collide with `__lambda_0`"
        );
    }

    // --- tree-walking helpers ------------------------------------------

    fn contains_env_load(e: &Expr) -> bool {
        match e {
            Expr::ClosureEnvLoad { .. } => true,
            Expr::Binary { lhs, rhs, .. } => contains_env_load(lhs) || contains_env_load(rhs),
            Expr::Unary { operand, .. } => contains_env_load(operand),
            Expr::Call { callee, args, .. } => {
                contains_env_load(callee) || args.iter().any(contains_env_load)
            }
            Expr::Match {
                scrutinee, arms, ..
            } => contains_env_load(scrutinee) || arms.iter().any(|a| contains_env_load(&a.body)),
            Expr::Block(b) => b.tail.as_ref().map(contains_env_load).unwrap_or(false),
            Expr::ClosureRecord { env_exprs, .. } => env_exprs.iter().any(contains_env_load),
            _ => false,
        }
    }

    fn find_env_load<'a>(e: &'a Expr, target_name: &str) -> Option<&'a Expr> {
        match e {
            Expr::ClosureEnvLoad { name, .. } if name == target_name => Some(e),
            Expr::Binary { lhs, rhs, .. } => {
                find_env_load(lhs, target_name).or_else(|| find_env_load(rhs, target_name))
            }
            Expr::Unary { operand, .. } => find_env_load(operand, target_name),
            Expr::Call { callee, args, .. } => find_env_load(callee, target_name)
                .or_else(|| args.iter().find_map(|a| find_env_load(a, target_name))),
            Expr::Match {
                scrutinee, arms, ..
            } => find_env_load(scrutinee, target_name).or_else(|| {
                arms.iter()
                    .find_map(|a| find_env_load(&a.body, target_name))
            }),
            Expr::Block(b) => b.tail.as_ref().and_then(|t| find_env_load(t, target_name)),
            Expr::ClosureRecord { env_exprs, .. } => {
                env_exprs.iter().find_map(|e| find_env_load(e, target_name))
            }
            _ => None,
        }
    }

    fn find_ident<'a>(e: &'a Expr, target_name: &str) -> Option<&'a Expr> {
        match e {
            Expr::Ident(n, _) if n == target_name => Some(e),
            Expr::Binary { lhs, rhs, .. } => {
                find_ident(lhs, target_name).or_else(|| find_ident(rhs, target_name))
            }
            Expr::Unary { operand, .. } => find_ident(operand, target_name),
            Expr::Call { callee, args, .. } => find_ident(callee, target_name)
                .or_else(|| args.iter().find_map(|a| find_ident(a, target_name))),
            Expr::Match {
                scrutinee, arms, ..
            } => find_ident(scrutinee, target_name)
                .or_else(|| arms.iter().find_map(|a| find_ident(&a.body, target_name))),
            Expr::Block(b) => b.tail.as_ref().and_then(|t| find_ident(t, target_name)),
            _ => None,
        }
    }
}
