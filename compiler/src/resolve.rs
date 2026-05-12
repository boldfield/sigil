//! Name resolution — plan A1 Stage 1 task 6, extended through every
//! plan that introduced new binders.
//!
//! Sigil's "no shadowing, ever" tenet (`README.md` §"Design philosophy:
//! fight the priors") is enforced here. The rule is:
//!
//! - **Let bindings and fn params are unique across the entire
//!   function.** No two such bindings can share a name, regardless of
//!   scope — disjoint match arms, disjoint if/else branches, and
//!   nested lambda bodies cannot reuse a name a let or fn param has
//!   already bound elsewhere in the same fn. This matches the
//!   typechecker's `env_insert` invariant (a single flat per-fn env
//!   that asserts uniqueness on insertion for lets and fn params);
//!   without this pass, two lets sharing a name in disjoint scopes
//!   slip through to typecheck and trip the assert in debug builds
//!   (silent last-write-wins in release).
//! - **Let bindings additionally cannot shadow** any name visible at
//!   the let site — outer fn params, outer lets, or outer construct-
//!   introduced bindings (lambda params, match patterns, handle arm
//!   params, return-arm bindings). This is the per-scope visibility
//!   check; the per-fn uniqueness check above is the broader rule.
//! - **Construct-introduced bindings** (lambda params, match arm
//!   pattern bindings, handle op-arm params + `k`, handle return-arm
//!   binding) introduce fresh scope: they MAY shadow outer names.
//!   This matches the typechecker's semantics in `check_lambda` /
//!   `check_match` / `check_handle` which use direct `env.insert` for
//!   these constructs (bypassing `env_insert`'s no-shadow assert) but
//!   route every let through `env_insert` (which fires the assert on
//!   shadow).
//! - **Self-collisions inside a single binder construct** — e.g.,
//!   `fn (x: Int, x: Int) => ...`, `Some(x, x) => ...`,
//!   `Effect.op(x, x, k) => ...` — are rejected. Two bindings within
//!   the same parameter list / pattern / handle arm cannot share a
//!   name, even though the construct as a whole gets fresh scope
//!   relative to its outer context.
//!
//! This pass is structural — it doesn't consult types. It assigns no
//! semantic information beyond the no-shadowing rejections; downstream
//! passes (typecheck, closure_convert) consume the unmodified AST.

use crate::ast::*;
use crate::errors::{CompilerError, Severity, Span};
use std::collections::BTreeSet;

#[derive(Clone, Debug)]
pub struct ResolvedProgram {
    pub program: Program,
}

pub fn resolve(program: Program) -> (ResolvedProgram, Vec<CompilerError>) {
    let mut errors = Vec::new();
    // Collect every constructor name declared by `type ... = | C(...) | ...`
    // items so the pattern walker can distinguish bare-ident ctor patterns
    // from binding-introducing ones. The parser emits `Pattern::Var(name)`
    // for any bare identifier in pattern position regardless of case
    // (`parser.rs:1686-1723`); typechecker promotes `Pattern::Var` to
    // nullary-ctor patterns at `check_pattern` time when the name resolves
    // to a ctor of the scrutinee's type. resolve.rs runs before typecheck
    // and has no scrutinee-type context, so the conservative reading is
    // "any name that could be a ctor of any type is treated as a non-
    // binding here." False negatives (a binding pattern whose name happens
    // to collide with some other type's ctor) are caught by typecheck's
    // `env_insert` assert in debug builds.
    let ctor_names = collect_ctor_names(&program);
    for item in &program.items {
        if let Item::Fn(f) = item {
            // Fn-level scope: starts with the fn's params. Param self-
            // collision is checked here so a malformed signature
            // (`fn f(x: Int, x: Int)`) trips before the body walks.
            let mut scope: BTreeSet<String> = BTreeSet::new();
            // `fn_lets` tracks every let / fn-param name bound anywhere
            // in this function so disjoint-scope siblings (`Nil =>
            // { let x = .. }` and `Cons(..) => { let x = .. }`) are
            // rejected up-front rather than slipping into typecheck
            // where `env_insert`'s debug-assert panics. Fn params seed
            // it for symmetry with typecheck (which routes both lets
            // and params through `env_insert`).
            let mut fn_lets: BTreeSet<String> = BTreeSet::new();
            for p in &f.params {
                if !scope.insert(p.name.clone()) {
                    push_redef(&mut errors, p.span.clone(), &p.name);
                }
                // `_` is the discard param — out-of-namespace, like
                // `let _ = ...` discards. Multiple `_` params are
                // independent placeholders, not shadowing.
                if p.name != "_" {
                    fn_lets.insert(p.name.clone());
                }
            }
            resolve_block(&f.body, &scope, &ctor_names, &mut errors, &mut fn_lets);
        }
    }
    (ResolvedProgram { program }, errors)
}

