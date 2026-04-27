//! Cranelift codegen — plan A1 Stage 1 task 12, extended in plan A2 task 24.
//!
//! Lowers the post-closure-conversion program to a native object file via
//! `cranelift-object`. Stage 2 (plan A2) extends the Stage-1 hello-world
//! walk with a real expression-tree lowerer: integer arithmetic (`iadd`,
//! `isub`, `imul`, `sdiv`, `srem`) with a zero-check trap on every
//! division, primitive comparisons via `icmp`, boolean logic via `band`
//! and `bor`, `match` on primitives lowered to a chain of `brif` blocks,
//! and `let`-bound identifiers stored in a flat per-function SSA env.
//!
//! Emission responsibilities:
//!
//! - A C-callable `main` shim that initialises the GC, calls user-`main`,
//!   then returns the user return value as a C int exit status.
//! - One function per user `fn`. Plan A2 still compiles only `main`;
//!   multi-arg user functions arrive in Stage 3 (plan A2 task 29+).
//! - String literals are emitted as read-only data bytes; the generated
//!   code calls `sigil_string_new(bytes, len)` to materialise a heap
//!   String (bumping the Boehm counter) and then passes the heap pointer
//!   to `sigil_println`.
//! - Arithmetic is native `i64` internally; `Int` values are tagged
//!   `(n << 1)` only at the user-`main` return boundary (for the
//!   c-`main` shim's `sshr_imm` untag path). `Bool` is represented as
//!   `i8` (`0`/`1`), `Char` as `i32`, `Byte` as `u8` in Cranelift IR.
//! - Division and remainder emit a divisor-zero check; the false arm
//!   calls `sigil_panic_arith_error("division by zero")` /
//!   `("remainder by zero")`, whose C-string payloads live in
//!   `.rodata` / `__TEXT,__cstring`.
//! - Safepoint metadata at every call site is accumulated through
//!   `StackMapBuilder` and written to `.sigil_stackmaps` (ELF) /
//!   `__SIGIL,__stackmaps` (Mach-O). The section carries a versioned
//!   header so a v2 precise-GC reader can recognise Stage 1's
//!   placeholder entries and bail / resynthesise from relocations rather
//!   than consuming them as real safepoint data. See PLAN_A1_DEVIATIONS
//!   (`[DEVIATION Task 0.11]`) for the rationale and the v0 → v1
//!   migration plan. Plan A2's new call sites (`sigil_panic_arith_error`
//!   per div/mod site) are added to the same placeholder stream.
//! - No interior pointers. Generated code never computes a pointer into
//!   the middle of a heap object; it calls runtime helpers that work with
//!   header pointers and extract transient payload views internally.
//!
//! Target-triple detection uses `target_lexicon::HOST` so the compiler
//! emits for whatever host it runs on; cross-compilation is not v1 scope.

use std::collections::BTreeMap;
use std::path::Path;

use cranelift::codegen::ir::{
    condcodes::IntCC, AbiParam, BlockArg, FuncRef, GlobalValue, Inst, Signature, UserFuncName,
};
use cranelift::codegen::isa;
use cranelift::codegen::settings;
use cranelift::prelude::*;
use cranelift_module::{default_libcall_names, DataDescription, Linkage, Module};
use cranelift_object::object::write::SectionKind;
use cranelift_object::object::BinaryFormat;
use cranelift_object::{ObjectBuilder, ObjectModule};
use target_lexicon::Triple;

use sigil_abi::stackmap::{
    STACKMAP_FLAG_PLACEHOLDER, STACKMAP_HEADER_SIZE, STACKMAP_MAGIC, STACKMAP_RECORD_SIZE,
    STACKMAP_VERSION_PLACEHOLDER,
};
use sigil_abi::tag::TAG_INT_SHIFT;
use sigil_header_constants::{header_word, MAX_CLOSURE_ENV_SLOTS, TAG_CLOSURE};

use crate::ast::{EnvSlotKind, TypeExpr};
use crate::closure_convert::ClosureConvertedProgram;
use crate::errors::Span;
use crate::typecheck::{CheckedProgram, Ty};

/// Per-user-function codegen registry. Populated before any body is
/// defined so direct calls and `ClosureRecord.code_fn_name` lookups can
/// resolve to a `FuncId` regardless of definition order.
struct UserFnEntry {
    func_id: cranelift_module::FuncId,
    signature: Signature,
    /// Cranelift type of each parameter, including `param_tys[0]` =
    /// `pointer_ty` for the closure_ptr convention-slot.
    #[allow(dead_code)]
    param_tys: Vec<Type>,
    ret_ty: Type,
}

/// Map a surface-syntax `TypeExpr` to the Cranelift IR type codegen uses
/// for values of that type. Plan A2's `TypeExpr` grammar is flat
/// (`Named(String)` only); names outside the v1 primitive set are
/// rejected by typecheck before codegen sees the program.
///
/// Plan A3 task 41: user-defined types (`type Name = ...`) are heap
/// records addressed by a GC pointer — any name that passes
/// typecheck's E0112 sweep and isn't a primitive is a registered user
/// type, represented at the ABI boundary as `pointer_ty`.
///
/// Invariant (Plan B task 48): the caller has already verified
/// monomorphization completed — the entry-point assertion in
/// `emit_object` rejects any program whose AST still carries
/// `TypeExpr::Apply` or generic-parameter references. So the
/// catchall fall-through to `pointer_ty` here is always a registered
/// user type at this point; an unrecognised name would have been
/// caught upstream.
fn cranelift_ty_for_type_expr(te: &TypeExpr, pointer_ty: Type) -> Type {
    match te.head_name() {
        "Int" => types::I64,
        "String" => pointer_ty,
        "Bool" | "Byte" | "Unit" => types::I8,
        "Char" => types::I32,
        _ => pointer_ty,
    }
}

/// Plan B task 48 — codegen-entry walker that asserts monomorphization
/// has erased every generic-application and every generic-parameter
/// reference. Called once at the top of `emit_object`. Closure point
/// for the verification-debt entry "Codegen path for un-monomorphized
/// generic params" in `PLAN_B_DEVIATIONS.md`.
///
/// Returns `true` if the program contains *any* of:
///   - a `TypeExpr::Apply` node anywhere in a fn signature, type
///     declaration, or let-binding annotation, or
///   - a `TypeExpr::Named(name, _)` where `name` is the surface name
///     of a generic parameter declared on an enclosing fn or type
///     (and is therefore not a primitive or a registered user-type
///     name from the type registry).
///
/// Both conditions indicate unmonomorphised IR — Task 49 will rewrite
/// such occurrences into concrete clones.
///
/// Plan B task 49 — `effect_row_var` is **NOT** rejected. Rows are
/// not monomorphized in v1 (effect dispatch is runtime-indirect; row
/// variables are erased at codegen). The original Plan B Task 48 guard
/// included `effect_row_var.is_some()`; Task 49 lifts that check
/// because monomorphized fns whose surface declared `![ ... | e]`
/// preserve their `effect_row_var` through this pass per the plan's
/// "Effect rows are not monomorphized" clause.
pub(crate) fn contains_apply_or_generic_ref(program: &crate::ast::Program) -> bool {
    use crate::ast::{Item, VariantFields};
    use std::collections::BTreeSet;
    for item in &program.items {
        match item {
            Item::Fn(f) => {
                // The fn's own `[A, B, ...]` makes the whole decl a
                // generic that monomorphization (Task 49) is required
                // to clone before codegen — short-circuit immediately.
                if !f.generic_params.is_empty() {
                    return true;
                }
                // No declared params at this level — the in-scope
                // set is empty, so `Named` references can only refer
                // to primitives or registered user types. We still
                // need to descend through `TypeExpr::Apply` shapes
                // (an `Apply` anywhere is a hard reject regardless
                // of the in-scope set).
                let in_scope: BTreeSet<String> = BTreeSet::new();
                for p in &f.params {
                    if type_expr_uses_apply_or_param(&p.ty, &in_scope) {
                        return true;
                    }
                }
                if type_expr_uses_apply_or_param(&f.return_type, &in_scope) {
                    return true;
                }
                if block_uses_generic(&f.body, &in_scope) {
                    return true;
                }
            }
            Item::Type(td) => {
                if !td.generic_params.is_empty() {
                    return true;
                }
                let in_scope: BTreeSet<String> = BTreeSet::new();
                for v in &td.variants {
                    match &v.fields {
                        VariantFields::Unit => {}
                        VariantFields::Positional(ts) => {
                            for t in ts {
                                if type_expr_uses_apply_or_param(t, &in_scope) {
                                    return true;
                                }
                            }
                        }
                        VariantFields::Record(fs) => {
                            for f in fs {
                                if type_expr_uses_apply_or_param(&f.ty, &in_scope) {
                                    return true;
                                }
                            }
                        }
                    }
                }
            }
            Item::Import(_) => {}
            // Plan B Task 55 — `effect Name { op: ... }` declarations
            // emit no codegen output (they only populate the
            // typecheck-time effect registry consulted by `perform`
            // dispatch and the runtime handler-stack ABI from Task
            // 56). Op signatures are checked under their own generic-
            // param substitution at typecheck time, so walking them
            // here is unnecessary; skip silently.
            Item::Effect(_) => {}
        }
    }
    false
}

fn type_expr_uses_apply_or_param(
    t: &TypeExpr,
    params: &std::collections::BTreeSet<String>,
) -> bool {
    match t {
        TypeExpr::Apply { args, .. } => {
            // An Apply node is itself a hard reject; we still recurse
            // so a nested Apply or generic-param ref also surfaces in
            // the diagnostic (defensive against future paths that
            // accept partial Apply but reject specific arg shapes).
            for a in args {
                if type_expr_uses_apply_or_param(a, params) {
                    // Already returning true; just keep walking
                    // would be wasteful — short-circuit.
                    return true;
                }
            }
            true
        }
        TypeExpr::Named(name, _) => params.contains(name),
    }
}

fn block_uses_generic(b: &crate::ast::Block, params: &std::collections::BTreeSet<String>) -> bool {
    use crate::ast::Stmt;
    for s in &b.stmts {
        match s {
            Stmt::Let(l) => {
                if type_expr_uses_apply_or_param(&l.ty, params) {
                    return true;
                }
                if expr_uses_generic(&l.value, params) {
                    return true;
                }
            }
            Stmt::Expr(e) => {
                if expr_uses_generic(e, params) {
                    return true;
                }
            }
            Stmt::Perform(p) => {
                for a in &p.args {
                    if expr_uses_generic(a, params) {
                        return true;
                    }
                }
            }
        }
    }
    if let Some(tail) = &b.tail {
        if expr_uses_generic(tail, params) {
            return true;
        }
    }
    false
}

fn expr_uses_generic(e: &crate::ast::Expr, params: &std::collections::BTreeSet<String>) -> bool {
    use crate::ast::Expr;
    match e {
        Expr::IntLit(_, _)
        | Expr::StringLit(_, _)
        | Expr::BoolLit(_, _)
        | Expr::CharLit(_, _)
        | Expr::Ident(_, _) => false,
        Expr::Binary { lhs, rhs, .. } => {
            expr_uses_generic(lhs, params) || expr_uses_generic(rhs, params)
        }
        Expr::Unary { operand, .. } => expr_uses_generic(operand, params),
        Expr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            expr_uses_generic(cond, params)
                || block_uses_generic(then_block, params)
                || block_uses_generic(else_block, params)
        }
        Expr::Block(b) => block_uses_generic(b, params),
        Expr::Match {
            scrutinee, arms, ..
        } => {
            if expr_uses_generic(scrutinee, params) {
                return true;
            }
            for a in arms {
                if expr_uses_generic(&a.body, params) {
                    return true;
                }
            }
            false
        }
        Expr::Call { callee, args, .. } => {
            if expr_uses_generic(callee, params) {
                return true;
            }
            for a in args {
                if expr_uses_generic(a, params) {
                    return true;
                }
            }
            false
        }
        Expr::Perform(p) => {
            for a in &p.args {
                if expr_uses_generic(a, params) {
                    return true;
                }
            }
            false
        }
        Expr::Lambda {
            params: lparams,
            return_type,
            body,
            // Plan B task 49 — lambda effect-row variable is not a
            // rejection condition; rows pass through to codegen
            // erasure like fn-decl effect rows do.
            effect_row_var: _,
            ..
        } => {
            if type_expr_uses_apply_or_param(return_type, params) {
                return true;
            }
            for p in lparams {
                if type_expr_uses_apply_or_param(&p.ty, params) {
                    return true;
                }
            }
            expr_uses_generic(body, params)
        }
        Expr::ClosureRecord { env_exprs, .. } => {
            for ee in env_exprs {
                if expr_uses_generic(ee, params) {
                    return true;
                }
            }
            false
        }
        Expr::ClosureEnvLoad { .. } => false,
        Expr::RecordLit { fields, .. } => {
            for f in fields {
                if expr_uses_generic(&f.value, params) {
                    return true;
                }
            }
            false
        }
        // Plan B task 53 — dead code today. Any program that reaches
        // codegen with an `Expr::Handle` already triggered the
        // `Item::Effect → return true` short-circuit at the top of
        // `contains_apply_or_generic_ref`, since a `handle` requires
        // an `effect` decl in scope to typecheck under Task 54+ (and
        // typecheck E0134 stops it before then anyway). This arm is
        // kept for the case where Task 54 lifts the `Item::Effect`
        // gate but `Expr::Handle` still needs a guard during the
        // CPS-transform handoff in Task 55. Reviewer feedback PR #19
        // item 4.
        Expr::Handle {
            body,
            return_arm,
            op_arms,
            ..
        } => {
            if expr_uses_generic(body, params) {
                return true;
            }
            if let Some(ra) = return_arm {
                if expr_uses_generic(&ra.body, params) {
                    return true;
                }
            }
            for arm in op_arms {
                if expr_uses_generic(&arm.body, params) {
                    return true;
                }
            }
            false
        }
    }
}

/// Plan B Task 55 (Phase 2 minimum) — codegen-entry guard for handler
/// constructs that the in-progress CPS path does not yet support.
/// Returns `Some(error_message)` when the program contains a `handle
/// <body> with { arms }` whose body would actually `perform` a non-IO
/// effect at runtime. Such programs need the full handler-frame setup
/// + CPS calling convention (Phase 3+); until then they're rejected
/// here with a clean compile-time error pointing at the in-progress
/// task.
///
/// IO `perform` in handle bodies is fine: `IO` is hard-wired in
/// `lower_perform` and doesn't route through `sigil_perform`, so the
/// runtime handler stack is never consulted for it.
///
/// **Conservative scope**: this walker only inspects `Expr::Perform`
/// nodes appearing directly in a handle body (or inside its blocks /
/// children). It does **not** chase fn calls into callee bodies — a
/// handle whose body calls a fn that itself performs a non-IO effect
/// would slip through this guard and crash at runtime when
/// `sigil_perform` walks an empty handler stack. This is acceptable
/// for the Phase 2 milestone because the e2e test program's body is
/// a literal; widening the guard to follow call edges lands with
/// Phase 3+ when the proper handler-frame setup ships.
pub(crate) fn unsupported_handle_construct(program: &crate::ast::Program) -> Option<String> {
    use std::collections::BTreeSet;
    // Globals reachable as bare `Expr::Ident` from anywhere — used by
    // Phase 4c's arm-body capture check to distinguish "global ref"
    // from "outer-scope capture". Top-level fn names + ctor names +
    // hardcoded builtins (`int_to_string`).
    let mut globals: BTreeSet<String> = BTreeSet::new();
    for item in &program.items {
        match item {
            crate::ast::Item::Fn(f) => {
                globals.insert(f.name.clone());
            }
            crate::ast::Item::Type(t) => {
                for v in &t.variants {
                    globals.insert(v.name.clone());
                }
            }
            crate::ast::Item::Effect(_) | crate::ast::Item::Import(_) => {}
        }
    }
    globals.insert("int_to_string".to_string());
    for item in &program.items {
        if let crate::ast::Item::Fn(f) = item {
            if let Some(msg) = block_unsupported_handle(&f.body, &globals) {
                return Some(format!("in fn `{}`: {}", f.name, msg));
            }
        }
    }
    None
}

fn block_unsupported_handle(
    b: &crate::ast::Block,
    globals: &std::collections::BTreeSet<String>,
) -> Option<String> {
    use crate::ast::Stmt;
    for s in &b.stmts {
        match s {
            Stmt::Let(l) => {
                if let Some(msg) = expr_unsupported_handle(&l.value, globals) {
                    return Some(msg);
                }
            }
            Stmt::Expr(e) => {
                if let Some(msg) = expr_unsupported_handle(e, globals) {
                    return Some(msg);
                }
            }
            Stmt::Perform(p) => {
                for a in &p.args {
                    if let Some(msg) = expr_unsupported_handle(a, globals) {
                        return Some(msg);
                    }
                }
            }
        }
    }
    if let Some(tail) = &b.tail {
        if let Some(msg) = expr_unsupported_handle(tail, globals) {
            return Some(msg);
        }
    }
    None
}

fn expr_unsupported_handle(
    e: &crate::ast::Expr,
    globals: &std::collections::BTreeSet<String>,
) -> Option<String> {
    use crate::ast::Expr;
    match e {
        Expr::IntLit(..)
        | Expr::StringLit(..)
        | Expr::BoolLit(..)
        | Expr::CharLit(..)
        | Expr::Ident(..)
        | Expr::ClosureEnvLoad { .. } => None,
        Expr::Binary { lhs, rhs, .. } => {
            expr_unsupported_handle(lhs, globals).or_else(|| expr_unsupported_handle(rhs, globals))
        }
        Expr::Unary { operand, .. } => expr_unsupported_handle(operand, globals),
        Expr::If {
            cond,
            then_block,
            else_block,
            ..
        } => expr_unsupported_handle(cond, globals)
            .or_else(|| block_unsupported_handle(then_block, globals))
            .or_else(|| block_unsupported_handle(else_block, globals)),
        Expr::Block(b) => block_unsupported_handle(b, globals),
        Expr::Match {
            scrutinee, arms, ..
        } => {
            if let Some(msg) = expr_unsupported_handle(scrutinee, globals) {
                return Some(msg);
            }
            for a in arms {
                if let Some(msg) = expr_unsupported_handle(&a.body, globals) {
                    return Some(msg);
                }
            }
            None
        }
        Expr::Call { callee, args, .. } => {
            if let Some(msg) = expr_unsupported_handle(callee, globals) {
                return Some(msg);
            }
            for a in args {
                if let Some(msg) = expr_unsupported_handle(a, globals) {
                    return Some(msg);
                }
            }
            None
        }
        Expr::Perform(_) => None,
        Expr::Lambda { body, .. } => expr_unsupported_handle(body, globals),
        Expr::ClosureRecord { env_exprs, .. } => {
            for ee in env_exprs {
                if let Some(msg) = expr_unsupported_handle(ee, globals) {
                    return Some(msg);
                }
            }
            None
        }
        Expr::RecordLit { fields, .. } => {
            for f in fields {
                if let Some(msg) = expr_unsupported_handle(&f.value, globals) {
                    return Some(msg);
                }
            }
            None
        }
        Expr::Handle {
            body,
            return_arm,
            op_arms,
            span,
        } => {
            // Phase 4c constraints (lifted incrementally as the
            // CPS path matures):
            //   - arms cannot reference `k` (Phase 4d lifts via
            //     continuation reification + lambda-lifting of the
            //     perform's continuation)
            //   - arm bodies cannot capture outer-scope free
            //     variables (the synthetic CPS arm fn's closure_ptr
            //     is null in Phase 4c — closure captures need
            //     closure-record allocation at handler-frame setup
            //     time, deferred until Phase 4d/onward when k
            //     reification needs the same machinery)
            //   - arm bodies cannot contain nested `Expr::Lambda` /
            //     `Expr::ClosureRecord` (closure_convert lifts
            //     lambdas to ClosureRecords; both shapes need
            //     captures support to lower correctly inside an
            //     arm fn — same closure point as the capture gate)
            //   - no return arm (Phase 4f lifts via a synthetic
            //     return-fn registered via
            //     sigil_handler_frame_set_return)
            //   - all arms reference the same effect (Phase 4e
            //     lifts via frame-per-effect)
            //   - body's non-IO performs only target the arm's
            //     effect (typecheck enforces this; codegen doesn't
            //     add an extra check)
            // Phase 4c LIFTED: arm bodies may be any expression
            // over op-args and globals (top-level fns, ctors,
            // builtins) — lowered through the regular `Lowerer`
            // with op-args bound from `args_ptr` at fn entry.
            if return_arm.is_some() {
                return Some(format!(
                    "`handle` expression at {:?} has a `return` arm — `return` \
                     arms are not yet supported in codegen (Plan B Task 55, in \
                     progress)",
                    span
                ));
            }
            if op_arms.is_empty() {
                // Defensive: parser guarantees at least one arm,
                // but codegen indexes op_arms[0] for the single-arm
                // path so guard explicitly.
                return Some(format!(
                    "`handle` expression at {:?} has no op-arms — codegen \
                     requires at least one (Plan B Task 55)",
                    span
                ));
            }
            // Phase 4a: multi-arm handles are now supported, but
            // all arms must reference the same effect (the runtime
            // `HandlerFrame`'s `effect_id` is a single u32 — multi-
            // effect handles need a frame-per-effect approach that
            // lands in Phase 4e). Reject mixed-effect handles
            // here with a clean diagnostic.
            let first_effect = &op_arms[0].effect;
            for arm in op_arms.iter().skip(1) {
                if &arm.effect != first_effect {
                    return Some(format!(
                        "`handle` expression at {:?} has arms targeting different \
                         effects (`{}` and `{}`) — multi-effect handlers are \
                         not yet supported in codegen (Plan B Task 55, in \
                         progress; arrives in Phase 4e via frame-per-effect)",
                        span, first_effect, arm.effect
                    ));
                }
            }
            // Phase 4a: also reject duplicate (effect, op) arm pairs
            // — the runtime frame's per-op slot is single-valued, so
            // two arms for the same op would race. Typecheck E0140
            // catches this earlier (before the staged-feature gates
            // were lifted), but the codegen-entry guard double-
            // checks defensively. Phase 4+ will share state with
            // typecheck if the redundancy bites.
            for (i, a) in op_arms.iter().enumerate() {
                for b in op_arms.iter().skip(i + 1) {
                    if a.effect == b.effect && a.op == b.op {
                        return Some(format!(
                            "`handle` expression at {:?} has duplicate arms for \
                             `{}.{}` — typecheck E0140 should have caught this; \
                             reaching codegen indicates an upstream invariant \
                             broke (Plan B Task 55)",
                            span, a.effect, a.op
                        ));
                    }
                }
            }
            // Phase 4c: per-arm validation — no `k` references, no
            // outer-scope captures, no nested `Lambda` /
            // `ClosureRecord` shapes. The arm body is otherwise free
            // to use any expression over its op-args + globals.
            for arm in op_arms.iter() {
                if let Some(msg) = arm_body_unsupported_construct(arm, globals) {
                    return Some(format!(
                        "`handle` expression at {:?} has arm `{}.{}` body that {} \
                         (Plan B Task 55, in progress)",
                        span, arm.effect, arm.op, msg
                    ));
                }
            }
            // Recurse into the body itself so a nested handle inside
            // the body (e.g. `handle (handle ... with { ... }) with
            // { ... }`) surfaces its own diagnostics. Without this,
            // the inner handle's multi-effect / return-arm restrictions
            // are never enforced — at runtime that can register arms
            // under the wrong effect_id and crash inside `sigil_perform`'s
            // handler-stack walk.
            if let Some(msg) = expr_unsupported_handle(body, globals) {
                return Some(msg);
            }
            // Recurse into arm bodies so nested handles deeper in
            // the AST surface their own diagnostics.
            for arm in op_arms {
                if let Some(msg) = expr_unsupported_handle(&arm.body, globals) {
                    return Some(msg);
                }
            }
            None
        }
    }
}

/// Plan B Task 55 (Phase 4c) — checks an arm body for the three Phase
/// 4c violations that the synthetic-fn lowerer can't yet handle:
///
///   1. **References to `k`** — Phase 4d reifies the perform's
///      continuation; until then any `k` reference would dispatch to
///      a null fn pointer at runtime.
///   2. **Captures from outer scope** — the synthetic CPS arm fn's
///      `closure_ptr` is null; an Ident referring to a binding outside
///      the arm's op-args / a top-level fn / a ctor / a builtin
///      (`int_to_string`) would resolve to nothing in the Lowerer's
///      env and panic.
///   3. **Nested `Lambda` / `ClosureRecord`** — these need the same
///      closure-record allocation machinery as #2; rejecting them
///      here keeps the diagnostic surface small in Phase 4c.
///
/// Returns `Some(reason_fragment)` on first violation; the caller
/// wraps it with `format!("... body that {} ...")` context. Scope
/// tracking: the walker maintains a stack of `BTreeSet<String>`
/// scopes (op-args at the bottom, let/match/handle bindings pushed/
/// popped as scopes open/close) so let-bound and pattern-bound names
/// inside the arm body don't trigger the capture check.
fn arm_body_unsupported_construct(
    arm: &crate::ast::HandleOpArm,
    globals: &std::collections::BTreeSet<String>,
) -> Option<String> {
    use std::collections::BTreeSet;
    let mut op_arg_scope: BTreeSet<String> = BTreeSet::new();
    for p in &arm.params {
        op_arg_scope.insert(p.name.clone());
    }
    let mut scopes: Vec<BTreeSet<String>> = vec![op_arg_scope];
    // The arm body itself starts in tail position — the result of the
    // arm body IS the synthetic CPS fn's return value (wrapped in
    // `sigil_next_step_done` or `sigil_next_step_call` depending on
    // whether the tail is a captured-k invocation).
    arm_body_walk(&arm.body, &mut scopes, &arm.k_name, globals, true)
}

