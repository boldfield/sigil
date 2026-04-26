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
use sigil_header_constants::{header_word, TAG_CLOSURE};

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
    for item in &program.items {
        if let crate::ast::Item::Fn(f) = item {
            if let Some(msg) = block_unsupported_handle(&f.body) {
                return Some(format!("in fn `{}`: {}", f.name, msg));
            }
        }
    }
    None
}

fn block_unsupported_handle(b: &crate::ast::Block) -> Option<String> {
    use crate::ast::Stmt;
    for s in &b.stmts {
        match s {
            Stmt::Let(l) => {
                if let Some(msg) = expr_unsupported_handle(&l.value) {
                    return Some(msg);
                }
            }
            Stmt::Expr(e) => {
                if let Some(msg) = expr_unsupported_handle(e) {
                    return Some(msg);
                }
            }
            Stmt::Perform(p) => {
                for a in &p.args {
                    if let Some(msg) = expr_unsupported_handle(a) {
                        return Some(msg);
                    }
                }
            }
        }
    }
    if let Some(tail) = &b.tail {
        if let Some(msg) = expr_unsupported_handle(tail) {
            return Some(msg);
        }
    }
    None
}

fn expr_unsupported_handle(e: &crate::ast::Expr) -> Option<String> {
    use crate::ast::Expr;
    match e {
        Expr::IntLit(..)
        | Expr::StringLit(..)
        | Expr::BoolLit(..)
        | Expr::CharLit(..)
        | Expr::Ident(..)
        | Expr::ClosureEnvLoad { .. } => None,
        Expr::Binary { lhs, rhs, .. } => {
            expr_unsupported_handle(lhs).or_else(|| expr_unsupported_handle(rhs))
        }
        Expr::Unary { operand, .. } => expr_unsupported_handle(operand),
        Expr::If {
            cond,
            then_block,
            else_block,
            ..
        } => expr_unsupported_handle(cond)
            .or_else(|| block_unsupported_handle(then_block))
            .or_else(|| block_unsupported_handle(else_block)),
        Expr::Block(b) => block_unsupported_handle(b),
        Expr::Match {
            scrutinee, arms, ..
        } => {
            if let Some(msg) = expr_unsupported_handle(scrutinee) {
                return Some(msg);
            }
            for a in arms {
                if let Some(msg) = expr_unsupported_handle(&a.body) {
                    return Some(msg);
                }
            }
            None
        }
        Expr::Call { callee, args, .. } => {
            if let Some(msg) = expr_unsupported_handle(callee) {
                return Some(msg);
            }
            for a in args {
                if let Some(msg) = expr_unsupported_handle(a) {
                    return Some(msg);
                }
            }
            None
        }
        Expr::Perform(_) => None,
        Expr::Lambda { body, .. } => expr_unsupported_handle(body),
        Expr::ClosureRecord { env_exprs, .. } => {
            for ee in env_exprs {
                if let Some(msg) = expr_unsupported_handle(ee) {
                    return Some(msg);
                }
            }
            None
        }
        Expr::RecordLit { fields, .. } => {
            for f in fields {
                if let Some(msg) = expr_unsupported_handle(&f.value) {
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
            // Phase 4b constraints (lifted incrementally as the
            // CPS path matures):
            //   - arm body is `Expr::IntLit` only (Phase 4c lifts
            //     via a CPS-aware lowerer that handles op-arg
            //     reads from `args_ptr`, `k` usage via
            //     `sigil_next_step_call`, and outer-scope captures
            //     through a closure record)
            //   - arms cannot reference `k` (Phase 4d lifts via
            //     continuation reification + lambda-lifting of the
            //     perform's continuation)
            //   - no return arm (Phase 4f lifts via a synthetic
            //     return-fn registered via
            //     sigil_handler_frame_set_return)
            //   - all arms reference the same effect (Phase 4e
            //     lifts via frame-per-effect)
            //   - body's non-IO performs only target the arm's
            //     effect (typecheck enforces this; codegen doesn't
            //     add an extra check)
            // Phase 4b LIFTED: arms may declare user params; non-
            // IO performs in the body may pass user args. Args are
            // packed by `lower_perform_non_io_to_value` into a
            // stack-allocated u64 buffer.
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
            // Phase 3b restriction (still in force in 4a): each arm
            // body must be `Expr::IntLit` (no captures, no `k` use,
            // no op-arg use). The synthetic-fn definition pass
            // lowers IntLit-only bodies via a hand-rolled Cranelift
            // sequence; richer bodies need a CPS-aware lowerer
            // arriving in Phase 4c.
            for arm in op_arms.iter() {
                match &arm.body {
                    crate::ast::Expr::IntLit(..) => {}
                    _ => {
                        return Some(format!(
                            "`handle` expression at {:?} has arm `{}.{}` body \
                             that is not a literal `Int` — Phase 3b/4a only \
                             support arms whose body is an `IntLit` (Plan B \
                             Task 55, in progress; richer arm bodies arrive \
                             in Phase 4c)",
                            span, arm.effect, arm.op
                        ));
                    }
                }
            }
            // Phase 4b lifts the previous "no user op-args" restriction.
            // Arms may now declare user params and perform sites in the
            // body may pass user args; they're packed by
            // `lower_perform_non_io_to_value` into a stack-allocated u64
            // buffer and read by `sigil_perform` (which copies them into
            // the dispatched `NextStep::Call`'s args slots before the
            // arm fn runs). The arm fn's body is still IntLit-only
            // (Phase 4c lifts that), so the arm fn currently ignores
            // the args_ptr it receives — but the FFI plumbing now
            // carries the packed buffer end-to-end so Phase 4c can
            // wire arg-binding consumption without re-touching the
            // perform side.
            // Recurse into the body itself so a nested handle inside
            // the body (e.g. `handle (handle ... with { ... }) with
            // { ... }`) surfaces its own diagnostics. Without this,
            // the inner handle's multi-effect / non-IntLit / return-arm
            // restrictions are never enforced — at runtime that can
            // register arms under the wrong effect_id and crash inside
            // `sigil_perform`'s handler-stack walk.
            if let Some(msg) = expr_unsupported_handle(body) {
                return Some(msg);
            }
            // Recurse into arm bodies so nested handles deeper in
            // the AST surface their own diagnostics.
            for arm in op_arms {
                if let Some(msg) = expr_unsupported_handle(&arm.body) {
                    return Some(msg);
                }
            }
            None
        }
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
    body: crate::ast::Expr,
}

/// Walk a block looking for `Expr::Handle` sites and allocating
/// synthetic CPS-fn metadata for each arm. Recurses into nested
/// expressions so handles inside nested blocks / matches / lambdas
/// also surface. The pre-pass is deliberately conservative — it
/// allocates `FuncId`s for every arm of every reachable handle, even
/// arms whose `Expr::Handle` site might end up dead-code-eliminated
/// by some future optimisation. Codegen never optimises handles
/// today, so over-allocation here is harmless.
fn collect_handle_arms_in_block(
    b: &crate::ast::Block,
    module: &mut ObjectModule,
    cps_arm_sig: &Signature,
    op_ids: &BTreeMap<(String, String), u32>,
    synth: &mut Vec<HandlerArmSynth>,
    indices: &mut BTreeMap<Span, Vec<usize>>,
) -> Result<(), String> {
    use crate::ast::Stmt;
    for s in &b.stmts {
        match s {
            Stmt::Let(l) => {
                collect_handle_arms_in_expr(&l.value, module, cps_arm_sig, op_ids, synth, indices)?;
            }
            Stmt::Expr(e) => {
                collect_handle_arms_in_expr(e, module, cps_arm_sig, op_ids, synth, indices)?;
            }
            Stmt::Perform(p) => {
                for a in &p.args {
                    collect_handle_arms_in_expr(a, module, cps_arm_sig, op_ids, synth, indices)?;
                }
            }
        }
    }
    if let Some(tail) = &b.tail {
        collect_handle_arms_in_expr(tail, module, cps_arm_sig, op_ids, synth, indices)?;
    }
    Ok(())
}

fn collect_handle_arms_in_expr(
    e: &crate::ast::Expr,
    module: &mut ObjectModule,
    cps_arm_sig: &Signature,
    op_ids: &BTreeMap<(String, String), u32>,
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
            collect_handle_arms_in_expr(lhs, module, cps_arm_sig, op_ids, synth, indices)?;
            collect_handle_arms_in_expr(rhs, module, cps_arm_sig, op_ids, synth, indices)
        }
        Expr::Unary { operand, .. } => {
            collect_handle_arms_in_expr(operand, module, cps_arm_sig, op_ids, synth, indices)
        }
        Expr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            collect_handle_arms_in_expr(cond, module, cps_arm_sig, op_ids, synth, indices)?;
            collect_handle_arms_in_block(then_block, module, cps_arm_sig, op_ids, synth, indices)?;
            collect_handle_arms_in_block(else_block, module, cps_arm_sig, op_ids, synth, indices)
        }
        Expr::Block(b) => {
            collect_handle_arms_in_block(b, module, cps_arm_sig, op_ids, synth, indices)
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            collect_handle_arms_in_expr(scrutinee, module, cps_arm_sig, op_ids, synth, indices)?;
            for a in arms {
                collect_handle_arms_in_expr(&a.body, module, cps_arm_sig, op_ids, synth, indices)?;
            }
            Ok(())
        }
        Expr::Call { callee, args, .. } => {
            collect_handle_arms_in_expr(callee, module, cps_arm_sig, op_ids, synth, indices)?;
            for a in args {
                collect_handle_arms_in_expr(a, module, cps_arm_sig, op_ids, synth, indices)?;
            }
            Ok(())
        }
        Expr::Lambda { body, .. } => {
            collect_handle_arms_in_expr(body, module, cps_arm_sig, op_ids, synth, indices)
        }
        Expr::ClosureRecord { env_exprs, .. } => {
            for ee in env_exprs {
                collect_handle_arms_in_expr(ee, module, cps_arm_sig, op_ids, synth, indices)?;
            }
            Ok(())
        }
        Expr::RecordLit { fields, .. } => {
            for f in fields {
                collect_handle_arms_in_expr(&f.value, module, cps_arm_sig, op_ids, synth, indices)?;
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
            collect_handle_arms_in_expr(body, module, cps_arm_sig, op_ids, synth, indices)?;
            for arm in op_arms {
                collect_handle_arms_in_expr(
                    &arm.body,
                    module,
                    cps_arm_sig,
                    op_ids,
                    synth,
                    indices,
                )?;
            }
            // Allocate one synthetic CPS fn per arm. Linker symbol
            // is `sigil_handler_arm_<global_index>` to keep names
            // unique without needing per-handle counters.
            let mut arm_indices: Vec<usize> = Vec::with_capacity(op_arms.len());
            for arm in op_arms {
                let global_idx = synth.len();
                let mangled = format!("sigil_handler_arm_{global_idx}");
                let func_id = module
                    .declare_function(&mangled, Linkage::Local, cps_arm_sig)
                    .map_err(|e| format!("declare {mangled}: {e}"))?;
                // Validate that the op_id is registered (op_ids
                // populated at end of typecheck for every effect's
                // ops). Unused at this site — the per-arm op_id is
                // looked up again at the Expr::Handle codegen site
                // — but failing fast here gives a clearer error
                // message than a unwrap deep inside lowering.
                let _ = op_ids
                    .get(&(arm.effect.clone(), arm.op.clone()))
                    .ok_or_else(|| {
                        format!(
                            "codegen pre-pass: op_id missing for `{}.{}` — typecheck-time \
                         E0138/E0139 should have caught this",
                            arm.effect, arm.op
                        )
                    })?;
                synth.push(HandlerArmSynth {
                    func_id,
                    body: arm.body.clone(),
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
    let checked: &CheckedProgram = &cc.cps.colored.mono.anf.checked;

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
        let mut cps_arm_sig = Signature::new(isa_call_conv(&module));
        cps_arm_sig.params.push(AbiParam::new(pointer_ty)); // closure_ptr
        cps_arm_sig.params.push(AbiParam::new(pointer_ty)); // args_ptr
        cps_arm_sig.params.push(AbiParam::new(types::I32)); // args_len
        cps_arm_sig.returns.push(AbiParam::new(pointer_ty)); // *mut NextStep
        for item in &checked.program.items {
            if let crate::ast::Item::Fn(f) = item {
                collect_handle_arms_in_block(
                    &f.body,
                    &mut module,
                    &cps_arm_sig,
                    &checked.op_ids,
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

    // --- Plan B Task 55 (Phase 3b): synthetic handler-arm CPS fns ------
    //
    // Each entry in `handler_arm_synth` was allocated a `FuncId` by
    // the pre-pass; here we define each fn's body. Every arm fn has
    // the uniform CPS calling convention `extern "C" fn(closure_ptr,
    // args_ptr, args_len) -> *mut NextStep`. Phase 3b restricts arm
    // bodies to literal `Expr::IntLit` (the `unsupported_handle_construct`
    // walker enforces this); the body computes the literal value,
    // wraps it via `sigil_next_step_done(value)`, and returns the
    // resulting NextStep pointer. Phase 4+ will lower richer arm
    // bodies through a dedicated CPS-aware lowerer.
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

                // Phase 3b restriction: arm body must be IntLit. The
                // codegen-entry guard rejects everything else, so this
                // is a build-time invariant.
                let value: i64 = match &synth.body {
                    crate::ast::Expr::IntLit(n, _) => *n,
                    other => unreachable!(
                        "codegen Phase 3b: arm body should be IntLit per \
                         unsupported_handle_construct guard; got {other:?}"
                    ),
                };
                let value_v = builder.ins().iconst(types::I64, value);
                let next_step_done_ref = module.declare_func_in_func(next_step_done, builder.func);
                let done_call = builder.ins().call(next_step_done_ref, &[value_v]);
                // TODO(Plan B Task 55, Phase 4c): no stackmap entry
                // for this `sigil_next_step_done` call. Safe today
                // only because (a) `closure_ptr` arg is null and (b)
                // an `IntLit` arm body has no GC roots. Once Phase 4c
                // lands richer arm bodies with captures, `closure_ptr`
                // becomes a live GC root across this call site and
                // any roots in scope must be threaded into the
                // stackmap (mirroring the `Lowerer::stackmap.push_*`
                // pattern at every other arena-allocating call). At
                // that point this synthetic-fn path needs to use the
                // full Lowerer machinery rather than the hand-rolled
                // sequence below.
                let next_step_ptr = builder.inst_results(done_call)[0];
                builder.ins().return_(&[next_step_ptr]);
                builder.finalize();
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

        // PHASE-4-RESTRICTION (Plan B Task 55, Phase 4d):
        // `null_k_closure` + `null_k_fn` pin the no-k-usage
        // restriction. Phase 4d reifies the perform's continuation
        // (the rest of computation after the perform site) into a
        // CPS-color closure-fn pair and passes it here so the arm can
        // invoke `k(value)` to resume. Until then arms ignore `k`
        // (single-shot Raise-style early-exit), and the codegen-entry
        // guard's IntLit-only-arm-body check enforces that.
        let null_k_closure = self.builder.ins().iconst(self.pointer_ty, 0);
        let null_k_fn = self.builder.ins().iconst(self.pointer_ty, 0);
        let perform_call = self.builder.ins().call(
            self.perform_ref,
            &[
                effect_id_v,
                op_id_v,
                args_ptr,
                args_len_v,
                null_k_closure,
                null_k_fn,
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
        // Returns u64 — cast to i64 for native consumption.
        let run_loop_call = self
            .builder
            .ins()
            .call(self.run_loop_ref, &[call_next_step]);
        self.stackmap
            .push_placeholder(function_code_offset(&self.builder, run_loop_call));
        self.builder.inst_results(run_loop_call)[0]
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
                // PHASE-4-RESTRICTION (Plan B Task 55, Phase 4b/4c):
                // null `closure_ptr` for every arm slot. Correct
                // today because Phase 3b restricts arm bodies to
                // `IntLit` only — no captures, no op-arg use, no `k`
                // use, so the synthetic arm fn doesn't read its
                // closure pointer. Phase 4b adds op-arg unpacking,
                // 4c adds richer arm bodies (which may reference
                // outer-scope captures), and 4d adds `k`-reifying
                // arms — each requires a real closure record
                // threaded through here so the synthetic fn can
                // recover its environment. The codegen-entry guard
                // currently rejects all three shapes; lifting the
                // guard MUST land alongside changes to this slot.
                let null_ptr = self.builder.ins().iconst(self.pointer_ty, 0);
                for (arm, fn_ref) in op_arms.iter().zip(arm_refs.iter()) {
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
                    let set_call = self.builder.ins().call(
                        self.handler_frame_set_arm_ref,
                        &[frame_ptr, op_id_v, fn_ptr_v, null_ptr],
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
            env_len < 31,
            "closure env >= 31 slots exceeds 6-bit header count field (tag 0xFF descriptor is v2)"
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