fn collect_ctor_names(program: &Program) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for item in &program.items {
        if let Item::Type(td) = item {
            for variant in &td.variants {
                names.insert(variant.name.clone());
            }
        }
    }
    names
}

fn push_redef(errors: &mut Vec<CompilerError>, span: Span, name: &str) {
    errors.push(CompilerError::new(
        Severity::Error,
        crate::errors::code("E0020"),
        span,
        format!("redefinition of `{name}` — Sigil forbids shadowing"),
    ));
}

/// Walk a `Block`. Lets accumulate into a per-block scope (cloned from
/// the outer scope so blocks are LIFO-fresh — a let inside an inner
/// block doesn't survive past the block end). Within the block, lets
/// check against the running scope for visibility-shadowing AND
/// against `fn_lets` for the broader per-fn uniqueness rule.
fn resolve_block(
    b: &Block,
    outer_scope: &BTreeSet<String>,
    ctor_names: &BTreeSet<String>,
    errors: &mut Vec<CompilerError>,
    fn_lets: &mut BTreeSet<String>,
) {
    let mut scope = outer_scope.clone();
    for s in &b.stmts {
        match s {
            Stmt::Let(l) => {
                // Walk the RHS *before* the let-binding enters scope —
                // a self-referential RHS (`let x: Int = x`) refers to
                // the outer `x` if any, not to the binding being
                // defined. Aligns with non-recursive let semantics.
                resolve_expr(&l.value, &scope, ctor_names, errors, fn_lets);
                // `_` is a discard binding (sequential `let _: T = expr;`
                // for side effect with the value thrown away). Multiple
                // `let _` in the same scope are NOT shadowing — they're
                // independent discards. Idiomatic in every other lang
                // with this convention; suppressing E0020 here closes a
                // documented Sigil-API friction point that LLMs hit
                // when chaining discards (e.g. driving a state machine
                // over multiple inputs).
                if l.name == "_" {
                    scope.insert(l.name.clone());
                } else {
                    // Two checks fire E0020:
                    //   1. `scope.insert` returns false → the name is
                    //      visible at this let site (outer let, fn param,
                    //      pattern binding, lambda param, etc.). The
                    //      classic "no shadow visible binders" case.
                    //   2. `fn_lets.insert` returns false → the name has
                    //      already been bound by a let or fn-param
                    //      somewhere else in this function. Catches
                    //      disjoint-scope siblings (match arm A and arm
                    //      B both `let completed_field_text = ...`)
                    //      that the per-scope check misses because the
                    //      sibling binding isn't visible here.
                    // Fire E0020 at most once even when both checks
                    // would trip (e.g. `let x = 1; let x = 2;` in one
                    // block — scope and fn_lets both already have `x`).
                    let scope_had = !scope.insert(l.name.clone());
                    let fn_had = !fn_lets.insert(l.name.clone());
                    if scope_had || fn_had {
                        push_redef(errors, l.span.clone(), &l.name);
                    }
                }
            }
            Stmt::Expr(e) => resolve_expr(e, &scope, ctor_names, errors, fn_lets),
            Stmt::Perform(p) => {
                for a in &p.args {
                    resolve_expr(a, &scope, ctor_names, errors, fn_lets);
                }
            }
        }
    }
    if let Some(t) = &b.tail {
        resolve_expr(t, &scope, ctor_names, errors, fn_lets);
    }
}