/// Plan B Task 55 (Phase 4d) — walks an arm body to surface the still-
/// unsupported shapes. Scope tracking + tail-position tracking:
///
/// - Captures from the surrounding fn's local env (let-bindings,
///   fn-params) ARE allowed at Phase 4d (the codegen site allocates a
///   per-arm closure record from these).
/// - Captures from the surrounding fn's *closure record* (rewritten by
///   closure_convert into `Expr::ClosureEnvLoad` because the
///   surrounding fn is a synthetic lambda fn) are still rejected with
///   a Phase-4e-pointing diagnostic — Phase 4d MVP can't materialise
///   these without a closure-convert side-table extension that lifts
///   alongside the colorer's handler-discharge refinement in 4e.
/// - `k(arg)` in tail position is allowed (lowers to
///   `sigil_next_step_call(k_closure, k_fn, 1)` returning to the
///   trampoline); non-tail `k` uses are rejected with a Phase-4e-
///   pointing diagnostic.
/// - `k` as a value (`Expr::Ident(k_name, ..)` not in callee position
///   of an `Expr::Call`) is also rejected (Phase 4e — multi-shot /
///   higher-order continuation manipulation).
/// - Nested `Expr::Lambda` / `Expr::ClosureRecord` in arm bodies stay
///   rejected (closure-convert side-table extension required, beyond
///   Phase 4d MVP scope).
fn arm_body_walk(
    e: &crate::ast::Expr,
    scopes: &mut Vec<std::collections::BTreeSet<String>>,
    k_name: &str,
    globals: &std::collections::BTreeSet<String>,
    tail: bool,
) -> Option<String> {
    use crate::ast::Expr;
    match e {
        Expr::IntLit(..) | Expr::BoolLit(..) | Expr::CharLit(..) | Expr::StringLit(..) => None,
        Expr::ClosureEnvLoad { name, .. } => {
            // Phase 4d MVP doesn't support arm-body captures whose
            // origin is the surrounding fn's closure record (handle
            // inside a synthetic lambda fn). The Phase 4d closure
            // allocation reads values out of `Lowerer.env` by name —
            // a name only present as a `ClosureEnvLoad` slot on the
            // surrounding fn's closure_ptr is invisible to that
            // lookup. Phase 4e ships the closure-convert side-table
            // extension that surfaces these to codegen via per-fn
            // (name, kind, index) lookup.
            Some(format!(
                "captures outer-scope binding `{name}` via the surrounding fn's \
                 closure record (handle inside a lambda; closure_convert rewrote \
                 the reference into a ClosureEnvLoad) — Phase 4d MVP supports \
                 captures from the surrounding fn's local env only; closure-of- \
                 surrounding-lambda captures arrive in Phase 4e"
            ))
        }
        Expr::Ident(name, _) => {
            if name == k_name {
                // `k` as a value (not in callee position). Multi-shot
                // / higher-order continuation manipulation requires
                // Phase 4e (heap-allocated re-invokable continuation).
                return Some(format!(
                    "references continuation `{name}` as a value (not as the \
                     callee of a tail-position call) — Phase 4d MVP supports \
                     `{name}(arg)` in tail position only; multi-shot or \
                     higher-order use of `k` arrives in Phase 4e"
                ));
            }
            if globals.contains(name) {
                return None;
            }
            for scope in scopes.iter() {
                if scope.contains(name) {
                    return None;
                }
            }
            // Capture of the surrounding fn's local env — allowed at
            // Phase 4d (codegen builds a per-arm closure record).
            None
        }
        Expr::Binary { lhs, rhs, .. } => arm_body_walk(lhs, scopes, k_name, globals, false)
            .or_else(|| arm_body_walk(rhs, scopes, k_name, globals, false)),
        Expr::Unary { operand, .. } => arm_body_walk(operand, scopes, k_name, globals, false),
        Expr::If {
            cond,
            then_block,
            else_block,
            ..
        } => arm_body_walk(cond, scopes, k_name, globals, false)
            // `If` then/else branches are NOT propagated as tail
            // for `k`-call detection: `arm_body_tail_is_k_call`
            // (which the synth pass uses to route tail-k vs done)
            // only recurses through `Expr::Block` tails. If we
            // accept `if c { k(x) } else { k(y) }` as tail-k here,
            // the detector returns `None` and the synth-pass falls
            // into the non-tail path; `lower_expr` then tries to
            // resolve `k` as an indirect callee and panics with
            // `unreachable!("indirect call …")`. The walker stays
            // strictly aligned with the detector's recursion shape;
            // multi-branch tail-`k` lowerings (join-block returning
            // `*NextStep`) are deferred to a future phase.
            .or_else(|| arm_body_walk_block(then_block, scopes, k_name, globals, false))
            .or_else(|| arm_body_walk_block(else_block, scopes, k_name, globals, false)),
        Expr::Block(b) => arm_body_walk_block(b, scopes, k_name, globals, tail),
        Expr::Match {
            scrutinee, arms, ..
        } => {
            if let Some(r) = arm_body_walk(scrutinee, scopes, k_name, globals, false) {
                return Some(r);
            }
            for a in arms {
                let mut pat_scope: std::collections::BTreeSet<String> =
                    std::collections::BTreeSet::new();
                arm_body_collect_pattern_bindings(&a.pattern, &mut pat_scope);
                scopes.push(pat_scope);
                // Match arm bodies are NOT in tail position for
                // `k`-call detection — same rationale as `Expr::If`
                // above (the synth-pass detector
                // `arm_body_tail_is_k_call` recurses only through
                // `Expr::Block` tails). Multi-branch tail-`k` shapes
                // (`match s { Variant1 => k(x), Variant2 => k(y) }`)
                // are walker-rejected as non-tail; lifting requires
                // a join-block lowering deferred to a future phase.
                let r = arm_body_walk(&a.body, scopes, k_name, globals, false);
                scopes.pop();
                if r.is_some() {
                    return r;
                }
            }
            None
        }
        Expr::Call { callee, args, .. } => {
            // Phase 4d tail-position k-call: `k(single_arg)` as the
            // tail expression of the arm body. Allowed; lowers to
            // `sigil_next_step_call(k_closure, k_fn, 1)`. Non-tail
            // k-calls and arity-mismatched k-calls are rejected.
            if let Expr::Ident(callee_name, _) = callee.as_ref() {
                if callee_name == k_name {
                    if !tail {
                        return Some(format!(
                            "uses continuation `{k_name}` in non-tail position — \
                             Phase 4d MVP supports `{k_name}(arg)` only as the \
                             tail expression of an arm body (the synchronous \
                             `sigil_run_loop` shape produces algebraic-correct \
                             results when k is invoked in tail position with \
                             k_fn = sigil_continuation_identity); arm bodies \
                             that compute around a continuation invocation \
                             require the colorer's handler-discharge refinement \
                             that ships in Phase 4e"
                        ));
                    }
                    if args.len() != 1 {
                        return Some(format!(
                            "calls continuation `{k_name}` with {arity} arg(s); \
                             continuation arity is fixed at 1 (the perform's \
                             return value)",
                            arity = args.len()
                        ));
                    }
                    // Tail-position `k(arg)`: walk the single arg in
                    // non-tail position (it must not itself reify k or
                    // contain disallowed shapes; outer-scope captures
                    // in the arg are allowed under Phase 4d's normal
                    // capture path).
                    return arm_body_walk(&args[0], scopes, k_name, globals, false);
                }
            }
            // Generic call. Callee + args are non-tail.
            if let Some(r) = arm_body_walk(callee, scopes, k_name, globals, false) {
                return Some(r);
            }
            for a in args {
                if let Some(r) = arm_body_walk(a, scopes, k_name, globals, false) {
                    return Some(r);
                }
            }
            None
        }
        Expr::Perform(p) => {
            for a in &p.args {
                if let Some(r) = arm_body_walk(a, scopes, k_name, globals, false) {
                    return Some(r);
                }
            }
            None
        }
        Expr::Lambda { .. } => Some(
            "contains a nested lambda — lambdas in arm bodies require a \
             closure-convert side-table extension distinct from Phase 4d MVP \
             (closure point: future phase, beyond 4e's calling-convention shift)"
                .to_string(),
        ),
        Expr::ClosureRecord { .. } => Some(
            "contains a nested ClosureRecord (lambda lifted by closure_convert) — \
             closures in arm bodies require a closure-convert side-table \
             extension distinct from Phase 4d MVP (closure point: future phase, \
             beyond 4e's calling-convention shift)"
                .to_string(),
        ),
        Expr::RecordLit { fields, .. } => {
            for f in fields {
                if let Some(r) = arm_body_walk(&f.value, scopes, k_name, globals, false) {
                    return Some(r);
                }
            }
            None
        }
        Expr::Handle {
            body: inner_body,
            return_arm: inner_return,
            op_arms: inner_op_arms,
            ..
        } => {
            // Nested handle inside an arm body. Walk inner shapes
            // with their own scope frames so inner op-args / inner
            // return-arm binding don't escape the inner scope. The
            // outer walker (`expr_unsupported_handle`) will have
            // already validated the inner handle's structural
            // constraints (multi-effect, return-arm, etc.) — we just
            // need to keep the capture check honest for the inner
            // arm bodies and the outer body's continuation.
            //
            // Body of the nested handle is non-tail w.r.t. THIS arm
            // (the nested handle's own `lower_expr` consumes its
            // body's value); tail position only re-enters at the
            // nested arm bodies if the nested handle expression is
            // itself in this arm's tail position.
            if let Some(r) = arm_body_walk(inner_body, scopes, k_name, globals, false) {
                return Some(r);
            }
            for inner_arm in inner_op_arms {
                let mut inner_scope: std::collections::BTreeSet<String> =
                    std::collections::BTreeSet::new();
                for p in &inner_arm.params {
                    inner_scope.insert(p.name.clone());
                }
                inner_scope.insert(inner_arm.k_name.clone());
                scopes.push(inner_scope);
                // Inner arm has its own k_name; the outer arm's k
                // is shadowed inside the inner arm body per Sigil's
                // lexical scoping rules (the inner k is a fresh
                // binding). Pass the inner k_name to the recursive
                // walk so the violation message names the right one.
                // Inner arm body's tail position is independent of
                // the outer arm's tail (the inner CPS arm fn wraps
                // its own tail expression).
                let r = arm_body_walk(&inner_arm.body, scopes, &inner_arm.k_name, globals, true);
                scopes.pop();
                if r.is_some() {
                    return r;
                }
            }
            if let Some(ra) = inner_return {
                let mut ra_scope: std::collections::BTreeSet<String> =
                    std::collections::BTreeSet::new();
                ra_scope.insert(ra.binding.clone());
                scopes.push(ra_scope);
                let r = arm_body_walk(&ra.body, scopes, k_name, globals, false);
                scopes.pop();
                if r.is_some() {
                    return r;
                }
            }
            None
        }
    }
}

fn arm_body_walk_block(
    b: &crate::ast::Block,
    scopes: &mut Vec<std::collections::BTreeSet<String>>,
    k_name: &str,
    globals: &std::collections::BTreeSet<String>,
    tail: bool,
) -> Option<String> {
    use crate::ast::Stmt;
    // Sequential let/expr/perform statements. Names introduced by
    // `Stmt::Let` are visible to subsequent stmts and the tail; we
    // accumulate them into a single scope frame that grows as we
    // walk. The walk pushes the (initially empty) scope before
    // walking the let value's RHS so the let name itself is NOT in
    // scope of its own RHS.
    //
    // Tail position propagates only to the block's tail expression;
    // statement-position exprs (let RHS, expr stmts, perform args)
    // are never in tail position.
    let mut local: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for s in &b.stmts {
        match s {
            Stmt::Let(l) => {
                scopes.push(local.clone());
                let r = arm_body_walk(&l.value, scopes, k_name, globals, false);
                scopes.pop();
                if r.is_some() {
                    return r;
                }
                local.insert(l.name.clone());
            }
            Stmt::Expr(e) => {
                scopes.push(local.clone());
                let r = arm_body_walk(e, scopes, k_name, globals, false);
                scopes.pop();
                if r.is_some() {
                    return r;
                }
            }
            Stmt::Perform(p) => {
                scopes.push(local.clone());
                let mut found = None;
                for a in &p.args {
                    if let Some(r) = arm_body_walk(a, scopes, k_name, globals, false) {
                        found = Some(r);
                        break;
                    }
                }
                scopes.pop();
                if found.is_some() {
                    return found;
                }
            }
        }
    }
    if let Some(tail_expr) = &b.tail {
        scopes.push(local);
        let r = arm_body_walk(tail_expr, scopes, k_name, globals, tail);
        scopes.pop();
        return r;
    }
    None
}

fn arm_body_collect_pattern_bindings(
    p: &crate::ast::Pattern,
    out: &mut std::collections::BTreeSet<String>,
) {
    use crate::ast::{CtorPatternFields, Pattern};
    match p {
        Pattern::Var(name, _) => {
            out.insert(name.clone());
        }
        Pattern::Wildcard(_)
        | Pattern::IntLit(..)
        | Pattern::BoolLit(..)
        | Pattern::CharLit(..) => {}
        Pattern::Tuple(ps, _) => {
            for sub in ps {
                arm_body_collect_pattern_bindings(sub, out);
            }
        }
        Pattern::Ctor { fields, .. } => match fields {
            CtorPatternFields::Unit => {}
            CtorPatternFields::Positional(ps) => {
                for sub in ps {
                    arm_body_collect_pattern_bindings(sub, out);
                }
            }
            CtorPatternFields::Record(fs) => {
                for f in fs {
                    arm_body_collect_pattern_bindings(&f.pattern, out);
                }
            }
        },
    }
}

/// Plan B Task 55 (Phase 3b) — per-handler-arm synthetic CPS fn
/// metadata. One entry per arm of every `Expr::Handle` reached by the
/// pre-pass walk in `emit_object`. The arm body is captured by value
/// so the synthetic-fn definition pass can lower it without re-walking
/// the program. `func_id` is allocated up-front so calling sites can
/// `module.declare_func_in_func` against it before the body is
/// defined.
#[derive(Debug)]
struct HandlerArmSynth {
    func_id: cranelift_module::FuncId,
    /// The arm body, post-rewrite for Phase 4d closure captures: every
    /// reference to a name in `captures` (whether originally an
    /// `Expr::Ident` or a closure_convert-rewritten
    /// `Expr::ClosureEnvLoad`) is replaced with an
    /// `Expr::ClosureEnvLoad { index, kind, name, .. }` reading from
    /// the synthetic arm fn's `closure_ptr` at the arm-local slot
    /// index given by the position of `name` in `captures`. References
    /// to op-arg names / `k_name` / globals (top-level fn / ctor /
    /// builtin) pass through unchanged. The pre-pass performs this
    /// rewrite once at `FuncId` allocation time so the synth pass at
    /// the bottom of `emit_object` can lower the body directly without
    /// re-walking the captures list.
    body: crate::ast::Expr,
    /// Plan B Task 55 (Phase 4c) — declared op-arg names from the arm
    /// header (`Effect.op(name1, name2, ..., k)`). Used by the
    /// synthetic-fn definition pass to bind the unpacked op-args into
    /// the Lowerer's env so the arm body can reference them. Empty
    /// for zero-arg ops; trailing `k` is tracked separately via
    /// `k_name`.
    arg_names: Vec<String>,
    /// Plan B Task 55 (Phase 4d) — the arm's continuation binding
    /// name (the trailing `k` of `Effect.op(arg1, arg2, ..., k) =>
    /// body`). Used by the synth-pass tail-k detector to recognise
    /// `Expr::Call { callee: Ident(k_name), .. }` shapes in the arm
    /// body's tail position and lower them as `sigil_next_step_call`
    /// against `(k_closure_loaded, k_fn_loaded, 1)` instead of
    /// `sigil_next_step_done(value)`.
    k_name: String,
    /// Plan B Task 55 (Phase 4c) — Cranelift type per op-arg, parallel
    /// to `arg_names`. Resolved from the matching `EffectOp`'s declared
    /// `params` via `cranelift_ty_for_type_expr`. Used by the synthetic-
    /// fn entry to truncate the I64-widened slot value back to the
    /// original Cranelift type (`I8` for Bool/Byte/Unit via `ireduce`,
    /// `I32` for Char via `ireduce`, `I64` and pointer-typed values
    /// stay as-is).
    arg_types: Vec<Type>,
    /// Plan B Task 55 (Phase 4c) — Cranelift type of the arm body's
    /// result. The body's lowered `Value` is widened to `I64` (matching
    /// `sigil_next_step_done`'s signature) before the wrap call;
    /// `lower_perform_non_io_to_value` mirror-narrows on the perform
    /// side so the perform's `type_of_expr` (the op's declared return
    /// type) and the actual lowered Cranelift `Value` agree.
    body_ty: Type,
    /// Plan B Task 55 (Phase 4d) — captures consumed by this arm body,
    /// in arm-local slot order matching `body`'s rewritten
    /// `Expr::ClosureEnvLoad { index }` references. Each entry is the
    /// captured name plus its `EnvSlotKind` (used for the closure
    /// record's GC bitmap and for the per-slot load/store widening
    /// shape). The corresponding env-expr — the value to write into
    /// the closure record's slot — is built at `Expr::Handle` codegen
    /// time by looking up the name in the surrounding `Lowerer.env`
    /// (or, when the surrounding fn is a closure_convert-lifted
    /// lambda, by emitting a `lower_closure_env_load` against the
    /// outer fn's `closure_ptr`). Empty for arm bodies with no outer-
    /// scope references; codegen passes `null` as the arm slot's
    /// `closure_ptr` in that case (no allocation needed).
    captures: Vec<ArmCapture>,
}

/// Plan B Task 55 (Phase 4d) — one captured outer-scope binding for a
/// synthetic CPS arm fn. Stored in `HandlerArmSynth::captures` in
/// arm-local slot order; the `Expr::Handle` codegen site allocates a
/// closure record whose env slots parallel this list, and the
/// rewritten `body` references each capture via
/// `Expr::ClosureEnvLoad { index, kind, name, .. }` where `index` is
/// the slot index into this list.
#[derive(Clone, Debug)]
struct ArmCapture {
    /// Source-level name; matches an entry in the surrounding fn's
    /// lexical env (a let-binding, fn-param, or, when the surrounding
    /// fn is a synthetic lambda fn, an enclosing-fn capture
    /// closure_convert rewrote into an `Expr::ClosureEnvLoad`).
    name: String,
    /// Slot kind for the closure-record encoding. Drives the
    /// closure-record header bitmap (pointer slots are GC-tracked,
    /// non-pointer slots are not) and the per-slot load/store widening
    /// shape (`I8` zero-extend for Bool/Byte/Unit, `I32` zero-extend
    /// for Char, direct stores for `I64`/pointer-typed slots). Derived
    /// at typecheck time via `slot_kind_for_ty(Ty)` and looked up via
    /// the `CheckedProgram::handle_arm_captures` side-table at codegen
    /// pre-pass time.
    kind: crate::ast::EnvSlotKind,
}

/// Walk a block looking for `Expr::Handle` sites and allocating
/// synthetic CPS-fn metadata for each arm. Recurses into nested
/// expressions so handles inside nested blocks / matches / lambdas
/// also surface. The pre-pass is deliberately conservative — it
/// allocates `FuncId`s for every arm of every reachable handle, even
/// arms whose `Expr::Handle` site might end up dead-code-eliminated
/// by some future optimisation. Codegen never optimises handles
/// today, so over-allocation here is harmless.
/// Bundle of pre-pass context threaded through the recursive walk.
/// Avoids 7+ positional args; assembled once at `emit_object` entry.
struct ArmSynthCtx<'a> {
    cps_arm_sig: &'a Signature,
    op_ids: &'a BTreeMap<(String, String), u32>,
    /// Plan B Task 55 (Phase 4c): the EffectDecl registry, used to
    /// resolve each arm's op-arg `TypeExpr`s into Cranelift `Type`s
    /// stored on the resulting `HandlerArmSynth`.
    effects: &'a BTreeMap<String, crate::ast::EffectDecl>,
    /// Plan B Task 55 (Phase 4c): pointer width for resolving
    /// `String` / user-type `TypeExpr`s. Constant per `emit_object`
    /// call; threaded so the Cranelift type computation lives in one
    /// place.
    pointer_ty: Type,
    /// Plan B Task 55 (Phase 4d): typecheck-side per-handle-per-arm
    /// captures map. Keyed by the handle expression's span; outer
    /// `Vec` parallels `Expr::Handle::op_arms` in declaration order.
    /// Empty inner vec = no captures (codegen passes null `closure_ptr`
    /// for that arm). Used by the pre-pass to size the per-arm
    /// closure record and to rewrite captured-name `Expr::Ident` and
    /// `Expr::ClosureEnvLoad` references in the arm body into
    /// arm-local-indexed `Expr::ClosureEnvLoad` slots.
    handle_arm_captures: &'a BTreeMap<Span, Vec<Vec<(String, crate::typecheck::Ty)>>>,
}

fn collect_handle_arms_in_block(
    b: &crate::ast::Block,
    module: &mut ObjectModule,
    ctx: &ArmSynthCtx<'_>,
    synth: &mut Vec<HandlerArmSynth>,
    indices: &mut BTreeMap<Span, Vec<usize>>,
) -> Result<(), String> {
    use crate::ast::Stmt;
    for s in &b.stmts {
        match s {
            Stmt::Let(l) => {
                collect_handle_arms_in_expr(&l.value, module, ctx, synth, indices)?;
            }
            Stmt::Expr(e) => {
                collect_handle_arms_in_expr(e, module, ctx, synth, indices)?;
            }
            Stmt::Perform(p) => {
                for a in &p.args {
                    collect_handle_arms_in_expr(a, module, ctx, synth, indices)?;
                }
            }
        }
    }
    if let Some(tail) = &b.tail {
        collect_handle_arms_in_expr(tail, module, ctx, synth, indices)?;
    }
    Ok(())
}

fn collect_handle_arms_in_expr(
    e: &crate::ast::Expr,
    module: &mut ObjectModule,
    ctx: &ArmSynthCtx<'_>,
    synth: &mut Vec<HandlerArmSynth>,
    indices: &mut BTreeMap<Span, Vec<usize>>,
) -> Result<(), String> {
    use crate::ast::Expr;
    match e {
        Expr::IntLit(..)
        | Expr::StringLit(..)
        | Expr::BoolLit(..)
        | Expr::CharLit(..)
        | Expr::Ident(..)
        | Expr::ClosureEnvLoad { .. }
        | Expr::Perform(_) => Ok(()),
        Expr::Binary { lhs, rhs, .. } => {
            collect_handle_arms_in_expr(lhs, module, ctx, synth, indices)?;
            collect_handle_arms_in_expr(rhs, module, ctx, synth, indices)
        }
        Expr::Unary { operand, .. } => {
            collect_handle_arms_in_expr(operand, module, ctx, synth, indices)
        }
        Expr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            collect_handle_arms_in_expr(cond, module, ctx, synth, indices)?;
            collect_handle_arms_in_block(then_block, module, ctx, synth, indices)?;
            collect_handle_arms_in_block(else_block, module, ctx, synth, indices)
        }
        Expr::Block(b) => collect_handle_arms_in_block(b, module, ctx, synth, indices),
        Expr::Match {
            scrutinee, arms, ..
        } => {
            collect_handle_arms_in_expr(scrutinee, module, ctx, synth, indices)?;
            for a in arms {
                collect_handle_arms_in_expr(&a.body, module, ctx, synth, indices)?;
            }
            Ok(())
        }
        Expr::Call { callee, args, .. } => {
            collect_handle_arms_in_expr(callee, module, ctx, synth, indices)?;
            for a in args {
                collect_handle_arms_in_expr(a, module, ctx, synth, indices)?;
            }
            Ok(())
        }
        Expr::Lambda { body, .. } => collect_handle_arms_in_expr(body, module, ctx, synth, indices),
        Expr::ClosureRecord { env_exprs, .. } => {
            for ee in env_exprs {
                collect_handle_arms_in_expr(ee, module, ctx, synth, indices)?;
            }
            Ok(())
        }
        Expr::RecordLit { fields, .. } => {
            for f in fields {
                collect_handle_arms_in_expr(&f.value, module, ctx, synth, indices)?;
            }
            Ok(())
        }
        Expr::Handle {
            body,
            op_arms,
            span,
            ..
        } => {
            // Recurse into body + arm bodies so nested handles also
            // surface. Then allocate FuncIds for this handle's arms.
            collect_handle_arms_in_expr(body, module, ctx, synth, indices)?;
            for arm in op_arms {
                collect_handle_arms_in_expr(&arm.body, module, ctx, synth, indices)?;
            }
            // Allocate one synthetic CPS fn per arm. Linker symbol
            // is `sigil_handler_arm_<global_index>` to keep names
            // unique without needing per-handle counters.
            let mut arm_indices: Vec<usize> = Vec::with_capacity(op_arms.len());
            for arm in op_arms {
                let global_idx = synth.len();
                let mangled = format!("sigil_handler_arm_{global_idx}");
                let func_id = module
                    .declare_function(&mangled, Linkage::Local, ctx.cps_arm_sig)
                    .map_err(|e| format!("declare {mangled}: {e}"))?;
                // Validate that the op_id is registered (op_ids
                // populated at end of typecheck for every effect's
                // ops). Unused at this site — the per-arm op_id is
                // looked up again at the Expr::Handle codegen site
                // — but failing fast here gives a clearer error
                // message than a unwrap deep inside lowering.
                let _ = ctx
                    .op_ids
                    .get(&(arm.effect.clone(), arm.op.clone()))
                    .ok_or_else(|| {
                        format!(
                            "codegen pre-pass: op_id missing for `{}.{}` — typecheck-time \
                         E0138/E0139 should have caught this",
                            arm.effect, arm.op
                        )
                    })?;
                // Plan B Task 55 (Phase 4c): resolve op-arg names +
                // Cranelift types from the EffectDecl. Typecheck
                // E0141 enforces arity so the zip is well-defined;
                // E0138 / E0139 ensure the registry lookup succeeds.
                let eff_decl = ctx.effects.get(&arm.effect).ok_or_else(|| {
                    format!(
                        "codegen pre-pass: effect `{}` missing from registry — \
                         typecheck-time E0138 should have caught this",
                        arm.effect
                    )
                })?;
                let op_decl = eff_decl
                    .ops
                    .iter()
                    .find(|o| o.name == arm.op)
                    .ok_or_else(|| {
                        format!(
                            "codegen pre-pass: op `{}.{}` missing from EffectDecl — \
                             typecheck-time E0139 should have caught this",
                            arm.effect, arm.op
                        )
                    })?;
                debug_assert_eq!(
                    arm.params.len(),
                    op_decl.params.len(),
                    "codegen pre-pass: arm `{}.{}` arity mismatch (typecheck E0141 \
                     should have caught this)",
                    arm.effect,
                    arm.op
                );
                let arg_names: Vec<String> = arm.params.iter().map(|p| p.name.clone()).collect();
                let arg_types: Vec<Type> = op_decl
                    .params
                    .iter()
                    .map(|te| cranelift_ty_for_type_expr(te, ctx.pointer_ty))
                    .collect();
                let body_ty = cranelift_ty_for_type_expr(&op_decl.return_type, ctx.pointer_ty);

                // Plan B Task 55 (Phase 4d): build the arm's captures
                // list from the typecheck-side `handle_arm_captures`
                // side-table and rewrite the arm body so every
                // captured-name reference loads from `closure_ptr` at
                // an arm-local slot index. Empty captures vec means
                // the arm body references nothing outside its op-args
                // / `k_name` / globals — codegen passes `null` as
                // this arm's `closure_ptr` (no closure record alloc).
                //
                // `arm_local_idx` is the per-arm declaration index
                // *for this handle*, derived from `arm_indices.len()`
                // BEFORE this arm gets pushed. Locking the side-table
                // lookup index to the explicit name — rather than
                // computing it inline at the `.get()` call — means a
                // future refactor that flips push order surfaces as
                // an immediate off-by-one rather than silently mis-
                // reading captures from the wrong arm.
                let arm_local_idx: usize = arm_indices.len();
                let captures_typed: &[(String, crate::typecheck::Ty)] = ctx
                    .handle_arm_captures
                    .get(span)
                    .and_then(|per_arm| per_arm.get(arm_local_idx))
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);
                let captures: Vec<ArmCapture> = captures_typed
                    .iter()
                    .map(|(name, ty)| ArmCapture {
                        name: name.clone(),
                        kind: crate::closure_convert::slot_kind_for_ty(ty),
                    })
                    .collect();
                let rewritten_body =
                    rewrite_arm_body_with_captures(&arm.body, &captures, &arg_names, &arm.k_name);
                synth.push(HandlerArmSynth {
                    func_id,
                    body: rewritten_body,
                    arg_names,
                    arg_types,
                    body_ty,
                    captures,
                    k_name: arm.k_name.clone(),
                });
                arm_indices.push(global_idx);
            }
            // The pre-pass walks each fn body exactly once and is
            // the only writer to `indices`. A duplicate span here
            // means the AST contains two `Expr::Handle` nodes that
            // share a span — either a future monomorphisation pass
            // that clones a handle-bearing fn body (the per-monomorph
            // color variance question — see PB4 deviation entry),
            // closure conversion that lifts a lambda whose body
            // contains a handle, or any other AST-cloning transform.
            // Today that doesn't happen; assert it loudly so any
            // future regression surfaces here rather than as a
            // silent overwrite that ships wrong `FuncRef`s to one of
            // the aliased handles. Same shape as the `op_names`
            // dedup → `debug_assert!` swap from review-fixup
            // commit `54b4a60`. The `insert` lives outside the
            // assert so the side effect runs in release builds too.
            let prev = indices.insert(span.clone(), arm_indices);
            debug_assert!(
                prev.is_none(),
                "codegen pre-pass: duplicate handle span {span:?} — \
                 pre-pass invariant violated"
            );
            Ok(())
        }
    }
}