/// Walk an expression. Compound expressions recurse into children;
/// binder-introducing constructs (Lambda, Match, Handle) push a fresh
/// scope before walking their bodies.
fn resolve_expr(
    e: &Expr,
    scope: &BTreeSet<String>,
    ctor_names: &BTreeSet<String>,
    errors: &mut Vec<CompilerError>,
    fn_lets: &mut BTreeSet<String>,
) {
    match e {
        // Leaves and post-closure-conversion shapes (resolve runs pre-CC,
        // so the post-CC variants are unreachable in practice).
        Expr::IntLit(_, _)
        | Expr::FloatLit(_, _)
        | Expr::BoolLit(_, _)
        | Expr::CharLit(_, _)
        | Expr::UnitLit(_)
        | Expr::StringLit(_, _)
        | Expr::Ident(_, _)
        | Expr::ClosureRecord { .. }
        | Expr::ClosureEnvLoad { .. } => {}
        Expr::Call { callee, args, .. } => {
            resolve_expr(callee, scope, ctor_names, errors, fn_lets);
            for a in args {
                resolve_expr(a, scope, ctor_names, errors, fn_lets);
            }
        }
        Expr::Perform(p) => {
            for a in &p.args {
                resolve_expr(a, scope, ctor_names, errors, fn_lets);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            resolve_expr(lhs, scope, ctor_names, errors, fn_lets);
            resolve_expr(rhs, scope, ctor_names, errors, fn_lets);
        }
        Expr::Unary { operand, .. } => resolve_expr(operand, scope, ctor_names, errors, fn_lets),
        Expr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            resolve_expr(cond, scope, ctor_names, errors, fn_lets);
            resolve_block(then_block, scope, ctor_names, errors, fn_lets);
            resolve_block(else_block, scope, ctor_names, errors, fn_lets);
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            resolve_expr(scrutinee, scope, ctor_names, errors, fn_lets);
            for arm in arms {
                let mut inner = scope.clone();
                let mut arm_pattern_seen: BTreeSet<String> = BTreeSet::new();
                collect_pattern_bindings_with_dup_check(
                    &arm.pattern,
                    &mut arm_pattern_seen,
                    ctor_names,
                    errors,
                );
                inner.extend(arm_pattern_seen);
                resolve_expr(&arm.body, &inner, ctor_names, errors, fn_lets);
            }
        }
        Expr::Block(b) => resolve_block(b, scope, ctor_names, errors, fn_lets),
        Expr::Lambda { params, body, .. } => {
            let mut inner = scope.clone();
            let mut lambda_param_seen: BTreeSet<String> = BTreeSet::new();
            for p in params {
                // `_` is a discard param — `fn(_, _) -> ...` is legal
                // (two unused params), mirroring patterns and let-
                // bindings. The dup-check skips only `_`.
                if p.name != "_" && !lambda_param_seen.insert(p.name.clone()) {
                    push_redef(errors, p.span.clone(), &p.name);
                }
                // The set's `insert` is idempotent on the second
                // occurrence (set membership has no value to overwrite);
                // we add to `inner` once so the body walk sees the name
                // in scope regardless of whether the dup-check fired.
                inner.insert(p.name.clone());
            }
            resolve_expr(body, &inner, ctor_names, errors, fn_lets);
        }
        Expr::RecordLit { fields, .. } => {
            for f in fields {
                resolve_expr(&f.value, scope, ctor_names, errors, fn_lets);
            }
        }
        Expr::Tuple { elems, .. } => {
            for el in elems {
                resolve_expr(el, scope, ctor_names, errors, fn_lets);
            }
        }
        Expr::Handle {
            body,
            return_arm,
            op_arms,
            ..
        } => {
            resolve_expr(body, scope, ctor_names, errors, fn_lets);
            for arm in op_arms {
                let mut inner = scope.clone();
                let mut arm_param_seen: BTreeSet<String> = BTreeSet::new();
                for p in &arm.params {
                    if !arm_param_seen.insert(p.name.clone()) {
                        push_redef(errors, p.span.clone(), &p.name);
                    }
                    inner.insert(p.name.clone());
                }
                if !arm_param_seen.insert(arm.k_name.clone()) {
                    push_redef(errors, arm.k_span.clone(), &arm.k_name);
                }
                inner.insert(arm.k_name.clone());
                resolve_expr(&arm.body, &inner, ctor_names, errors, fn_lets);
            }
            if let Some(ra) = return_arm {
                let mut inner = scope.clone();
                inner.insert(ra.binding.clone());
                resolve_expr(&ra.body, &inner, ctor_names, errors, fn_lets);
            }
        }
    }
}