/// Plan B Task 55 (Phase 4d) — detect whether an arm body's tail
/// expression is a `k(arg)` call. Returns `Some(arg_expr)` when the
/// body has the shape:
///
///   * `k(arg)` — direct call, OR
///   * `block { stmts...; tail: k(arg) }` — block-wrapped, OR
///   * recursively, blocks all the way down to a final `k(arg)` tail.
///
/// Returns `None` for shapes where the tail is anything else (a value,
/// an `if`/`match` expression, a non-`k` call, etc.). The walker's
/// `tail` parameter aligns with this detection: only positions reached
/// here would have been allowed as tail-`k` callees by the walker.
///
/// `If`/`Match` branch tails are deliberately NOT propagated through —
/// Phase 4d MVP doesn't support multi-branch tail-`k` shapes (e.g.,
/// `if c { k(x) } else { k(y) }`); those would need a join-block
/// lowering with both branches producing `*NextStep` values, deferred
/// to a later phase. The walker rejects k-calls inside If/Match
/// branches as non-tail.
fn arm_body_tail_is_k_call<'a>(
    body: &'a crate::ast::Expr,
    k_name: &str,
) -> Option<&'a crate::ast::Expr> {
    use crate::ast::Expr;
    match body {
        Expr::Call { callee, args, .. } if args.len() == 1 => {
            if let Expr::Ident(n, _) = callee.as_ref() {
                if n == k_name {
                    return Some(&args[0]);
                }
            }
            None
        }
        Expr::Block(b) => b
            .tail
            .as_ref()
            .and_then(|t| arm_body_tail_is_k_call(t, k_name)),
        _ => None,
    }
}

/// Plan B Task 55 (Phase 4d) — rewrite an arm body so every captured-
/// name reference becomes an `Expr::ClosureEnvLoad` reading from the
/// synthetic arm fn's `closure_ptr` at an arm-local slot index.
///
/// Inputs:
///   - `body` — the post-closure-conversion arm body. May contain plain
///     `Expr::Ident` references to enclosing-fn locals (let-bindings,
///     fn-params; closure_convert leaves these as Idents because the
///     surrounding fn is the binding scope) AND
///     `Expr::ClosureEnvLoad` nodes (closure_convert produces these
///     when the surrounding fn is a synthetic lambda fn whose own
///     captures appear inside the arm body — the Idents were rewritten
///     against the enclosing-fn's capture indices, which DON'T match
///     the arm fn's `closure_ptr` layout, so we re-index them here).
///   - `captures` — arm-local capture slots in declaration order.
///     Position in this list is the slot index in the arm's closure
///     record at runtime.
///   - `arg_names`, `k_name` — arm-bound names (op-args + the
///     continuation). References to these stay as `Expr::Ident` and
///     resolve through the synth-pass `Lowerer.env`.
///
/// Scope tracking: maintains a stack of `BTreeSet<String>` scopes so
/// inner shadowing doesn't trip the rewrite. `let n = ... ; n + 1`
/// inside the arm body adds `n` to the current scope; an outer-fn
/// `n` would shadow correctly. Nested constructs that introduce
/// bindings (`Match` patterns, `Block` lets, nested `Handle` op-arms
/// or return arms, `Lambda` params, `RecordLit` and `ClosureRecord`
/// don't introduce bindings) push/pop scopes.
///
/// Idempotent: applying the rewrite twice produces the same AST
/// (the second pass sees the rewritten ClosureEnvLoad indices and
/// just re-emits them — same name → same slot index).
fn rewrite_arm_body_with_captures(
    body: &crate::ast::Expr,
    captures: &[ArmCapture],
    arg_names: &[String],
    k_name: &str,
) -> crate::ast::Expr {
    use std::collections::BTreeSet;
    let mut bottom: BTreeSet<String> = BTreeSet::new();
    for n in arg_names {
        bottom.insert(n.clone());
    }
    bottom.insert(k_name.to_string());
    let mut scopes: Vec<BTreeSet<String>> = vec![bottom];
    rewrite_expr(body, captures, &mut scopes)
}

fn capture_index(captures: &[ArmCapture], name: &str) -> Option<usize> {
    captures.iter().position(|c| c.name == name)
}

fn name_in_active_scope(name: &str, scopes: &[std::collections::BTreeSet<String>]) -> bool {
    scopes.iter().any(|s| s.contains(name))
}

fn rewrite_expr(
    e: &crate::ast::Expr,
    captures: &[ArmCapture],
    scopes: &mut Vec<std::collections::BTreeSet<String>>,
) -> crate::ast::Expr {
    use crate::ast::Expr;
    use std::collections::BTreeSet;
    match e {
        Expr::IntLit(..) | Expr::StringLit(..) | Expr::BoolLit(..) | Expr::CharLit(..) => e.clone(),
        Expr::Ident(name, span) => {
            if name_in_active_scope(name, scopes) {
                e.clone()
            } else if let Some(idx) = capture_index(captures, name) {
                Expr::ClosureEnvLoad {
                    kind: captures[idx].kind,
                    index: idx,
                    name: name.clone(),
                    span: span.clone(),
                }
            } else {
                e.clone()
            }
        }
        // Closure_convert may have rewritten an Ident to ClosureEnvLoad
        // with the enclosing fn's capture indices. Re-index against the
        // arm's captures list (matched by `name`); kind stays the same
        // (closure_convert's slot_kind_for_ty(ty) == this rewriter's
        // input kind for the same ty).
        Expr::ClosureEnvLoad { name, span, .. } => {
            if let Some(idx) = capture_index(captures, name) {
                Expr::ClosureEnvLoad {
                    kind: captures[idx].kind,
                    index: idx,
                    name: name.clone(),
                    span: span.clone(),
                }
            } else {
                // Not in this arm's captures map — leave unchanged.
                // This branch fires for ClosureEnvLoads inside nested
                // lambdas whose captures don't surface to the arm's
                // own capture set. Arm-body walker rejection of
                // nested Lambda/ClosureRecord (preserved in Phase 4d)
                // means this is currently unreachable in well-formed
                // programs; keeping the pass-through is defensive.
                e.clone()
            }
        }
        Expr::Binary { op, lhs, rhs, span } => Expr::Binary {
            op: *op,
            lhs: Box::new(rewrite_expr(lhs, captures, scopes)),
            rhs: Box::new(rewrite_expr(rhs, captures, scopes)),
            span: span.clone(),
        },
        Expr::Unary { op, operand, span } => Expr::Unary {
            op: *op,
            operand: Box::new(rewrite_expr(operand, captures, scopes)),
            span: span.clone(),
        },
        Expr::If {
            cond,
            then_block,
            else_block,
            span,
        } => Expr::If {
            cond: Box::new(rewrite_expr(cond, captures, scopes)),
            then_block: Box::new(rewrite_block(then_block, captures, scopes)),
            else_block: Box::new(rewrite_block(else_block, captures, scopes)),
            span: span.clone(),
        },
        Expr::Match {
            scrutinee,
            arms,
            span,
        } => {
            let new_scrutinee = rewrite_expr(scrutinee, captures, scopes);
            let new_arms: Vec<crate::ast::MatchArm> = arms
                .iter()
                .map(|arm| {
                    let mut arm_scope: BTreeSet<String> = BTreeSet::new();
                    crate::typecheck::pattern_bindings(&arm.pattern, &mut arm_scope);
                    scopes.push(arm_scope);
                    let new_body = rewrite_expr(&arm.body, captures, scopes);
                    scopes.pop();
                    crate::ast::MatchArm {
                        pattern: arm.pattern.clone(),
                        body: new_body,
                        span: arm.span.clone(),
                    }
                })
                .collect();
            Expr::Match {
                scrutinee: Box::new(new_scrutinee),
                arms: new_arms,
                span: span.clone(),
            }
        }
        Expr::Block(b) => Expr::Block(Box::new(rewrite_block(b, captures, scopes))),
        Expr::Call { callee, args, span } => Expr::Call {
            callee: Box::new(rewrite_expr(callee, captures, scopes)),
            args: args
                .iter()
                .map(|a| rewrite_expr(a, captures, scopes))
                .collect(),
            span: span.clone(),
        },
        Expr::Perform(p) => {
            let new_args: Vec<Expr> = p
                .args
                .iter()
                .map(|a| rewrite_expr(a, captures, scopes))
                .collect();
            Expr::Perform(crate::ast::PerformExpr {
                effect: p.effect.clone(),
                op: p.op.clone(),
                args: new_args,
                span: p.span.clone(),
            })
        }
        Expr::Lambda {
            params,
            return_type,
            effects,
            effect_row_var,
            body,
            span,
        } => {
            // Phase 4d walker preserves the nested-lambda rejection for
            // arm bodies, but the rewriter still descends defensively
            // so a future loosening doesn't silently misbehave.
            let mut lambda_scope: BTreeSet<String> = BTreeSet::new();
            for p in params {
                lambda_scope.insert(p.name.clone());
            }
            scopes.push(lambda_scope);
            let new_body = rewrite_expr(body, captures, scopes);
            scopes.pop();
            Expr::Lambda {
                params: params.clone(),
                return_type: return_type.clone(),
                effects: effects.clone(),
                effect_row_var: effect_row_var.clone(),
                body: Box::new(new_body),
                span: span.clone(),
            }
        }
        Expr::ClosureRecord {
            code_fn_name,
            env_exprs,
            env_slot_kinds,
            span,
        } => {
            let new_env: Vec<Expr> = env_exprs
                .iter()
                .map(|ee| rewrite_expr(ee, captures, scopes))
                .collect();
            Expr::ClosureRecord {
                code_fn_name: code_fn_name.clone(),
                env_exprs: new_env,
                env_slot_kinds: env_slot_kinds.clone(),
                span: span.clone(),
            }
        }
        Expr::RecordLit { name, fields, span } => {
            let new_fields: Vec<crate::ast::RecordFieldLit> = fields
                .iter()
                .map(|f| crate::ast::RecordFieldLit {
                    name: f.name.clone(),
                    value: rewrite_expr(&f.value, captures, scopes),
                    span: f.span.clone(),
                })
                .collect();
            Expr::RecordLit {
                name: name.clone(),
                fields: new_fields,
                span: span.clone(),
            }
        }
        Expr::Handle {
            body,
            return_arm,
            op_arms,
            span,
        } => {
            let new_body = rewrite_expr(body, captures, scopes);
            let new_return_arm = return_arm.as_ref().map(|ra| {
                let mut ra_scope: BTreeSet<String> = BTreeSet::new();
                ra_scope.insert(ra.binding.clone());
                scopes.push(ra_scope);
                let new_b = rewrite_expr(&ra.body, captures, scopes);
                scopes.pop();
                Box::new(crate::ast::HandleReturnArm {
                    binding: ra.binding.clone(),
                    binding_span: ra.binding_span.clone(),
                    body: new_b,
                    span: ra.span.clone(),
                })
            });
            let new_op_arms: Vec<crate::ast::HandleOpArm> = op_arms
                .iter()
                .map(|arm| {
                    let mut arm_scope: BTreeSet<String> = BTreeSet::new();
                    for p in &arm.params {
                        arm_scope.insert(p.name.clone());
                    }
                    arm_scope.insert(arm.k_name.clone());
                    scopes.push(arm_scope);
                    let new_arm_body = rewrite_expr(&arm.body, captures, scopes);
                    scopes.pop();
                    crate::ast::HandleOpArm {
                        body: new_arm_body,
                        ..arm.clone()
                    }
                })
                .collect();
            Expr::Handle {
                body: Box::new(new_body),
                return_arm: new_return_arm,
                op_arms: new_op_arms,
                span: span.clone(),
            }
        }
    }
}

fn rewrite_block(
    b: &crate::ast::Block,
    captures: &[ArmCapture],
    scopes: &mut Vec<std::collections::BTreeSet<String>>,
) -> crate::ast::Block {
    use crate::ast::{Block, LetStmt, Stmt};
    // Push a fresh scope frame for the block's own let-bindings, then
    // pop on exit. Mirrors the Match-arm / Lambda-param / Handle-arm
    // patterns used elsewhere in `rewrite_expr`. Earlier shapes (mutate
    // top frame + roll-back-by-name on exit) corrupted the parent scope
    // when a block-local `let X` shadowed a name X already present in
    // the parent — the rollback removed X from the parent regardless
    // of pre-block presence. Push/pop sidesteps that entirely:
    // shadowing inner names live only inside the new frame and the
    // parent frame is byte-identical before and after.
    let block_scope: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    scopes.push(block_scope);
    let mut new_stmts: Vec<Stmt> = Vec::with_capacity(b.stmts.len());
    for s in &b.stmts {
        match s {
            Stmt::Let(l) => {
                // RHS evaluated under current scopes (the let `name`
                // is not yet in scope when its initializer runs).
                let new_value = rewrite_expr(&l.value, captures, scopes);
                new_stmts.push(Stmt::Let(LetStmt {
                    name: l.name.clone(),
                    ty: l.ty.clone(),
                    value: new_value,
                    span: l.span.clone(),
                }));
                // Then add the binding to the top (block-local) scope
                // frame so subsequent stmts and the block tail see it.
                if let Some(top) = scopes.last_mut() {
                    let _ = top.insert(l.name.clone());
                }
            }
            Stmt::Expr(e) => {
                new_stmts.push(Stmt::Expr(rewrite_expr(e, captures, scopes)));
            }
            Stmt::Perform(p) => {
                let new_args: Vec<crate::ast::Expr> = p
                    .args
                    .iter()
                    .map(|a| rewrite_expr(a, captures, scopes))
                    .collect();
                new_stmts.push(Stmt::Perform(crate::ast::PerformExpr {
                    effect: p.effect.clone(),
                    op: p.op.clone(),
                    args: new_args,
                    span: p.span.clone(),
                }));
            }
        }
    }
    let new_tail = b.tail.as_ref().map(|t| rewrite_expr(t, captures, scopes));
    scopes.pop();
    Block {
        stmts: new_stmts,
        tail: new_tail,
        span: b.span.clone(),
    }
}

/// Mangle a Sigil-level fn name into a linker-legal symbol name.
/// `main` keeps the historical mangling `sigil_user_main` (the C-main
/// shim calls this symbol). Other names get a `sigil_user_` prefix with
/// `$` from synthetic names (`$lambda_N`) rewritten to `__` so the
/// result is legal on both ELF and Mach-O.
fn mangle_user_fn(name: &str) -> String {
    if name == "main" {
        return "sigil_user_main".to_string();
    }
    let sanitized = name.replace('$', "__");
    format!("sigil_user_{sanitized}")
}

/// Accumulator for safepoint records emitted during function lowering.
///
/// Wire-format constants (`STACKMAP_MAGIC`, `STACKMAP_VERSION_PLACEHOLDER`,
/// `STACKMAP_HEADER_SIZE`, `STACKMAP_RECORD_SIZE`, `STACKMAP_FLAG_PLACEHOLDER`)
/// live in `sigil-abi::stackmap` (Plan B Stage 4.5.5). The runtime's
/// section parser (`sigil_runtime::stackmap::parse_section`) reads
/// against the same constants.
///
/// Plan A1 emits **version 0 (placeholder)** records:
///
/// ```text
/// header  = magic:4 "SGST" | version:4 | record_count:4              // 12 bytes
/// record  = pc_offset:4    | live_count:2 (always 0 in v0) | flags:2 //  8 bytes
/// ```
///
/// `flags` has bit 0 (`STACKMAP_FLAG_PLACEHOLDER`) set in v0 so a v2
/// reader that only understands version 1 can detect stale placeholder
/// records on a per-record basis as well as via the version field.
///
/// Version 1 (Plan B) will reuse the same header; the record format
/// gains a live-value list per record and `pc_offset` becomes a real
/// post-regalloc code offset via Cranelift's safepoint API.
///
/// Stage 1 populates each record with an opaque placeholder (the
/// Cranelift `Inst` handle of the call site, not a real post-regalloc
/// code offset) and `live_count = 0`. Plan B replaces `push_placeholder`
/// with a real `push(pc_offset, live_values)` API backed by Cranelift's
/// safepoint metadata. The section header's version field is bumped at
/// the same time so existing consumers can distinguish the formats.
pub struct StackMapBuilder {
    records: Vec<StackMapRecord>,
}

struct StackMapRecord {
    pc_offset_placeholder: u32,
}

impl StackMapBuilder {
    pub fn new() -> Self {
        Self {
            records: Vec::new(),
        }
    }

    /// Record a safepoint call site. Stage 1 stores the Cranelift `Inst`
    /// handle as the `pc_offset` field — this is a placeholder; the
    /// `STACKMAP_FLAG_PLACEHOLDER` bit in the record's flags makes that
    /// status visible to downstream readers.
    pub fn push_placeholder(&mut self, pc_offset_placeholder: u32) {
        self.records.push(StackMapRecord {
            pc_offset_placeholder,
        });
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Serialise the section body (header + all records). Little-endian
    /// on the host; the section is not relocated, so endianness of the
    /// emitter matches endianness of the consumer.
    pub fn serialize(&self) -> Vec<u8> {
        let mut out =
            Vec::with_capacity(STACKMAP_HEADER_SIZE + self.records.len() * STACKMAP_RECORD_SIZE);
        out.extend_from_slice(STACKMAP_MAGIC);
        out.extend_from_slice(&STACKMAP_VERSION_PLACEHOLDER.to_le_bytes());
        out.extend_from_slice(&(self.records.len() as u32).to_le_bytes());
        for r in &self.records {
            out.extend_from_slice(&r.pc_offset_placeholder.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes()); // live_count = 0 in v0
            out.extend_from_slice(&STACKMAP_FLAG_PLACEHOLDER.to_le_bytes());
        }
        out
    }
}

impl Default for StackMapBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Compile `cc` to an object file at `out_path`. Returns `Ok(())` on
/// success. Stage 1 compilation is deterministic given identical input on
/// the same host.
pub fn emit_object(cc: &ClosureConvertedProgram, out_path: &Path) -> Result<(), String> {
    // Plan A1 Stage 1 stops at the hello-world shape. Validate that shape
    // up front so codegen can assume it.
    let checked: &CheckedProgram = &cc.colored.mono.anf.checked;

    // Plan B task 48 — codegen-entry guard. The verification-debt
    // entry "Codegen path for un-monomorphized generic params" in
    // `PLAN_B_DEVIATIONS.md` reserves this assertion: monomorphization
    // (Task 49) must complete before codegen, so the IR reaching this
    // point cannot contain `TypeExpr::Apply` or generic-parameter
    // references. If either survives, the assert fires loudly rather
    // than letting `cranelift_ty_for_type_expr` silently lower an
    // unmonomorphised type as a pointer (which would crash at the
    // platform-call boundary).
    assert!(
        !contains_apply_or_generic_ref(&checked.program),
        "codegen invariant: monomorphization (Task 49) must complete before codegen; \
         received program still contains TypeExpr::Apply or generic-parameter references — \
         see PLAN_B_DEVIATIONS verification-debt entry \"Codegen path for un-monomorphized \
         generic params\""
    );

    // Plan B Task 55 (Phase 2 minimum) — `handle <body> with { arms
    // }` is supported only when the body contains no non-IO `perform`
    // (the handle-frame setup, push/pop, and arm-fn synthesis path
    // ships in Phase 3+ once the CPS calling convention lands). Any
    // handle expression with a non-IO `perform` in its body would
    // need the unimplemented runtime dispatch and is rejected here
    // with a clean compile-time error. IO `perform` in handle bodies
    // is fine — `IO` is special-cased in `lower_perform` and doesn't
    // route through `sigil_perform`.
    if let Some(msg) = unsupported_handle_construct(&checked.program) {
        return Err(msg);
    }

    let string_literals = &checked.string_literals;

    // Plan A3 task 40: build per-type layout descriptors once before any
    // function body is lowered. Layouts are shared across allocation
    // sites (Task 41.1) and match decision trees (Task 41.2). Errors
    // from `build_layouts` map to E0130 "type layout too large"; Plan
    // A3 v1's surface stays well under the 63-word ceiling, so this
    // only fires if a future user type exceeds the header's count
    // field. The returned map is indexed by type name and iterates
    // alphabetically (BTreeMap ordering), giving reproducible tag
    // assignment across builds.
    let type_layouts =
        crate::layout::build_layouts(&checked.types).map_err(|e| format!("E0130: {e}"))?;
    let ctor_index = crate::layout::build_ctor_index(&type_layouts);

    // Build the Cranelift ISA for the current host.
    let triple = Triple::host();
    let mut flag_builder = settings::builder();
    // is_pic = true lets the linker produce PIE executables; matches the
    // design's "position-independent, fully relocatable" commitment.
    flag_builder
        .set("is_pic", "true")
        .map_err(|e| format!("cranelift flag is_pic: {e}"))?;
    // Deterministic register allocation is not a flag; regalloc2 is
    // deterministic under the same input.
    let isa_builder =
        isa::lookup(triple.clone()).map_err(|e| format!("cranelift isa for {triple}: {e}"))?;
    let isa = isa_builder
        .finish(settings::Flags::new(flag_builder))
        .map_err(|e| format!("cranelift isa finish: {e}"))?;
    let pointer_ty = isa.pointer_type();

    let obj_builder = ObjectBuilder::new(isa, "sigil_program", default_libcall_names())
        .map_err(|e| format!("cranelift-object builder: {e}"))?;
    let mut module = ObjectModule::new(obj_builder);

    // Declare runtime symbols we'll call.
    let gc_init = module
        .declare_function(
            "sigil_gc_init",
            Linkage::Import,
            &Signature::new(isa_call_conv(&module)),
        )
        .map_err(|e| format!("declare sigil_gc_init: {e}"))?;

    let mut string_new_sig = Signature::new(isa_call_conv(&module));
    string_new_sig.params.push(AbiParam::new(pointer_ty)); // bytes
    string_new_sig.params.push(AbiParam::new(pointer_ty)); // len
    string_new_sig.returns.push(AbiParam::new(pointer_ty)); // heap ptr
    let string_new = module
        .declare_function("sigil_string_new", Linkage::Import, &string_new_sig)
        .map_err(|e| format!("declare sigil_string_new: {e}"))?;

    let mut println_sig = Signature::new(isa_call_conv(&module));
    println_sig.params.push(AbiParam::new(pointer_ty));
    let println = module
        .declare_function("sigil_println", Linkage::Import, &println_sig)
        .map_err(|e| format!("declare sigil_println: {e}"))?;

    // Plan A2: arithmetic-abort panic. `sigil_panic_arith_error(*const
    // c_char) -> !`. Cranelift doesn't know the noreturn — we emit a
    // `trap` after the call to satisfy Cranelift's terminator
    // invariant; the runtime exits the process before the trap runs.
    let mut panic_arith_sig = Signature::new(isa_call_conv(&module));
    panic_arith_sig.params.push(AbiParam::new(pointer_ty));
    let panic_arith = module
        .declare_function("sigil_panic_arith_error", Linkage::Import, &panic_arith_sig)
        .map_err(|e| format!("declare sigil_panic_arith_error: {e}"))?;

    // Plan A2 task 32: `sigil_alloc(header: u64, payload_bytes: usize)
    // -> *mut u8`. Heap allocation for closure records (and any future
    // codegen-level alloc). `payload_bytes` is declared as
    // `pointer_ty` since the Rust signature uses `usize`.
    let mut alloc_sig = Signature::new(isa_call_conv(&module));
    alloc_sig.params.push(AbiParam::new(types::I64)); // header
    alloc_sig.params.push(AbiParam::new(pointer_ty)); // payload_bytes (usize)
    alloc_sig.returns.push(AbiParam::new(pointer_ty));
    let alloc = module
        .declare_function("sigil_alloc", Linkage::Import, &alloc_sig)
        .map_err(|e| format!("declare sigil_alloc: {e}"))?;

    // Plan A2 task 34: `sigil_int_to_string(n: i64) -> *mut u8`. The
    // runtime formats `n` as decimal, allocates a fresh String on the
    // Boehm heap, and returns the 8-byte-aligned header pointer
    // suitable for `sigil_println` consumption. Seeded into the
    // typechecker's `fn_env` as the builtin `int_to_string(Int) ->
    // String !` (see `typecheck::builtin_fn_env`); callers reach it
    // via `Expr::Call { callee: Ident("int_to_string"), .. }` unless
    // a user `fn int_to_string` shadows the builtin.
    let mut int_to_string_sig = Signature::new(isa_call_conv(&module));
    int_to_string_sig.params.push(AbiParam::new(types::I64)); // n
    int_to_string_sig.returns.push(AbiParam::new(pointer_ty)); // heap String ptr
    let int_to_string = module
        .declare_function("sigil_int_to_string", Linkage::Import, &int_to_string_sig)
        .map_err(|e| format!("declare sigil_int_to_string: {e}"))?;

    // Plan B Task 55 (Phase 3a) — runtime handler-frame imports.
    // Phase 3a wires the frame allocation + push/pop ABI from Task
    // 56 around every `handle` body. Arms stay null in this commit
    // (the existing `unsupported_handle_construct` guard rejects
    // programs whose body would actually perform the handled effect,
    // so the runtime never invokes an arm fn pointer). Phase 3b adds
    // arm-fn synthesis + `sigil_perform` lowering + arm dispatch;
    // Phase 4+ adds continuation-using arms + multi-shot.

    // sigil_handler_frame_new(effect_id: u32, arm_count: u32) -> *mut HandlerFrame
    let mut handler_frame_new_sig = Signature::new(isa_call_conv(&module));
    handler_frame_new_sig.params.push(AbiParam::new(types::I32)); // effect_id
    handler_frame_new_sig.params.push(AbiParam::new(types::I32)); // arm_count
    handler_frame_new_sig
        .returns
        .push(AbiParam::new(pointer_ty));
    let handler_frame_new = module
        .declare_function(
            "sigil_handler_frame_new",
            Linkage::Import,
            &handler_frame_new_sig,
        )
        .map_err(|e| format!("declare sigil_handler_frame_new: {e}"))?;

    // sigil_handle_push(frame: *mut HandlerFrame)
    let mut handle_push_sig = Signature::new(isa_call_conv(&module));
    handle_push_sig.params.push(AbiParam::new(pointer_ty));
    let handle_push = module
        .declare_function("sigil_handle_push", Linkage::Import, &handle_push_sig)
        .map_err(|e| format!("declare sigil_handle_push: {e}"))?;

    // sigil_handle_pop() -> *mut HandlerFrame (return discarded at
    // codegen sites; the runtime tracks the popped pointer for GC
    // reasons but codegen has no use for it).
    let mut handle_pop_sig = Signature::new(isa_call_conv(&module));
    handle_pop_sig.returns.push(AbiParam::new(pointer_ty));
    let handle_pop = module
        .declare_function("sigil_handle_pop", Linkage::Import, &handle_pop_sig)
        .map_err(|e| format!("declare sigil_handle_pop: {e}"))?;

    // Plan B Task 55 (Phase 3b) — three more handler-ABI imports.
    //
    // sigil_handler_frame_set_arm(frame, op_id: u32,
    //                             fn_ptr: *mut u8, closure_ptr: *mut u8)
    let mut handler_frame_set_arm_sig = Signature::new(isa_call_conv(&module));
    handler_frame_set_arm_sig
        .params
        .push(AbiParam::new(pointer_ty));
    handler_frame_set_arm_sig
        .params
        .push(AbiParam::new(types::I32));
    handler_frame_set_arm_sig
        .params
        .push(AbiParam::new(pointer_ty));
    handler_frame_set_arm_sig
        .params
        .push(AbiParam::new(pointer_ty));
    let handler_frame_set_arm = module
        .declare_function(
            "sigil_handler_frame_set_arm",
            Linkage::Import,
            &handler_frame_set_arm_sig,
        )
        .map_err(|e| format!("declare sigil_handler_frame_set_arm: {e}"))?;

    // sigil_perform(effect_id: u32, op_id: u32,
    //               args_ptr: *const u64, args_len: u32,
    //               k_closure_ptr: *mut u8, k_fn_ptr: *mut u8)
    //               -> *mut NextStep
    let mut perform_sig = Signature::new(isa_call_conv(&module));
    perform_sig.params.push(AbiParam::new(types::I32)); // effect_id
    perform_sig.params.push(AbiParam::new(types::I32)); // op_id
    perform_sig.params.push(AbiParam::new(pointer_ty)); // args_ptr
    perform_sig.params.push(AbiParam::new(types::I32)); // args_len
    perform_sig.params.push(AbiParam::new(pointer_ty)); // k_closure_ptr
    perform_sig.params.push(AbiParam::new(pointer_ty)); // k_fn_ptr
    perform_sig.returns.push(AbiParam::new(pointer_ty)); // *mut NextStep
    let perform_func = module
        .declare_function("sigil_perform", Linkage::Import, &perform_sig)
        .map_err(|e| format!("declare sigil_perform: {e}"))?;

    // sigil_next_step_done(value: u64) -> *mut NextStep
    let mut next_step_done_sig = Signature::new(isa_call_conv(&module));
    next_step_done_sig.params.push(AbiParam::new(types::I64));
    next_step_done_sig.returns.push(AbiParam::new(pointer_ty));
    let next_step_done = module
        .declare_function("sigil_next_step_done", Linkage::Import, &next_step_done_sig)
        .map_err(|e| format!("declare sigil_next_step_done: {e}"))?;

    // sigil_run_loop(initial: *mut NextStep) -> u64. Drives the CPS
    // trampoline to a terminal NextStep::Done and returns its value.
    // Phase 3b uses this to dispatch the NextStep::Call that
    // `sigil_perform` returns: perform never returns a Done directly
    // — it builds a Call to the matching arm + (k_closure, k_fn);
    // the arm runs and returns Done(value). `sigil_run_loop` does
    // the dispatch and returns the final Done's value as u64.
    let mut run_loop_sig = Signature::new(isa_call_conv(&module));
    run_loop_sig.params.push(AbiParam::new(pointer_ty));
    run_loop_sig.returns.push(AbiParam::new(types::I64));
    let run_loop = module
        .declare_function("sigil_run_loop", Linkage::Import, &run_loop_sig)
        .map_err(|e| format!("declare sigil_run_loop: {e}"))?;

    // Plan B Task 55 (Phase 4d) — sigil_next_step_call(closure_ptr,
    // fn_ptr, arg_count) -> *mut NextStep. Allocates a NextStep::Call
    // record from the per-dispatch arena. Codegen emits this for
    // tail-position `k(arg)` lowering inside synthetic arm fns: the
    // returned NextStep is what the arm fn returns, the trampoline
    // dispatches the Call, the args buffer (written via
    // sigil_next_step_args_ptr) carries the single u64 arg.
    let mut next_step_call_sig = Signature::new(isa_call_conv(&module));
    next_step_call_sig.params.push(AbiParam::new(pointer_ty)); // closure_ptr
    next_step_call_sig.params.push(AbiParam::new(pointer_ty)); // fn_ptr
    next_step_call_sig.params.push(AbiParam::new(types::I32)); // arg_count
    next_step_call_sig.returns.push(AbiParam::new(pointer_ty));
    let next_step_call = module
        .declare_function("sigil_next_step_call", Linkage::Import, &next_step_call_sig)
        .map_err(|e| format!("declare sigil_next_step_call: {e}"))?;

    // sigil_next_step_args_ptr(ns: *mut NextStep) -> *mut u64.
    // Returns the args buffer pointer for a NextStep::Call (or null
    // for Done / zero-arg). Codegen writes args via this pointer.
    let mut next_step_args_ptr_sig = Signature::new(isa_call_conv(&module));
    next_step_args_ptr_sig
        .params
        .push(AbiParam::new(pointer_ty));
    next_step_args_ptr_sig
        .returns
        .push(AbiParam::new(pointer_ty));
    let next_step_args_ptr = module
        .declare_function(
            "sigil_next_step_args_ptr",
            Linkage::Import,
            &next_step_args_ptr_sig,
        )
        .map_err(|e| format!("declare sigil_next_step_args_ptr: {e}"))?;

    // Plan B Task 55 (Phase 4d) — sigil_continuation_identity, the
    // CPS-arm-fn-ABI runtime intrinsic that codegen emits as `k_fn`
    // at every non-IO perform site. When dispatched via
    // sigil_run_loop, returns Done(args_ptr[0]). Rationale + hardening
    // notes: see runtime/src/handlers.rs and the
    // `[DEVIATION Task 55] Phase 4d` entry in PLAN_B_DEVIATIONS.md.
    let mut continuation_identity_sig = Signature::new(isa_call_conv(&module));
    continuation_identity_sig
        .params
        .push(AbiParam::new(pointer_ty)); // closure_ptr
    continuation_identity_sig
        .params
        .push(AbiParam::new(pointer_ty)); // args_ptr
    continuation_identity_sig
        .params
        .push(AbiParam::new(types::I32)); // args_len
    continuation_identity_sig
        .returns
        .push(AbiParam::new(pointer_ty));
    let continuation_identity = module
        .declare_function(
            "sigil_continuation_identity",
            Linkage::Import,
            &continuation_identity_sig,
        )
        .map_err(|e| format!("declare sigil_continuation_identity: {e}"))?;

    // C-callable main: int main(int argc, char **argv). Stage 1 ignores argv.
    let mut main_sig = Signature::new(isa_call_conv(&module));
    main_sig.params.push(AbiParam::new(types::I32));
    main_sig.params.push(AbiParam::new(pointer_ty));
    main_sig.returns.push(AbiParam::new(types::I32));
    let main = module
        .declare_function("main", Linkage::Export, &main_sig)
        .map_err(|e| format!("declare main: {e}"))?;

    // Pre-declare every user function (original + synthetic $lambda_N from
    // closure conversion) under the **closure calling convention**:
    //
    //   (closure_ptr: *u8, user_arg1: T1, ..., user_argN: TN) -> ret_ty
    //
    // closure_ptr is the heap address of the fn's runtime closure record
    // (or a null pointer for direct calls to top-level fns with no
    // captured environment). Direct callers in codegen pass null;
    // `ClosureRecord`-returning callees pass the allocated record's ptr.
    //
    // Plan A2 task 32 introduces this ABI for all user fns — including
    // the top-level `main`. User-main keeps its mangled name
    // `sigil_user_main` (the C-main shim calls that); its Cranelift
    // signature gains the closure_ptr param at index 0 (always null from
    // the shim). Tagging the return value happens inside main's lowering;
    // other user fns return their raw Cranelift value.
    let mut user_fns: BTreeMap<String, UserFnEntry> = BTreeMap::new();
    for item in &checked.program.items {
        if let crate::ast::Item::Fn(f) = item {
            let mut sig = Signature::new(isa_call_conv(&module));
            // arg 0: closure_ptr (always pointer-sized).
            sig.params.push(AbiParam::new(pointer_ty));
            let mut param_tys: Vec<Type> = Vec::with_capacity(f.params.len() + 1);
            param_tys.push(pointer_ty);
            for p in &f.params {
                let t = cranelift_ty_for_type_expr(&p.ty, pointer_ty);
                sig.params.push(AbiParam::new(t));
                param_tys.push(t);
            }
            let ret_ty = cranelift_ty_for_type_expr(&f.return_type, pointer_ty);
            sig.returns.push(AbiParam::new(ret_ty));

            let mangled = mangle_user_fn(&f.name);
            let func_id = module
                .declare_function(&mangled, Linkage::Local, &sig)
                .map_err(|e| format!("declare {mangled}: {e}"))?;
            user_fns.insert(
                f.name.clone(),
                UserFnEntry {
                    func_id,
                    signature: sig,
                    param_tys,
                    ret_ty,
                },
            );
        }
    }

    let user_main = user_fns
        .get("main")
        .map(|uf| uf.func_id)
        .ok_or_else(|| "codegen requires a `main` function".to_string())?;

    // Plan B Task 55 (Phase 3b) — pre-pass for synthetic handler-arm
    // CPS fns. Walk every user fn body; for each `Expr::Handle`
    // encountered, allocate one `FuncId` per arm (uniform CPS
    // calling convention `extern "C" fn(closure_ptr, args_ptr,
    // args_len) -> *mut NextStep`). The arm body Expr is captured
    // by value so the synthetic-fn definition pass at the bottom
    // of `emit_object` can lower it without re-walking the program.
    //
    // Allocation happens before any user fn is defined so calling
    // sites (the `Expr::Handle` lowering in `Lowerer`) can look up
    // the arm `FuncId`s via `module.declare_func_in_func` against
    // their fn-local FuncRefs. Definitions happen after all user
    // fns are defined to keep the `module.define_function` cycle
    // simple (no nested-builder gymnastics).
    let mut handler_arm_synth: Vec<HandlerArmSynth> = Vec::new();
    let mut handler_arm_indices: BTreeMap<Span, Vec<usize>> = BTreeMap::new();
    {
        let cps_arm_sig = cps_signature(pointer_ty, &module);
        let arm_synth_ctx = ArmSynthCtx {
            cps_arm_sig: &cps_arm_sig,
            op_ids: &checked.op_ids,
            effects: &checked.effects,
            pointer_ty,
            handle_arm_captures: &checked.handle_arm_captures,
        };
        for item in &checked.program.items {
            if let crate::ast::Item::Fn(f) = item {
                collect_handle_arms_in_block(
                    &f.body,
                    &mut module,
                    &arm_synth_ctx,
                    &mut handler_arm_synth,
                    &mut handler_arm_indices,
                )?;
            }
        }
    }

    // Accumulate safepoint records. Stage 1 writes placeholder records
    // (see StackMapBuilder's doc comment).
    let mut stackmap = StackMapBuilder::new();

    // Define string-literal data objects: one DataId per literal, payload
    // is the raw UTF-8 bytes with no header.
    let mut lit_ids = Vec::new();
    for (idx, (_span, s)) in string_literals.iter().enumerate() {
        let name = format!("sigil_str_lit_{idx}");
        let id = module
            .declare_data(&name, Linkage::Local, false, false)
            .map_err(|e| format!("declare {name}: {e}"))?;
        let mut data = DataDescription::new();
        data.define(s.as_bytes().to_vec().into_boxed_slice());
        data.set_segment_section(".rodata", &name);
        module
            .define_data(id, &data)
            .map_err(|e| format!("define {name}: {e}"))?;
        lit_ids.push(id);
    }

    // Plan A2: static null-terminated C strings for the arith-error
    // reasons. `sigil_panic_arith_error` consumes them as `*const
    // c_char`. The trailing `\0` is included in the data payload.
    //
    // Symbol names describe the content for the linker's symbol table;
    // Mach-O section names (third arg) are capped at 16 chars and use
    // abbreviated forms. Keep the two independent.
    let div_zero_msg_id = declare_cstring(
        &mut module,
        "sigil_arith_msg_div_zero",
        "_sigil_amsg_dz",
        b"division by zero",
    )?;
    let mod_zero_msg_id = declare_cstring(
        &mut module,
        "sigil_arith_msg_mod_zero",
        "_sigil_amsg_mz",
        b"remainder by zero",
    )?;

    // --- define every user fn (original + synthetic $lambda_N) ----------
    //
    // Each fn gets its own Lowerer instance. The user-fn registry
    // (`user_fns`) and the per-fn FuncRefs are rebuilt for each
    // FunctionBuilder because `declare_func_in_func` returns a FuncRef
    // scoped to the function being defined.
    let mut ctx = module.make_context();
    let mut fb_ctx = FunctionBuilderContext::new();
    for item in &checked.program.items {
        let f = match item {
            crate::ast::Item::Fn(f) => f.as_ref(),
            _ => continue,
        };
        let entry = &user_fns[&f.name];

        ctx.func.signature = entry.signature.clone();
        ctx.func.name = UserFuncName::user(0, entry.func_id.as_u32());
        {
            let mut builder = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
            let block = builder.create_block();
            builder.append_block_params_for_function_params(block);
            builder.switch_to_block(block);
            builder.seal_block(block);

            // Runtime FuncRefs for this fn's definition.
            let string_new_ref = module.declare_func_in_func(string_new, builder.func);
            let println_ref = module.declare_func_in_func(println, builder.func);
            let panic_arith_ref = module.declare_func_in_func(panic_arith, builder.func);
            let alloc_ref = module.declare_func_in_func(alloc, builder.func);
            let int_to_string_ref = module.declare_func_in_func(int_to_string, builder.func);
            // Plan B Task 55 (Phase 3a) — handler-frame ABI refs.
            let handler_frame_new_ref =
                module.declare_func_in_func(handler_frame_new, builder.func);
            let handle_push_ref = module.declare_func_in_func(handle_push, builder.func);
            let handle_pop_ref = module.declare_func_in_func(handle_pop, builder.func);
            // Plan B Task 55 (Phase 3b) — frame_set_arm + perform + run_loop.
            let handler_frame_set_arm_ref =
                module.declare_func_in_func(handler_frame_set_arm, builder.func);
            let perform_ref = module.declare_func_in_func(perform_func, builder.func);
            let run_loop_ref = module.declare_func_in_func(run_loop, builder.func);
            // Plan B Task 55 (Phase 4d) — `sigil_continuation_identity`
            // ref. User-fn-side perform sites pass this as the
            // `k_fn_ptr` arg; arm-fn-side tail-k lowering uses
            // `next_step_call` / `next_step_args_ptr` (declared at
            // the synth-pass site in the loop below; user fns don't
            // emit those calls themselves).
            let continuation_identity_ref =
                module.declare_func_in_func(continuation_identity, builder.func);
            // Per-handle synthetic arm fn refs, keyed by handle span.
            // Each entry maps a handle's span to the per-arm FuncRefs
            // used for `func_addr` when populating the runtime
            // `HandlerFrame`'s arm slot.
            let handler_arm_refs_per_handle: BTreeMap<Span, Vec<FuncRef>> = handler_arm_indices
                .iter()
                .map(|(span, idx_vec)| {
                    let refs: Vec<FuncRef> = idx_vec
                        .iter()
                        .map(|&i| {
                            module.declare_func_in_func(handler_arm_synth[i].func_id, builder.func)
                        })
                        .collect();
                    (span.clone(), refs)
                })
                .collect();

            // FuncRefs for every user fn — needed for direct calls
            // (`Expr::Call` with `Ident` callee) and for `func_addr`
            // when a `ClosureRecord` stores the synthetic fn's address.
            let user_fn_refs: BTreeMap<String, FuncRef> = user_fns
                .iter()
                .map(|(name, uf)| {
                    (
                        name.clone(),
                        module.declare_func_in_func(uf.func_id, builder.func),
                    )
                })
                .collect();

            // String-literal GVs + lengths keyed by source span. Closure
            // conversion can reorder the walk relative to typecheck's
            // source-order list, so positional cursoring is unsafe once
            // multiple fns are compiled. Span-keyed linear-search is
            // O(small) and robust.
            let lit_gvs: Vec<(Span, GlobalValue, usize)> = string_literals
                .iter()
                .enumerate()
                .map(|(idx, (span, s))| {
                    let gv = module.declare_data_in_func(lit_ids[idx], builder.func);
                    (span.clone(), gv, s.len())
                })
                .collect();
            let div_zero_gv = module.declare_data_in_func(div_zero_msg_id, builder.func);
            let mod_zero_gv = module.declare_data_in_func(mod_zero_msg_id, builder.func);

            // Seed the per-fn env with user params. Block param 0 is the
            // closure_ptr; user params follow.
            let block_params: Vec<Value> = builder.block_params(block).to_vec();
            let closure_ptr = block_params[0];
            let mut env = BTreeMap::new();
            for (i, p) in f.params.iter().enumerate() {
                env.insert(p.name.clone(), block_params[i + 1]);
            }

            let is_main = f.name == "main";
            let mut lowerer = Lowerer {
                builder,
                stackmap: &mut stackmap,
                env,
                pointer_ty,
                closure_ptr,
                lit_gvs,
                div_zero_gv,
                mod_zero_gv,
                string_new_ref,
                println_ref,
                panic_arith_ref,
                alloc_ref,
                int_to_string_ref,
                handler_frame_new_ref,
                handle_push_ref,
                handle_pop_ref,
                handler_frame_set_arm_ref,
                perform_ref,
                run_loop_ref,
                handler_arm_refs_per_handle,
                handler_arm_synth: &handler_arm_synth,
                handler_arm_indices: &handler_arm_indices,
                continuation_identity_ref,
                effect_ids: &checked.effect_ids,
                op_ids: &checked.op_ids,
                effects: &checked.effects,
                user_fn_refs,
                user_fns: &user_fns,
                type_layouts: &type_layouts,
                ctor_index: &ctor_index,
                match_scrut_tys: &checked.match_scrut_tys,
            };

            let tail_val = lowerer.lower_block(&f.body);

            // main tags its Int return with `ishl_imm TAG_INT_SHIFT`
            // so the C-main shim can `sshr_imm` → i32. Other user fns
            // return their raw Cranelift value; callers (user code)
            // use it directly.
            //
            // Invariant: main's return type is always `Int` — the
            // typechecker rejects any other signature via E0041
            // ("fn main has wrong signature"). See QUESTIONS.md
            // [PLAN-A3] main-return-tagging, resolved 2026-04-24 as
            // option (a): the main→Int constraint is structural, so
            // this unconditional shift is safe. Any future relaxation
            // of main's type surface must revise this site alongside.
            //
            // Plan B A3-carryover decision: the broader tagged-vs-raw
            // Int ABI question logged in PLAN_B_DEVIATIONS resolved to
            // "raw i64 within user code, tag at the C-ABI boundary" —
            // this is the C-ABI boundary. `TAG_INT_SHIFT` centralises
            // the shift amount so any future ABI revision edits one
            // constant in `sigil-abi` rather than hunting inline
            // literals.
            let ret_val = match tail_val {
                Some(v) if is_main => lowerer.builder.ins().ishl_imm(v, i64::from(TAG_INT_SHIFT)),
                Some(v) => v,
                // No tail → Unit. Return a zero of the expected Cranelift
                // type. For main, ret_ty is i64 (tagged); `iconst(I64, 0)`
                // represents tagged-Int zero.
                None => lowerer.builder.ins().iconst(entry.ret_ty, 0),
            };
            lowerer.builder.ins().return_(&[ret_val]);
            lowerer.builder.finalize();
        }
        module
            .define_function(entry.func_id, &mut ctx)
            .map_err(|e| format!("define {}: {e}", f.name))?;
        module.clear_context(&mut ctx);
    }

    // --- main shim -------------------------------------------------------
    ctx.func.signature = main_sig.clone();
    ctx.func.name = UserFuncName::user(0, main.as_u32());
    {
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
        let block = builder.create_block();
        builder.append_block_params_for_function_params(block);
        builder.switch_to_block(block);
        builder.seal_block(block);

        let gc_init_ref = module.declare_func_in_func(gc_init, builder.func);
        let user_main_ref = module.declare_func_in_func(user_main, builder.func);
        let init_call = builder.ins().call(gc_init_ref, &[]);
        stackmap.push_placeholder(function_code_offset(&builder, init_call));
        // user-main takes the closure-calling-convention closure_ptr as
        // arg 0. The shim is not a closure entry point, so it passes a
        // null pointer; main's body never reads it.
        let null_closure = builder.ins().iconst(pointer_ty, 0);
        let um_call = builder.ins().call(user_main_ref, &[null_closure]);
        stackmap.push_placeholder(function_code_offset(&builder, um_call));

        // user-main returns a tagged Int; untag to i32 via arithmetic
        // right-shift and narrow. Overflow beyond i32 is not observable in
        // v1 (main returns Int, and hello-world returns 0). The shift
        // amount is `TAG_INT_SHIFT` — paired with the `ishl_imm` in the
        // user-main return path above; both sites reference the same
        // `sigil-abi` constant so they cannot drift.
        let tagged = builder.inst_results(um_call)[0];
        let untagged = builder.ins().sshr_imm(tagged, i64::from(TAG_INT_SHIFT));
        let narrowed = builder.ins().ireduce(types::I32, untagged);
        builder.ins().return_(&[narrowed]);
        builder.finalize();
    }
    module
        .define_function(main, &mut ctx)
        .map_err(|e| format!("define main: {e}"))?;
    module.clear_context(&mut ctx);

    // --- Plan B Task 55 (Phase 4c): synthetic handler-arm CPS fns ------
    //
    // Each entry in `handler_arm_synth` was allocated a `FuncId` by
    // the pre-pass; here we define each fn's body. Every arm fn has
    // the uniform CPS calling convention `extern "C" fn(closure_ptr,
    // args_ptr, args_len) -> *mut NextStep`.
    //
    // Phase 4c lifts the Phase 3b "IntLit-only arm body" restriction:
    // bodies are now lowered through a real `Lowerer` instance with
    // op-args bound from `args_ptr` at fn entry. The walker
    // (`unsupported_handle_construct`) still enforces the remaining
    // Phase 4c restrictions (no `k` use, no outer-scope captures, no
    // nested `Lambda` / `ClosureRecord`) so the synthetic fn never
    // needs a non-null `closure_ptr` and the env stays bounded by the
    // op-args.
    //
    // The body's lowered Cranelift `Value` is widened to I64 via
    // `uextend` if narrower (matching `sigil_next_step_done`'s I64
    // signature) before the wrap call; `lower_perform_non_io_to_value`
    // mirror-narrows on the perform side so the perform's
    // `type_of_expr` (the op's declared return type) and the actual
    // lowered Cranelift `Value` agree.
    {
        let cps_arm_sig = {
            let mut s = Signature::new(isa_call_conv(&module));
            s.params.push(AbiParam::new(pointer_ty)); // closure_ptr
            s.params.push(AbiParam::new(pointer_ty)); // args_ptr
            s.params.push(AbiParam::new(types::I32)); // args_len
            s.returns.push(AbiParam::new(pointer_ty)); // *mut NextStep
            s
        };
        for synth in &handler_arm_synth {
            ctx.func.signature = cps_arm_sig.clone();
            ctx.func.name = UserFuncName::user(0, synth.func_id.as_u32());
            {
                let mut builder = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
                let block = builder.create_block();
                builder.append_block_params_for_function_params(block);
                builder.switch_to_block(block);
                builder.seal_block(block);

                // Per-arm-fn FFI refs. Same shape as the user-fn
                // loop above — each `module.declare_func_in_func`
                // returns a `FuncRef` scoped to this fn's builder,
                // so we have to redeclare per-fn (cannot reuse the
                // user-fn loop's FuncRefs across function bodies).
                let string_new_ref = module.declare_func_in_func(string_new, builder.func);
                let println_ref = module.declare_func_in_func(println, builder.func);
                let panic_arith_ref = module.declare_func_in_func(panic_arith, builder.func);
                let alloc_ref = module.declare_func_in_func(alloc, builder.func);
                let int_to_string_ref = module.declare_func_in_func(int_to_string, builder.func);
                let handler_frame_new_ref =
                    module.declare_func_in_func(handler_frame_new, builder.func);
                let handle_push_ref = module.declare_func_in_func(handle_push, builder.func);
                let handle_pop_ref = module.declare_func_in_func(handle_pop, builder.func);
                let handler_frame_set_arm_ref =
                    module.declare_func_in_func(handler_frame_set_arm, builder.func);
                let perform_ref = module.declare_func_in_func(perform_func, builder.func);
                let run_loop_ref = module.declare_func_in_func(run_loop, builder.func);
                let next_step_done_ref = module.declare_func_in_func(next_step_done, builder.func);
                // Plan B Task 55 (Phase 4d) — tail-k lowering refs.
                let next_step_call_ref = module.declare_func_in_func(next_step_call, builder.func);
                let next_step_args_ptr_ref =
                    module.declare_func_in_func(next_step_args_ptr, builder.func);
                let continuation_identity_ref =
                    module.declare_func_in_func(continuation_identity, builder.func);

                // Per-handle synthetic arm-fn refs, keyed by handle
                // span. Phase 4c walker rejects nested Handle inside
                // arm bodies via the capture / lambda gates (a nested
                // handle's body would generally need access to the
                // outer arm's bindings to be useful), but we still
                // build the map defensively so a simple constant-
                // bodied nested handle doesn't accidentally crash if
                // the walker's gates are loosened in a future phase.
                let handler_arm_refs_per_handle: BTreeMap<Span, Vec<FuncRef>> = handler_arm_indices
                    .iter()
                    .map(|(span, idx_vec)| {
                        let refs: Vec<FuncRef> = idx_vec
                            .iter()
                            .map(|&i| {
                                module.declare_func_in_func(
                                    handler_arm_synth[i].func_id,
                                    builder.func,
                                )
                            })
                            .collect();
                        (span.clone(), refs)
                    })
                    .collect();

                let user_fn_refs: BTreeMap<String, FuncRef> = user_fns
                    .iter()
                    .map(|(name, uf)| {
                        (
                            name.clone(),
                            module.declare_func_in_func(uf.func_id, builder.func),
                        )
                    })
                    .collect();

                let lit_gvs: Vec<(Span, GlobalValue, usize)> = string_literals
                    .iter()
                    .enumerate()
                    .map(|(idx, (span, s))| {
                        let gv = module.declare_data_in_func(lit_ids[idx], builder.func);
                        (span.clone(), gv, s.len())
                    })
                    .collect();
                let div_zero_gv = module.declare_data_in_func(div_zero_msg_id, builder.func);
                let mod_zero_gv = module.declare_data_in_func(mod_zero_msg_id, builder.func);

                // Block params: 0 = closure_ptr (null in Phase 4c),
                // 1 = args_ptr, 2 = args_len (unused — walker
                // enforces arity through typecheck E0141).
                let block_params: Vec<Value> = builder.block_params(block).to_vec();
                let closure_ptr = block_params[0];
                let args_ptr = block_params[1];
                let _args_len = block_params[2];

                // Unpack op-args from `args_ptr` and bind them in
                // the env. Each slot is a u64; narrower declared
                // types (I8 Bool/Byte/Unit, I32 Char) get `ireduce`'d
                // back, mirroring the `uextend` widening in
                // `lower_perform_non_io_to_value`.
                let mut env: BTreeMap<String, Value> = BTreeMap::new();
                for (i, (name, declared_ty)) in synth
                    .arg_names
                    .iter()
                    .zip(synth.arg_types.iter())
                    .enumerate()
                {
                    let widened = builder.ins().load(
                        types::I64,
                        MemFlags::trusted(),
                        args_ptr,
                        (i * 8) as i32,
                    );
                    let value = if *declared_ty == types::I64 {
                        widened
                    } else if declared_ty.is_int() && declared_ty.bits() < 64 {
                        builder.ins().ireduce(*declared_ty, widened)
                    } else {
                        // pointer_ty (String, user-type pointers):
                        // already I64 on every supported target. A
                        // future float / 32-bit-target port would
                        // need an ireduce or bitcast branch here.
                        // `assert_eq!` (not `debug_assert_eq!`) so a
                        // future floats addition or 32-bit-target
                        // port that smuggles a non-pointer-width
                        // value through this path panics in *both*
                        // dev and release builds — symmetric with
                        // `lower_perform_non_io_to_value`'s
                        // perform-side widening fallthrough at
                        // codegen.rs:2542 per the deviation entry's
                        // "mirror" hardening discipline.
                        assert_eq!(
                            *declared_ty, pointer_ty,
                            "codegen Phase 4c: unexpected op-arg Cranelift type \
                             {declared_ty:?} unpacking from args_ptr in arm fn"
                        );
                        widened
                    };
                    env.insert(name.clone(), value);
                }

                // Plan B Task 55 (Phase 4d): load `k_closure` and
                // `k_fn` from the args buffer at positions [N], [N+1]
                // where N = arg_names.len(). The runtime appends
                // these two pointer-width slots after the user args
                // (per the `[DEVIATION Task 56]` uniform CPS calling
                // convention entry). The synth-pass tail-`k` lowering
                // below uses these as the `closure_ptr` / `fn_ptr`
                // args to `sigil_next_step_call` when the arm body's
                // tail expression is `k(arg)`.
                let n_user_args = synth.arg_names.len();
                let k_closure_v = builder.ins().load(
                    pointer_ty,
                    MemFlags::trusted(),
                    args_ptr,
                    (n_user_args * 8) as i32,
                );
                let k_fn_v = builder.ins().load(
                    pointer_ty,
                    MemFlags::trusted(),
                    args_ptr,
                    ((n_user_args + 1) * 8) as i32,
                );

                let mut lowerer = Lowerer {
                    builder,
                    stackmap: &mut stackmap,
                    env,
                    pointer_ty,
                    closure_ptr,
                    lit_gvs,
                    div_zero_gv,
                    mod_zero_gv,
                    string_new_ref,
                    println_ref,
                    panic_arith_ref,
                    alloc_ref,
                    int_to_string_ref,
                    handler_frame_new_ref,
                    handle_push_ref,
                    handle_pop_ref,
                    handler_frame_set_arm_ref,
                    perform_ref,
                    run_loop_ref,
                    handler_arm_refs_per_handle,
                    handler_arm_synth: &handler_arm_synth,
                    handler_arm_indices: &handler_arm_indices,
                    continuation_identity_ref,
                    effect_ids: &checked.effect_ids,
                    op_ids: &checked.op_ids,
                    effects: &checked.effects,
                    user_fn_refs,
                    user_fns: &user_fns,
                    type_layouts: &type_layouts,
                    ctor_index: &ctor_index,
                    match_scrut_tys: &checked.match_scrut_tys,
                };

                // Plan B Task 55 (Phase 4d): route between the two
                // arm-body lowering paths based on the body's tail
                // shape:
                //
                //   - **Tail-`k(arg)` path**: lower any pre-tail
                //     stmts, lower the arg in non-tail position,
                //     widen to I64, build `NextStep::Call(k_closure,
                //     k_fn=identity-by-default, /*arg_count=*/1)`,
                //     write the widened arg into the args buffer at
                //     slot 0, return the NextStep pointer. The
                //     surrounding native fn's `sigil_run_loop`
                //     dispatches the Call into
                //     `sigil_continuation_identity`, which returns
                //     `Done(arg)`; `run_loop` returns the value to
                //     the perform site.
                //
                //   - **Non-tail / no-`k` path**: existing Phase 4c
                //     flow — lower body via `lower_expr`, widen to
                //     I64 (matching `sigil_next_step_done`'s
                //     signature), build `NextStep::Done(value)`,
                //     return the NextStep pointer.
                //
                // The walker (`arm_body_unsupported_construct`)
                // enforces that any `k`-call inside the body is in
                // the tail position recognised by
                // `arm_body_tail_is_k_call`; non-tail `k` use is
                // rejected with a Phase-4e-pointing diagnostic.
                let next_step_ptr =
                    if let Some(arg_expr) = arm_body_tail_is_k_call(&synth.body, &synth.k_name) {
                        // --- Tail-`k(arg)` path ---
                        lowerer.lower_arm_body_pre_tail_k_stmts(&synth.body);
                        let arg_value = lowerer.lower_expr(arg_expr);
                        let arg_ty = lowerer.builder.func.dfg.value_type(arg_value);
                        let widened_arg = if arg_ty == types::I64 {
                            arg_value
                        } else if arg_ty.is_int() && arg_ty.bits() < 64 {
                            lowerer.builder.ins().uextend(types::I64, arg_value)
                        } else {
                            assert_eq!(
                                arg_ty, pointer_ty,
                                "codegen Phase 4d: unexpected k-arg Cranelift type \
                             {arg_ty:?} for sigil_next_step_call slot widen — \
                             Phase 4d MVP supports I64 (Int), I32 (Char), I8 \
                             (Bool/Byte/Unit), and pointer_ty (String / \
                             user-type pointers); floats and 32-bit-target \
                             pointer types need a dedicated branch"
                            );
                            arg_value
                        };
                        let one_v = lowerer.builder.ins().iconst(types::I32, 1);
                        let call_ns = lowerer
                            .builder
                            .ins()
                            .call(next_step_call_ref, &[k_closure_v, k_fn_v, one_v]);
                        lowerer
                            .stackmap
                            .push_placeholder(function_code_offset(&lowerer.builder, call_ns));
                        let ns_ptr = lowerer.builder.inst_results(call_ns)[0];
                        let argp_call = lowerer
                            .builder
                            .ins()
                            .call(next_step_args_ptr_ref, &[ns_ptr]);
                        lowerer
                            .stackmap
                            .push_placeholder(function_code_offset(&lowerer.builder, argp_call));
                        let argp_v = lowerer.builder.inst_results(argp_call)[0];
                        lowerer
                            .builder
                            .ins()
                            .store(MemFlags::trusted(), widened_arg, argp_v, 0);
                        ns_ptr
                    } else {
                        // --- Non-tail / no-`k` path (Phase 4c shape) ---
                        let body_value = lowerer.lower_expr(&synth.body);
                        let widened_body = if synth.body_ty == types::I64 {
                            body_value
                        } else if synth.body_ty.is_int() && synth.body_ty.bits() < 64 {
                            lowerer.builder.ins().uextend(types::I64, body_value)
                        } else {
                            assert_eq!(
                                synth.body_ty, pointer_ty,
                                "codegen Phase 4c: unexpected arm-body Cranelift type \
                             {:?} for sigil_next_step_done wrap",
                                synth.body_ty
                            );
                            body_value
                        };
                        let done_call = lowerer
                            .builder
                            .ins()
                            .call(next_step_done_ref, &[widened_body]);
                        // Stackmap entry: any pointer-typed op-args bound in
                        // env (String / user-type) are GC roots live across
                        // this `sigil_next_step_done` arena allocation.
                        lowerer
                            .stackmap
                            .push_placeholder(function_code_offset(&lowerer.builder, done_call));
                        lowerer.builder.inst_results(done_call)[0]
                    };
                lowerer.builder.ins().return_(&[next_step_ptr]);
                lowerer.builder.finalize();
            }
            module
                .define_function(synth.func_id, &mut ctx)
                .map_err(|e| format!("define handler arm fn: {e}"))?;
            module.clear_context(&mut ctx);
        }
    }

    // --- finish and add the stackmap section ----------------------------
    let mut product = module.finish();

    let section_bytes = stackmap.serialize();
    let is_macho = matches!(product.object.format(), BinaryFormat::MachO);
    let (segment_bytes, section_name): (&[u8], &[u8]) = if is_macho {
        (b"__SIGIL", b"__stackmaps")
    } else {
        // ELF: segment ignored, section name is the .section directive.
        (b"", b".sigil_stackmaps")
    };
    let section_id = product.object.add_section(
        segment_bytes.to_vec(),
        section_name.to_vec(),
        SectionKind::ReadOnlyData,
    );
    {
        let section = product.object.section_mut(section_id);
        section.set_data(section_bytes, 8);
    }

    let bytes = product.emit().map_err(|e| format!("object emit: {e}"))?;
    std::fs::write(out_path, bytes).map_err(|e| format!("write {}: {}", out_path.display(), e))?;
    Ok(())
}

/// Declare and define a null-terminated C string as a module-local
/// read-only data object. Returns the cstring's `DataId` so callers can
/// `declare_data_in_func` it and derive a `symbol_value` pointer at
/// codegen time. The terminating `\0` is appended here; callers pass
/// the raw bytes.
///
/// `symbol_name` identifies the data in the object's symbol table and
/// can be as long as the linker allows (hundreds of chars). `section`
/// names the output section containing the bytes; Mach-O caps section
/// names at **16 characters** (including the NUL), so the two must be
/// provided separately rather than sharing a name. ELF accepts either.
fn declare_cstring(
    module: &mut ObjectModule,
    symbol_name: &str,
    section: &str,
    bytes: &[u8],
) -> Result<cranelift_module::DataId, String> {
    debug_assert!(
        section.len() < 16,
        "Mach-O section name `{section}` exceeds the 16-char limit"
    );
    let id = module
        .declare_data(symbol_name, Linkage::Local, false, false)
        .map_err(|e| format!("declare {symbol_name}: {e}"))?;
    let mut payload = Vec::with_capacity(bytes.len() + 1);
    payload.extend_from_slice(bytes);
    payload.push(0);
    let mut data = DataDescription::new();
    data.define(payload.into_boxed_slice());
    data.set_segment_section(".rodata", section);
    module
        .define_data(id, &data)
        .map_err(|e| format!("define {symbol_name}: {e}"))?;
    Ok(id)
}

/// Tree-walking lowerer — plan A2 task 24.
///
/// Walks a typechecked + elaborated AST and emits Cranelift IR into an
/// in-flight `FunctionBuilder`. The `Lowerer` owns the builder (moved
/// in) and holds short-lived references to the shared stackmap
/// accumulator and pre-declared module-level refs. It does **not** own
/// the `ObjectModule` — all GVs / FuncRefs the lowerer needs are
/// declared before the lowerer is constructed, so the walk stays free
/// of aliasing concerns with the module.
///
/// # Cranelift type mapping
///
/// | Sigil type | Cranelift IR type |
/// |------------|-------------------|
/// | `Int`      | `i64`             |
/// | `Bool`     | `i8` (0 / 1)      |
/// | `Char`     | `i32`             |
/// | `Byte`     | `i8` (0..255)     |
/// | `String`   | `pointer_ty` (heap header ptr) |
/// | `Unit`     | `i8` (always 0 — placeholder rep) |
///
/// # Environment
///
/// Resolve rejects shadowing, so the per-function environment is flat:
/// a single `BTreeMap<String, Value>`. Elaborate's synthetic `$elab_tN`
/// names are globally unique across a program, so they coexist with
/// user-authored names without collision.
///
/// # Int tagging
///
/// Arithmetic is performed on native `i64` values. Tagging to the
/// `(n << 1)` Sigil `Value` encoding happens exactly once per user-
/// function, at the `return` site. This keeps iadd/isub/imul/sdiv/srem
/// emissions unadorned and lets Cranelift's peephole optimiser see
/// arithmetic as plain 64-bit integer math.
struct Lowerer<'a, 'b> {
    builder: FunctionBuilder<'a>,
    stackmap: &'a mut StackMapBuilder,
    env: BTreeMap<String, Value>,
    pointer_ty: Type,