/// Walk a pattern collecting its bindings. Duplicate bindings within
/// the same pattern (`Some(x, x)`, `(x, x)`, `Point { x, x: y }` once
/// renames land) emit E0020 against the second occurrence's span.
///
/// Mirrors `typecheck::pattern_bindings` but also emits a diagnostic
/// on duplicates rather than silently deduplicating into a `BTreeSet`.
///
/// `Pattern::Var(name)` whose name is in `ctor_names` is treated as a
/// (presumed) nullary-constructor pattern with no binding — typechecker
/// will perform the precise scrutinee-aware promotion at
/// `check_pattern` time. Skipping ctor names here prevents false
/// positives like `(None, None)` firing E0020 for "duplicate `None`."
fn collect_pattern_bindings_with_dup_check(
    p: &Pattern,
    seen: &mut BTreeSet<String>,
    ctor_names: &BTreeSet<String>,
    errors: &mut Vec<CompilerError>,
) {
    match p {
        Pattern::Wildcard(_)
        | Pattern::IntLit(_, _)
        | Pattern::BoolLit(_, _)
        | Pattern::CharLit(_, _) => {}
        Pattern::Var(name, span) => {
            // Conservative ctor-name check: any name that resolves to a
            // declared constructor (across any user type) is treated as
            // a non-binding here. The typechecker has scrutinee-type
            // context and performs the precise promotion at
            // `check_pattern`. A name that's a ctor for some other type
            // but not the scrutinee's type stays a binding under
            // typecheck — that gap is caught by `env_insert`'s no-shadow
            // assert in debug builds.
            if ctor_names.contains(name) {
                return;
            }
            if !seen.insert(name.clone()) {
                push_redef(errors, span.clone(), name);
            }
        }
        Pattern::Tuple(pats, _) => {
            for sub in pats {
                collect_pattern_bindings_with_dup_check(sub, seen, ctor_names, errors);
            }
        }
        Pattern::Ctor { fields, .. } => match fields {
            CtorPatternFields::Unit => {}
            CtorPatternFields::Positional(ps) => {
                for sub in ps {
                    collect_pattern_bindings_with_dup_check(sub, seen, ctor_names, errors);
                }
            }
            CtorPatternFields::Record(fs) => {
                for f in fs {
                    collect_pattern_bindings_with_dup_check(&f.pattern, seen, ctor_names, errors);
                }
            }
        },
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::{lexer::lex, parser::parse};

    fn pipeline(src: &str) -> Vec<CompilerError> {
        let (toks, lex_errs) = lex("x.sigil", src);
        let (prog, parse_errs) = parse("x.sigil", &toks);
        let (_resolved, res_errs) = resolve(prog);
        let mut all = lex_errs;
        all.extend(parse_errs);
        all.extend(res_errs);
        all
    }

    fn has_e0020(errs: &[CompilerError]) -> bool {
        errs.iter().any(|e| e.code.as_str() == "E0020")
    }

    #[test]
    fn hello_world_resolves_clean() {
        let src = "import std.io\nfn main() -> Int ![IO] { perform IO.println(\"hi\"); 0 }\n";
        assert!(pipeline(src).is_empty());
    }

    #[test]
    fn redefinition_is_e0020() {
        let src = "fn main() -> Int ![] { let x: Int = 1; let x: Int = 2; 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_e0020(&errs),
            "expected E0020 redef error, got: {errs:?}"
        );
    }

    #[test]
    fn underscore_discard_let_is_legal() {
        // `_` is the discard binding — multiple `let _: T = expr;`
        // in the same scope are independent discards, not shadowing.
        // Closes the LLM-authorship friction documented in H02 (haiku
        // writes 7 sequential `let _: Int = check_and_print(...)` to
        // drive a state machine over multiple inputs).
        let src = "fn main() -> Int ![] { \
                     let _: Int = 1; \
                     let _: Int = 2; \
                     let _: Int = 3; \
                     0 \
                   }\n";
        let errs = pipeline(src);
        assert!(
            !has_e0020(&errs),
            "expected NO E0020 for multiple `let _`, got: {errs:?}"
        );
    }

    #[test]
    fn underscore_discard_lambda_param_is_legal() {
        // Lambda params accept `_` as a non-binding wildcard. Two `_`
        // params should not fire E0020.
        let src = "fn main() -> Int ![] { \
                     let f: (Int, Int) -> Int ![] = \
                         fn (_: Int, _: Int) -> Int ![] => 0; \
                     f(1, 2) \
                   }\n";
        let errs = pipeline(src);
        assert!(
            !has_e0020(&errs),
            "expected NO E0020 for `fn (_, _) ...`, got: {errs:?}"
        );
    }

    // ----------------------------------------------------------------
    // F-1 audit follow-up: nested-scope shadowing rejected. README's
    // "no shadowing, ever" tenet enforced through every binder shape.
    // ----------------------------------------------------------------

    #[test]
    fn let_shadowing_inner_block_fires_e0020() {
        // Outer `let x` then an `if`-branch `let x`. Pre-fix: shallow
        // walk missed the inner block. Post-fix: inner `let x`
        // shadows outer `x` → E0020.
        let src = "fn main() -> Int ![] { \
                     let x: Int = 1; \
                     if true { let x: Int = 2; 0 } else { 0 } \
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_e0020(&errs),
            "let inside an if-branch must NOT shadow outer let: {errs:?}"
        );
    }

    #[test]
    fn let_inside_lambda_body_shadowing_outer_fires_e0020() {
        // Outer fn has `let x`; lambda body has another `let x`.
        // The lambda body's scope inherits the outer let; a second
        // `let x` inside the lambda body shadows it.
        let src = "fn main() -> Int ![] { \
                     let x: Int = 1; \
                     let f: (Int) -> Int ![] = fn (y: Int) -> Int ![] => { let x: Int = 2; x + y }; \
                     f(0) \
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_e0020(&errs),
            "let inside a lambda body must NOT shadow outer let: {errs:?}"
        );
    }

    #[test]
    fn let_inside_match_arm_shadowing_outer_fires_e0020() {
        // Outer `let x`; match arm body has block with `let x`.
        let src = "type Opt = | None | Some(Int)\n\
                   fn main() -> Int ![] { \
                     let x: Int = 1; \
                     match Some(0) { Some(_) => { let x: Int = 2; x }, None => x } \
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_e0020(&errs),
            "let inside a match arm body must NOT shadow outer let: {errs:?}"
        );
    }

    #[test]
    fn let_inside_let_rhs_shadowing_outer_fires_e0020() {
        // Outer `let x`; another `let y` whose RHS contains a match
        // with a block arm that has its own `let x`. The inner block
        // sees outer `x` in scope → second `let x` shadows.
        let src = "type Opt = | None | Some(Int)\n\
                   fn main() -> Int ![] { \
                     let x: Int = 1; \
                     let y: Int = match Some(0) { Some(_) => { let x: Int = 2; x }, None => 0 }; \
                     y \
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_e0020(&errs),
            "let inside a match arm in a let-RHS must NOT shadow outer let: {errs:?}"
        );
    }

    #[test]
    fn lambda_param_can_shadow_outer_let() {
        // Construct-introduced binding (lambda param) gets fresh scope:
        // it MAY shadow outer names. This matches typecheck semantics
        // (direct `env.insert`, not `env_insert`).
        let src = "fn main() -> Int ![] { \
                     let x: Int = 1; \
                     let f: (Int) -> Int ![] = fn (x: Int) -> Int ![] => x + 1; \
                     f(0) \
                   }\n";
        let errs = pipeline(src);
        assert!(
            !has_e0020(&errs),
            "lambda param may shadow outer let (fresh-scope construct): {errs:?}"
        );
    }

    #[test]
    fn match_pattern_binding_can_shadow_outer_let() {
        // Match pattern bindings get fresh scope, may shadow.
        let src = "type Opt = | None | Some(Int)\n\
                   fn main() -> Int ![] { \
                     let x: Int = 1; \
                     match Some(0) { Some(x) => x, None => 0 } \
                   }\n";
        let errs = pipeline(src);
        assert!(
            !has_e0020(&errs),
            "match pattern binding may shadow outer let: {errs:?}"
        );
    }

    #[test]
    fn handle_arm_param_can_shadow_outer_let() {
        // Handle arm params + k_name get fresh scope, may shadow.
        let src = "effect Eff { op: (Int) -> Int }\n\
                   fn main() -> Int ![Eff] { \
                     let x: Int = 1; \
                     handle 0 with { Eff.op(x, k) => x + 1 } \
                   }\n";
        let errs = pipeline(src);
        assert!(
            !has_e0020(&errs),
            "handle arm param may shadow outer let: {errs:?}"
        );
    }

    #[test]
    fn lambda_param_self_collision_fires_e0020() {
        // Two lambda params with the same name within the same arg list
        // is a redefinition within the construct's own scope — E0020.
        let src = "fn main() -> Int ![] { \
                     let f: (Int, Int) -> Int ![] = fn (x: Int, x: Int) -> Int ![] => x; \
                     f(1, 2) \
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_e0020(&errs),
            "lambda with two params named `x` must fire E0020: {errs:?}"
        );
    }

    #[test]
    fn pattern_self_collision_in_ctor_fires_e0020() {
        // `Pair(x, x)` reuses `x` within a single pattern.
        let src = "type Pair = | Pair(Int, Int)\n\
                   fn main() -> Int ![] { \
                     match Pair(1, 2) { Pair(x, x) => x } \
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_e0020(&errs),
            "pattern `Pair(x, x)` must fire E0020 on duplicate binding: {errs:?}"
        );
    }

    #[test]
    fn pattern_self_collision_in_tuple_fires_e0020() {
        // `(x, x)` tuple pattern with duplicate binding.
        let src = "fn main() -> Int ![] { \
                     match (1, 2) { (x, x) => x } \
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_e0020(&errs),
            "tuple pattern `(x, x)` must fire E0020 on duplicate binding: {errs:?}"
        );
    }

    #[test]
    fn handle_arm_param_self_collision_fires_e0020() {
        // `Eff.op(x, x, k) =>` — two arm params named `x`.
        let src = "effect Eff { op: (Int, Int) -> Int }\n\
                   fn main() -> Int ![Eff] { \
                     handle 0 with { Eff.op(x, x, k) => x } \
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_e0020(&errs),
            "handle arm with duplicate params must fire E0020: {errs:?}"
        );
    }

    #[test]
    fn handle_arm_k_name_collides_with_param_fires_e0020() {
        // `Eff.op(k, k) =>` — first param named `k` collides with the
        // continuation binding name.
        let src = "effect Eff { op: (Int) -> Int }\n\
                   fn main() -> Int ![Eff] { \
                     handle 0 with { Eff.op(k, k) => 0 } \
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_e0020(&errs),
            "handle arm where param shadows k_name must fire E0020: {errs:?}"
        );
    }

    #[test]
    fn fn_param_shadowed_by_let_in_body_fires_e0020() {
        // Pre-fix this was caught (top-level let against fn params via
        // shallow walk). Pin the existing behavior so the rewrite
        // doesn't regress it.
        let src = "fn f(x: Int) -> Int ![] { let x: Int = 99; x }\n\
                   fn main() -> Int ![] { f(0) }\n";
        let errs = pipeline(src);
        assert!(
            has_e0020(&errs),
            "let in fn body shadowing fn param must fire E0020: {errs:?}"
        );
    }

    #[test]
    fn nested_fns_are_independent_scopes() {
        // Different fns have independent scopes — `x` in `f` doesn't
        // shadow `x` in `main` because they're separate declarations.
        let src = "fn f() -> Int ![] { let x: Int = 1; x }\n\
                   fn main() -> Int ![] { let x: Int = 2; x }\n";
        let errs = pipeline(src);
        assert!(
            !has_e0020(&errs),
            "separate fns must have independent scopes: {errs:?}"
        );
    }

    #[test]
    fn deeply_nested_let_shadowing_fires_e0020() {
        // `if -> if -> let x` shadowing the outermost `let x`.
        let src = "fn main() -> Int ![] { \
                     let x: Int = 1; \
                     if true { \
                       if true { let x: Int = 2; 0 } else { 0 } \
                     } else { 0 } \
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_e0020(&errs),
            "deeply nested let shadow must fire E0020: {errs:?}"
        );
    }

    #[test]
    fn tuple_pattern_with_duplicate_nullary_ctors_does_not_fire_e0020() {
        // PR #65 review: the parser emits `Pattern::Var(name, _)` for
        // any bare identifier in pattern position. Without ctor-aware
        // pattern walking, `(None, None)` would falsely fire E0020 on
        // the second `None` because `BTreeSet::insert` returns false.
        // Typechecker promotes `Pattern::Var("None", _)` to a nullary-
        // ctor pattern at check_pattern time; resolve.rs mirrors that
        // by consulting `collect_ctor_names` before treating a bare
        // ident as a binding.
        let src = "type Opt = | None | Some(Int)\n\
                   fn main() -> Int ![] { \
                     match (None, None) { (None, None) => 0, _ => 1 } \
                   }\n";
        let errs = pipeline(src);
        assert!(
            !has_e0020(&errs),
            "tuple pattern with duplicate nullary ctors must NOT fire E0020 \
             (parser emits Pattern::Var; resolve must skip ctor names): {errs:?}"
        );
    }

    #[test]
    fn let_in_arm_body_shadowing_arm_pattern_ctor_name_does_not_fire_e0020() {
        // PR #65 review (second-order): when a match-arm pattern is a
        // nullary ctor, no binding is introduced — so a `let` inside
        // the arm body whose name matches the ctor is a fresh binding,
        // not a shadow. Pre-fix this falsely fired E0020 because
        // resolve treated `Pattern::Var("A")` as a binding and added
        // it to inner scope.
        let src = "type B = | A | Other\n\
                   fn main() -> Int ![] { \
                     match A { A => { let A: Int = 99; A }, _ => 0 } \
                   }\n";
        let errs = pipeline(src);
        assert!(
            !has_e0020(&errs),
            "let inside arm body whose name matches an arm-pattern ctor must \
             NOT fire E0020: {errs:?}"
        );
    }

    #[test]
    fn return_arm_binding_can_shadow_outer_let() {
        // PR #65 review #1 — completes the binder-shape coverage matrix.
        // Return-arm binding `v` is a fresh-scope construct (typecheck
        // uses direct `env.insert` at typecheck.rs:5255); it MAY shadow
        // outer let `v`. resolve.rs's Handle return-arm path inserts
        // `ra.binding` into the inner scope without dup-checking
        // against outer; this test pins that intentional asymmetry.
        let src = "effect Eff { op: () -> Int }\n\
                   fn main() -> Int ![Eff] { \
                     let v: Int = 1; \
                     handle 0 with { \
                       Eff.op(k) => 0, \
                       return(v) => v + 1 \
                     } \
                   }\n";
        let errs = pipeline(src);
        assert!(
            !has_e0020(&errs),
            "return-arm binding may shadow outer let (fresh-scope construct): {errs:?}"
        );
    }

    #[test]
    fn let_can_shadow_top_level_fn_name() {
        // PR #65 review #2 — typecheck.rs:3765 explicitly says
        // "params/lets shadow top-level fns of the same name within
        // their scope" (PR #6 review fix). resolve.rs mirrors this by
        // NOT seeding the per-fn scope with top-level fn names —
        // they're not in the structural shadow set. Pin the behavior
        // so a future fn-name leak into the resolve scope would fail
        // this test.
        let src = "fn foo() -> Int ![] { 0 }\n\
                   fn main() -> Int ![] { let foo: Int = 99; foo }\n";
        let errs = pipeline(src);
        assert!(
            !has_e0020(&errs),
            "let in fn body may shadow a top-level fn name: {errs:?}"
        );
    }

    #[test]
    fn let_self_referential_rhs_resolves_against_outer_scope() {
        // `let x: Int = x` where outer `x` is in scope: the RHS resolves
        // to outer `x`, the binding then shadows. The shadow check
        // fires; the RHS walks before the binding enters scope so no
        // recursive-self issue.
        let src = "fn main() -> Int ![] { \
                     let x: Int = 1; \
                     let x: Int = x; \
                     x \
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_e0020(&errs),
            "let with self-referential RHS still fires E0020 on the rebind: {errs:?}"
        );
    }

    // ----------------------------------------------------------------
    // Per-fn uniqueness: lets in disjoint scopes can't share a name
    // (matches typecheck's flat env_insert invariant). Pre-fix, the
    // per-visibility check missed disjoint siblings and they tripped
    // typecheck's debug_assert at runtime instead.
    // ----------------------------------------------------------------

    #[test]
    fn let_in_disjoint_match_arms_same_name_fires_e0020() {
        // Two `let x` in disjoint match arm bodies. Neither sees the
        // other at its own let site, but both bind the same name in
        // the same function — typecheck's flat env asserts on the
        // second insert.
        let src = "type Opt = | None | Some(Int)\n\
                   fn main() -> Int ![] { \
                     match Some(0) { \
                       Some(_) => { let x: Int = 1; x }, \
                       None => { let x: Int = 2; x } \
                     } \
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_e0020(&errs),
            "lets in disjoint match arms cannot share a name: {errs:?}"
        );
    }

    #[test]
    fn let_in_disjoint_if_branches_same_name_fires_e0020() {
        // Two `let x` in if/else branches.
        let src = "fn main() -> Int ![] { \
                     if true { let x: Int = 1; x } else { let x: Int = 2; x } \
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_e0020(&errs),
            "lets in disjoint if/else branches cannot share a name: {errs:?}"
        );
    }

    #[test]
    fn let_in_disjoint_match_arms_different_names_resolves_clean() {
        // Sanity check: disjoint arm lets with *different* names are
        // legal. The per-fn tracker shouldn't over-fire.
        let src = "type Opt = | None | Some(Int)\n\
                   fn main() -> Int ![] { \
                     match Some(0) { \
                       Some(_) => { let a: Int = 1; a }, \
                       None => { let b: Int = 2; b } \
                     } \
                   }\n";
        let errs = pipeline(src);
        assert!(
            !has_e0020(&errs),
            "disjoint arm lets with different names are legal: {errs:?}"
        );
    }

    #[test]
    fn match_pattern_bindings_in_disjoint_arms_can_share_name() {
        // Patterns in disjoint arms still get fresh scope and can
        // share names. Per-fn uniqueness applies to lets and fn
        // params, not to construct-introduced bindings (matches
        // typecheck's direct env.insert path for patterns).
        let src = "type LR = | L(Int) | R(Int)\n\
                   fn main() -> Int ![] { \
                     match L(0) { \
                       L(x) => x, \
                       R(x) => x \
                     } \
                   }\n";
        let errs = pipeline(src);
        assert!(
            !has_e0020(&errs),
            "patterns in disjoint match arms can share a name: {errs:?}"
        );
    }

    #[test]
    fn let_in_nested_match_arms_same_name_fires_e0020() {
        // Reproduces the user-surfaced shape — a nested match where
        // the outer match's Nil arm and the inner match's then-branch
        // (inside Cons) both declare `let completed_field_text`.
        let src = "type LC = | LCNil | LCCons(Int, Int)\n\
                   fn main() -> Int ![] { \
                     match LCCons(1, 2) { \
                       LCNil => { let v: Int = 0; v }, \
                       LCCons(_, _) => { \
                         if true { let v: Int = 1; v } else { 2 } \
                       } \
                     } \
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_e0020(&errs),
            "nested disjoint arms cannot share a let name: {errs:?}"
        );
    }

    #[test]
    fn discard_let_in_disjoint_arms_does_not_fire_e0020() {
        // Multiple `let _` in disjoint arms are independent discards,
        // not shadowing — same carve-out as in the same scope.
        let src = "type Opt = | None | Some(Int)\n\
                   fn main() -> Int ![] { \
                     match Some(0) { \
                       Some(_) => { let _: Int = 1; 0 }, \
                       None => { let _: Int = 2; 0 } \
                     } \
                   }\n";
        let errs = pipeline(src);
        assert!(
            !has_e0020(&errs),
            "discard `let _` in disjoint arms is not shadowing: {errs:?}"
        );
    }
}