    /// Arg-0 of the current fn's entry block: the closure record
    /// pointer under the closure calling convention (plan A2 task 32).
    /// Direct callers pass null; `ClosureRecord`-returning callees
    /// pass the allocated record's header pointer. `ClosureEnvLoad`
    /// lowers a load against `closure_ptr + 16 + 8 * index` (past the
    /// 8-byte header and the 8-byte code_ptr word).
    closure_ptr: Value,

    /// Per-string-literal `(span, GV, byte-length)` tuples declared at
    /// fn-entry time. Span-keyed so closure-conversion reordering of
    /// the walk (hoisted `$lambda_N` bodies carry the string literals
    /// that originally lived inside their lambda expressions) doesn't
    /// desynchronise the lookup from typecheck's source-order list.
    lit_gvs: Vec<(Span, GlobalValue, usize)>,

    /// `declare_data_in_func` refs for the arith-panic cstrings.
    div_zero_gv: GlobalValue,
    mod_zero_gv: GlobalValue,

    string_new_ref: FuncRef,
    println_ref: FuncRef,
    panic_arith_ref: FuncRef,
    alloc_ref: FuncRef,

    /// Runtime ref for `sigil_int_to_string(i64) -> *u8`. Plan A2 task
    /// 34 wires the language builtin `int_to_string(Int) -> String !`
    /// to this symbol; `lower_call` dispatches to it when the callee is
    /// `Ident("int_to_string")` and no user fn of the same name
    /// shadows it.
    int_to_string_ref: FuncRef,

    /// Plan B Task 55 (Phase 3a) — handler-frame ABI runtime refs
    /// from Task 56. `lower_expr` for `Expr::Handle` calls
    /// `sigil_handler_frame_new(effect_id, arm_count)`, then in
    /// Phase 3b emits one `sigil_handler_frame_set_arm(frame, op_id,
    /// fn_ptr, null_closure)` per arm with the synthetic arm fn's
    /// pointer (via `func_addr`), then `sigil_handle_push(frame)`
    /// before the body, and `sigil_handle_pop()` after.
    handler_frame_new_ref: FuncRef,
    handle_push_ref: FuncRef,
    handle_pop_ref: FuncRef,
    /// Plan B Task 55 (Phase 3b) — `sigil_handler_frame_set_arm`
    /// runtime ref. Called once per arm during `Expr::Handle`
    /// lowering with `(frame, op_id, fn_ptr, null_closure_ptr)`.
    handler_frame_set_arm_ref: FuncRef,
    /// Plan B Task 55 (Phase 3b) — `sigil_perform` runtime ref.
    /// `lower_perform_non_io_to_value` calls this for non-IO
    /// effects; the result is a `*mut NextStep` of tag `CALL`
    /// pointing at the matching arm fn. The Call NextStep is then
    /// passed to `sigil_run_loop` (`run_loop_ref`) which dispatches
    /// it (invokes the arm fn with packed args) and returns the
    /// final `Done` value as u64.
    perform_ref: FuncRef,
    /// Plan B Task 55 (Phase 3b) — `sigil_run_loop` runtime ref.
    /// Called by `lower_perform_non_io_to_value` to drive the CPS
    /// trampoline from the `NextStep::Call` returned by
    /// `sigil_perform`. Returns the final `NextStep::Done`'s value
    /// as u64; native code uses this directly.
    run_loop_ref: FuncRef,
    /// Plan B Task 55 (Phase 3b) — per-handle-span synthetic arm fn
    /// refs. Used by `Expr::Handle` codegen to emit `func_addr`
    /// pointers for `sigil_handler_frame_set_arm`. Keyed by the
    /// handle expression's span; each entry is a `Vec<FuncRef>` in
    /// arm-declaration order.
    handler_arm_refs_per_handle: BTreeMap<Span, Vec<FuncRef>>,
    /// Plan B Task 55 (Phase 4d) — global `HandlerArmSynth` slice from
    /// the codegen pre-pass. Used by `Expr::Handle` codegen to look
    /// up each arm's `captures` list (parallel to the arm's slot in
    /// the runtime closure record) and build env_exprs at
    /// frame-setup time. Indexed via `handler_arm_indices`.
    handler_arm_synth: &'b [HandlerArmSynth],
    /// Plan B Task 55 (Phase 4d) — per-handle-span list of arm
    /// indices into `handler_arm_synth`. Mirrors the keys of
    /// `handler_arm_refs_per_handle`. Used to walk an
    /// `Expr::Handle`'s arms without re-walking the AST.
    handler_arm_indices: &'b BTreeMap<Span, Vec<usize>>,
    /// Plan B Task 55 (Phase 4d) — `sigil_continuation_identity`
    /// runtime intrinsic. `lower_perform_non_io_to_value` emits
    /// `func_addr(continuation_identity_ref)` as the `k_fn_ptr` arg
    /// to every non-IO `sigil_perform` call site so a tail-`k(arg)`
    /// arm body's `sigil_next_step_call` dispatches into the
    /// identity continuation, producing a terminal `Done(arg)`.
    continuation_identity_ref: FuncRef,
    /// Plan B Task 55 (Phase 3a) — effect-name → effect_id (u32) map
    /// from typecheck. `Expr::Handle` codegen looks up the handle's
    /// declared effect (the unique effect name in its arms; Phase 3a
    /// only supports single-effect handles) to pass as the
    /// `effect_id` arg to `sigil_handler_frame_new`.
    effect_ids: &'b BTreeMap<String, u32>,
    /// Plan B Task 55 (Phase 3b) — `(effect_name, op_name)` → op_id
    /// (u32) map from typecheck. `lower_perform` for non-IO effects
    /// looks up the op_id to pass as the second arg to
    /// `sigil_perform`.
    op_ids: &'b BTreeMap<(String, String), u32>,
    /// Plan B Task 55 (Phase 3b) — effect declaration registry from
    /// typecheck. Used by `type_of_expr` for `Expr::Perform` to
    /// look up the op's return type, which determines the Cranelift
    /// type of the perform expression's value (extracted from the
    /// returned NextStep).
    effects: &'b BTreeMap<String, crate::ast::EffectDecl>,

    /// Per-fn FuncRefs for every user fn (original + synthetic
    /// `$lambda_N`). Used for direct calls and for `func_addr` when
    /// populating a `ClosureRecord`'s `code_ptr` slot.
    user_fn_refs: BTreeMap<String, FuncRef>,

    /// Shared user-fn registry (FuncId + signature). Immutable over a
    /// fn's lowering — the Lowerer only reads return types and param
    /// types to size signature lookups.
    user_fns: &'b BTreeMap<String, UserFnEntry>,

    /// Plan A3 task 40 layout descriptors for every user-defined
    /// type in the program. Keyed by type name. Codegen reads the
    /// per-variant tag, payload word count, pointer bitmap, and
    /// field types to emit constructor allocations (task 41.1) and
    /// match decision trees (task 41.2). Built once at `emit_object`
    /// entry.
    type_layouts: &'b BTreeMap<String, crate::layout::TypeLayout>,

    /// Constructor-name → (type_name, variant_index) index rebuilt
    /// from `type_layouts` (Plan A3 task 40). Used to recognise
    /// bare `Expr::Ident(ctor)`, `Expr::Call { callee: Ident(ctor), .. }`,
    /// and `Expr::RecordLit` sites in lowering; look-up resolves in
    /// O(log n).
    ctor_index: &'b BTreeMap<String, (String, usize)>,

    /// Plan A3 task 41.2 — per-match scrutinee types keyed by the match
    /// expression's span. `lower_match` uses this to disambiguate
    /// `Pattern::Var(name)` between a fresh binding and a nullary-ctor
    /// promotion. Synthetic matches produced by elaborate's if→match
    /// desugaring are absent from this map; `lower_match` falls back
    /// to primitive-scalar dispatch in that case (which is correct
    /// because if-desugar only emits `Pattern::BoolLit` arms).
    match_scrut_tys: &'b BTreeMap<Span, Ty>,
}

impl<'a, 'b> Lowerer<'a, 'b> {
    /// Lower a `Block`. Returns the tail expression's value if any,
    /// `None` when the block has no tail (statement-only block, value
    /// is `Unit`).
    fn lower_block(&mut self, b: &crate::ast::Block) -> Option<Value> {
        for s in &b.stmts {
            self.lower_stmt(s);
        }
        b.tail.as_ref().map(|t| self.lower_expr(t))
    }

    /// Plan B Task 55 (Phase 4d) — lower the prefix statements of a
    /// tail-`k` arm body. Walks `Expr::Block`-wrapped layers, lowering
    /// each block's stmts, recursing through the tail. Stops at the
    /// non-Block leaf (the `k(arg)` call) — the synth pass lowers the
    /// arg explicitly afterward and emits `sigil_next_step_call`.
    /// Mirrors `arm_body_tail_is_k_call`'s recursion shape so the two
    /// stay in lockstep.
    fn lower_arm_body_pre_tail_k_stmts(&mut self, body: &crate::ast::Expr) {
        use crate::ast::Expr;
        if let Expr::Block(b) = body {
            for s in &b.stmts {
                self.lower_stmt(s);
            }
            if let Some(t) = &b.tail {
                self.lower_arm_body_pre_tail_k_stmts(t);
            }
        }
    }

    fn lower_stmt(&mut self, s: &crate::ast::Stmt) {
        use crate::ast::Stmt;
        match s {
            Stmt::Let(l) => {
                let v = self.lower_expr(&l.value);
                self.env.insert(l.name.clone(), v);
            }
            Stmt::Expr(e) => {
                let _ = self.lower_expr(e);
            }
            Stmt::Perform(p) => {
                // IO performs go through the side-effect-only path
                // (`lower_perform` returns no value); non-IO performs
                // route through `sigil_perform` and discard the
                // returned value, mirroring the dispatch in
                // `Expr::Perform`. Without this dispatch, statement-
                // form non-IO performs (`perform Raise.fail();`) hit
                // the IO assertion in `lower_perform` and crash the
                // compiler.
                if p.effect == "IO" {
                    self.lower_perform(p);
                } else {
                    let _ = self.lower_perform_non_io_to_value(p);
                }
            }
        }
    }

    fn lower_perform(&mut self, p: &crate::ast::PerformExpr) {
        // Plan A2 only recognises `IO.println(String)`; typecheck
        // (E0042/E0043/E0044) rejects every other shape before codegen
        // sees the program, so we assume the happy path. Non-IO
        // performs are handled inline in `Expr::Perform`'s lowering
        // because they return a value (extracted from the NextStep
        // returned by `sigil_perform`); this `lower_perform` helper
        // remains the IO-only side-effect path.
        assert_eq!(
            p.effect, "IO",
            "non-IO effect reached lower_perform; \
                                    `Expr::Perform` should dispatch non-IO inline"
        );
        assert_eq!(p.op, "println", "non-println IO op reached codegen");
        assert_eq!(p.args.len(), 1, "IO.println arg count is not 1");

        let heap = self.lower_expr(&p.args[0]);
        let call = self.builder.ins().call(self.println_ref, &[heap]);
        self.stackmap
            .push_placeholder(function_code_offset(&self.builder, call));
    }

    /// Plan B Task 55 (Phase 4b) — lower a non-IO `perform Effect.op(args...)`
    /// site. Phase 4b adds args-buffer packing on the perform side: each
    /// user arg is lowered, widened to `u64` if narrower, and stored at
    /// offset `i*8` in a stack-allocated `[u64; args_len]` buffer. The
    /// buffer's address + length are passed to `sigil_perform`, which
    /// copies them into the dispatched `NextStep::Call`'s args slots
    /// before the arm fn runs.
    ///
    /// The continuation pair `(k_closure_ptr, k_fn_ptr)` stays null in
    /// Phase 4b — Phase 4d reifies the perform's continuation. The
    /// codegen-entry guard still rejects arms that reference `k`.
    ///
    /// `sigil_perform` returns a `NextStep::Call` that targets the
    /// matching arm fn; we hand that off to `sigil_run_loop`, which
    /// drives the CPS trampoline to a terminal `NextStep::Done` and
    /// returns the value as u64. Native code uses the u64 directly
    /// (cast to i64 via the type system; for Int returns this is the
    /// raw untagged Int per the Plan B Int ABI).
    fn lower_perform_non_io_to_value(&mut self, p: &crate::ast::PerformExpr) -> Value {
        // Bound check (defense-in-depth — runtime's `sigil_perform`
        // is the source of truth and aborts with a named effect_id /
        // op_id message on `args_len + 2 > MAX_INLINE_ARGS`). The
        // compiler-side `debug_assert!` here catches the bug in dev
        // builds before linking; release builds let the runtime's
        // named guard fire so users get the better diagnostic. v1
        // ops use 0–2 user args; the cap (30 user args after
        // subtracting the two implicit `(k_closure, k_fn)` slots)
        // is a forward-compat boundary documented in the Task 56
        // MAX_INLINE_ARGS deviation.
        debug_assert!(
            (p.args.len() as u32).saturating_add(2) <= sigil_abi::effect::MAX_INLINE_ARGS,
            "codegen: non-IO perform `{}.{}` has {} user args, exceeding \
             MAX_INLINE_ARGS - 2 = {} (boxing path arrives in a future plan)",
            p.effect,
            p.op,
            p.args.len(),
            sigil_abi::effect::MAX_INLINE_ARGS - 2
        );
        let effect_id = match self.effect_ids.get(&p.effect) {
            Some(id) => *id,
            None => unreachable!(
                "codegen: effect `{}` missing from effect_ids map; \
                 typecheck-time E0042 should have caught this",
                p.effect
            ),
        };
        let op_id = match self.op_ids.get(&(p.effect.clone(), p.op.clone())) {
            Some(id) => *id,
            None => unreachable!(
                "codegen: op_id missing for `{}.{}`; typecheck-time \
                 E0043 should have caught this",
                p.effect, p.op
            ),
        };
        let effect_id_v = self.builder.ins().iconst(types::I32, effect_id as i64);
        let op_id_v = self.builder.ins().iconst(types::I32, op_id as i64);

        // Phase 4b — pack user args into a stack-allocated `[u64; N]`
        // buffer. The buffer lives in this fn's frame, which outlives
        // `sigil_perform` (the runtime copies args into the dispatched
        // `NextStep::Call`'s slots before returning), so a stack slot
        // is sound under Phase 4b's synchronous calling pattern. Each
        // arg is widened to u64 via `uextend` if its Cranelift type
        // is narrower than I64; pointer-typed args (already pointer-
        // width on supported targets) store directly.
        //
        // Empty-args case: `args_ptr` stays null, `args_len == 0`. The
        // runtime accepts a null `args_ptr` only when `args_len == 0`
        // (per the safety contract on `sigil_perform`).
        let (args_ptr, args_len_v) = if p.args.is_empty() {
            (
                self.builder.ins().iconst(self.pointer_ty, 0),
                self.builder.ins().iconst(types::I32, 0),
            )
        } else {
            // Lower each arg first so any side effects in the arg
            // expressions sequence before the slot stores in source
            // order. (This matches Cranelift's typical evaluation
            // order; explicit ordering keeps the intent clear.)
            let arg_values: Vec<Value> = p.args.iter().map(|a| self.lower_expr(a)).collect();

            // 8-byte slots; align_shift = 3 means 2^3 = 8-byte aligned,
            // matching the runtime's `args_ptr.add(i)` u64-stride
            // reads in `sigil_perform`.
            //
            // PHASE-4-RESTRICTION (Plan B Task 55, Phase 4d):
            // stack-slot allocation here is sound *only* under the
            // synchronous `lower_perform_non_io_to_value` →
            // `sigil_perform` → `sigil_run_loop` chain that returns
            // before this fn does (the runtime copies args into the
            // dispatched `NextStep::Call`'s arena slots before
            // returning, so the stack slot only needs to outlive the
            // synchronous call). Phase 4d converts perform sites to
            // return `NextStep::Call` to the caller's trampoline
            // rather than synchronously calling `sigil_run_loop` from
            // native code; at that point this stack slot dies before
            // the trampoline reads it on the next dispatch and the
            // args buffer must migrate to arena allocation via
            // `sigil_arena_alloc`. Closure point cross-references the
            // `[DEVIATION Task 55] Native callers drive sigil_run_loop
            // synchronously` entry. See also the `[DEVIATION Task 55]
            // Phase 4b — args-buffer packing on perform side` entry.
            let slot_bytes = (p.args.len() * 8) as u32;
            let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                slot_bytes,
                3,
            ));

            for (i, arg_v) in arg_values.into_iter().enumerate() {
                let arg_ty = self.builder.func.dfg.value_type(arg_v);
                let widened = if arg_ty == types::I64 {
                    arg_v
                } else if arg_ty.is_int() && arg_ty.bits() < 64 {
                    // I8 (Bool/Byte/Unit) and I32 (Char) widen by
                    // zero-extension. Sigil's surface Int type is
                    // already I64; smaller integer types are all
                    // unsigned-by-convention payload values, so
                    // `uextend` preserves the value bit-pattern the
                    // arm fn would observe under direct call.
                    self.builder.ins().uextend(types::I64, arg_v)
                } else {
                    // pointer_ty is I64 on every supported target
                    // (x86_64-linux, aarch64-darwin); v1 has no F32
                    // / F64 surface type. A future floats addition
                    // would silently miscompile through this fall-
                    // through (storing the bit-pattern as if it
                    // were a pointer-sized value), so we panic
                    // here in *both* dev and release builds rather
                    // than `debug_assert_eq!` — a cheap insurance
                    // policy until v2 either adds floats with an
                    // explicit branch or this assertion fires and
                    // forces the question. A future 32-bit target
                    // port would also need a uextend or bitcast
                    // branch here for pointer args.
                    assert_eq!(
                        arg_ty, self.pointer_ty,
                        "codegen: unexpected arg Cranelift type \
                         {arg_ty:?} for non-IO perform args buffer \
                         — Phase 4b only supports I64 (Int), I32 (Char), \
                         I8 (Bool/Byte/Unit), and pointer_ty (String / \
                         user-type pointers); floats and 32-bit-target \
                         pointer types need a dedicated branch"
                    );
                    arg_v
                };
                self.builder
                    .ins()
                    .stack_store(widened, slot, (i * 8) as i32);
            }
            (
                self.builder.ins().stack_addr(self.pointer_ty, slot, 0),
                self.builder.ins().iconst(types::I32, p.args.len() as i64),
            )
        };

        // Plan B Task 55 (Phase 4d): `k_fn_ptr` is the address of the
        // runtime intrinsic `sigil_continuation_identity`. When a
        // synthetic CPS arm fn invokes its captured `k(arg)` in tail
        // position, codegen lowers the call as
        // `sigil_next_step_call(loaded_k_closure, loaded_k_fn,
        // /*arg_count=*/1)` and writes `arg` to the args buffer's
        // slot 0; the trampoline dispatches the resulting Call into
        // `sigil_continuation_identity(null, args_ptr=&[arg],
        // args_len=1)`, which returns `Done(arg)` from the arena. The
        // surrounding native fn's `sigil_run_loop` invocation
        // observes the Done and returns `arg` to the perform site.
        //
        // `k_closure_ptr` is null because the identity continuation
        // is closure-less; future Phase 4e replaces this constant
        // with a real lambda-lifted continuation closure when arm
        // bodies invoke `k` non-tail or for multi-shot semantics.
        //
        // Bisecting hint: a regression that produces a tail-`k(arg)`
        // returning the wrong value should treat this site (and the
        // arm-fn tail-k lowering at the synth pass) as the prime
        // suspect — see the `[DEVIATION Task 55] Phase 4d` entry.
        let null_k_closure = self.builder.ins().iconst(self.pointer_ty, 0);
        let k_fn = self
            .builder
            .ins()
            .func_addr(self.pointer_ty, self.continuation_identity_ref);
        let perform_call = self.builder.ins().call(
            self.perform_ref,
            &[
                effect_id_v,
                op_id_v,
                args_ptr,
                args_len_v,
                null_k_closure,
                k_fn,
            ],
        );
        self.stackmap
            .push_placeholder(function_code_offset(&self.builder, perform_call));
        let call_next_step = self.builder.inst_results(perform_call)[0];
        // `sigil_perform` returns a `NextStep::Call` (it builds the
        // Call to the arm + (k_closure, k_fn); it does not invoke
        // the arm itself). Hand off to `sigil_run_loop` which
        // dispatches the Call (invokes the arm), then any further
        // Calls the arm returns, until a terminal `Done(value)`.
        // Returns u64.
        let run_loop_call = self
            .builder
            .ins()
            .call(self.run_loop_ref, &[call_next_step]);
        self.stackmap
            .push_placeholder(function_code_offset(&self.builder, run_loop_call));
        let widened = self.builder.inst_results(run_loop_call)[0];

        // Phase 4c: narrow the run_loop result back to the op's
        // declared return type. The arm fn widens its body value to
        // I64 before `sigil_next_step_done` (matching the FFI
        // signature); without this mirror narrow on the perform side,
        // surrounding code that consumes a non-Int perform result
        // (e.g. Bool I8, Char I32) would see an I64 Cranelift value
        // where `type_of_expr` predicts a narrower type, producing
        // a verifier failure or worse a silent type-mismatch.
        // Both registry lookups are typecheck invariants: E0042
        // catches unknown effects, E0043 catches unknown ops. Falling
        // back to `I64` here would silently emit wrong-typed Cranelift
        // values for non-Int return types under any future typecheck
        // regression that left the registry incomplete; `unreachable!`
        // surfaces the regression at codegen time instead.
        let eff = self.effects.get(&p.effect).unwrap_or_else(|| {
            unreachable!(
                "codegen: effect `{}` missing from effects registry; typecheck-time \
                 E0042 should have caught this",
                p.effect
            )
        });
        let op_decl = eff.ops.iter().find(|o| o.name == p.op).unwrap_or_else(|| {
            unreachable!(
                "codegen: op `{}.{}` missing from EffectDecl.ops; typecheck-time \
                 E0043 should have caught this",
                p.effect, p.op
            )
        });
        let return_ty = cranelift_ty_for_type_expr(&op_decl.return_type, self.pointer_ty);
        if return_ty == types::I64 {
            widened
        } else if return_ty.is_int() && return_ty.bits() < 64 {
            self.builder.ins().ireduce(return_ty, widened)
        } else {
            // pointer_ty (String, user-type pointers): on supported
            // targets pointer_ty == I64, so the value is already the
            // right width. A future float / 32-bit-target port would
            // need an ireduce or bitcast branch here. Hardened to
            // `assert!` (mirrors the perform-side widening fallthrough
            // in this same fn) so a future regression panics in dev
            // and release rather than silently producing a wrong-
            // typed Cranelift Value.
            assert_eq!(
                return_ty, self.pointer_ty,
                "codegen Phase 4c: unexpected op return type {return_ty:?} \
                 narrowing run_loop result for `{}.{}` — Phase 4c only \
                 supports I64 (Int), I32 (Char), I8 (Bool/Byte/Unit), \
                 and pointer_ty (String / user-type pointers)",
                p.effect, p.op
            );
            widened
        }
    }

    /// Lower an expression to an SSA value. The value's Cranelift
    /// type follows the mapping in the `Lowerer` doc comment.
    fn lower_expr(&mut self, e: &crate::ast::Expr) -> Value {
        use crate::ast::Expr;
        match e {
            Expr::IntLit(n, _) => self.builder.ins().iconst(types::I64, *n),
            Expr::BoolLit(b, _) => self.builder.ins().iconst(types::I8, if *b { 1 } else { 0 }),
            Expr::CharLit(c, _) => self.builder.ins().iconst(types::I32, *c as i64),
            Expr::StringLit(_, span) => self.lower_string_literal(span),
            Expr::Ident(name, _) => {
                // Plan A3 task 41.1: if the identifier isn't in the
                // local env and matches a registered nullary
                // constructor, lower as a user-type allocation (Unit
                // variant, zero fields). The typechecker's
                // nullary-ctor promotion path (task 38.2) already
                // gated this case to be a Ty::User result.
                if let Some(v) = self.env.get(name) {
                    *v
                } else if let Some((type_name, variant_idx)) = self.ctor_index.get(name).cloned() {
                    self.lower_ctor_alloc(&type_name, variant_idx, &[])
                } else {
                    unreachable!("codegen: unknown ident `{name}`")
                }
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                let l = self.lower_expr(lhs);
                let r = self.lower_expr(rhs);
                self.emit_binop(*op, l, r)
            }
            Expr::Unary { op, operand, .. } => {
                let v = self.lower_expr(operand);
                self.emit_unop(*op, v)
            }
            Expr::Match {
                scrutinee,
                arms,
                span,
            } => self.lower_match(scrutinee, arms, span),
            Expr::Block(b) => self
                .lower_block(b)
                .unwrap_or_else(|| self.builder.ins().iconst(types::I8, 0)),
            Expr::If { .. } => {
                // Elaborate (plan A2 task 23) desugars all `if`s to
                // `match`; reaching one here is an internal invariant
                // violation.
                unreachable!("codegen: Expr::If should have been desugared by elaborate")
            }
            Expr::Perform(p) => {
                if p.effect == "IO" {
                    self.lower_perform(p);
                    // IO.println returns Unit — represented as `i8 0`.
                    self.builder.ins().iconst(types::I8, 0)
                } else {
                    // Plan B Task 55 (Phase 3b) — non-IO perform
                    // routes through `sigil_perform`. `sigil_perform`
                    // builds a `NextStep::Call` to the matching arm;
                    // `lower_perform_non_io_to_value` then hands that
                    // off to `sigil_run_loop`, which dispatches the
                    // call (and any further calls the arm returns)
                    // until a terminal `Done(value)` and returns the
                    // value as u64. Codegen never reads the
                    // `NextStep` layout directly.
                    self.lower_perform_non_io_to_value(p)
                }
            }
            Expr::Call { callee, args, .. } => self.lower_call(callee, args),
            Expr::Lambda { .. } => {
                // Closure conversion (plan A2 task 31) rewrites every
                // `Expr::Lambda` into an `Expr::ClosureRecord`; codegen
                // only sees the post-CC form. Hitting this arm means
                // closure conversion skipped a lambda.
                unreachable!(
                    "codegen: Expr::Lambda should have been replaced by ClosureRecord in closure_convert"
                )
            }
            Expr::ClosureRecord {
                code_fn_name,
                env_exprs,
                env_slot_kinds,
                ..
            } => self.lower_closure_record(code_fn_name, env_exprs, env_slot_kinds),
            Expr::ClosureEnvLoad { index, kind, .. } => self.lower_closure_env_load(*index, *kind),
            // Plan A3 task 41.1: record literal `Ctor { f: v, .. }`
            // lowers to `sigil_alloc(header, payload_bytes)` followed
            // by a discriminant store and per-field stores at the
            // declared-order offsets (field names reordered to match
            // the type declaration).
            Expr::RecordLit { name, fields, .. } => {
                let (type_name, variant_idx) =
                    self.ctor_index.get(name).cloned().unwrap_or_else(|| {
                        unreachable!("codegen: RecordLit `{name}` not in ctor index")
                    });
                let layout = &self.type_layouts[&type_name];
                let variant = &layout.variants[variant_idx];
                // Reorder the user's field values to match the declared
                // field order. The typechecker guarantees every declared
                // field is present exactly once (E0115 otherwise).
                let ordered_values: Vec<Value> = variant
                    .field_names
                    .iter()
                    .map(|decl_name| {
                        let ast = fields
                            .iter()
                            .find(|f| &f.name == decl_name)
                            .unwrap_or_else(|| {
                                unreachable!(
                                    "codegen: RecordLit `{name}` missing field `{decl_name}` post-typecheck"
                                )
                            });
                        self.lower_expr(&ast.value)
                    })
                    .collect();
                self.lower_ctor_alloc(&type_name, variant_idx, &ordered_values)
            }
            Expr::Handle {
                body,
                op_arms,
                span,
                ..
            } => {
                // Plan B Task 55 (Phase 3b): allocate a handler
                // frame, populate each arm slot with the synthetic
                // CPS fn's pointer (`func_addr`), push the frame,
                // evaluate the body inline (which may call
                // `sigil_perform` for non-IO effects), and pop the
                // frame. Frame's `closure_ptr` for each arm is null
                // (Phase 3b restricts arm bodies to literal
                // expressions with no captures).
                let effect_name = &op_arms[0].effect;
                let effect_id = match self.effect_ids.get(effect_name) {
                    Some(id) => *id,
                    None => unreachable!(
                        "codegen: effect `{effect_name}` missing from effect_ids \
                         map; typecheck-time E0138 should have caught this"
                    ),
                };
                let arm_count = op_arms.len() as u32;
                let effect_id_v = self.builder.ins().iconst(types::I32, effect_id as i64);
                let arm_count_v = self.builder.ins().iconst(types::I32, arm_count as i64);
                let frame_call = self
                    .builder
                    .ins()
                    .call(self.handler_frame_new_ref, &[effect_id_v, arm_count_v]);
                self.stackmap
                    .push_placeholder(function_code_offset(&self.builder, frame_call));
                let frame_ptr = self.builder.inst_results(frame_call)[0];

                // Populate each arm slot with the synthetic CPS fn's
                // pointer. The pre-pass allocated one `FuncRef` per
                // arm in declaration order; pair them with the AST
                // arms to compute op_ids.
                let arm_refs = self
                    .handler_arm_refs_per_handle
                    .get(span)
                    .unwrap_or_else(|| {
                        unreachable!(
                            "codegen: handler_arm_refs_per_handle missing entry for \
                             handle span {span:?}; pre-pass allocation should have \
                             registered every reachable Expr::Handle"
                        )
                    })
                    .clone();
                // Plan B Task 55 (Phase 4d): build a per-arm closure
                // record when the arm body has captures; pass null
                // `closure_ptr` when it doesn't.
                //
                // Captures are computed at typecheck time and stored
                // in `HandlerArmSynth.captures` (parallel to the arm
                // body's rewritten `Expr::ClosureEnvLoad { index }`
                // references). For each capture, look up the value in
                // the surrounding fn's `Lowerer.env` (the
                // closure-conversion pass already rewrote any
                // enclosing-fn-capture Idents into `ClosureEnvLoad`
                // nodes that resolve via `lower_closure_env_load`,
                // which the rewrite pass re-indexed to the arm's slot
                // — we just lower them in the outer Lowerer here
                // since we're still inside the surrounding fn).
                //
                // Bisecting hint (Phase 4d MVP, see PLAN_B_DEVIATIONS):
                // a regression at this site producing a wrong-typed
                // capture or a missing GC bitmap bit is the prime
                // suspect for "captured outer-scope binding reads zero
                // / wrong-typed value at arm runtime" failures. The
                // bitmap is computed inside `alloc_arm_closure_record`
                // from each capture's `EnvSlotKind::is_pointer()`.
                let arm_indices_for_handle = self
                    .handler_arm_indices
                    .get(span)
                    .unwrap_or_else(|| {
                        unreachable!(
                            "codegen: handler_arm_indices missing entry for handle \
                             span {span:?}; pre-pass should have registered every \
                             reachable Expr::Handle"
                        )
                    })
                    .clone();
                debug_assert_eq!(
                    arm_indices_for_handle.len(),
                    op_arms.len(),
                    "codegen Phase 4d: arm_indices length must match op_arms"
                );
                let null_ptr = self.builder.ins().iconst(self.pointer_ty, 0);
                for ((arm, fn_ref), &synth_idx) in op_arms
                    .iter()
                    .zip(arm_refs.iter())
                    .zip(arm_indices_for_handle.iter())
                {
                    let op_id = match self.op_ids.get(&(arm.effect.clone(), arm.op.clone())) {
                        Some(id) => *id,
                        None => unreachable!(
                            "codegen: op_id missing for `{}.{}`; typecheck-time \
                             E0138/E0139 should have caught this",
                            arm.effect, arm.op
                        ),
                    };
                    let op_id_v = self.builder.ins().iconst(types::I32, op_id as i64);
                    let fn_ptr_v = self.builder.ins().func_addr(self.pointer_ty, *fn_ref);

                    // Phase 4d: build closure record from captures, or
                    // pass null when the arm has none.
                    let captures = self.handler_arm_synth[synth_idx].captures.clone();
                    let arm_closure_ptr = if captures.is_empty() {
                        null_ptr
                    } else {
                        self.alloc_arm_closure_record(&captures)
                    };

                    let set_call = self.builder.ins().call(
                        self.handler_frame_set_arm_ref,
                        &[frame_ptr, op_id_v, fn_ptr_v, arm_closure_ptr],
                    );
                    self.stackmap
                        .push_placeholder(function_code_offset(&self.builder, set_call));
                }

                let push_call = self.builder.ins().call(self.handle_push_ref, &[frame_ptr]);
                self.stackmap
                    .push_placeholder(function_code_offset(&self.builder, push_call));
                let body_val = self.lower_expr(body);
                let pop_call = self.builder.ins().call(self.handle_pop_ref, &[]);
                self.stackmap
                    .push_placeholder(function_code_offset(&self.builder, pop_call));
                body_val
            }
        }
    }

    /// Lower `Expr::Call`. Direct-calls the callee when it is an
    /// `Ident` of a user fn (passing a null closure_ptr) or a
    /// `ClosureRecord` (passing the allocated record's ptr). Any other
    /// callee shape would require an indirect call via `call_indirect`
    /// — which is deferred to Plan A3 when the `TypeExpr::Fn` surface
    /// syntax lands and fn-typed lets become expressible. See
    /// `PLAN_A2_DEVIATIONS.md` `[Task 32]` for the rationale.
    fn lower_call(&mut self, callee: &crate::ast::Expr, args: &[crate::ast::Expr]) -> Value {
        use crate::ast::Expr;
        match callee {
            // Plan A3 task 41.1: positional constructor application
            // `Ctor(a, b, ..)` where `Ctor` is a registered ctor name
            // and not shadowed by a user fn or local. Lowers to heap
            // allocation + discriminant + per-field stores in the
            // declared field order (same order as args, since these
            // are positional).
            Expr::Ident(name, _)
                if !self.user_fn_refs.contains_key(name)
                    && !self.env.contains_key(name)
                    && self.ctor_index.contains_key(name) =>
            {
                let (type_name, variant_idx) = self.ctor_index[name].clone();
                let field_vals: Vec<Value> = args.iter().map(|a| self.lower_expr(a)).collect();
                self.lower_ctor_alloc(&type_name, variant_idx, &field_vals)
            }
            Expr::Ident(name, _) if self.user_fn_refs.contains_key(name) => {
                let arg_vals: Vec<Value> = args.iter().map(|a| self.lower_expr(a)).collect();
                let func_ref = self.user_fn_refs[name];
                let null_closure = self.builder.ins().iconst(self.pointer_ty, 0);
                let mut all_args: Vec<Value> = Vec::with_capacity(arg_vals.len() + 1);
                all_args.push(null_closure);
                all_args.extend(arg_vals);
                let call = self.builder.ins().call(func_ref, &all_args);
                self.stackmap
                    .push_placeholder(function_code_offset(&self.builder, call));
                self.builder.inst_results(call)[0]
            }
            // Plan A2 task 34: language builtin `int_to_string(Int) ->
            // String !`. Typecheck seeds the signature via
            // `typecheck::builtin_fn_env`; ordering matters — the
            // `user_fn_refs` arm above wins if a user defined an
            // `int_to_string` fn of their own. `sigil_int_to_string`
            // allocates a fresh Boehm-managed String, so the call is a
            // safepoint and gets a placeholder stackmap record like
            // any other heap-touching call.
            Expr::Ident(name, _) if name == "int_to_string" => {
                assert_eq!(args.len(), 1, "int_to_string builtin arg count is not 1");
                let arg_val = self.lower_expr(&args[0]);
                let call = self.builder.ins().call(self.int_to_string_ref, &[arg_val]);
                self.stackmap
                    .push_placeholder(function_code_offset(&self.builder, call));
                self.builder.inst_results(call)[0]
            }
            Expr::ClosureRecord { code_fn_name, .. } => {
                // Evaluate the ClosureRecord first (allocates + stores
                // the closure on the heap) and use its pointer as the
                // callee's closure_ptr.
                let closure_value = self.lower_expr(callee);
                let arg_vals: Vec<Value> = args.iter().map(|a| self.lower_expr(a)).collect();
                let func_ref = *self.user_fn_refs.get(code_fn_name).unwrap_or_else(|| {
                    unreachable!(
                        "codegen: closure-record code_fn_name `{code_fn_name}` not registered"
                    )
                });
                let mut all_args: Vec<Value> = Vec::with_capacity(arg_vals.len() + 1);
                all_args.push(closure_value);
                all_args.extend(arg_vals);
                let call = self.builder.ins().call(func_ref, &all_args);
                self.stackmap
                    .push_placeholder(function_code_offset(&self.builder, call));
                self.builder.inst_results(call)[0]
            }
            _ => {
                // Indirect calls (callee is a bound local or an
                // arbitrary expression producing a closure value) land
                // in Plan A3. Plan A2 cannot reach this arm from a
                // well-typed program because `TypeExpr::Fn` is deferred
                // — there is no surface syntax to declare a let or a
                // param of function type, so every well-typed callee
                // reduces to `Ident(top_level_fn)` or `ClosureRecord`
                // at this point.
                unreachable!(
                    "codegen: indirect call (callee = {callee:?}) deferred to Plan A3 (TypeExpr::Fn not in A2)"
                )
            }
        }
    }

    /// Allocate a closure record `{header, code_ptr, env[0], ...,
    /// env[N-1]}` on the GC heap and return its header pointer. See
    /// `runtime/src/header.rs` for the 8-byte header layout; bit 0 of
    /// the pointer bitmap is always `0` here (code_ptr is not a GC
    /// pointer), and bits `1..=N` reflect `env_slot_kinds[k].is_pointer()`.
    ///
    /// All env slots are stored as 8-byte words — smaller types
    /// (`Bool`, `Byte`, `Unit` as `i8`; `Char` as `i32`) are
    /// zero-extended on store and truncated on load. `String` and
    /// `Closure` values are already pointer-sized.
    /// Plan B Task 55 (Phase 4d) — allocate a TAG_CLOSURE record for a
    /// synthetic CPS arm fn's captured environment. Layout matches
    /// `lower_closure_record` (header + code_ptr slot + env slots) so
    /// the GC bitmap and slot-load machinery the runtime already uses
    /// for user-level closures applies unchanged.
    ///
    /// Each capture's value is sourced from `self.env[name]` — the
    /// surrounding fn's lexical env at the handle expression's
    /// position. References to surrounding-fn-captures (handle inside
    /// a synthetic lambda fn whose own captures appear in the arm
    /// body) are NOT supported by the Phase 4d MVP — closure_convert
    /// rewrites those to `Expr::ClosureEnvLoad` nodes that the
    /// `unsupported_handle_construct` walker rejects with a Phase-4e-
    /// pointing diagnostic.
    ///
    /// The arm fn's `closure_ptr` parameter (block_param 0) receives
    /// this record's pointer at runtime, set on the handler frame's
    /// arm slot via `sigil_handler_frame_set_arm`. Inside the arm fn,
    /// references load via the existing `lower_closure_env_load`
    /// against the arm-local index.
    ///
    /// The code_ptr slot at offset 8 is unused (the runtime dispatches
    /// via `HandlerFrame.arms[i].fn_ptr`, set separately to the
    /// arm-fn's `func_addr`). It's stored as null to keep the layout
    /// uniform with user-level closures and avoid a divergent GC
    /// bitmap shape.
    fn alloc_arm_closure_record(&mut self, captures: &[ArmCapture]) -> Value {
        let env_len = captures.len();
        assert!(
            env_len > 0,
            "alloc_arm_closure_record on empty captures — caller should pass null directly"
        );
        assert!(
            env_len < MAX_CLOSURE_ENV_SLOTS,
            "arm closure env >= {MAX_CLOSURE_ENV_SLOTS} slots exceeds the bitmap layout"
        );

        let mut bitmap: u32 = 0;
        for (i, c) in captures.iter().enumerate() {
            if c.kind.is_pointer() {
                bitmap |= 1u32 << (i + 1);
            }
        }
        let count: u8 = 1 + env_len as u8;
        let header: u64 = header_word(TAG_CLOSURE, count, bitmap);
        let payload_bytes: i64 = 8 + 8 * env_len as i64;

        // Lower env values FIRST, in source order — same discipline
        // as `lower_closure_record`. Each value is read out of
        // `self.env` by name; absent names indicate the surrounding-fn-
        // capture case (synthetic lambda fn), unsupported by the MVP.
        let env_vals: Vec<Value> = captures
            .iter()
            .map(|c| {
                self.env.get(&c.name).copied().unwrap_or_else(|| {
                    unreachable!(
                        "codegen Phase 4d: arm capture `{}` not found in surrounding \
                         fn's lexical env at handle codegen time. This indicates \
                         the surrounding fn is a synthetic lambda fn whose own \
                         captures appear in the arm body — a case the Phase 4d \
                         MVP does not support and the codegen-entry walker should \
                         have rejected (closure point: Plan B Task 55, Phase 4e — \
                         see the `[DEVIATION Task 55] Phase 4d` entry in \
                         PLAN_B_DEVIATIONS.md). Reaching this `unreachable!` \
                         indicates the walker's `Expr::ClosureEnvLoad` rejection \
                         path was incorrectly relaxed alongside the Phase 4d \
                         `Expr::Ident`-capture lift.",
                        c.name
                    )
                })
            })
            .collect();

        let header_v = self.builder.ins().iconst(types::I64, header as i64);
        let payload_v = self.builder.ins().iconst(self.pointer_ty, payload_bytes);
        let alloc_call = self
            .builder
            .ins()
            .call(self.alloc_ref, &[header_v, payload_v]);
        self.stackmap
            .push_placeholder(function_code_offset(&self.builder, alloc_call));
        let closure_ptr = self.builder.inst_results(alloc_call)[0];

        // Store null at offset 8 (code_ptr slot — unused by arm fns;
        // the runtime dispatches via `HandlerFrame.arms[i].fn_ptr`).
        let null_v = self.builder.ins().iconst(self.pointer_ty, 0);
        self.builder
            .ins()
            .store(MemFlags::trusted(), null_v, closure_ptr, 8);

        // Store env slots at offset 16 + 8*i with widening matching
        // `lower_closure_record` so `lower_closure_env_load` reads
        // them back consistently.
        for (i, (raw, capture)) in env_vals.iter().zip(captures.iter()).enumerate() {
            let slot_val = match capture.kind {
                EnvSlotKind::Int => *raw,
                EnvSlotKind::Bool | EnvSlotKind::Byte | EnvSlotKind::Unit => {
                    self.builder.ins().uextend(types::I64, *raw)
                }
                EnvSlotKind::Char => self.builder.ins().uextend(types::I64, *raw),
                EnvSlotKind::String | EnvSlotKind::Closure | EnvSlotKind::User => *raw,
            };
            let offset: i32 = 16 + 8 * i as i32;
            self.builder
                .ins()
                .store(MemFlags::trusted(), slot_val, closure_ptr, offset);
        }

        closure_ptr
    }

    fn lower_closure_record(
        &mut self,
        code_fn_name: &str,
        env_exprs: &[crate::ast::Expr],
        env_slot_kinds: &[EnvSlotKind],
    ) -> Value {
        assert_eq!(
            env_exprs.len(),
            env_slot_kinds.len(),
            "closure_convert should emit parallel env_exprs / env_slot_kinds"
        );
        let env_len = env_exprs.len();
        assert!(
            env_len < MAX_CLOSURE_ENV_SLOTS,
            "closure env >= {MAX_CLOSURE_ENV_SLOTS} slots exceeds the bitmap layout (tag 0xFF descriptor is v2)"
        );

        // Header bitmap: bit 0 = code_ptr (not a pointer), bit k+1 set
        // iff env slot k holds a GC-managed pointer.
        let mut bitmap: u32 = 0;
        for (i, kind) in env_slot_kinds.iter().enumerate() {
            if kind.is_pointer() {
                bitmap |= 1u32 << (i + 1);
            }
        }
        // Payload word count: 1 (code_ptr) + env_len (one word per slot).
        let count: u8 = 1 + env_len as u8;
        // Header word assembled via the shared `sigil-header-constants`
        // crate so the bit-layout formula is a single-point-of-edit
        // across the compiler and runtime (PR #7 review item 3).
        let header: u64 = header_word(TAG_CLOSURE, count, bitmap);
        let payload_bytes: i64 = 8 + 8 * env_len as i64; // code_ptr + env slots

        // Lower env_exprs to Cranelift Values in source order; each
        // Value will be extended to i64 before store.
        let env_vals: Vec<Value> = env_exprs.iter().map(|e| self.lower_expr(e)).collect();

        // Call sigil_alloc(header, payload_bytes) -> *u8.
        let header_v = self.builder.ins().iconst(types::I64, header as i64);
        let payload_v = self.builder.ins().iconst(self.pointer_ty, payload_bytes);
        let alloc_call = self
            .builder
            .ins()
            .call(self.alloc_ref, &[header_v, payload_v]);
        self.stackmap
            .push_placeholder(function_code_offset(&self.builder, alloc_call));
        let closure_ptr = self.builder.inst_results(alloc_call)[0];

        // Store code_ptr at offset 8 (past header).
        let code_fn_ref = *self.user_fn_refs.get(code_fn_name).unwrap_or_else(|| {
            unreachable!("codegen: ClosureRecord code_fn_name `{code_fn_name}` not registered")
        });
        let code_ptr = self.builder.ins().func_addr(self.pointer_ty, code_fn_ref);
        self.builder
            .ins()
            .store(MemFlags::trusted(), code_ptr, closure_ptr, 8);

        // Store env slots at offset 16 + 8*i, each as an 8-byte word.
        for (i, (raw, kind)) in env_vals.iter().zip(env_slot_kinds.iter()).enumerate() {
            let slot_val = match kind {
                EnvSlotKind::Int => *raw, // i64 already
                EnvSlotKind::Bool | EnvSlotKind::Byte | EnvSlotKind::Unit => {
                    // i8 → i64 (zero-extend; Sigil primitives are
                    // unsigned in their bit-level storage).
                    self.builder.ins().uextend(types::I64, *raw)
                }
                EnvSlotKind::Char => self.builder.ins().uextend(types::I64, *raw),
                EnvSlotKind::String | EnvSlotKind::Closure | EnvSlotKind::User => *raw,
            };
            let offset: i32 = 16 + 8 * i as i32;
            self.builder
                .ins()
                .store(MemFlags::trusted(), slot_val, closure_ptr, offset);
        }

        closure_ptr
    }

    /// Plan A3 task 41.1 constructor allocation.
    ///
    /// Emits `sigil_alloc(header, payload_bytes)`, stores the
    /// variant's 1-byte discriminant at payload word 0, and stores
    /// each field value at subsequent payload words in the variant's
    /// declared order. Returns the header pointer (never an interior
    /// pointer — the callers load fields back via offsets from the
    /// header pointer).
    ///
    /// Every ctor allocation is a safepoint (heap-touching call); the
    /// placeholder stackmap record is pushed at the `sigil_alloc`
    /// instruction, matching Plan A2's closure-record pattern.
    fn lower_ctor_alloc(
        &mut self,
        type_name: &str,
        variant_index: usize,
        field_values: &[Value],
    ) -> Value {
        let layout = &self.type_layouts[type_name];
        let variant = &layout.variants[variant_index];
        debug_assert_eq!(
            field_values.len(),
            variant.field_count(),
            "ctor field count mismatch for `{type_name}::{}`",
            variant.name,
        );

        let header = crate::layout::variant_header_word(layout.type_tag, variant);
        let payload_bytes: i64 = (variant.payload_words as i64) * 8;

        let header_v = self.builder.ins().iconst(types::I64, header as i64);
        let size_v = self.builder.ins().iconst(self.pointer_ty, payload_bytes);
        let alloc_call = self.builder.ins().call(self.alloc_ref, &[header_v, size_v]);
        self.stackmap
            .push_placeholder(function_code_offset(&self.builder, alloc_call));
        let ptr = self.builder.inst_results(alloc_call)[0];

        // Discriminant in payload word 0 (bytes 8..16 past header).
        // We store the full 8-byte word even though only the low
        // byte carries meaning — matches the word-aligned store the
        // match-side discriminant load uses, avoids partial-write
        // aliasing.
        let disc_v = self
            .builder
            .ins()
            .iconst(types::I64, i64::from(variant.discriminant));
        self.builder
            .ins()
            .store(MemFlags::trusted(), disc_v, ptr, 8);

        // Fields in payload words 1..N. Each field stores an 8-byte
        // word; sub-word primitives (Bool, Byte, Char, Unit) are
        // zero-extended on store, pointer-typed fields flow through
        // unchanged.
        for (i, &val) in field_values.iter().enumerate() {
            let val_ty = self.builder.func.dfg.value_type(val);
            let store_val = if val_ty == types::I64 || val_ty == self.pointer_ty {
                val
            } else {
                self.builder.ins().uextend(types::I64, val)
            };
            // Offset = 8 (header) + 8 (discriminant word) + 8*i.
            let offset: i32 = 16 + 8 * i as i32;
            self.builder
                .ins()
                .store(MemFlags::trusted(), store_val, ptr, offset);
        }

        ptr
    }

    /// Load the `index`-th env slot from the current fn's closure_ptr.
    /// The load width matches the slot kind; i64 slot words are
    /// truncated on load for sub-word types.
    fn lower_closure_env_load(&mut self, index: usize, kind: EnvSlotKind) -> Value {
        let offset: i32 = 16 + 8 * index as i32;
        let raw =
            self.builder
                .ins()
                .load(types::I64, MemFlags::trusted(), self.closure_ptr, offset);
        match kind {
            EnvSlotKind::Int => raw,
            EnvSlotKind::Bool | EnvSlotKind::Byte | EnvSlotKind::Unit => {
                self.builder.ins().ireduce(types::I8, raw)
            }
            EnvSlotKind::Char => self.builder.ins().ireduce(types::I32, raw),
            EnvSlotKind::String | EnvSlotKind::Closure | EnvSlotKind::User => {
                if self.pointer_ty == types::I64 {
                    raw
                } else {
                    // Plan A2 targets are 64-bit; the else branch is a
                    // defensive path for hypothetical 32-bit hosts.
                    self.builder.ins().ireduce(self.pointer_ty, raw)
                }
            }
        }
    }

    /// Emit a `sigil_string_new(bytes, len)` call for a string literal
    /// identified by its source span, returning the heap-pointer SSA
    /// value.
    fn lower_string_literal(&mut self, span: &Span) -> Value {
        let (gv, len) = self
            .lit_gvs
            .iter()
            .find(|(s, _, _)| s == span)
            .map(|(_, g, l)| (*g, *l))
            .unwrap_or_else(|| {
                unreachable!("codegen: string literal at span {span:?} not declared")
            });
        let bytes_ptr = self.builder.ins().symbol_value(self.pointer_ty, gv);
        let len_v = self.builder.ins().iconst(self.pointer_ty, len as i64);
        let call = self
            .builder
            .ins()
            .call(self.string_new_ref, &[bytes_ptr, len_v]);
        self.stackmap
            .push_placeholder(function_code_offset(&self.builder, call));
        self.builder.inst_results(call)[0]
    }

    fn emit_binop(&mut self, op: crate::ast::BinOp, l: Value, r: Value) -> Value {
        use crate::ast::BinOp;
        match op {
            BinOp::Add => self.builder.ins().iadd(l, r),
            BinOp::Sub => self.builder.ins().isub(l, r),
            BinOp::Mul => self.builder.ins().imul(l, r),
            BinOp::Div => {
                self.trap_on_zero(r, self.div_zero_gv);
                self.builder.ins().sdiv(l, r)
            }
            BinOp::Mod => {
                self.trap_on_zero(r, self.mod_zero_gv);
                self.builder.ins().srem(l, r)
            }
            BinOp::Eq => self.builder.ins().icmp(IntCC::Equal, l, r),
            BinOp::NotEq => self.builder.ins().icmp(IntCC::NotEqual, l, r),
            BinOp::Lt => self.builder.ins().icmp(IntCC::SignedLessThan, l, r),
            BinOp::Gt => self.builder.ins().icmp(IntCC::SignedGreaterThan, l, r),
            BinOp::LtEq => self.builder.ins().icmp(IntCC::SignedLessThanOrEqual, l, r),
            BinOp::GtEq => self
                .builder
                .ins()
                .icmp(IntCC::SignedGreaterThanOrEqual, l, r),
            // Bitwise-on-`i8` is bool-on-`{0,1}` because typecheck
            // restricts `&& ||` operands to `Bool` ({0, 1}).
            BinOp::And => self.builder.ins().band(l, r),
            BinOp::Or => self.builder.ins().bor(l, r),
        }
    }

    fn emit_unop(&mut self, op: crate::ast::UnOp, v: Value) -> Value {
        use crate::ast::UnOp;
        match op {
            UnOp::Neg => self.builder.ins().ineg(v),
            // `!bool` over `{0, 1}` on `i8`: XOR with 1 flips bit 0.
            UnOp::Not => self.builder.ins().bxor_imm(v, 1),
        }
    }

    /// Emit a divisor-zero check. If `divisor` is zero, jumps to a
    /// fresh block that calls `sigil_panic_arith_error(msg_gv)`
    /// (noreturn) and traps; otherwise fall through.
    ///
    /// The `trap` after the call is required because Cranelift cannot
    /// represent `-> !`; the runtime exits the process before the trap
    /// runs. The trap's presence satisfies Cranelift's terminator
    /// invariant and prevents the optimiser from sinking code past
    /// the call.
    fn trap_on_zero(&mut self, divisor: Value, msg_gv: GlobalValue) {
        let ok = self.builder.create_block();
        let bad = self.builder.create_block();

        self.builder.ins().brif(divisor, ok, &[], bad, &[]);

        // Emit the panic block.
        self.builder.switch_to_block(bad);
        self.builder.seal_block(bad);
        let msg_ptr = self.builder.ins().symbol_value(self.pointer_ty, msg_gv);
        let call = self.builder.ins().call(self.panic_arith_ref, &[msg_ptr]);
        self.stackmap
            .push_placeholder(function_code_offset(&self.builder, call));
        // `sigil_panic_arith_error` is `-> !` in the runtime; the trap
        // is never reached at run-time but satisfies Cranelift.
        self.builder
            .ins()
            .trap(TrapCode::unwrap_user(TRAP_ARITH_ABORT));

        // Resume in the ok block.
        self.builder.switch_to_block(ok);
        self.builder.seal_block(ok);
    }

    /// Lower `match scrutinee { pat => body, ... }` to a per-arm
    /// decision tree followed by a continue block whose single parameter
    /// carries the match's result value.
    ///
    /// Arm strategy:
    /// - A true catch-all arm (`_` or `Pattern::Var(name)` that is not a
    ///   nullary-constructor promotion) emits an unconditional jump with
    ///   the name (if any) bound to the scrutinee value. Any later arms
    ///   are dead and skipped.
    /// - Every other arm emits a chain of tests through
    ///   `emit_pattern_test`: primitive-literal compares, discriminant
    ///   compares for constructor patterns, and recursive tests for
    ///   sub-patterns inside a matched constructor (Plan A3 task 41.2).
    ///   Failed tests fall through to a per-arm `next` block that hosts
    ///   the subsequent arm's test.
    ///
    /// Exhaustiveness is enforced at typecheck (E0066 for primitives,
    /// E0120 for user types — including full nested Maranget coverage
    /// of ctor field patterns as of the Plan B carryover, commit
    /// `62ba42a`). `TRAP_NONEXHAUSTIVE_MATCH` is a defensive safety
    /// net that should not fire on a well-typed program: it guards
    /// codegen-internal bugs and any future surface (e.g. infinite
    /// primitive domains under Stage 6 effects) where the typechecker
    /// cannot statically prove coverage.
    fn lower_match(
        &mut self,
        scrutinee: &crate::ast::Expr,
        arms: &[crate::ast::MatchArm],
        match_span: &Span,
    ) -> Value {
        let s = self.lower_expr(scrutinee);
        let scrut_ty = self.match_scrut_tys.get(match_span).cloned();

        // Predict the result type from the first arm's body. Pattern
        // bindings introduced by the first arm are added to a preview
        // map so `type_of_expr` can look up their Cranelift types before
        // any arm body is actually lowered.
        let mut preview: BTreeMap<String, Type> = BTreeMap::new();
        self.predict_pattern_bindings(&arms[0].pattern, scrut_ty.as_ref(), &mut preview);
        let result_ty = self.type_of_expr(&arms[0].body, &preview);
        let cont = self.builder.create_block();
        self.builder.append_block_param(cont, result_ty);

        let mut chain_terminated = false;
        for arm in arms.iter() {
            if self.is_catchall_pattern(&arm.pattern, scrut_ty.as_ref()) {
                // Unconditional arm. If the pattern is a `Pattern::Var`,
                // bind the name to the scrutinee value over the arm
                // body; `Pattern::Wildcard` binds nothing.
                let saved = match &arm.pattern {
                    crate::ast::Pattern::Var(name, _) => {
                        let prev = self.env.insert(name.clone(), s);
                        Some((name.clone(), prev))
                    }
                    _ => None,
                };
                let v = self.lower_expr(&arm.body);
                if let Some((name, prev)) = saved {
                    match prev {
                        Some(p) => {
                            self.env.insert(name, p);
                        }
                        None => {
                            self.env.remove(&name);
                        }
                    }
                }
                self.builder.ins().jump(cont, &[BlockArg::Value(v)]);
                chain_terminated = true;
                break;
            }

            // Conditional arm: emit tests, then enter a dedicated body
            // block. `emit_pattern_test` branches to `next` on any test
            // failure and leaves the builder positioned in the
            // "all tests passed" block on success; we jump from there
            // into `body` with bindings installed.
            let body = self.builder.create_block();
            let next = self.builder.create_block();
            let mut bindings: Vec<(String, Value)> = Vec::new();
            self.emit_pattern_test(&arm.pattern, s, scrut_ty.as_ref(), next, &mut bindings);
            self.builder.ins().jump(body, &[]);

            self.builder.switch_to_block(body);
            self.builder.seal_block(body);
            // Install bindings, snapshot prior env entries for restore.
            let saved: Vec<(String, Option<Value>)> = bindings
                .into_iter()
                .map(|(name, val)| {
                    let prev = self.env.insert(name.clone(), val);
                    (name, prev)
                })
                .collect();
            let v = self.lower_expr(&arm.body);
            for (name, prev) in saved {
                match prev {
                    Some(p) => {
                        self.env.insert(name, p);
                    }
                    None => {
                        self.env.remove(&name);
                    }
                }
            }
            self.builder.ins().jump(cont, &[BlockArg::Value(v)]);

            self.builder.switch_to_block(next);
            self.builder.seal_block(next);
        }

        // Defensive trap: typecheck's exhaustiveness rules (E0066 for
        // primitives, E0120 for user types) guarantee every well-typed
        // program reaches a catch-all or enumerates every variant. Plan
        // A3 v1 does not extend exhaustiveness into nested constructor
        // positions, so a mismatched sub-pattern in an otherwise-covered
        // top-level variant falls through to this trap at runtime.
        if !chain_terminated {
            self.builder
                .ins()
                .trap(TrapCode::unwrap_user(TRAP_NONEXHAUSTIVE_MATCH));
        }

        self.builder.switch_to_block(cont);
        self.builder.seal_block(cont);
        self.builder.block_params(cont)[0]
    }

    /// Emit tests for `pat` against the SSA value `scrut` (semantic
    /// type `scrut_ty`). On any test failure, branches to `next`. On
    /// success, leaves the builder positioned in the "all tests passed"
    /// block; `bindings` accumulates `(name, Value)` pairs the caller
    /// must install in `self.env` before lowering the arm body.
    ///
    /// Intermediate blocks chain via `brif`; each block is sealed
    /// immediately after its terminator is emitted so Cranelift's
    /// `FunctionBuilder` bookkeeping stays consistent.
    fn emit_pattern_test(
        &mut self,
        pat: &crate::ast::Pattern,
        scrut: Value,
        scrut_ty: Option<&Ty>,
        next: Block,
        bindings: &mut Vec<(String, Value)>,
    ) {
        use crate::ast::{CtorPatternFields, Pattern};
        match pat {
            Pattern::Wildcard(_) => { /* no test, no binding */ }
            Pattern::IntLit(n, _) => self.emit_scalar_eq(scrut, types::I64, *n, next),
            Pattern::BoolLit(b, _) => self.emit_scalar_eq(scrut, types::I8, i64::from(*b), next),
            Pattern::CharLit(c, _) => self.emit_scalar_eq(scrut, types::I32, *c as i64, next),
            Pattern::Var(name, _) => {
                // Nullary-ctor promotion: if the scrutinee is a user
                // type whose registry lists `name` as a Unit variant,
                // the pattern is a discriminant check (no binding).
                if let Some(variant) = self.nullary_ctor_promotion(name, scrut_ty) {
                    self.emit_discriminant_eq(scrut, variant.discriminant, next);
                } else {
                    bindings.push((name.clone(), scrut));
                }
            }
            Pattern::Ctor { name, fields, .. } => {
                let (type_name, variant_index) =
                    self.ctor_index.get(name).cloned().unwrap_or_else(|| {
                        unreachable!("codegen: ctor pattern `{name}` not in ctor_index")
                    });
                // Clone the VariantLayout so subsequent calls that take
                // `&mut self` don't hold an immutable borrow of
                // `self.type_layouts` across the recursion.
                let variant = self.type_layouts[&type_name].variants[variant_index].clone();
                self.emit_discriminant_eq(scrut, variant.discriminant, next);
                match fields {
                    CtorPatternFields::Unit => {}
                    CtorPatternFields::Positional(pats) => {
                        for (i, sub) in pats.iter().enumerate() {
                            let field_ty = &variant.field_tys[i];
                            let field_val = self.load_field_value(scrut, i, field_ty);
                            self.emit_pattern_test(sub, field_val, Some(field_ty), next, bindings);
                        }
                    }
                    CtorPatternFields::Record(pat_fields) => {
                        for f in pat_fields {
                            let idx = variant
                                .field_names
                                .iter()
                                .position(|n| n == &f.name)
                                .unwrap_or_else(|| {
                                    unreachable!(
                                        "codegen: record ctor pattern field `{}` not declared for `{name}`",
                                        f.name
                                    )
                                });
                            let field_ty = &variant.field_tys[idx];
                            let field_val = self.load_field_value(scrut, idx, field_ty);
                            self.emit_pattern_test(
                                &f.pattern,
                                field_val,
                                Some(field_ty),
                                next,
                                bindings,
                            );
                        }
                    }
                }
            }
            Pattern::Tuple(_, _) => {
                unreachable!(
                    "codegen: Pattern::Tuple reaches lowering (typecheck should reject with E0117)"
                )
            }
        }
    }

    /// Emit `scrut == imm` at Cranelift type `ty` and split control flow:
    /// fall through to a freshly-sealed "keep" block on equality, branch
    /// to `next` otherwise.
    fn emit_scalar_eq(&mut self, scrut: Value, ty: Type, imm: i64, next: Block) {
        let lit = self.builder.ins().iconst(ty, imm);
        let eq = self.builder.ins().icmp(IntCC::Equal, scrut, lit);
        let keep = self.builder.create_block();
        self.builder.ins().brif(eq, keep, &[], next, &[]);
        self.builder.switch_to_block(keep);
        self.builder.seal_block(keep);
    }

    /// Load a user-type record's discriminant (payload word 0 at byte
    /// offset 8 from the object pointer) and branch on equality against
    /// the expected discriminant.
    fn emit_discriminant_eq(&mut self, ptr: Value, expected: u8, next: Block) {
        let disc = self
            .builder
            .ins()
            .load(types::I64, MemFlags::trusted(), ptr, 8);
        let expected_v = self.builder.ins().iconst(types::I64, i64::from(expected));
        let eq = self.builder.ins().icmp(IntCC::Equal, disc, expected_v);
        let keep = self.builder.create_block();
        self.builder.ins().brif(eq, keep, &[], next, &[]);
        self.builder.switch_to_block(keep);
        self.builder.seal_block(keep);
    }

    /// Load a user-type record's field at payload word `index + 1`
    /// (byte offset `16 + 8*index` from the object pointer). The i64
    /// word is reduced to the declared field type's Cranelift width for
    /// sub-word primitives; pointer-typed fields flow through unchanged.
    fn load_field_value(&mut self, ptr: Value, index: usize, field_ty: &Ty) -> Value {
        let offset: i32 = 16 + 8 * index as i32;
        let raw = self
            .builder
            .ins()
            .load(types::I64, MemFlags::trusted(), ptr, offset);
        match field_ty {
            Ty::Int => raw,
            Ty::Bool | Ty::Byte | Ty::Unit => self.builder.ins().ireduce(types::I8, raw),
            Ty::Char => self.builder.ins().ireduce(types::I32, raw),
            Ty::String | Ty::Fn(_) | Ty::User(_, _) => raw,
            // Plan B task 48 invariant — codegen-entry walker
            // (`contains_apply_or_generic_ref`) rejects programs
            // whose AST has surface generic syntax, so a stray
            // `Ty::Var` here is an internal-compiler bug.
            Ty::Var(_) => unreachable!(
                "codegen: Ty::Var is impossible after Plan B task 48 codegen-entry guard"
            ),
        }
    }

    /// Return the `VariantLayout` for `name` if it names a nullary
    /// (Unit) variant of `scrut_ty`'s declared type. This is the
    /// promotion rule mirrored at typecheck's `Pattern::Var` arm —
    /// codegen must agree to avoid binding a name the checker
    /// already resolved as a ctor reference (and vice versa).
    fn nullary_ctor_promotion(
        &self,
        name: &str,
        scrut_ty: Option<&Ty>,
    ) -> Option<crate::layout::VariantLayout> {
        let Some(Ty::User(type_name, _)) = scrut_ty else {
            return None;
        };
        let (ctor_type_name, variant_index) = self.ctor_index.get(name)?.clone();
        if ctor_type_name != *type_name {
            return None;
        }
        let variant = self
            .type_layouts
            .get(type_name)?
            .variants
            .get(variant_index)?;
        if variant.field_count() == 0 {
            Some(variant.clone())
        } else {
            None
        }
    }

    /// Whether `pat` would accept the scrutinee unconditionally given
    /// `scrut_ty`. Used by `lower_match` to short-circuit the test
    /// chain and skip building per-arm body/next blocks for a catch-
    /// all arm (mirrors the Plan A2 wildcard fast-path).
    fn is_catchall_pattern(&self, pat: &crate::ast::Pattern, scrut_ty: Option<&Ty>) -> bool {
        use crate::ast::Pattern;
        match pat {
            Pattern::Wildcard(_) => true,
            Pattern::Var(name, _) => self.nullary_ctor_promotion(name, scrut_ty).is_none(),
            _ => false,
        }
    }

    /// Walk `pat` and fill `out` with every Pattern::Var binding name
    /// mapped to its Cranelift type, as it would appear in the arm
    /// body. Mirrors `emit_pattern_test`'s binding logic without
    /// touching the IR builder.
    ///
    /// Nested constructor patterns descend into their fields; record
    /// patterns resolve to declared-order indices to pick the right
    /// field type.
    fn predict_pattern_bindings(
        &self,
        pat: &crate::ast::Pattern,
        scrut_ty: Option<&Ty>,
        out: &mut BTreeMap<String, Type>,
    ) {
        use crate::ast::{CtorPatternFields, Pattern};
        match pat {
            Pattern::Wildcard(_)
            | Pattern::IntLit(..)
            | Pattern::BoolLit(..)
            | Pattern::CharLit(..) => {}
            Pattern::Var(name, _) => {
                if self.nullary_ctor_promotion(name, scrut_ty).is_some() {
                    return;
                }
                let ty = match scrut_ty {
                    Some(t) => self.cranelift_ty_of(t),
                    None => types::I64,
                };
                out.insert(name.clone(), ty);
            }
            Pattern::Ctor { name, fields, .. } => {
                let (type_name, variant_index) = match self.ctor_index.get(name).cloned() {
                    Some(x) => x,
                    None => return,
                };
                let variant = match self
                    .type_layouts
                    .get(&type_name)
                    .and_then(|l| l.variants.get(variant_index))
                {
                    Some(v) => v.clone(),
                    None => return,
                };
                match fields {
                    CtorPatternFields::Unit => {}
                    CtorPatternFields::Positional(pats) => {
                        for (i, sub) in pats.iter().enumerate() {
                            if let Some(field_ty) = variant.field_tys.get(i) {
                                self.predict_pattern_bindings(sub, Some(field_ty), out);
                            }
                        }
                    }
                    CtorPatternFields::Record(pat_fields) => {
                        for f in pat_fields {
                            if let Some(idx) = variant.field_names.iter().position(|n| n == &f.name)
                            {
                                if let Some(field_ty) = variant.field_tys.get(idx) {
                                    self.predict_pattern_bindings(&f.pattern, Some(field_ty), out);
                                }
                            }
                        }
                    }
                }
            }
            Pattern::Tuple(_, _) => {}
        }
    }

    /// Cranelift representation of a semantic `Ty`. Mirrors the store/
    /// load width choices in `lower_ctor_alloc` and `load_field_value`.
    fn cranelift_ty_of(&self, ty: &Ty) -> Type {
        match ty {
            Ty::Int => types::I64,
            Ty::Bool | Ty::Byte | Ty::Unit => types::I8,
            Ty::Char => types::I32,
            Ty::String | Ty::Fn(_) | Ty::User(_, _) => self.pointer_ty,
            // Plan B task 48: surface-AST guard at codegen entry
            // ensures `Ty::Var` cannot reach this point. A stray
            // var means the guard is broken.
            Ty::Var(_) => unreachable!(
                "codegen: Ty::Var is impossible after Plan B task 48 codegen-entry guard"
            ),
        }
    }

    /// Structural Cranelift-type predictor. Used by `lower_match` to
    /// size the continue-block parameter before any arm body is
    /// emitted. Agrees with `lower_expr`'s emitted types by
    /// construction.
    ///
    /// `preview` overlays the normal env lookup with match-arm-local
    /// `Pattern::Var` bindings that are not yet installed in `self.env`
    /// — critical for nested matches whose first arm references a
    /// binding introduced by an outer match's arm.
    fn type_of_expr(&self, e: &crate::ast::Expr, preview: &BTreeMap<String, Type>) -> Type {
        use crate::ast::{BinOp, Expr, UnOp};
        match e {
            Expr::IntLit(..) => types::I64,
            Expr::BoolLit(..) => types::I8,
            Expr::Perform(p) => {
                // IO.println returns Unit (i8 0); non-IO performs
                // return the op's declared return type, which we
                // look up in the effect registry. Phase 3b only
                // supports Int returns; Phase 4+ extends.
                if p.effect == "IO" {
                    types::I8
                } else if let Some(eff) = self.effects.get(&p.effect) {
                    if let Some(op) = eff.ops.iter().find(|o| o.name == p.op) {
                        cranelift_ty_for_type_expr(&op.return_type, self.pointer_ty)
                    } else {
                        // Build-time invariant — typecheck E0043
                        // catches unknown ops.
                        types::I64
                    }
                } else {
                    // Build-time invariant — typecheck E0042 catches
                    // unknown effects.
                    types::I64
                }
            }
            Expr::CharLit(..) => types::I32,
            Expr::StringLit(..) | Expr::RecordLit { .. } => self.pointer_ty,
            Expr::Ident(name, _) => {
                if let Some(v) = self.env.get(name) {
                    self.builder.func.dfg.value_type(*v)
                } else if let Some(ty) = preview.get(name) {
                    *ty
                } else if self.ctor_index.contains_key(name) {
                    // Plan A3 task 41.1: a bare-ident nullary
                    // constructor allocates a heap record — result is
                    // a pointer.
                    self.pointer_ty
                } else {
                    unreachable!("type_of_expr: unknown ident `{name}`")
                }
            }
            Expr::Binary { op, .. } => match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => types::I64,
                _ => types::I8, // comparison / logic → Bool
            },
            Expr::Unary { op, .. } => match op {
                UnOp::Neg => types::I64,
                UnOp::Not => types::I8,
            },
            Expr::Match { arms, span, .. } => {
                // Propagate the preview down, extending with any pattern
                // bindings this inner match's first arm introduces so
                // nested-match result-type prediction sees them.
                let inner_scrut_ty = self.match_scrut_tys.get(span).cloned();
                let mut inner_preview = preview.clone();
                self.predict_pattern_bindings(
                    &arms[0].pattern,
                    inner_scrut_ty.as_ref(),
                    &mut inner_preview,
                );
                self.type_of_expr(&arms[0].body, &inner_preview)
            }
            Expr::If { then_block, .. } => match &then_block.tail {
                Some(t) => self.type_of_expr(t, preview),
                None => types::I8,
            },
            Expr::Block(b) => match &b.tail {
                Some(t) => self.type_of_expr(t, preview),
                None => types::I8,
            },
            Expr::Call { callee, .. } => match callee.as_ref() {
                // Plan A3 task 41.1: constructor application returns a
                // heap pointer to the newly-allocated user-type record.
                Expr::Ident(name, _)
                    if self.ctor_index.contains_key(name) && !self.user_fns.contains_key(name) =>
                {
                    self.pointer_ty
                }
                Expr::Ident(name, _) if self.user_fns.contains_key(name) => {
                    self.user_fns[name].ret_ty
                }
                // Plan A2 task 34: the `int_to_string` builtin returns
                // a Sigil `String`, which is a heap pointer.
                Expr::Ident(name, _) if name == "int_to_string" => self.pointer_ty,
                Expr::ClosureRecord { code_fn_name, .. } => self
                    .user_fns
                    .get(code_fn_name)
                    .map(|e| e.ret_ty)
                    .unwrap_or(types::I64),
                // Indirect calls don't exist in Plan A2 (see lower_call);
                // defensively return i64.
                _ => types::I64,
            },
            // Lambda type prediction is a Task-31/32 concern; closure
            // conversion ensures `Lambda` is always rewritten before
            // codegen sees it. Kept as a defensive fall-back.
            Expr::Lambda { .. } => self.pointer_ty,
            // Closure records are heap-allocated; a load from the
            // closure env uses the slot's kind to pick its Cranelift
            // type. Both land as real implementations in task 32.
            Expr::ClosureRecord { .. } => self.pointer_ty,
            Expr::ClosureEnvLoad { kind, .. } => match kind {
                crate::ast::EnvSlotKind::Int => types::I64,
                crate::ast::EnvSlotKind::Bool
                | crate::ast::EnvSlotKind::Byte
                | crate::ast::EnvSlotKind::Unit => types::I8,
                crate::ast::EnvSlotKind::Char => types::I32,
                crate::ast::EnvSlotKind::String
                | crate::ast::EnvSlotKind::Closure
                | crate::ast::EnvSlotKind::User => self.pointer_ty,
            },
            // Plan B Task 55 (Phase 2 minimum) — `handle BODY with {
            // arms }` lowers to `BODY` when the body contains no
            // non-IO perform (the codegen-entry guard
            // `unsupported_handle_construct` enforces this). The
            // handle's Cranelift type is therefore the body's type;
            // typecheck has already verified all arms unify with the
            // body via E0044/E0065. Phase 3+ will replace this with
            // a handler-overall computation reading from a typecheck-
            // populated side-table.
            Expr::Handle { body, .. } => self.type_of_expr(body, preview),
        }
    }
}

/// Cranelift trap codes Plan A2 uses. Values are in the user-trap
/// range (`TrapCode::user`). Plan B will rename these when the effect
/// runtime has a richer trap catalogue.
const TRAP_ARITH_ABORT: u8 = 0x40;
const TRAP_NONEXHAUSTIVE_MATCH: u8 = 0x41;

/// Best-effort PC-offset approximation for Stage 1's placeholder stackmap.
/// Cranelift's real stack-map API ships in Plan B; the number here is a
/// deterministic-enough integer that keeps the record format parseable.
fn function_code_offset(_b: &FunctionBuilder<'_>, call_inst: Inst) -> u32 {
    // Inst indices are stable within a function. Plan B will replace this
    // with the real post-regalloc code-offset Cranelift exposes via
    // CallSiteRelocInfo; for Stage 1 we keep it deterministic by using the
    // inst index.
    call_inst.as_u32()
}

fn isa_call_conv(_m: &ObjectModule) -> isa::CallConv {
    // System-V on Linux, AArch64 on macOS — both are what target_lexicon
    // picks as the default for the host; relying on the default keeps the
    // selection deterministic.
    isa::CallConv::triple_default(&Triple::host())
}

/// Plan B Task 55, Phase 4d / 4e — uniform CPS calling convention used
/// by every CPS-form fn callable from the trampoline:
///
/// ```text
/// extern "C" fn(closure_ptr: *const u8, args_ptr: *const u64, args_len: u32) -> *mut NextStep
/// ```
///
/// At HEAD this signature is shared by:
///
/// - **Synthetic handler-arm fns** (Phase 3b – 4d MVP) — one per
///   `Effect.op(args, k) => body` arm; declared in `emit_object`'s
///   pre-pass that walks `Expr::Handle` sites.
/// - **The runtime intrinsic `sigil_continuation_identity`** (Phase 4d
///   MVP) — packs its single u64 arg as a terminal `NextStep::Done`.
///   Codegen emits its address as `k_fn` at every non-IO perform site.
///
/// Phase 4e (in-flight on `plan-b-task-55-phase-4e`) extends usage to:
///
/// - **CPS-color user fns** — declared with this signature at the
///   user-fn pre-pass loop in `emit_object`, driven by
///   [`crate::color::ColoredProgram::cps_color_user_fns`].
/// - **Synthesised continuation closures** for non-tail-`k` arms and
///   non-tail-CPS-call yields in CPS-color user fn bodies — allocated
///   at the same pre-pass that allocates per-arm CPS fn synths today.
///
/// The shared helper exists so a future tweak to the CPS calling
/// convention (e.g., adding an `effect_id` param for tracing, or a
/// `flags` slot) lands in one place rather than four. Reusing it for
/// the user-fn signature pre-pass also makes the colorer-driven ABI
/// selection's call site read as `cps_signature(pointer_ty)` rather
/// than re-constructing the same shape inline — keeping the diff
/// localised when the upcoming Phase 4e codegen-consumes-color commit
/// adds the new call site.
fn cps_signature(pointer_ty: Type, module: &ObjectModule) -> Signature {
    let mut sig = Signature::new(isa_call_conv(module));
    sig.params.push(AbiParam::new(pointer_ty)); // closure_ptr
    sig.params.push(AbiParam::new(pointer_ty)); // args_ptr
    sig.params.push(AbiParam::new(types::I32)); // args_len
    sig.returns.push(AbiParam::new(pointer_ty)); // *mut NextStep
    sig
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;

    #[test]
    fn stackmap_record_size_is_eight() {
        assert_eq!(STACKMAP_RECORD_SIZE, 8);
    }

    #[test]
    fn stackmap_header_layout() {
        // Constants pin the shipped format. The single source is
        // `sigil_abi::stackmap`; any future v1 bump (Plan B Task 55+)
        // lands there, and both this builder and the runtime parser
        // pick it up automatically.
        assert_eq!(STACKMAP_MAGIC, b"SGST");
        assert_eq!(STACKMAP_VERSION_PLACEHOLDER, 0);
        assert_eq!(STACKMAP_HEADER_SIZE, 12);
        assert_eq!(STACKMAP_FLAG_PLACEHOLDER, 0x0001);
    }

    #[test]
    fn stackmap_builder_empty_serializes_to_header_only() {
        let b = StackMapBuilder::new();
        let bytes = b.serialize();
        assert_eq!(bytes.len(), STACKMAP_HEADER_SIZE);
        assert_eq!(&bytes[0..4], STACKMAP_MAGIC);
        assert_eq!(
            u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            STACKMAP_VERSION_PLACEHOLDER,
        );
        assert_eq!(
            u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            0,
        );
    }

    #[test]
    fn stackmap_builder_round_trips_placeholder_records() {
        let mut b = StackMapBuilder::new();
        b.push_placeholder(0x1111_2222);
        b.push_placeholder(0x3333_4444);
        let bytes = b.serialize();
        assert_eq!(b.len(), 2);
        assert_eq!(bytes.len(), STACKMAP_HEADER_SIZE + 2 * STACKMAP_RECORD_SIZE,);
        // Record 0.
        let r0 = STACKMAP_HEADER_SIZE;
        assert_eq!(
            u32::from_le_bytes([bytes[r0], bytes[r0 + 1], bytes[r0 + 2], bytes[r0 + 3]]),
            0x1111_2222,
        );
        assert_eq!(u16::from_le_bytes([bytes[r0 + 4], bytes[r0 + 5]]), 0);
        assert_eq!(
            u16::from_le_bytes([bytes[r0 + 6], bytes[r0 + 7]]),
            STACKMAP_FLAG_PLACEHOLDER,
        );
        // Record 1.
        let r1 = STACKMAP_HEADER_SIZE + STACKMAP_RECORD_SIZE;
        assert_eq!(
            u32::from_le_bytes([bytes[r1], bytes[r1 + 1], bytes[r1 + 2], bytes[r1 + 3]]),
            0x3333_4444,
        );
    }

    // ===== Plan B task 48 — codegen-entry walker tests =====

    #[test]
    fn walker_rejects_residual_apply_in_fn_param() {
        // A program whose AST carries a `TypeExpr::Apply` in a fn
        // param signature must round-trip the walker as `true`.
        // Constructed directly via AST types, independent of any
        // surface-syntax path that produces such a program.
        use crate::ast::{Block, FnDecl, Item, Param, Program, TypeExpr};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let prog = Program {
            file: "x.sigil".to_string(),
            items: vec![Item::Fn(Box::new(FnDecl {
                name: "f".to_string(),
                name_span: span.clone(),
                generic_params: Vec::new(),
                params: vec![Param {
                    name: "xs".to_string(),
                    ty: TypeExpr::Apply {
                        name: "List".to_string(),
                        args: vec![TypeExpr::Named("Int".to_string(), span.clone())],
                        span: span.clone(),
                    },
                    span: span.clone(),
                }],
                return_type: TypeExpr::Named("Int".to_string(), span.clone()),
                effects: Vec::new(),
                effect_row_var: None,
                body: Block {
                    stmts: Vec::new(),
                    tail: None,
                    span: span.clone(),
                },
                span: span.clone(),
            }))],
        };
        assert!(
            contains_apply_or_generic_ref(&prog),
            "walker must reject residual TypeExpr::Apply in fn param"
        );
    }

    #[test]
    fn walker_rejects_generic_param_decl() {
        use crate::ast::{Block, FnDecl, GenericParam, Item, Program, TypeExpr};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let prog = Program {
            file: "x.sigil".to_string(),
            items: vec![Item::Fn(Box::new(FnDecl {
                name: "id".to_string(),
                name_span: span.clone(),
                generic_params: vec![GenericParam {
                    name: "A".to_string(),
                    span: span.clone(),
                }],
                params: Vec::new(),
                return_type: TypeExpr::Named("Int".to_string(), span.clone()),
                effects: Vec::new(),
                effect_row_var: None,
                body: Block {
                    stmts: Vec::new(),
                    tail: None,
                    span: span.clone(),
                },
                span: span.clone(),
            }))],
        };
        assert!(
            contains_apply_or_generic_ref(&prog),
            "walker must reject fn with declared [A]"
        );
    }

    #[test]
    fn walker_rejects_generic_type_decl() {
        use crate::ast::{GenericParam, Item, Program, TypeDecl};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let prog = Program {
            file: "x.sigil".to_string(),
            items: vec![Item::Type(Box::new(TypeDecl {
                name: "List".to_string(),
                name_span: span.clone(),
                generic_params: vec![GenericParam {
                    name: "A".to_string(),
                    span: span.clone(),
                }],
                variants: Vec::new(),
                span: span.clone(),
            }))],
        };
        assert!(
            contains_apply_or_generic_ref(&prog),
            "walker must reject type with declared [A]"
        );
    }

    #[test]
    fn walker_accepts_concrete_program() {
        use crate::ast::{Block, FnDecl, Item, Program, TypeExpr};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let prog = Program {
            file: "x.sigil".to_string(),
            items: vec![Item::Fn(Box::new(FnDecl {
                name: "main".to_string(),
                name_span: span.clone(),
                generic_params: Vec::new(),
                params: Vec::new(),
                return_type: TypeExpr::Named("Int".to_string(), span.clone()),
                effects: Vec::new(),
                effect_row_var: None,
                body: Block {
                    stmts: Vec::new(),
                    tail: None,
                    span: span.clone(),
                },
                span: span.clone(),
            }))],
        };
        assert!(
            !contains_apply_or_generic_ref(&prog),
            "walker must accept fully-concrete program"
        );
    }

    #[test]
    fn walker_accepts_program_with_effect_decl() {
        // Plan B Task 55 — `Item::Effect` produces no codegen output
        // and the entry walker no longer short-circuits on it. This
        // test pins the new behavior: a program containing an
        // `effect` declaration alongside a concrete `main` walks
        // cleanly past the entry guard.
        use crate::ast::{Block, EffectDecl, EffectOp, FnDecl, Item, Program, TypeExpr};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let prog = Program {
            file: "x.sigil".to_string(),
            items: vec![
                Item::Effect(Box::new(EffectDecl {
                    name: "Raise".to_string(),
                    name_span: span.clone(),
                    generic_params: Vec::new(),
                    resumes_many: false,
                    ops: vec![EffectOp {
                        name: "fail".to_string(),
                        name_span: span.clone(),
                        params: vec![TypeExpr::Named("String".to_string(), span.clone())],
                        return_type: TypeExpr::Named("Int".to_string(), span.clone()),
                        span: span.clone(),
                    }],
                    span: span.clone(),
                })),
                Item::Fn(Box::new(FnDecl {
                    name: "main".to_string(),
                    name_span: span.clone(),
                    generic_params: Vec::new(),
                    params: Vec::new(),
                    return_type: TypeExpr::Named("Int".to_string(), span.clone()),
                    effects: Vec::new(),
                    effect_row_var: None,
                    body: Block {
                        stmts: Vec::new(),
                        tail: None,
                        span: span.clone(),
                    },
                    span: span.clone(),
                })),
            ],
        };
        assert!(
            !contains_apply_or_generic_ref(&prog),
            "walker must accept program with effect decl + concrete main"
        );
    }
}
