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
use crate::color::ColoredProgram;
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
    /// Plan B Task 55, Phase 4e — selected calling convention for
    /// this fn. Populated by [`compute_user_fn_abi`] at the pre-pass
    /// loop in `emit_object`.
    ///
    /// Native-ABI fns use the existing closure-convention signature
    /// `(closure_ptr, user_arg1, ..., user_argN) -> ret_ty`.
    /// CPS-ABI fns use the uniform CPS calling convention
    /// `(closure_ptr, args_ptr, args_len) -> *mut NextStep` (per
    /// [`cps_signature`]).
    ///
    /// Consumed by the user-fn pre-pass loop in `emit_object` to
    /// drive signature selection: `UserFnAbi::Sync` fns get the
    /// existing closure-convention signature; `UserFnAbi::Cps` fns
    /// get [`cps_signature`]. The body emit pass branches on this
    /// field too — `Cps` fns get the CPS body shape (perform site
    /// returning `*mut NextStep` directly); `Sync` fns get the
    /// existing native body lowering. The native-of-CPS call site
    /// wrapper at `lower_call` consults this field on the callee's
    /// `UserFnEntry` to decide whether to emit the inlined
    /// run_loop driver.
    abi: UserFnAbi,
}

/// Plan B Task 55, Phase 4e — calling-convention selection for a
/// user-defined fn.
///
/// **Naming note (PR #26 should-fix #3 at `a2840b6`):** these
/// variants name the *runtime calling shape*, not the colorer's
/// classification. A fn classified [`crate::color::Color::Cps`]
/// can still get `UserFnAbi::Sync` (when its body shape isn't
/// supported by the CPS body lowering yet — e.g., a CPS-color
/// `main` with multiple stmts). The two namespaces are
/// deliberately orthogonal: `Color` is what the analysis says,
/// `UserFnAbi` is what codegen will actually emit. Don't conflate
/// `UserFnAbi::Sync` with `Color::Native`.
///
/// `Sync` corresponds to the existing closure-convention signature
/// every user fn has used since Plan A2 task 32:
/// `(closure_ptr, user_arg1, ..., user_argN) -> ret_ty`. The fn
/// body is lowered through the standard [`Lowerer`] path, with
/// `lower_perform_non_io_to_value` driving `sigil_run_loop`
/// synchronously at non-IO perform sites (the Phase 4d MVP shape).
/// All `Color::Native` fns and any `Color::Cps` fn whose body
/// shape isn't supported by the CPS body lowering yet get this
/// ABI and remain on the synchronous path.
///
/// `Cps` corresponds to the uniform CPS calling convention
/// (`cps_signature`): `(closure_ptr, args_ptr, args_len) -> *mut
/// NextStep`. The fn body emits a `NextStep` directly — for the
/// `is_simple_tail_perform_with_pure_args_body` shape, this is
/// just `sigil_perform(...)` returning its NextStep. The fn's
/// caller drives `sigil_run_loop` (when called from a `Sync` fn
/// via the inlined wrapper) or returns the NextStep up to the
/// surrounding trampoline (when called from a `Cps` fn).
///
/// Selection rule (per [`compute_user_fn_abi`]): a fn is `Cps` iff
/// it is colored CPS by [`crate::color::ColoredProgram`] AND its
/// body matches a shape codegen can lower in CPS form (initially:
/// only [`is_simple_tail_perform_with_pure_args_body`]). All
/// other fns — including CPS-color fns whose body shape isn't
/// yet supported — get `Sync` and continue to use the
/// synchronous-`run_loop` path. The synchronous path is correct
/// under tail-position perform shapes (the Phase 4d MVP guarantee)
/// but has the discard-`k` cross-call-boundary correctness gap
/// that Phase 4e closes once the lambda-lifting machinery (a
/// later commit on this branch) covers more body shapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UserFnAbi {
    Sync,
    Cps,
}

/// Plan B Task 55, Phase 4e — decide which calling convention a
/// user fn should be emitted under.
///
/// Returns [`UserFnAbi::Cps`] iff `name` is colored CPS by `colored`
/// AND `body` matches a shape codegen can lower in CPS form. At
/// HEAD that shape is exactly [`is_simple_tail_perform_with_pure_args_body`] —
/// future commits widen the predicate as the body lowering covers
/// more shapes (stmts before tail perform, conditional/match in
/// tail position, eventually arbitrary CPS-color bodies via the
/// lambda-lifting machinery).
///
/// Conservative: false negatives are acceptable (a CPS-color fn
/// gets `Sync` ABI and uses the synchronous-`run_loop` path,
/// preserving the Phase 4d MVP behaviour); false positives are not
/// (would cause incomplete CPS-form codegen output OR — pre-D1-fix
/// — a hard `assert!`-crash in the body emit branch on otherwise-
/// valid Sigil programs).
///
/// **D1 history (PR #26 mid-flight at `33f2231`):** the
/// `33f2231` slice gated on color + body shape only, missing the
/// downstream codegen restrictions on user-fn arity and tail-perform
/// arg count. The fix at `5a0459a` added arity-0 gates here, so a
/// fn like `fn helper(x: Int) -> Int ![E] { perform E.op(x) }`
/// would fall through to `Sync` cleanly instead of crashing at
/// body emit. **This commit removes those arity gates** alongside
/// the body emission and caller wrapper widening — arity-N user
/// fns + arity-N pure-args performs are now CPS-eligible.
///
/// Order of checks matters: color first (cheap), then body shape
/// (also cheap). A reordering that swapped them would change
/// observable behavior for the `compute_user_fn_abi_sync_when_
/// color_native_short_circuits_regardless_of_body_shape` test
/// pinning.
fn compute_user_fn_abi(
    name: &str,
    body: &crate::ast::Block,
    colored: &ColoredProgram,
) -> UserFnAbi {
    if !colored.needs_cps_transform(name) {
        return UserFnAbi::Sync;
    }
    if is_simple_tail_perform_with_pure_args_body(body)
        || is_simple_yield_then_constant_tail_body(body)
    {
        return UserFnAbi::Cps;
    }
    // Let-yield-then-pure-tail shape — captures-bearing slice
    // (this commit). Lifts the prior arity-0 restriction: helpers
    // with user params referenced in the tail are now CPS-eligible
    // because the synth-cont can capture them via closure record.
    // The pre-pass populates `CpsContinuationKind::LetBindThenTail
    // .captures` via `collect_synth_cont_captures`; helper's body
    // emit allocates the closure record at the perform site and
    // passes its pointer as `k_closure`.
    if is_simple_let_yield_then_pure_tail_body(body) {
        return UserFnAbi::Cps;
    }
    UserFnAbi::Sync
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
    // Plan B Task 55, Phase 4e captures+ Slice B — non-tail-`k` arm
    // body of shape `{ let r: Ty = k(arg); pure_tail }` is allowed
    // when:
    //   - `arg` satisfies `expr_is_pure` (no nested calls / yields).
    //   - `pure_tail` satisfies `expr_is_pure`.
    //   - `pure_tail` references only `r` and globals (no op-args,
    //     no outer-scope captures, no `k` reference).
    //
    // The pre-pass allocates a post-arm-k synth fn for matching arms
    // and the synth-arm-fn body emit takes the non-tail-`k` path
    // emitting `Call(k_closure, k_fn, [arg, null,
    // post_arm_k_fn_addr])`.
    if let Some(shape) = arm_body_let_then_pure_tail_shape(&arm.body, &arm.k_name) {
        if !expr_is_pure(shape.arg_expr) {
            // Fall through to the regular walker so the diagnostic
            // it emits points at the specific yield-able sub-shape
            // in `arg`. Not an early return.
        } else if !expr_is_pure(shape.tail_expr) {
            // Same: let the regular walker surface the specific
            // yield-able sub-shape in `tail`.
        } else if let Some(diag) = arm_body_post_arm_k_tail_free_vars_ok(
            shape.tail_expr,
            shape.binding_name,
            &arm.k_name,
            globals,
        ) {
            return Some(diag);
        } else {
            // Slice B accepted shape: `arg` pure, `tail` pure,
            // `tail` free vars ⊆ `{r} ∪ globals`. Allow.
            return None;
        }
    }
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
                collect_pattern_bindings(&a.pattern, &mut pat_scope);
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
    /// Plan B Task 55, Phase 4e captures+ Slice B — set when the arm
    /// body matches the `{ let r: Ty = k(arg); pure_tail }` shape
    /// recognised by [`arm_body_let_then_pure_tail_shape`]. The pre-
    /// pass allocates a separate `FuncId` for the post-arm-k synth
    /// fn (the lambda-lifted `pure_tail` continuation); the synth-
    /// arm-fn body emit takes a non-tail-`k` path that emits
    /// `Call(k_closure, k_fn, [arg, null_post_arm_k_closure,
    /// post_arm_k_fn_addr])`; the post-arm-k synth fn's body, defined
    /// in its own definition pass at the bottom of `emit_object`,
    /// reads `r` from `args_ptr[0]` and lowers `pure_tail` to
    /// `Done(result)`. `None` for tail-`k` arms or for arms whose
    /// body doesn't match the let-then-pure-tail shape.
    post_arm_k: Option<PostArmKSynth>,
}

/// Plan B Task 55, Phase 4e captures+ Slice B — synthetic post-arm-k
/// continuation for an arm body of shape `{ let r: Ty = k(arg);
/// pure_tail }`. Built by the pre-pass when
/// [`arm_body_let_then_pure_tail_shape`] matches; consumed by the
/// post-arm-k synth fn definition pass at the bottom of `emit_object`.
///
/// Slice B first-commit restrictions:
///   - `pure_tail` must reference only `r` and globals (no op-args
///     and no outer-scope captures yet — those need a closure-record
///     allocation site at the arm-fn that mirrors the helper-side
///     captures-bearing slice from PR #26's `a5ee4c6`).
///   - `arg_expr` must be pure (no nested `k(arg)`-of-`k(arg)`,
///     no Call, no Perform).
///
/// The `func_id` is the post-arm-k synth fn's FuncId. The arm-fn body
/// emit emits `Call(k_closure, k_fn, [arg_value, null_pointer,
/// func_addr(func_id)])` so the helper's synth-cont (Slice A) reads
/// args_ptr[1..3] and dispatches into this fn with `[result]`.
#[derive(Clone, Debug)]
struct PostArmKSynth {
    /// Linker symbol name `sigil_handler_post_arm_k_<global_index>`;
    /// allocated in the pre-pass alongside the arm-fn's `FuncId`.
    func_id: cranelift_module::FuncId,
    /// Source-level binding name (the `r` in `let r = k(arg);
    /// pure_tail`). Bound in the post-arm-k synth fn's env at fn
    /// entry from `args_ptr[0]`, narrowed per [`Self::binding_ty`].
    binding_name: String,
    /// Cranelift type the binding is narrowed to at fn entry. Derived
    /// from the `LetStmt::ty` via [`cranelift_ty_for_type_expr`].
    binding_ty: Type,
    /// The `arg` expression in `k(arg)`. Lowered in the arm-fn body
    /// emit's non-tail-`k` path. Must satisfy [`expr_is_pure`].
    arg_expr: crate::ast::Expr,
    /// The `pure_tail` expression that the post-arm-k synth fn
    /// lowers. Must satisfy [`expr_is_pure`] and reference only
    /// `binding_name` plus globals. The post-arm-k synth fn returns
    /// `Done(widen(tail_value, I64))`.
    tail_expr: crate::ast::Expr,
    /// Cranelift type of `tail_expr`'s lowered value, used to widen
    /// to I64 before `sigil_next_step_done`. Equals the arm body's
    /// overall result type (since the let-binding shadows the
    /// perform's value into the tail), which for Slice B equals the
    /// arm's `body_ty` (the op's declared return type).
    tail_ty: Type,
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

/// Plan B Task 55, Phase 4e — synthetic continuation closure for a
/// CPS-color user fn whose body matches the **stmt-then-constant-
/// tail** shape (per [`is_simple_yield_then_constant_tail_body`]).
///
/// Each entry represents one synthesised fn that codegen emits as
/// a standalone CPS-ABI fn: the parent helper's body builds
/// `sigil_perform(..., k_fn = &this_synth_cont)`, and the trampoline
/// dispatches into `this_synth_cont` when the arm calls `k(value)`.
///
/// First-slice constraints (this commit's restriction):
///   - The synth-cont's body is just `Done(constant)` — no captures
///     of helper's user params or k_closure / k_fn yet (those need
///     the closure-convert side-table extension, future commit).
///   - The constant is restricted to [`crate::ast::Expr::IntLit`]
///     in this slice; future commits widen to other literals.
///
/// **Forward concerns (PR #26 mid-flight at `b818fc3`)** for the
/// captures-bearing slice (next major commit):
///
/// 1. **Synth-cont's binding-behavior shift.** The current
///    "k_arg discarded" semantics are correct for `Stmt::Perform;
///    constant_tail` because the source has nowhere to bind the
///    perform's result. Future shapes need different binding —
///    for `let x = perform E.op(); rest_using_x` the synth-cont
///    binds `args_ptr[0]` (the k_arg) as `x` in its env, then
///    runs `rest_using_x` with `x` in scope; for `perform E.op();
///    rest_using_no_binding` the synth-cont ignores `args_ptr[0]`
///    and runs `rest_using_no_binding` (with captures for any
///    referenced names if `rest_using_no_binding` isn't
///    constant). Per the reviewer: "the synth-cont's binding
///    behavior must shift from 'discard k's arg' to 'bind k's
///    arg as the perform's result name' depending on whether the
///    source has a `let x = perform ...` form."
///
/// 2. **Lowerer-driven body emission.** The current synth-cont
///    body is hand-rolled (just iconst + sigil_next_step_done +
///    return). Future shapes need a Lowerer with env populated
///    from captures + the k_arg binding (for the let-form case).
///    Per the reviewer: "Does the constrained-context Lowerer
///    reuse extend cleanly to that, or does the lambda-lifting
///    commit need a full Lowerer construction with the standard
///    pipeline?" Verify in the captures-slice's review.
///
/// 3. **Closure-convert side-table extension.** Synth-conts
///    capturing user params or let-bindings from the parent fn
///    need closure records. Closure_convert ran before codegen,
///    so synth-conts bypass that pipeline — they need their own
///    capture-record allocation alongside the FuncId pre-pass.
///    Mirrors the `HandlerArmSynth.captures` pattern from Phase
///    4d MVP.
#[derive(Clone, Debug)]
struct CpsContinuationSynth {
    /// Cranelift FuncId for this synthesised continuation. Allocated
    /// at the user-fn pre-pass alongside the user fn's FuncId so
    /// the user-fn body emit can reference it via `func_addr` when
    /// building the `sigil_perform` call's `k_fn` arg.
    func_id: cranelift_module::FuncId,
    /// The user fn this synth-cont belongs to. Keys into the
    /// `cps_continuation_synth_indices` map for body-emit-time
    /// FuncId lookup.
    parent_fn_name: String,
    /// Discriminates between body-shape variants (constant-tail vs
    /// let-bind-then-tail). The synth-cont definition pass at the
    /// bottom of `emit_object` matches on this to pick the right
    /// emission strategy.
    kind: CpsContinuationKind,
}

/// Plan B Task 55, Phase 4e — one captured user-param of the parent
/// helper that the synth-cont closure record holds. Mirrors the
/// shape of [`ArmCapture`] used by Phase 4d's per-arm closure
/// records — we keep a separate struct (rather than aliasing) so
/// the captures-bearing slice's intent is explicit at the type
/// level: synth-cont captures come from the parent helper's user
/// params via free-var analysis on the tail expression, not from
/// the typechecker's `handle_arm_captures` side-table.
///
/// At HEAD the captures only cover helper's user params (free names
/// in the tail that resolve to a `Param`). Future widenings may
/// add captures from let-bindings in multi-stmt bodies (chained
/// synth-conts), or from surrounding-lambda closures (the
/// closure-convert side-table extension).
#[derive(Clone, Debug)]
struct SynthContCapture {
    /// Source-level name (matches a helper param's name).
    name: String,
    /// Slot kind for the closure-record encoding; drives bitmap
    /// + load/store widening.
    kind: EnvSlotKind,
}

/// Plan B Task 55, Phase 4e — discriminates the body-shape variants
/// of a [`CpsContinuationSynth`].
#[derive(Clone, Debug)]
enum CpsContinuationKind {
    /// **Constant-tail shape** (per
    /// [`is_simple_yield_then_constant_tail_body`]): the synth-cont
    /// emits `Done(constant_value)` ignoring `args_ptr[0]` (the
    /// k_arg). Used when the parent helper's body is `Stmt::Perform;
    /// IntLit(N)` — the perform's value is discarded by the Stmt,
    /// and the tail is a compile-time literal.
    ConstantDone {
        /// The literal value to wrap as `Done(...)`. At HEAD this
        /// is always an `i64` (IntLit value); future widenings
        /// support BoolLit / CharLit / StringLit.
        constant_value: i64,
    },
    /// **Let-bind-then-tail shape** (per
    /// [`is_simple_let_yield_then_pure_tail_body`]): the synth-cont
    /// reads `args_ptr[0]` as the let-binding's value, binds it in
    /// a fresh Lowerer's env under `binding_name`, loads any
    /// `captures` from `closure_ptr`, lowers `tail_expr` via
    /// Lowerer (which can reference `binding_name`, captured user
    /// params, and other pure shapes), widens the result to I64,
    /// returns `Done(value)`. Used when the parent helper's body
    /// is `let name = perform ...; tail_expr` and the tail is a
    /// pure expression that may reference `name` and/or helper's
    /// user params.
    ///
    /// **Captures-bearing extension (this commit's slice)**: the
    /// `captures` list enumerates user params of the parent helper
    /// that the tail expression references by name. The synth-cont
    /// reads them from `closure_ptr` at fn entry (via the same
    /// `lower_closure_env_load` machinery user lambdas use). The
    /// parent helper's body emit allocates the closure record at
    /// the perform site and passes its pointer as `k_closure` to
    /// `sigil_perform`. When `captures` is empty, the parent
    /// passes null `k_closure` (no allocation).
    LetBindThenTail {
        /// Source-level name of the let-binding (the name `args_ptr
        /// [0]` is bound to in the synth-cont's env).
        binding_name: String,
        /// Cranelift type the let-binding declares. The synth-cont
        /// loads `args_ptr[0]` as `i64` then `ireduce`s back to
        /// this type for binding (mirrors the user-arg unpack
        /// discipline at the parent helper's body emit).
        binding_ty: Type,
        /// The post-yield rest-of-body that the synth-cont lowers
        /// via Lowerer.lower_expr. Pure per the classifier; may
        /// reference `binding_name` and any of `captures`. Boxed
        /// because `Expr` has a large representation and clippy
        /// flags `clippy::large_enum_variant` on the unboxed
        /// shape.
        tail_expr: Box<crate::ast::Expr>,
        /// Cranelift type of the tail expression's value. Used to
        /// widen to I64 before wrapping in `Done(...)`.
        tail_ty: Type,
        /// Captures of the parent helper's user params that the
        /// tail expression references. Computed via free-var
        /// analysis at the user-fn pre-pass. Empty when the
        /// helper is arity-0 OR when the tail doesn't reference
        /// any user params (e.g., constant tail). When empty, the
        /// parent helper passes null `k_closure`; when non-empty,
        /// the parent allocates a closure record holding the
        /// captures and passes its pointer.
        captures: Vec<SynthContCapture>,
    },
}

/// Plan B Task 55, Phase 4e — derive an [`EnvSlotKind`] from a
/// surface-syntax [`crate::ast::TypeExpr`]. Used at the synth-cont
/// pre-pass to compute `SynthContCapture::kind` from a parent
/// helper's parameter declarations.
///
/// Mirrors [`crate::closure_convert::slot_kind_for_ty`] but works
/// on the post-monomorphization `TypeExpr` rather than the
/// typechecker's `Ty` (which the codegen-side pre-pass doesn't
/// have direct access to). `TypeExpr::Apply` is rejected at the
/// codegen-entry walker before this function runs (per Plan B
/// Task 49's monomorphization invariant), so we only need to
/// handle `TypeExpr::Named`. The name resolution mirrors
/// `cranelift_ty_for_type_expr`'s primitive-name table; non-
/// primitive names are treated as user-type pointers.
fn slot_kind_for_type_expr_post_mono(te: &crate::ast::TypeExpr) -> EnvSlotKind {
    match te {
        crate::ast::TypeExpr::Named(name, _) => match name.as_str() {
            "Int" => EnvSlotKind::Int,
            "Bool" => EnvSlotKind::Bool,
            "Char" => EnvSlotKind::Char,
            "Byte" => EnvSlotKind::Byte,
            "Unit" => EnvSlotKind::Unit,
            "String" => EnvSlotKind::String,
            // User-defined types (and any monomorphized name from
            // mangle_type) are pointer-typed.
            _ => EnvSlotKind::User,
        },
        crate::ast::TypeExpr::Apply { .. } => unreachable!(
            "codegen Phase 4e: slot_kind_for_type_expr_post_mono received \
             TypeExpr::Apply — monomorphization (Task 49) should have erased it"
        ),
    }
}

/// Plan B Task 55, Phase 4e — collect the names that the synth-cont
/// must capture from the parent helper.
///
/// Walks `tail_expr` and harvests every `Expr::Ident` whose name
/// matches a helper param AND isn't shadowed by `bound`. The
/// `bound` set initially contains the let-binding's name (so
/// `let x = perform; x + threshold` correctly captures `threshold`
/// but not `x`).
///
/// Recursive shapes (Block, Match, If) extend `bound` with locally-
/// introduced let-bindings as the walker descends. Shape mirrors
/// `expr_is_pure` since the classifier already restricted to pure
/// tail expressions — yield-able shapes (Call, Perform, Lambda,
/// ClosureRecord, Handle) won't appear here.
///
/// Returns captures in source-encounter order (deduplicated). The
/// order matters because the closure record's slots are positional
/// — `captures[i]` is at `closure_ptr + 16 + 8*i` and the synth-cont
/// reads them in this same order.
fn collect_synth_cont_captures(
    tail_expr: &crate::ast::Expr,
    let_binding_name: &str,
    helper_params: &[crate::ast::Param],
) -> Vec<SynthContCapture> {
    let mut bound: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    bound.insert(let_binding_name.to_string());
    let mut out: Vec<SynthContCapture> = Vec::new();
    walk_collect_captures(tail_expr, &mut bound, helper_params, &mut out);
    out
}

fn walk_collect_captures(
    e: &crate::ast::Expr,
    bound: &mut std::collections::BTreeSet<String>,
    helper_params: &[crate::ast::Param],
    out: &mut Vec<SynthContCapture>,
) {
    use crate::ast::Expr;
    match e {
        Expr::IntLit(..)
        | Expr::StringLit(..)
        | Expr::BoolLit(..)
        | Expr::CharLit(..)
        | Expr::ClosureEnvLoad { .. } => {}
        Expr::Ident(name, _) => {
            if !bound.contains(name) {
                if let Some(param) = helper_params.iter().find(|p| p.name == *name) {
                    if !out.iter().any(|c| c.name == *name) {
                        out.push(SynthContCapture {
                            name: name.clone(),
                            kind: slot_kind_for_type_expr_post_mono(&param.ty),
                        });
                    }
                }
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            walk_collect_captures(lhs, bound, helper_params, out);
            walk_collect_captures(rhs, bound, helper_params, out);
        }
        Expr::Unary { operand, .. } => {
            walk_collect_captures(operand, bound, helper_params, out);
        }
        Expr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            walk_collect_captures(cond, bound, helper_params, out);
            walk_collect_captures_block(then_block, bound, helper_params, out);
            walk_collect_captures_block(else_block, bound, helper_params, out);
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            walk_collect_captures(scrutinee, bound, helper_params, out);
            for arm in arms {
                // PR #26 mid-flight at a5ee4c6 review item #1: each
                // match arm's pattern introduces fresh names that
                // shadow the surrounding scope (helper's params,
                // outer let-bindings) for the duration of the arm
                // body's evaluation. The walker must add those
                // pattern-bound names to `bound` before recursing
                // into the arm body, so a free Ident in the body
                // that resolves to a pattern binding doesn't get
                // mis-captured as a helper user-param. Use a per-
                // arm clone so subsequent arms see the original
                // bound set (pattern bindings are arm-local).
                let mut arm_bound = bound.clone();
                collect_pattern_bindings(&arm.pattern, &mut arm_bound);
                walk_collect_captures(&arm.body, &mut arm_bound, helper_params, out);
            }
        }
        Expr::Block(b) => walk_collect_captures_block(b, bound, helper_params, out),
        Expr::RecordLit { fields, .. } => {
            for f in fields {
                walk_collect_captures(&f.value, bound, helper_params, out);
            }
        }
        // Yield-able shapes — classifier rejects, but defensive:
        Expr::Call { .. }
        | Expr::Perform(_)
        | Expr::Handle { .. }
        | Expr::Lambda { .. }
        | Expr::ClosureRecord { .. } => {}
    }
}

/// Plan B Task 55, Phase 4e — collect names that a [`Pattern`] binds.
///
/// Used by [`walk_collect_captures`] when descending into a match
/// arm body to extend the `bound` set with pattern-introduced
/// names so they shadow surrounding-scope captures.
///
/// Pattern binding shapes (per `Pattern` enum at ast.rs):
///   - `IntLit` / `BoolLit` / `CharLit` / `Wildcard`: no binding
///   - `Var(name)`: binds `name`
///   - `Tuple(patterns)`: recursively
///   - `Ctor { fields: Unit }`: no binding
///   - `Ctor { fields: Positional(patterns) }`: recursively from each
///   - `Ctor { fields: Record(record_fields) }`: recursively from
///     each `CtorPatternField.pattern`
fn collect_pattern_bindings(p: &crate::ast::Pattern, out: &mut std::collections::BTreeSet<String>) {
    use crate::ast::{CtorPatternFields, Pattern};
    match p {
        Pattern::IntLit(..)
        | Pattern::BoolLit(..)
        | Pattern::CharLit(..)
        | Pattern::Wildcard(_) => {}
        Pattern::Var(name, _) => {
            out.insert(name.clone());
        }
        Pattern::Tuple(patterns, _) => {
            for sub in patterns {
                collect_pattern_bindings(sub, out);
            }
        }
        Pattern::Ctor { fields, .. } => match fields {
            CtorPatternFields::Unit => {}
            CtorPatternFields::Positional(patterns) => {
                for sub in patterns {
                    collect_pattern_bindings(sub, out);
                }
            }
            CtorPatternFields::Record(record_fields) => {
                for rf in record_fields {
                    collect_pattern_bindings(&rf.pattern, out);
                }
            }
        },
    }
}

fn walk_collect_captures_block(
    b: &crate::ast::Block,
    bound: &mut std::collections::BTreeSet<String>,
    helper_params: &[crate::ast::Param],
    out: &mut Vec<SynthContCapture>,
) {
    use crate::ast::Stmt;
    let saved: std::collections::BTreeSet<String> = bound.clone();
    for s in &b.stmts {
        match s {
            Stmt::Let(l) => {
                walk_collect_captures(&l.value, bound, helper_params, out);
                bound.insert(l.name.clone());
            }
            Stmt::Expr(e) => walk_collect_captures(e, bound, helper_params, out),
            Stmt::Perform(_) => {}
        }
    }
    if let Some(t) = &b.tail {
        walk_collect_captures(t, bound, helper_params, out);
    }
    *bound = saved;
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

                // Plan B Task 55, Phase 4e captures+ Slice B —
                // detect the non-tail-`k` arm body shape `{ let r:
                // Ty = k(arg); pure_tail }` and allocate a separate
                // FuncId for the post-arm-k synth fn. The detector
                // runs against the rewritten body (post-captures
                // rewrite); for Slice B's first commit, captures
                // are restricted to the empty set when the shape
                // matches (the post-arm-k tail-free-vars walker
                // rejects op-arg / outer-scope references). A
                // future captures-bearing extension will allow
                // captures into the post-arm-k synth fn by
                // allocating a closure record at the arm-fn body
                // emit site mirroring PR #26's `a5ee4c6` slice for
                // helper synth-conts.
                let post_arm_k = if let Some(shape) =
                    arm_body_let_then_pure_tail_shape(&rewritten_body, &arm.k_name)
                {
                    let post_arm_k_idx = synth.len();
                    let post_arm_k_mangled = format!("sigil_handler_post_arm_k_{post_arm_k_idx}");
                    let post_arm_k_func_id = module
                        .declare_function(&post_arm_k_mangled, Linkage::Local, ctx.cps_arm_sig)
                        .map_err(|e| format!("declare {post_arm_k_mangled}: {e}"))?;
                    let binding_ty = cranelift_ty_for_type_expr(shape.binding_te, ctx.pointer_ty);
                    Some(PostArmKSynth {
                        func_id: post_arm_k_func_id,
                        binding_name: shape.binding_name.to_string(),
                        binding_ty,
                        arg_expr: shape.arg_expr.clone(),
                        tail_expr: shape.tail_expr.clone(),
                        // For Slice B's accepted shape, the arm
                        // body's overall type IS the tail's type
                        // (the let-binding's value flows into the
                        // synth-cont; the synth-cont's result flows
                        // back as `r`; tail produces the arm's
                        // overall result), and the arm's overall
                        // type equals the op's declared return type
                        // (already resolved into `body_ty`).
                        tail_ty: body_ty,
                    })
                } else {
                    None
                };

                synth.push(HandlerArmSynth {
                    func_id,
                    body: rewritten_body,
                    arg_names,
                    arg_types,
                    body_ty,
                    captures,
                    k_name: arm.k_name.clone(),
                    post_arm_k,
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

/// Plan B Task 55, Phase 4e captures+ Slice B — recognise the
/// non-tail-`k` arm body shape `{ let r: Ty = k(arg); pure_tail }`.
///
/// Returns the matched components when the arm body's structure is
/// exactly:
/// ```text
///     Expr::Block {
///         stmts: [Stmt::Let { name, ty, value: Expr::Call {
///             callee: Expr::Ident(k_name, _),
///             args: [arg_expr],
///         } }],
///         tail: Some(tail_expr),
///     }
/// ```
///
/// Slice B first-commit restrictions enforced by the caller (the
/// pre-pass + walker pair), NOT this detector:
///   - `arg_expr` must satisfy [`expr_is_pure`] (no nested calls,
///     performs, lambdas, closure records).
///   - `tail_expr` must satisfy [`expr_is_pure`] and reference only
///     `name` (the let-binding) plus globals — no op-args, no outer-
///     scope captures, no `k_name` references. The pre-pass'
///     `arm_body_post_arm_k_tail_free_vars_only_in` helper enforces
///     this.
///
/// This is the arm-side analogue of the helper-body's
/// [`is_simple_let_yield_then_pure_tail_body`] from PR #26's `a5ee4c6`
/// captures-bearing slice. The post-arm-k synth fn's role for the
/// arm body parallels what the helper's `LetBindThenTail`
/// `CpsContinuationKind` synth-cont does for the helper body.
///
/// Returned references all borrow from `body`'s sub-tree.
fn arm_body_let_then_pure_tail_shape<'a>(
    body: &'a crate::ast::Expr,
    k_name: &str,
) -> Option<ArmBodyLetThenPureTailMatch<'a>> {
    use crate::ast::{Expr, Stmt};
    let block = match body {
        Expr::Block(b) => b,
        _ => return None,
    };
    if block.stmts.len() != 1 {
        return None;
    }
    let let_stmt = match &block.stmts[0] {
        Stmt::Let(l) => l,
        _ => return None,
    };
    let (callee, args) = match &let_stmt.value {
        Expr::Call { callee, args, .. } => (callee.as_ref(), args),
        _ => return None,
    };
    let callee_is_k = matches!(callee, Expr::Ident(n, _) if n == k_name);
    if !callee_is_k || args.len() != 1 {
        return None;
    }
    let tail = block.tail.as_ref()?;
    Some(ArmBodyLetThenPureTailMatch {
        binding_name: &let_stmt.name,
        binding_te: &let_stmt.ty,
        arg_expr: &args[0],
        tail_expr: tail,
    })
}

/// Plan B Task 55, Phase 4e captures+ Slice B — borrowed view of a
/// matched [`arm_body_let_then_pure_tail_shape`] result. All
/// references live for `'a` (the arm body's lifetime).
struct ArmBodyLetThenPureTailMatch<'a> {
    binding_name: &'a str,
    binding_te: &'a crate::ast::TypeExpr,
    arg_expr: &'a crate::ast::Expr,
    tail_expr: &'a crate::ast::Expr,
}

/// Plan B Task 55, Phase 4e captures+ Slice B — verify the post-arm-k
/// synth fn's tail expression references only the let-binding name
/// plus globals.
///
/// Walks `tail_expr` collecting every [`crate::ast::Expr::Ident`]
/// it encounters and rejecting if any name is outside the allowed
/// set `{binding_name} ∪ globals`. Used by the pre-pass to enforce
/// Slice B's "no op-args / no outer-scope captures in `tail`"
/// restriction. Also rejects `tail_expr` if it references the
/// continuation `k` (multi-shot / non-tail-of-non-tail handling
/// requires Slice C/B-extensions).
///
/// Returns `None` on success; `Some(diagnostic)` to surface the
/// rejection reason. The walker's recursion shape matches
/// [`expr_is_pure`]'s — so a future `Expr` variant added to one
/// must be added to the other.
fn arm_body_post_arm_k_tail_free_vars_ok(
    tail_expr: &crate::ast::Expr,
    binding_name: &str,
    k_name: &str,
    globals: &std::collections::BTreeSet<String>,
) -> Option<String> {
    use crate::ast::Expr;
    match tail_expr {
        Expr::IntLit(..) | Expr::BoolLit(..) | Expr::CharLit(..) | Expr::StringLit(..) => None,
        Expr::Ident(name, _) => {
            if name == binding_name || globals.contains(name) {
                None
            } else if name == k_name {
                Some(format!(
                    "Slice B: post-`k` tail of arm body references continuation \
                     `{k_name}` — multi-shot / further-non-tail uses require Slice C \
                     (heap-reified continuation)"
                ))
            } else {
                Some(format!(
                    "Slice B: post-`k` tail of arm body references `{name}`, which is \
                     neither the let-binding (`{binding_name}`) nor a global; op-arg / \
                     outer-scope captures into the post-arm-k synth fn require a future \
                     captures-bearing extension of Slice B (parallel to PR #26's `a5ee4c6` \
                     captures-bearing slice for the helper synth-cont)"
                ))
            }
        }
        Expr::ClosureEnvLoad { name, .. } => Some(format!(
            "Slice B: post-`k` tail of arm body reads `{name}` via a \
             surrounding-lambda closure record (closure_convert rewrote it into a \
             ClosureEnvLoad) — closure-of-surrounding-lambda captures into the \
             post-arm-k synth fn arrive in Slice D"
        )),
        Expr::Binary { lhs, rhs, .. } => {
            arm_body_post_arm_k_tail_free_vars_ok(lhs, binding_name, k_name, globals).or_else(
                || arm_body_post_arm_k_tail_free_vars_ok(rhs, binding_name, k_name, globals),
            )
        }
        Expr::Unary { operand, .. } => {
            arm_body_post_arm_k_tail_free_vars_ok(operand, binding_name, k_name, globals)
        }
        Expr::If {
            cond,
            then_block,
            else_block,
            ..
        } => arm_body_post_arm_k_tail_free_vars_ok(cond, binding_name, k_name, globals)
            .or_else(|| {
                arm_body_post_arm_k_tail_free_vars_ok_block(
                    then_block,
                    binding_name,
                    k_name,
                    globals,
                )
            })
            .or_else(|| {
                arm_body_post_arm_k_tail_free_vars_ok_block(
                    else_block,
                    binding_name,
                    k_name,
                    globals,
                )
            }),
        Expr::Match {
            scrutinee, arms, ..
        } => arm_body_post_arm_k_tail_free_vars_ok(scrutinee, binding_name, k_name, globals)
            .or_else(|| {
                arms.iter().find_map(|a| {
                    arm_body_post_arm_k_tail_free_vars_ok(&a.body, binding_name, k_name, globals)
                })
            }),
        Expr::Block(b) => {
            arm_body_post_arm_k_tail_free_vars_ok_block(b, binding_name, k_name, globals)
        }
        Expr::RecordLit { fields, .. } => fields.iter().find_map(|f| {
            arm_body_post_arm_k_tail_free_vars_ok(&f.value, binding_name, k_name, globals)
        }),
        // Yield-able shapes: `expr_is_pure` already rejected these
        // before this fn was called, so this is unreachable in
        // current callers. Defensive: surface the regression
        // immediately if a future caller drops the purity gate.
        Expr::Call { .. }
        | Expr::Perform(_)
        | Expr::Handle { .. }
        | Expr::Lambda { .. }
        | Expr::ClosureRecord { .. } => Some(format!(
            "Slice B internal invariant: post-`k` tail of arm body contains a \
             yield-able shape ({}) that the purity gate should have rejected — \
             a caller bypassed `expr_is_pure`",
            std::any::type_name::<Expr>()
        )),
    }
}

/// Helper for [`arm_body_post_arm_k_tail_free_vars_ok`]: walk a
/// [`crate::ast::Block`]'s stmts + tail with the same free-var
/// restriction. `Stmt::Let` extends the allowed set with its name
/// for the rest of the block (mirrors normal lexical scoping);
/// `Stmt::Perform` is rejected (already excluded by purity gate).
fn arm_body_post_arm_k_tail_free_vars_ok_block(
    b: &crate::ast::Block,
    binding_name: &str,
    k_name: &str,
    globals: &std::collections::BTreeSet<String>,
) -> Option<String> {
    use crate::ast::Stmt;
    // For Slice B's first commit, we restrict to one binding name
    // (the outer `let r`) — extending the allowed set to include
    // inner `let`s would require threading a mutable set through
    // the recursion. Inner `let`s in the tail are pure (per
    // `expr_is_pure`'s `block_is_pure`), so the inner-let value's
    // free vars are still subject to the outer `{r, globals}`
    // restriction; an inner-let-bound name then appears as a free
    // Ident in the inner-let's continuation, which we'd want to
    // permit. Defer the multi-let-in-tail support until a future
    // slice; for Slice B's surface, reject any inner Stmt::Let.
    for s in &b.stmts {
        match s {
            Stmt::Let(l) => {
                return Some(format!(
                    "Slice B: post-`k` tail of arm body contains an inner \
                     `let {}` — multi-binding tails arrive in a future captures-bearing \
                     extension; today's surface is one outer let with a pure tail",
                    l.name
                ));
            }
            Stmt::Expr(e) => {
                if let Some(d) =
                    arm_body_post_arm_k_tail_free_vars_ok(e, binding_name, k_name, globals)
                {
                    return Some(d);
                }
            }
            Stmt::Perform(_) => {
                return Some(
                    "Slice B internal invariant: post-`k` tail of arm body contains a \
                     `Stmt::Perform` that the purity gate should have rejected"
                        .to_string(),
                );
            }
        }
    }
    if let Some(t) = &b.tail {
        return arm_body_post_arm_k_tail_free_vars_ok(t, binding_name, k_name, globals);
    }
    None
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
    // Plan B Task 55, Phase 4e — synthetic continuation closures
    // for stmt-then-constant-tail CPS-color user fns. Allocated in
    // the same pass as user fn FuncIds so the user-fn body emit
    // can `func_addr` to the synth-cont when building the
    // `sigil_perform` call's `k_fn` arg. Keys: parent fn name.
    let mut cps_continuation_synth: Vec<CpsContinuationSynth> = Vec::new();
    let mut cps_continuation_synth_indices: BTreeMap<String, usize> = BTreeMap::new();

    let mut user_fns: BTreeMap<String, UserFnEntry> = BTreeMap::new();
    for item in &checked.program.items {
        if let crate::ast::Item::Fn(f) = item {
            // Plan B Task 55, Phase 4e — per-fn ABI selection.
            // `compute_user_fn_abi` returns `Cps` iff the colorer
            // marks the fn CPS AND its body matches
            // `is_simple_tail_perform_with_pure_args_body`. The
            // signature, param_tys, and ret_ty all branch on this
            // decision. The previous commit (cbe95fc) populated the
            // field; this commit consumes it for signature selection.
            // The transitional `#[allow(dead_code)]` on
            // `UserFnEntry::abi` is removed at the same time.
            let abi = compute_user_fn_abi(&f.name, &f.body, &cc.colored);
            let (sig, param_tys, ret_ty) = match abi {
                UserFnAbi::Sync => {
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
                    (sig, param_tys, ret_ty)
                }
                UserFnAbi::Cps => {
                    // Uniform CPS calling convention: (closure_ptr,
                    // args_ptr, args_len) -> *mut NextStep. Matches
                    // the synthetic-arm-fn ABI from Phase 4d and the
                    // `cps_signature` helper. User args are unpacked
                    // from `args_ptr` at fn entry; the trailing two
                    // slots carry (k_closure, k_fn). The fn's
                    // declared return type is preserved as `ret_ty`
                    // here so the native-of-CPS wrapper at call
                    // sites knows how to narrow the trampoline's
                    // u64 result back to the user-visible type.
                    let sig = cps_signature(pointer_ty, &module);
                    let param_tys = vec![pointer_ty, pointer_ty, types::I32];
                    let ret_ty = cranelift_ty_for_type_expr(&f.return_type, pointer_ty);
                    (sig, param_tys, ret_ty)
                }
            };

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
                    abi,
                },
            );

            // Plan B Task 55, Phase 4e — if the fn matches a
            // synth-cont-eligible body shape, allocate a synthetic
            // continuation closure FuncId. The synth-cont body is
            // emitted at the bottom of `emit_object` (mirrors the
            // `HandlerArmSynth` definition pass pattern).
            //
            // Two shapes at this commit:
            //   - `is_simple_yield_then_constant_tail_body` →
            //     `CpsContinuationKind::ConstantDone`.
            //   - `is_simple_let_yield_then_pure_tail_body` →
            //     `CpsContinuationKind::LetBindThenTail`.
            if abi == UserFnAbi::Cps {
                let kind: Option<CpsContinuationKind> =
                    if is_simple_yield_then_constant_tail_body(&f.body) {
                        let constant_value = match &f.body.tail {
                            Some(crate::ast::Expr::IntLit(n, _)) => *n,
                            _ => unreachable!(
                                "is_simple_yield_then_constant_tail_body classifier \
                                 guarantees tail is IntLit"
                            ),
                        };
                        Some(CpsContinuationKind::ConstantDone { constant_value })
                    } else if is_simple_let_yield_then_pure_tail_body(&f.body) {
                        let let_stmt = match &f.body.stmts[0] {
                            crate::ast::Stmt::Let(l) => l.clone(),
                            _ => unreachable!(
                                "is_simple_let_yield_then_pure_tail_body classifier \
                                 guarantees stmts[0] is Stmt::Let"
                            ),
                        };
                        let tail_expr = match &f.body.tail {
                            Some(t) => t.clone(),
                            None => unreachable!(
                                "is_simple_let_yield_then_pure_tail_body classifier \
                                 guarantees tail is Some"
                            ),
                        };
                        let binding_ty = cranelift_ty_for_type_expr(&let_stmt.ty, pointer_ty);
                        let tail_ty = cranelift_ty_for_type_expr(&f.return_type, pointer_ty);
                        // Captures-bearing slice (this commit):
                        // free-var analysis on the tail to find
                        // helper user params referenced. Empty for
                        // arity-0 helpers OR for tails that don't
                        // reference helper's params.
                        let captures =
                            collect_synth_cont_captures(&tail_expr, &let_stmt.name, &f.params);
                        Some(CpsContinuationKind::LetBindThenTail {
                            binding_name: let_stmt.name,
                            binding_ty,
                            tail_expr: Box::new(tail_expr),
                            tail_ty,
                            captures,
                        })
                    } else {
                        None
                    };

                if let Some(kind) = kind {
                    let synth_cont_sig = cps_signature(pointer_ty, &module);
                    // Global-index-based name avoids collisions with
                    // user fns containing `$` (synthetic lambda fns).
                    // Mirrors `sigil_handler_arm_{N}` from Phase 3b.
                    let synth_cont_idx = cps_continuation_synth.len();
                    let synth_cont_name = format!("sigil_post_yield_cont_{synth_cont_idx}");
                    let synth_cont_func_id = module
                        .declare_function(&synth_cont_name, Linkage::Local, &synth_cont_sig)
                        .map_err(|e| format!("declare {synth_cont_name}: {e}"))?;
                    cps_continuation_synth.push(CpsContinuationSynth {
                        func_id: synth_cont_func_id,
                        parent_fn_name: f.name.clone(),
                        kind,
                    });
                    cps_continuation_synth_indices.insert(f.name.clone(), synth_cont_idx);
                }
            }
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
    //
    // Plan B Task 55, Phase 4e — `prepare_per_fn_refs(...)` extraction.
    // The `per_fn_refs_ctx` below holds the cross-fn FuncIds +
    // side-tables that `prepare_per_fn_refs` consumes to produce
    // the per-fn FuncRef set. Constructed once here; reused at the
    // three call sites that need a full per-fn FuncRef set:
    // user-fn body emit (this loop), synth-arm-fn body emit, and
    // synth-cont definition pass for `LetBindThenTail`. Closes the
    // FFI-ref dedup deferred-must-fix flagged in PR #26 mid-flight
    // reviews at `33f2231`, `a5ee4c6`, and `2be70ce`. The
    // `TODO(plan-b-task-55-phase-4e/ffi-ref-extraction)` marker
    // added in `f7d4a64` is removed at this commit.
    let per_fn_refs_ctx = PerFnRefsCtx {
        string_new,
        println,
        panic_arith,
        alloc,
        int_to_string,
        handler_frame_new,
        handle_push,
        handle_pop,
        handler_frame_set_arm,
        perform_func,
        run_loop,
        next_step_done,
        next_step_call,
        next_step_args_ptr,
        continuation_identity,
        handler_arm_indices: &handler_arm_indices,
        handler_arm_synth: &handler_arm_synth,
        user_fns: &user_fns,
        string_literals,
        lit_ids: &lit_ids,
        div_zero_msg_id,
        mod_zero_msg_id,
    };

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

            // Plan B Task 55, Phase 4e — per-fn FuncRefs + DataRefs +
            // side-table reflections. See `prepare_per_fn_refs` for
            // the layout. Some refs (`next_step_done_ref`,
            // `next_step_call_ref`, `next_step_args_ptr_ref`) aren't
            // used directly by the user-fn body lowering (only
            // synth-arm-fn body emit needs them); unreferenced FuncRefs
            // sit in `dfg.ext_funcs` without producing relocations, so
            // the emitted object code is unaffected.
            let PerFnRefs {
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
                next_step_done_ref: _,
                next_step_call_ref: _,
                next_step_args_ptr_ref: _,
                continuation_identity_ref,
                handler_arm_refs_per_handle,
                user_fn_refs,
                lit_gvs,
                div_zero_gv,
                mod_zero_gv,
            } = prepare_per_fn_refs(&mut module, &mut builder, &per_fn_refs_ctx);

            // Plan B Task 55, Phase 4e — branch on the per-fn ABI
            // selected at the pre-pass.
            //
            // `UserFnAbi::Cps` fns have CPS calling convention block
            // params `(closure_ptr, args_ptr, args_len)` and return
            // `*mut NextStep`. The body emission is hand-rolled
            // here for the strictest body shape `is_simple_tail_
            // perform_with_pure_args_body` accepts: empty stmts +
            // tail = non-IO `Expr::Perform` with pure args. The
            // Lowerer-driven path used for `UserFnAbi::Sync` fns
            // isn't appropriate because Lowerer.lower_block returns
            // an `Option<Value>` (tail value) rather than a NextStep
            // pointer; routing the tail perform through that path
            // would invoke `lower_perform_non_io_to_value` which
            // synchronously drives `sigil_run_loop` — exactly the
            // shape the CPS-ABI path replaces. We use Lowerer for
            // the perform's pure args (so Idents / arithmetic /
            // pure compounds work) but build the perform call site
            // manually to return its NextStep up to the trampoline.
            //
            // Future Phase 4e commits widening the body shape to
            // stmt-then-tail-perform (via the lambda-lifting
            // machinery) will restructure this branch — the
            // synthetic continuation closure pre-pass + body
            // lowering for non-tail yields land alongside.
            //
            // **Arity widening at this commit**: was arity-0 only
            // (user fn params + tail perform args both required to
            // be empty). Now supports arity-N for both — user
            // params unpack from `args_ptr[0..N*8]`, perform args
            // pack into a fresh stack slot via the Phase 4b machinery
            // generalised here.
            if entry.abi == UserFnAbi::Cps {
                // Plan B Task 55, Phase 4e — branch on body shape.
                // Two CPS-eligible shapes at HEAD:
                //
                //   (1) **Tail-perform**: `body.tail =
                //       Some(Expr::Perform(p))` with pure args.
                //       Helper's perform site uses helper's caller's
                //       `(k_closure, k_fn)` (loaded from `args_ptr`)
                //       as the perform's continuation — when the arm
                //       calls `k(arg)`, the trampoline forwards arg
                //       to the caller's continuation.
                //
                //   (2) **Yield-then-constant-tail**: `body.stmts =
                //       [Stmt::Perform(p)]` + `body.tail =
                //       Some(Expr::IntLit(...))`. Helper synthesises
                //       a continuation closure (`CpsContinuationSynth`,
                //       allocated in the user-fn pre-pass) whose
                //       body returns `Done(constant)` for the tail
                //       expression. Helper's perform site uses that
                //       synth-cont's address as `k_fn` (with null
                //       `k_closure` — no captures in this slice). The
                //       synth-cont ignores the perform's value (the
                //       Stmt::Perform discards it) and returns the
                //       constant; the trampoline then unwinds to the
                //       wrapper's `sigil_run_loop` driver.
                //
                // The `CpsContinuationSynth` is the lambda-lift for
                // the post-yield rest-of-body. First-slice restriction:
                // tail is a constant literal (no captures needed). Future
                // commits widen to non-constant tails (with closure
                // captures) and multi-yield bodies.
                // Resolve body shape via the synth-cont side-table.
                // Three CPS-ABI body shapes at HEAD:
                //   - tail-perform (no synth-cont): perform is body.tail
                //   - yield-then-constant-tail: perform is stmts[0] as
                //     Stmt::Perform; synth-cont returns Done(constant)
                //   - let-yield-then-pure-tail: perform is stmts[0] as
                //     Stmt::Let.value; synth-cont binds k_arg as the
                //     let's name, lowers the tail
                let synth_cont_idx_opt = cps_continuation_synth_indices.get(&f.name).copied();
                let (body_perform, synth_cont_func_id_opt) = if let Some(idx) = synth_cont_idx_opt {
                    let p = match &f.body.stmts[0] {
                        crate::ast::Stmt::Perform(p) => p.clone(),
                        crate::ast::Stmt::Let(l) => match &l.value {
                            crate::ast::Expr::Perform(p) => p.clone(),
                            _ => unreachable!(
                                "is_simple_let_yield_then_pure_tail_body \
                                 classifier guarantees Let value is Expr::Perform"
                            ),
                        },
                        _ => unreachable!(
                            "synth-cont allocated for fn `{}` but stmts[0] \
                             is neither Stmt::Perform nor Stmt::Let — pre-pass \
                             invariant broken",
                            f.name
                        ),
                    };
                    let synth_cont_func_id = cps_continuation_synth[idx].func_id;
                    (p, Some(synth_cont_func_id))
                } else {
                    let p = match &f.body.tail {
                        Some(crate::ast::Expr::Perform(p)) => p.clone(),
                        _ => unreachable!(
                            "codegen Phase 4e: CPS-ABI fn `{}` body is not a \
                             simple tail perform nor a yield-then-(constant|\
                             let-pure-tail) — `compute_user_fn_abi` invariant \
                             broken",
                            f.name
                        ),
                    };
                    (p, None)
                };
                let block_params: Vec<Value> = builder.block_params(block).to_vec();
                let closure_ptr = block_params[0];
                let args_ptr = block_params[1];
                // block_params[2] = args_len; per `cps_signature`'s
                // convention, this is the user-arg count packed
                // before the trailing (k_closure, k_fn) pair. The
                // body emit code knows the count statically from
                // `f.params.len()`, so the runtime arg_len is
                // redundant here — it's a future hook for tooling
                // (e.g., a runtime-checked arity assertion in
                // `--debug-counters` builds).
                let _args_len = block_params[2];
                let user_arg_count = f.params.len();

                // Phase 1 — unpack user args from `args_ptr` at
                // offsets `i*8` and bind them in the env. Each slot
                // is a u64; narrower declared types (I8 Bool/Byte/
                // Unit, I32 Char) get `ireduce`'d back, mirroring
                // the `uextend` widening on the perform-side args
                // buffer. Pointer-typed args (String, user-type
                // pointers) load directly because pointer_ty == I64
                // on every supported target.
                let mut env: BTreeMap<String, Value> = BTreeMap::new();
                for (i, p) in f.params.iter().enumerate() {
                    let declared_ty = cranelift_ty_for_type_expr(&p.ty, pointer_ty);
                    let widened = builder.ins().load(
                        types::I64,
                        MemFlags::trusted(),
                        args_ptr,
                        (i * 8) as i32,
                    );
                    let value = if declared_ty == types::I64 {
                        widened
                    } else if declared_ty.is_int() && declared_ty.bits() < 64 {
                        builder.ins().ireduce(declared_ty, widened)
                    } else {
                        // Pointer-typed: I64-equivalent on supported
                        // targets. Same `assert_eq!` discipline as
                        // the synth-arm-fn op-arg unpack and the
                        // perform-side widening — symmetric mirror.
                        assert_eq!(
                            declared_ty, pointer_ty,
                            "codegen Phase 4e: unexpected user-param Cranelift type \
                             {declared_ty:?} unpacking from args_ptr in CPS-ABI fn `{}`",
                            f.name
                        );
                        widened
                    };
                    env.insert(p.name.clone(), value);
                }

                // Phase 2 — choose (k_closure, k_fn) for the perform
                // call based on the body shape:
                //
                //   - **Tail-perform shape** (`synth_cont_func_id_opt
                //     == None`): use helper's caller's continuation,
                //     loaded from `args_ptr` at the user-arg-count-
                //     shifted offsets. The wrapper at the call site
                //     writes (k_closure, k_fn) at the same offsets
                //     computed via `k_closure_offset(user_arg_count)`
                //     / `k_fn_offset(user_arg_count)`, so the read/
                //     write sites stay in lockstep across arity
                //     widening (S1 fix from PR #26 mid-flight at
                //     33f2231).
                //
                //   - **Yield-then-constant-tail shape** (`synth_cont
                //     _func_id_opt == Some(id)`): use the lambda-
                //     lifted post-yield continuation. `k_closure`
                //     is null because the synth-cont has no
                //     captures in this slice; `k_fn` is the
                //     synth-cont's func_addr. Helper's caller's
                //     continuation (which the wrapper still passes
                //     at the trailing offsets) is unused here —
                //     synth-cont returns Done(constant) directly,
                //     and the trampoline unwinds to the wrapper's
                //     `sigil_run_loop` driver which observes the
                //     Done.
                //
                // **Forward concern (PR #26 mid-flight at b818fc3):**
                // null k_closure is correct for the constant-tail
                // shape because the synth-cont has no captures.
                // The captures-bearing slice (next major commit)
                // needs to allocate a closure record per yield
                // point capturing helper's user params + helper's
                // own k_closure / k_fn (for forwarding non-discard-k
                // arm values to helper's caller's continuation).
                // At that point this branch's null becomes
                // `alloc_synth_cont_closure_record(captures)` —
                // see `CpsContinuationSynth`'s docstring for the
                // full forward-concern enumeration.
                let (k_closure_loaded, k_fn_loaded) =
                    if let Some(synth_cont_func_id) = synth_cont_func_id_opt {
                        let synth_cont_ref =
                            module.declare_func_in_func(synth_cont_func_id, builder.func);
                        let synth_cont_addr = builder.ins().func_addr(pointer_ty, synth_cont_ref);

                        // Captures-bearing slice: if the synth-cont's
                        // kind is LetBindThenTail with non-empty
                        // captures, alloc a closure record holding
                        // the captured user-param values from
                        // helper's env. Otherwise pass null
                        // k_closure (synth-cont has no captures,
                        // mirroring `alloc_arm_closure_record`'s
                        // null-for-empty convention).
                        let synth_cont_idx = cps_continuation_synth_indices[&f.name];
                        let captures: Vec<SynthContCapture> =
                            match &cps_continuation_synth[synth_cont_idx].kind {
                                CpsContinuationKind::LetBindThenTail { captures, .. } => {
                                    captures.clone()
                                }
                                _ => Vec::new(),
                            };

                        let k_closure = if captures.is_empty() {
                            builder.ins().iconst(pointer_ty, 0)
                        } else {
                            // Inline the closure-record allocation
                            // (parallel to `alloc_arm_closure_record`
                            // but called pre-Lowerer-construction
                            // because k_closure must be ready before
                            // the Lowerer's perform-args lowering
                            // phase). Layout: TAG_CLOSURE header at
                            // offset 0, null code_ptr at offset 8,
                            // env slots at offset 16 + 8*i.
                            assert!(
                                captures.len() < MAX_CLOSURE_ENV_SLOTS,
                                "synth-cont closure env >= {MAX_CLOSURE_ENV_SLOTS} \
                                 slots exceeds the bitmap layout"
                            );
                            let mut bitmap: u32 = 0;
                            for (i, c) in captures.iter().enumerate() {
                                if c.kind.is_pointer() {
                                    bitmap |= 1u32 << (i + 1);
                                }
                            }
                            let count: u8 = 1 + captures.len() as u8;
                            let header: u64 = header_word(TAG_CLOSURE, count, bitmap);
                            let payload_bytes: i64 = 8 + 8 * captures.len() as i64;

                            let header_v = builder.ins().iconst(types::I64, header as i64);
                            let payload_v = builder.ins().iconst(pointer_ty, payload_bytes);
                            let alloc_ref = module.declare_func_in_func(alloc, builder.func);
                            let alloc_call = builder.ins().call(alloc_ref, &[header_v, payload_v]);
                            stackmap.push_placeholder(function_code_offset(&builder, alloc_call));
                            let cp = builder.inst_results(alloc_call)[0];

                            // Null code_ptr at offset 8.
                            let null_v = builder.ins().iconst(pointer_ty, 0);
                            builder.ins().store(MemFlags::trusted(), null_v, cp, 8);

                            // Env slots — read each capture from
                            // helper's env (populated in Phase 1
                            // from args_ptr unpack), widen to I64
                            // per kind, store at offset 16 + 8*i.
                            for (i, capture) in captures.iter().enumerate() {
                                let raw = match env.get(&capture.name) {
                                    Some(v) => *v,
                                    None => unreachable!(
                                        "codegen Phase 4e: synth-cont capture `{}` not \
                                         found in helper `{}`'s env at body-emit time. \
                                         Free-var analysis at the pre-pass should have \
                                         restricted captures to helper's user params, \
                                         which Phase 1 unpacks into env.",
                                        capture.name, f.name
                                    ),
                                };
                                let slot_val = match capture.kind {
                                    EnvSlotKind::Int => raw,
                                    EnvSlotKind::Bool
                                    | EnvSlotKind::Byte
                                    | EnvSlotKind::Unit
                                    | EnvSlotKind::Char => builder.ins().uextend(types::I64, raw),
                                    EnvSlotKind::String
                                    | EnvSlotKind::Closure
                                    | EnvSlotKind::User => raw,
                                };
                                let offset: i32 = 16 + 8 * i as i32;
                                builder
                                    .ins()
                                    .store(MemFlags::trusted(), slot_val, cp, offset);
                            }
                            cp
                        };
                        (k_closure, synth_cont_addr)
                    } else {
                        let k_closure = builder.ins().load(
                            pointer_ty,
                            MemFlags::trusted(),
                            args_ptr,
                            k_closure_offset(user_arg_count),
                        );
                        let k_fn = builder.ins().load(
                            pointer_ty,
                            MemFlags::trusted(),
                            args_ptr,
                            k_fn_offset(user_arg_count),
                        );
                        (k_closure, k_fn)
                    };

                // Phase 3 — resolve effect_id + op_id at fn entry.
                let effect_id = match checked.effect_ids.get(&body_perform.effect) {
                    Some(id) => *id,
                    None => unreachable!(
                        "codegen Phase 4e: effect `{}` missing from effect_ids \
                         map; typecheck E0042 should have caught this",
                        body_perform.effect
                    ),
                };
                let op_id = match checked
                    .op_ids
                    .get(&(body_perform.effect.clone(), body_perform.op.clone()))
                {
                    Some(id) => *id,
                    None => unreachable!(
                        "codegen Phase 4e: op_id missing for `{}.{}`; \
                         typecheck E0043 should have caught this",
                        body_perform.effect, body_perform.op
                    ),
                };
                let effect_id_v = builder.ins().iconst(types::I32, effect_id as i64);
                let op_id_v = builder.ins().iconst(types::I32, op_id as i64);

                // Phase 4 — construct a Lowerer to handle the
                // perform's pure-arg expressions. Required because
                // pure args may include Idents (referencing user
                // params we just bound to env), pure compound
                // expressions (Binary/If/Match/Block over pure
                // sub-shapes), and other patterns the classifier
                // accepts. Lowerer encapsulates the env lookup +
                // recursive lowering machinery; using it here
                // avoids re-implementing the same logic just for
                // the CPS body emission.
                //
                // The Lowerer's `lower_perform_non_io_to_value` is
                // NOT invoked from here — we lower only the perform
                // args, then build the sigil_perform call site
                // manually. This is what makes the CPS body emit
                // structurally different from the Sync path: the
                // Sync path's `lower_block` would route the tail
                // perform through the synchronous run_loop drive;
                // the CPS path returns the perform's NextStep up
                // to the trampoline directly.
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

                // Phase 5 — lower perform args via Lowerer; pack
                // into a stack slot. Mirrors the Phase 4b machinery
                // from `lower_perform_non_io_to_value`. Empty-args
                // case keeps the null `args_ptr` + `args_len = 0`
                // shape (per `sigil_perform`'s safety contract).
                let (perform_args_ptr, perform_args_len) = if body_perform.args.is_empty() {
                    (
                        lowerer.builder.ins().iconst(pointer_ty, 0),
                        lowerer.builder.ins().iconst(types::I32, 0),
                    )
                } else {
                    let arg_values: Vec<Value> = body_perform
                        .args
                        .iter()
                        .map(|a| lowerer.lower_expr(a))
                        .collect();
                    let slot_bytes = (body_perform.args.len() * 8) as u32;
                    let slot = lowerer.builder.create_sized_stack_slot(StackSlotData::new(
                        StackSlotKind::ExplicitSlot,
                        slot_bytes,
                        3,
                    ));
                    for (i, arg_v) in arg_values.into_iter().enumerate() {
                        let arg_ty = lowerer.builder.func.dfg.value_type(arg_v);
                        let widened = if arg_ty == types::I64 {
                            arg_v
                        } else if arg_ty.is_int() && arg_ty.bits() < 64 {
                            lowerer.builder.ins().uextend(types::I64, arg_v)
                        } else {
                            assert_eq!(
                                arg_ty, pointer_ty,
                                "codegen Phase 4e: unexpected perform-arg \
                                 Cranelift type {arg_ty:?} packing into args \
                                 buffer in CPS-ABI fn `{}`",
                                f.name
                            );
                            arg_v
                        };
                        lowerer
                            .builder
                            .ins()
                            .stack_store(widened, slot, (i * 8) as i32);
                    }
                    (
                        lowerer.builder.ins().stack_addr(pointer_ty, slot, 0),
                        lowerer
                            .builder
                            .ins()
                            .iconst(types::I32, body_perform.args.len() as i64),
                    )
                };

                // Phase 6 — build sigil_perform call. The fn's
                // `perform_ref` is used (declared earlier in the
                // user-fn body emit setup); `lowerer.perform_ref`
                // shadows it but they're the same FuncRef.
                let perform_call = lowerer.builder.ins().call(
                    lowerer.perform_ref,
                    &[
                        effect_id_v,
                        op_id_v,
                        perform_args_ptr,
                        perform_args_len,
                        k_closure_loaded,
                        k_fn_loaded,
                    ],
                );
                lowerer
                    .stackmap
                    .push_placeholder(function_code_offset(&lowerer.builder, perform_call));
                let next_step = lowerer.builder.inst_results(perform_call)[0];
                lowerer.builder.ins().return_(&[next_step]);
                lowerer.builder.finalize();
                // `finalize()` consumes the underlying `builder`,
                // ending the `&mut ctx.func` borrow. The module ops
                // below safely get `&mut ctx`.

                module
                    .define_function(entry.func_id, &mut ctx)
                    .map_err(|e| format!("define {}: {e}", f.name))?;
                module.clear_context(&mut ctx);
                continue;
            }

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

                // Plan B Task 55, Phase 4e — per-fn FuncRefs via
                // shared `prepare_per_fn_refs` helper. The synth-
                // arm-fn body emit uses ALL refs (including the
                // tail-`k` lowering refs `next_step_call_ref` and
                // `next_step_args_ptr_ref`).
                let PerFnRefs {
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
                    next_step_done_ref,
                    next_step_call_ref,
                    next_step_args_ptr_ref,
                    continuation_identity_ref,
                    handler_arm_refs_per_handle,
                    user_fn_refs,
                    lit_gvs,
                    div_zero_gv,
                    mod_zero_gv,
                } = prepare_per_fn_refs(&mut module, &mut builder, &per_fn_refs_ctx);

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
                let next_step_ptr = if let Some(post_arm_k) = &synth.post_arm_k {
                    // --- Slice B: non-tail `k(arg); pure_tail` path ---
                    //
                    // The arm body is `{ let r = k(arg); pure_tail }`.
                    // The post-arm-k synth fn (already FuncId-allocated
                    // at the pre-pass) lowers `pure_tail` taking `r`
                    // from `args_ptr[0]`. The arm fn here lowers `arg`
                    // and emits `Call(k_closure, k_fn, [arg, null,
                    // post_arm_k_fn_addr])`:
                    //   - The trampoline dispatches into the helper's
                    //     synth-cont k_fn with `args_ptr=[arg, null,
                    //     post_arm_k_fn_addr]`, `args_len=3`.
                    //   - The helper synth-cont (Slice A) reads the
                    //     trailing pair from `args_ptr[1..3]`, computes
                    //     the helper's post-perform body, dispatches
                    //     `Call(post_arm_k_*, [synth_cont_result])`.
                    //   - The trampoline dispatches into our post-arm-k
                    //     synth fn with `args_ptr=[synth_cont_result]`,
                    //     `args_len=1` — which the post-arm-k synth fn
                    //     reads as `r`, lowers `pure_tail`, returns
                    //     `Done(tail_value)`.
                    //
                    // Slice B first commit: `post_arm_k_closure` is
                    // null (no captures from arm-fn into post-arm-k).
                    // A future captures-bearing extension will
                    // allocate a closure record here mirroring PR #26
                    // `a5ee4c6`'s helper-synth-cont captures slice.
                    //
                    // Bisecting hint (per `[DEVIATION Task 55] Phase
                    // 4e captures+`): wrong values from non-tail-`k`
                    // arm-body e2e tests after Slice B mean either
                    // (a) the post-arm-k synth fn's body-emit reads
                    // `r` from the wrong offset / wrong type, (b) the
                    // arm fn's args_ptr stores at offsets 0/8/16
                    // don't match the helper synth-cont's reads at
                    // the same offsets (Slice A invariant), or (c)
                    // the post-arm-k synth fn's tail expression
                    // contains an unexpected free var that escaped
                    // the `arm_body_post_arm_k_tail_free_vars_ok`
                    // walker.
                    let arg_value = lowerer.lower_expr(&post_arm_k.arg_expr);
                    let arg_ty = lowerer.builder.func.dfg.value_type(arg_value);
                    let widened_arg = if arg_ty == types::I64 {
                        arg_value
                    } else if arg_ty.is_int() && arg_ty.bits() < 64 {
                        lowerer.builder.ins().uextend(types::I64, arg_value)
                    } else {
                        assert_eq!(
                            arg_ty, pointer_ty,
                            "codegen Phase 4e captures+ Slice B: unexpected k-arg \
                             Cranelift type {arg_ty:?} for non-tail-k arg widen"
                        );
                        arg_value
                    };
                    let post_arm_k_fn_ref =
                        module.declare_func_in_func(post_arm_k.func_id, lowerer.builder.func);
                    let post_arm_k_fn_addr = lowerer
                        .builder
                        .ins()
                        .func_addr(pointer_ty, post_arm_k_fn_ref);
                    let three_v = lowerer.builder.ins().iconst(types::I32, 3);
                    let call_ns = lowerer
                        .builder
                        .ins()
                        .call(next_step_call_ref, &[k_closure_v, k_fn_v, three_v]);
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
                    lowerer.builder.ins().store(
                        MemFlags::trusted(),
                        widened_arg,
                        argp_v,
                        POST_ARM_K_ARG_OFF,
                    );
                    let null_post_arm_k_closure = lowerer.builder.ins().iconst(pointer_ty, 0);
                    lowerer.builder.ins().store(
                        MemFlags::trusted(),
                        null_post_arm_k_closure,
                        argp_v,
                        POST_ARM_K_CLOSURE_OFF,
                    );
                    lowerer.builder.ins().store(
                        MemFlags::trusted(),
                        post_arm_k_fn_addr,
                        argp_v,
                        POST_ARM_K_FN_OFF,
                    );
                    ns_ptr
                } else if let Some(arg_expr) = arm_body_tail_is_k_call(&synth.body, &synth.k_name) {
                    // --- Tail-`k(arg)` path ---
                    //
                    // Plan B Task 55, Phase 4e captures+ Slice A —
                    // trailing-pair convention. The arm fn's
                    // `Call(k_closure, k_fn, ...)` now packs THREE
                    // slots: [arg, post_arm_k_closure, post_arm_k_fn].
                    // For tail-`k` arms (no post-`k` arm-body
                    // computation), `post_arm_k_closure` is null and
                    // `post_arm_k_fn` is the address of
                    // `sigil_continuation_identity`. The helper synth-
                    // cont (k_fn) reads the trailing pair from
                    // args_ptr[1..3] and dispatches its result to
                    // identity, which returns `Done(result)` — same
                    // observable behaviour as the prior `Done`-shaped
                    // synth-cont path, with one extra trampoline hop.
                    //
                    // Slice B (non-tail `k`) replaces this null/identity
                    // pair with the lambda-lifted post-arm-k synth fn
                    // when the arm body has post-`k` computation.
                    //
                    // Bisecting hint (per `[DEVIATION Task 55] Phase 4e
                    // captures+`): a regression that produces wrong
                    // values from any PR #26 captures-bearing test
                    // here means the trailing-pair convention is
                    // wrong — verify args_ptr[0..3] stores match the
                    // synth-cont's reads at offsets 0/8/16 and that
                    // identity's arity-1 invariant is still preserved
                    // (identity sees [result] of args_len=1, NOT the
                    // arm fn's args_len=3).
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
                    let three_v = lowerer.builder.ins().iconst(types::I32, 3);
                    let call_ns = lowerer
                        .builder
                        .ins()
                        .call(next_step_call_ref, &[k_closure_v, k_fn_v, three_v]);
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
                    // Trailing-pair convention: [arg,
                    // post_arm_k_closure, post_arm_k_fn] at offsets
                    // POST_ARM_K_ARG_OFF / POST_ARM_K_CLOSURE_OFF /
                    // POST_ARM_K_FN_OFF. For tail-`k` arms, the
                    // closure is null and the fn is identity.
                    lowerer.builder.ins().store(
                        MemFlags::trusted(),
                        widened_arg,
                        argp_v,
                        POST_ARM_K_ARG_OFF,
                    );
                    let null_post_arm_k_closure = lowerer.builder.ins().iconst(pointer_ty, 0);
                    lowerer.builder.ins().store(
                        MemFlags::trusted(),
                        null_post_arm_k_closure,
                        argp_v,
                        POST_ARM_K_CLOSURE_OFF,
                    );
                    let identity_fn_addr = lowerer
                        .builder
                        .ins()
                        .func_addr(pointer_ty, lowerer.continuation_identity_ref);
                    lowerer.builder.ins().store(
                        MemFlags::trusted(),
                        identity_fn_addr,
                        argp_v,
                        POST_ARM_K_FN_OFF,
                    );
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

    // Plan B Task 55, Phase 4e captures+ Slice B — post-arm-k synth
    // fn definition pass. For each `HandlerArmSynth` whose pre-pass
    // detected the non-tail-`k(arg); pure_tail` shape via
    // `arm_body_let_then_pure_tail_shape`, emit the post-arm-k synth
    // fn body. The fn signature is the standard CPS-ABI shape (same
    // as arm fns + helper synth-conts):
    //
    //     fn(closure_ptr, args_ptr, args_len) -> *mut NextStep
    //
    // Body:
    //   - Read `r` from `args_ptr[0]` as I64 (the helper's synth-
    //     cont dispatched `Call(post_arm_k_*, [synth_cont_result])`).
    //   - Narrow per `post_arm_k.binding_ty`.
    //   - Bind in env under `post_arm_k.binding_name`.
    //   - Lower `post_arm_k.tail_expr` via Lowerer.
    //   - Widen to I64 (matching `sigil_next_step_done`'s signature).
    //   - Return `Done(widened)` to the trampoline.
    //
    // First-commit restrictions:
    //   - `closure_ptr` is null at runtime (no captures) — the
    //     pre-pass' `arm_body_post_arm_k_tail_free_vars_ok` walker
    //     restricts `tail_expr` to free vars in `{r} ∪ globals`. A
    //     future captures-bearing extension would allocate a closure
    //     record at the arm-fn body emit and read it here.
    //   - `tail_expr` is pure (per `expr_is_pure`); no nested
    //     yields, no further `k` invocations. Multi-shot / chained
    //     non-tail-`k` use arrives in Slice C.
    //
    // The pass mirrors the synth-arm-fn body emit's general shape;
    // it's structurally simpler because the post-arm-k synth fn has
    // no op-args to unpack (just `r`) and no tail-`k` branching
    // (the tail is always pure).
    {
        let post_arm_k_sig = cps_signature(pointer_ty, &module);
        for synth in &handler_arm_synth {
            let post_arm_k = match &synth.post_arm_k {
                Some(p) => p,
                None => continue,
            };
            ctx.func.signature = post_arm_k_sig.clone();
            ctx.func.name = UserFuncName::user(0, post_arm_k.func_id.as_u32());

            let mut builder = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
            let mut stackmap = StackMapBuilder::new();
            let block = builder.create_block();
            builder.append_block_params_for_function_params(block);
            builder.switch_to_block(block);
            builder.seal_block(block);

            let block_params: Vec<Value> = builder.block_params(block).to_vec();
            // block_params[0] = closure_ptr (Slice B first-commit:
            // null at runtime since post-arm-k has no captures);
            // block_params[1] = args_ptr (= [r] from helper synth-
            // cont's `Call(post_arm_k_*, [synth_cont_result])`);
            // block_params[2] = args_len (== 1 from the helper
            // synth-cont's `Call(post_arm_k_*, [...])` which always
            // emits arg_count=1).
            let _args_len = block_params[2];

            let args_ptr = block_params[1];
            let r_widened = builder
                .ins()
                .load(types::I64, MemFlags::trusted(), args_ptr, 0);
            let r_value = if post_arm_k.binding_ty == types::I64 {
                r_widened
            } else if post_arm_k.binding_ty.is_int() && post_arm_k.binding_ty.bits() < 64 {
                builder.ins().ireduce(post_arm_k.binding_ty, r_widened)
            } else {
                assert_eq!(
                    post_arm_k.binding_ty, pointer_ty,
                    "codegen Phase 4e captures+ Slice B: unexpected post-arm-k binding \
                     Cranelift type {:?} for fn `{}`'s post-arm-k synth fn",
                    post_arm_k.binding_ty, synth.k_name
                );
                r_widened
            };

            let mut env: BTreeMap<String, Value> = BTreeMap::new();
            env.insert(post_arm_k.binding_name.clone(), r_value);

            let PerFnRefs {
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
                next_step_done_ref,
                next_step_call_ref: _,
                next_step_args_ptr_ref: _,
                continuation_identity_ref,
                handler_arm_refs_per_handle,
                user_fn_refs,
                lit_gvs,
                div_zero_gv,
                mod_zero_gv,
            } = prepare_per_fn_refs(&mut module, &mut builder, &per_fn_refs_ctx);

            let closure_ptr = block_params[0];
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

            let tail_value = lowerer.lower_expr(&post_arm_k.tail_expr);
            let widened_tail = if post_arm_k.tail_ty == types::I64 {
                tail_value
            } else if post_arm_k.tail_ty.is_int() && post_arm_k.tail_ty.bits() < 64 {
                lowerer.builder.ins().uextend(types::I64, tail_value)
            } else {
                assert_eq!(
                    post_arm_k.tail_ty, pointer_ty,
                    "codegen Phase 4e captures+ Slice B: unexpected post-arm-k tail \
                     Cranelift type {:?} for fn `{}`'s post-arm-k synth fn",
                    post_arm_k.tail_ty, synth.k_name
                );
                tail_value
            };
            let done_call = lowerer
                .builder
                .ins()
                .call(next_step_done_ref, &[widened_tail]);
            lowerer
                .stackmap
                .push_placeholder(function_code_offset(&lowerer.builder, done_call));
            let next_step = lowerer.builder.inst_results(done_call)[0];
            lowerer.builder.ins().return_(&[next_step]);
            lowerer.builder.finalize();

            module
                .define_function(post_arm_k.func_id, &mut ctx)
                .map_err(|e| format!("define post-arm-k synth fn: {e}"))?;
            module.clear_context(&mut ctx);
        }
    }

    // Plan B Task 55, Phase 4e — synthetic continuation closure
    // definition pass. For each `CpsContinuationSynth` allocated at
    // the user-fn pre-pass (one per CPS-color user fn matching the
    // stmt-then-constant-tail body shape), emit a fn body that
    // returns `Done(constant)`. Mirrors the synthetic-arm-fn
    // definition pass above; first-slice restriction is that the
    // synth-cont has no captures (closure_ptr unused, args_ptr
    // unused — the synth-cont ignores the perform's value because
    // `Stmt::Perform` discards it, and the constant tail is a
    // compile-time literal needing no captures).
    {
        let synth_cont_sig = cps_signature(pointer_ty, &module);
        for synth in &cps_continuation_synth {
            ctx.func.signature = synth_cont_sig.clone();
            ctx.func.name = UserFuncName::user(0, synth.func_id.as_u32());
            {
                let mut builder = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
                let block = builder.create_block();
                builder.append_block_params_for_function_params(block);
                builder.switch_to_block(block);
                builder.seal_block(block);

                let block_params: Vec<Value> = builder.block_params(block).to_vec();
                // block_params[0] = closure_ptr (synth-cont's own
                // captures record; unused for ConstantDone since
                // there are no captures); block_params[1] = args_ptr;
                // block_params[2] = args_len.
                //
                // Plan B Task 55, Phase 4e captures+ Slice A —
                // trailing-pair convention. The arm-fn tail-`k` emit
                // packs `args_ptr` as `[arg, post_arm_k_closure,
                // post_arm_k_fn]` (args_len=3). The synth-cont reads
                // `args_ptr[1]` and `args_ptr[2]` to obtain the
                // post-arm-k continuation pair, computes its result,
                // and dispatches `Call(post_arm_k_*, [result])`
                // instead of `Done(result)`. For tail-`k` arms the
                // pair is `(null, &sigil_continuation_identity)`, so
                // identity returns `Done(result)` — same observable
                // behaviour as the prior path with one extra
                // trampoline hop. For non-tail-`k` arms (Slice B)
                // the pair will be the lambda-lifted post-arm-k
                // synth fn.
                let _args_len = block_params[2];

                let next_step_call_ref = module.declare_func_in_func(next_step_call, builder.func);
                let next_step_args_ptr_ref =
                    module.declare_func_in_func(next_step_args_ptr, builder.func);

                // Slice A: load post-arm-k pair from args_ptr at the
                // trailing-pair offsets defined by the convention
                // (closure at +8, fn at +16; arg at +0). Used by both
                // ConstantDone and LetBindThenTail arms to dispatch
                // their result through the post-arm-k continuation.
                //
                // TODO(plan-b-task-55-phase-4e-captures/slice-b-stackmap-root):
                // For Slice A, `post_arm_k_closure` is always null
                // (the arm-fn tail-`k` direct emit packs `[arg, null,
                // &identity]`), so no GC root is needed across
                // `emit_dispatch_to_post_arm_k`'s arena allocations.
                // For Slice B (non-tail `k` lift), the arm-fn will
                // pack `[arg, post_arm_k_closure_ptr,
                // post_arm_k_fn_ptr]` where `post_arm_k_closure_ptr`
                // is a real heap-allocated TAG_CLOSURE record. The
                // SSA value loaded here lives across
                // `next_step_call`'s arena allocation inside
                // `emit_dispatch_to_post_arm_k`; Slice B must add a
                // stackmap entry on the load (or on the arena alloc
                // call) so Boehm-precise GC roots
                // `post_arm_k_closure` across the allocation.
                // Failure mode if missed: `post_arm_k_closure` is a
                // dangling pointer when the synth-cont returns its
                // `Call(post_arm_k_*, [result])`, the trampoline
                // dispatches into freed memory.
                let synth_cont_args_ptr = block_params[1];
                let post_arm_k_closure = builder.ins().load(
                    pointer_ty,
                    MemFlags::trusted(),
                    synth_cont_args_ptr,
                    POST_ARM_K_CLOSURE_OFF,
                );
                let post_arm_k_fn = builder.ins().load(
                    pointer_ty,
                    MemFlags::trusted(),
                    synth_cont_args_ptr,
                    POST_ARM_K_FN_OFF,
                );

                // Helper: emit the trailing `Call(post_arm_k_*,
                // [result])` dispatch. Used by both branches of the
                // match below. Returns the NextStep pointer to be
                // returned by the synth-cont fn.
                let emit_dispatch_to_post_arm_k = |builder: &mut FunctionBuilder<'_>,
                                                   stackmap: &mut StackMapBuilder,
                                                   result_value: Value|
                 -> Value {
                    let one_v = builder.ins().iconst(types::I32, 1);
                    let call_ns = builder.ins().call(
                        next_step_call_ref,
                        &[post_arm_k_closure, post_arm_k_fn, one_v],
                    );
                    stackmap.push_placeholder(function_code_offset(builder, call_ns));
                    let ns_ptr = builder.inst_results(call_ns)[0];
                    let argp_call = builder.ins().call(next_step_args_ptr_ref, &[ns_ptr]);
                    stackmap.push_placeholder(function_code_offset(builder, argp_call));
                    let argp_v = builder.inst_results(argp_call)[0];
                    builder
                        .ins()
                        .store(MemFlags::trusted(), result_value, argp_v, 0);
                    ns_ptr
                };

                match &synth.kind {
                    CpsContinuationKind::ConstantDone { constant_value } => {
                        // ConstantDone shape: synth-cont's body is
                        // just the parent helper's constant tail
                        // expression. Slice A: dispatch the constant
                        // through post-arm-k instead of returning
                        // `Done` directly.
                        let constant_v = builder.ins().iconst(types::I64, *constant_value);
                        let next_step =
                            emit_dispatch_to_post_arm_k(&mut builder, &mut stackmap, constant_v);
                        builder.ins().return_(&[next_step]);
                        builder.finalize();
                    }
                    CpsContinuationKind::LetBindThenTail {
                        binding_name,
                        binding_ty,
                        tail_expr,
                        tail_ty,
                        captures,
                    } => {
                        // Captures-bearing let-yield-then-pure-tail
                        // shape: load `args_ptr[0]` as I64, narrow
                        // to `binding_ty`, bind in env under
                        // `binding_name`. For each capture in
                        // `captures`, load from `closure_ptr` at
                        // offset `16 + 8*i` (mirroring user-lambda
                        // closure_env_load), narrow per kind, bind
                        // in env. Then lower `tail_expr` via
                        // Lowerer, widen result to I64, emit
                        // `Done(value)`.
                        //
                        // When `captures` is empty (arity-0 helper
                        // OR tail not referencing helper's params),
                        // helper passed null `closure_ptr`; the
                        // capture-load loop runs zero iterations
                        // and the env contains only the binding.
                        let args_ptr = block_params[1];
                        let widened =
                            builder
                                .ins()
                                .load(types::I64, MemFlags::trusted(), args_ptr, 0);
                        let bound_value = if *binding_ty == types::I64 {
                            widened
                        } else if binding_ty.is_int() && binding_ty.bits() < 64 {
                            builder.ins().ireduce(*binding_ty, widened)
                        } else {
                            assert_eq!(
                                *binding_ty, pointer_ty,
                                "codegen Phase 4e: unexpected synth-cont binding \
                                 type {binding_ty:?} for fn `{}`",
                                synth.parent_fn_name
                            );
                            widened
                        };

                        let mut env: BTreeMap<String, Value> = BTreeMap::new();
                        env.insert(binding_name.clone(), bound_value);

                        // Captures-bearing extension: read each
                        // capture from `closure_ptr` at offset 16 +
                        // 8*i. Mirrors `lower_closure_env_load`
                        // (which reads from `self.closure_ptr`); we
                        // emit the loads here directly because we
                        // pre-Lowerer-construction.
                        let synth_closure_ptr = block_params[0];
                        for (i, capture) in captures.iter().enumerate() {
                            let offset: i32 = 16 + 8 * i as i32;
                            let raw = builder.ins().load(
                                types::I64,
                                MemFlags::trusted(),
                                synth_closure_ptr,
                                offset,
                            );
                            let val = match capture.kind {
                                EnvSlotKind::Int => raw,
                                EnvSlotKind::Bool | EnvSlotKind::Byte | EnvSlotKind::Unit => {
                                    builder.ins().ireduce(types::I8, raw)
                                }
                                EnvSlotKind::Char => builder.ins().ireduce(types::I32, raw),
                                EnvSlotKind::String | EnvSlotKind::Closure | EnvSlotKind::User => {
                                    if pointer_ty == types::I64 {
                                        raw
                                    } else {
                                        builder.ins().ireduce(pointer_ty, raw)
                                    }
                                }
                            };
                            env.insert(capture.name.clone(), val);
                        }

                        // Plan B Task 55, Phase 4e — per-fn FuncRefs
                        // via shared `prepare_per_fn_refs` helper.
                        // The synth-cont (LetBindThenTail) body
                        // doesn't use the tail-`k` lowering refs
                        // (those are arm-fn-only); unreferenced
                        // FuncRefs sit in `dfg.ext_funcs` without
                        // emitting relocations.
                        let PerFnRefs {
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
                            next_step_done_ref: _,
                            next_step_call_ref: _,
                            next_step_args_ptr_ref: _,
                            continuation_identity_ref,
                            handler_arm_refs_per_handle,
                            user_fn_refs,
                            lit_gvs,
                            div_zero_gv,
                            mod_zero_gv,
                        } = prepare_per_fn_refs(&mut module, &mut builder, &per_fn_refs_ctx);

                        let closure_ptr = block_params[0];
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

                        let tail_value = lowerer.lower_expr(tail_expr.as_ref());
                        let widened_tail = if *tail_ty == types::I64 {
                            tail_value
                        } else if tail_ty.is_int() && tail_ty.bits() < 64 {
                            lowerer.builder.ins().uextend(types::I64, tail_value)
                        } else {
                            assert_eq!(
                                *tail_ty, pointer_ty,
                                "codegen Phase 4e: unexpected synth-cont tail \
                                 type {tail_ty:?} for fn `{}`",
                                synth.parent_fn_name
                            );
                            tail_value
                        };

                        // Slice A: dispatch the tail value through the
                        // post-arm-k continuation (read at synth-cont
                        // entry from `args_ptr[1..3]`) instead of
                        // returning `Done` directly.
                        let next_step = emit_dispatch_to_post_arm_k(
                            &mut lowerer.builder,
                            lowerer.stackmap,
                            widened_tail,
                        );
                        lowerer.builder.ins().return_(&[next_step]);
                        lowerer.builder.finalize();
                    }
                }
            }
            module
                .define_function(synth.func_id, &mut ctx)
                .map_err(|e| format!("define synth-cont for `{}`: {e}", synth.parent_fn_name))?;
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
                // Plan B Task 55, Phase 4e — branch on the callee's
                // ABI. Sync callees use the existing closure-
                // convention direct call. Cps callees use the
                // inlined native↔CPS interop wrapper: pack args
                // (none for arity-0 callees) into a stack-slot u64
                // buffer, append (k_closure=null, k_fn=&identity)
                // for the continuation, call the CPS fn → *mut
                // NextStep, drive sigil_run_loop → u64, narrow
                // back to the callee's declared return type.
                let callee_entry = match self.user_fns.get(name) {
                    Some(e) => e,
                    None => unreachable!(
                        "codegen: user_fn_refs contains `{name}` but user_fns \
                         doesn't — pre-pass invariant broken"
                    ),
                };
                match callee_entry.abi {
                    UserFnAbi::Sync => {
                        let arg_vals: Vec<Value> =
                            args.iter().map(|a| self.lower_expr(a)).collect();
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
                    UserFnAbi::Cps => {
                        // Pack user args + (k_closure, k_fn) into a
                        // single stack slot of size `(N + 2) * 8`
                        // bytes. User args fill offsets `0..N*8`,
                        // each widened to u64 via `uextend` for
                        // narrower-than-I64 ints (mirrors Phase 4b
                        // `lower_perform_non_io_to_value`). The
                        // trailing pair `(null_k_closure,
                        // identity_k_fn)` lives at
                        // `k_closure_offset(N)` and `k_fn_offset(N)`
                        // — same offsets the callee's body emit
                        // reads from, which keeps writer/reader in
                        // lockstep across arity widening (S1 fix
                        // from PR #26 mid-flight at 33f2231).
                        //
                        // `k_closure = null` and `k_fn = sigil_
                        // continuation_identity` mirrors the Phase
                        // 4d MVP perform-site shape: when the
                        // trampoline eventually dispatches the
                        // callee's perform's arm, a `k(arg)`
                        // invocation rolls through identity to
                        // terminal Done(arg); a discard-k arm
                        // returns Done(arm_value) directly. Either
                        // way the synchronous wrapper unwinds to
                        // `sigil_run_loop`'s u64 return.
                        let user_arg_count = args.len();
                        let slot_bytes = ((user_arg_count + 2) * 8) as u32;
                        let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
                            StackSlotKind::ExplicitSlot,
                            slot_bytes,
                            3,
                        ));

                        // Lower + widen + store user args.
                        for (i, arg_expr) in args.iter().enumerate() {
                            let arg_v = self.lower_expr(arg_expr);
                            let arg_ty = self.builder.func.dfg.value_type(arg_v);
                            let widened = if arg_ty == types::I64 {
                                arg_v
                            } else if arg_ty.is_int() && arg_ty.bits() < 64 {
                                self.builder.ins().uextend(types::I64, arg_v)
                            } else {
                                assert_eq!(
                                    arg_ty, self.pointer_ty,
                                    "codegen Phase 4e: unexpected user-arg \
                                     Cranelift type {arg_ty:?} for native\u{2194}\
                                     CPS interop wrapper packing"
                                );
                                arg_v
                            };
                            self.builder
                                .ins()
                                .stack_store(widened, slot, (i * 8) as i32);
                        }

                        // Write trailing (k_closure, k_fn) pair.
                        let null_k_closure = self.builder.ins().iconst(self.pointer_ty, 0);
                        let identity_k_fn = self
                            .builder
                            .ins()
                            .func_addr(self.pointer_ty, self.continuation_identity_ref);
                        self.builder.ins().stack_store(
                            null_k_closure,
                            slot,
                            k_closure_offset(user_arg_count),
                        );
                        self.builder.ins().stack_store(
                            identity_k_fn,
                            slot,
                            k_fn_offset(user_arg_count),
                        );
                        let args_ptr = self.builder.ins().stack_addr(self.pointer_ty, slot, 0);
                        // D2 fix from PR #26 mid-flight at 33f2231:
                        // `args_len` per `cps_signature` convention
                        // is the user-arg count, NOT the trailing-
                        // pair slot count. Matches the perform-site
                        // precedent (`lower_perform_non_io_to_value`)
                        // and the runtime's `args_ptr=null ⟹
                        // args_len=0` contract.
                        let args_len = self.builder.ins().iconst(types::I32, user_arg_count as i64);

                        let func_ref = self.user_fn_refs[name];
                        let null_closure_ptr = self.builder.ins().iconst(self.pointer_ty, 0);
                        let cps_call = self
                            .builder
                            .ins()
                            .call(func_ref, &[null_closure_ptr, args_ptr, args_len]);
                        self.stackmap
                            .push_placeholder(function_code_offset(&self.builder, cps_call));
                        let next_step = self.builder.inst_results(cps_call)[0];

                        // Drive the trampoline. Returns u64.
                        let run_loop_call =
                            self.builder.ins().call(self.run_loop_ref, &[next_step]);
                        self.stackmap
                            .push_placeholder(function_code_offset(&self.builder, run_loop_call));
                        let raw_u64 = self.builder.inst_results(run_loop_call)[0];

                        // Narrow `raw_u64` back to the callee's
                        // declared return type. Mirrors the
                        // narrow-on-perform-side discipline from
                        // `lower_perform_non_io_to_value`.
                        let ret_ty = callee_entry.ret_ty;
                        if ret_ty == types::I64 {
                            raw_u64
                        } else if ret_ty.is_int() && ret_ty.bits() < 64 {
                            self.builder.ins().ireduce(ret_ty, raw_u64)
                        } else {
                            // Pointer-typed return: the trampoline's
                            // u64 is bit-identical to the pointer
                            // value on supported targets (pointer_ty
                            // == I64 on x86_64-linux + aarch64-darwin).
                            // No conversion needed; debug_assert pins
                            // the invariant.
                            debug_assert_eq!(
                                ret_ty, self.pointer_ty,
                                "codegen Phase 4e: CPS callee `{name}` \
                                 has unexpected ret_ty {ret_ty:?}; expected \
                                 I64 or pointer_ty"
                            );
                            raw_u64
                        }
                    }
                }
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

/// Plan B Task 55, Phase 4d/4e — `args_len` convention for the CPS
/// calling convention.
///
/// `args_len` is the **user-arg count** packed into `args_ptr`,
/// excluding the trailing `(k_closure, k_fn)` pair. With `N` user
/// args, `args_ptr` packs `[user_arg_0, user_arg_1, ...,
/// user_arg_{N-1}, k_closure, k_fn]` — total `N + 2` 8-byte slots,
/// but `args_len = N`. The continuation pair lives at byte offsets
/// `8*N` (k_closure) and `8*N + 8` (k_fn) relative to `args_ptr`;
/// see [`k_closure_offset`] and [`k_fn_offset`].
///
/// This matches the perform-site convention from Phase 4b
/// (`lower_perform_non_io_to_value`): `args_len` counts user args,
/// not slot count. The runtime contract on `sigil_perform`
/// `args_ptr=null ⟹ args_len=0` falls out naturally.
///
/// At the first Phase 4e slice (arity-0 CPS user fns), `N == 0`,
/// so `args_len = 0` and the continuation pair is at offsets 0
/// and 8 directly. The next slice (user-arg unpacking + perform-
/// args packing) generalises to arity-N.
const fn _cps_args_len_convention_doc() {}

/// Plan B Task 55, Phase 4d/4e — byte offset within `args_ptr` for
/// the `k_closure` slot, given the user-arg count packed before it.
/// Each user arg is 8 bytes; `k_closure` lives immediately after
/// the user args.
///
/// Used by both:
///   - the **CPS body emission** (`emit_object`'s user-fn body
///     emit branch) to load the captured continuation closure
///     pointer at fn entry.
///   - the **native↔CPS interop wrapper** (`Lowerer::lower_call`'s
///     direct-call arm for CPS callees) to write
///     `(null_k_closure, identity_k_fn)` into the args_ptr buffer.
///
/// Extracting both offsets into shared helpers (rather than
/// inlining `0` and `8` at two sites) keeps the writer and reader
/// in lockstep when arity-N user-arg unpacking lands and the
/// trailing-pair offsets shift by `N * 8`.
fn k_closure_offset(user_arg_count: usize) -> i32 {
    (user_arg_count as i32) * 8
}

/// Plan B Task 55, Phase 4d/4e — byte offset within `args_ptr` for
/// the `k_fn` slot. Always `k_closure_offset + 8`. Companion to
/// [`k_closure_offset`].
fn k_fn_offset(user_arg_count: usize) -> i32 {
    k_closure_offset(user_arg_count) + 8
}

/// Plan B Task 55, Phase 4e captures+ Slice A — trailing-pair
/// convention offsets within the synth-cont's incoming `args_ptr`.
///
/// The arm-fn tail-`k` body emit packs `Call(k_closure, k_fn,
/// /*arg_count=*/3)` with three slots:
///   - `args_ptr[POST_ARM_K_ARG_OFF]` (= 0): the `k(arg)` value the
///     arm passes to its captured continuation.
///   - `args_ptr[POST_ARM_K_CLOSURE_OFF]` (= 8): the post-arm-k
///     closure (null for tail-`k` arms; a real heap-allocated
///     TAG_CLOSURE record under Slice B's non-tail-`k` lift).
///   - `args_ptr[POST_ARM_K_FN_OFF]` (= 16): the post-arm-k fn ptr
///     (`&sigil_continuation_identity` for tail-`k` arms; a lambda-
///     lifted post-arm-k synth fn's `func_addr` under Slice B).
///
/// These offsets are FIXED (independent of any user-arg count) —
/// distinct from [`k_closure_offset`] / [`k_fn_offset`] which apply
/// to the user-fn-side `args_ptr` shape `[user_arg_0, ...,
/// user_arg_{N-1}, k_closure, k_fn]` whose trailing-pair offsets
/// shift by `N * 8`. The synth-cont always sees exactly the
/// 3-slot trailing-pair shape because the arm-fn always packs it.
const POST_ARM_K_ARG_OFF: i32 = 0;
const POST_ARM_K_CLOSURE_OFF: i32 = 8;
const POST_ARM_K_FN_OFF: i32 = 16;

/// Plan B Task 55, Phase 4e — input context for [`prepare_per_fn_refs`].
///
/// Holds the cross-fn FuncIds (declared once at `emit_object`'s top)
/// and the side-tables that drive per-fn FuncRef + DataRef
/// construction. Used at the three sites that need a full per-fn
/// FuncRef set: the user-fn body emit loop, the synth-arm-fn body
/// emit loop, and the synth-cont definition pass for
/// [`CpsContinuationKind::LetBindThenTail`]. Borrows are `'a`-bound
/// to the `emit_object` stack frame; the context is constructed
/// once and re-used for each fn.
///
/// Closes the FFI-ref dedup deferred-must-fix flagged in PR #26
/// mid-flight reviews at `33f2231`, `a5ee4c6`, and `2be70ce`. The
/// `TODO(plan-b-task-55-phase-4e/ffi-ref-extraction)` marker at
/// the user-fn body emit site (added in `f7d4a64`) is removed at
/// the same commit as this helper lands.
struct PerFnRefsCtx<'a> {
    string_new: cranelift_module::FuncId,
    println: cranelift_module::FuncId,
    panic_arith: cranelift_module::FuncId,
    alloc: cranelift_module::FuncId,
    int_to_string: cranelift_module::FuncId,
    handler_frame_new: cranelift_module::FuncId,
    handle_push: cranelift_module::FuncId,
    handle_pop: cranelift_module::FuncId,
    handler_frame_set_arm: cranelift_module::FuncId,
    perform_func: cranelift_module::FuncId,
    run_loop: cranelift_module::FuncId,
    next_step_done: cranelift_module::FuncId,
    next_step_call: cranelift_module::FuncId,
    next_step_args_ptr: cranelift_module::FuncId,
    continuation_identity: cranelift_module::FuncId,
    handler_arm_indices: &'a BTreeMap<Span, Vec<usize>>,
    handler_arm_synth: &'a [HandlerArmSynth],
    user_fns: &'a BTreeMap<String, UserFnEntry>,
    string_literals: &'a [(Span, String)],
    lit_ids: &'a [cranelift_module::DataId],
    div_zero_msg_id: cranelift_module::DataId,
    mod_zero_msg_id: cranelift_module::DataId,
}

/// Plan B Task 55, Phase 4e — per-fn FuncRefs / DataRefs / side-table
/// reflections produced by [`prepare_per_fn_refs`].
///
/// All fields are `module.declare_func_in_func` / `declare_data_in_func`
/// outputs scoped to the fn currently being built. Cranelift's
/// FuncRefs are per-Function; reusing across fn bodies is unsafe,
/// so each fn body emit calls [`prepare_per_fn_refs`] fresh.
///
/// Some fields are unused at certain call sites (e.g.,
/// `next_step_call_ref` and `next_step_args_ptr_ref` are only used
/// at the synth-arm-fn body emit's tail-`k` lowering path). The
/// unused FuncRefs *are* registered in the Function's `dfg.ext_funcs`
/// table — strictly speaking, Cranelift doesn't prune them. What's
/// true: no relocations are emitted for FuncRefs not referenced by
/// `Call` / `FuncAddr` instructions, so the *emitted object code* is
/// unaffected. The over-declaration cost is structural-only (a few
/// entries in the function's external-funcs table) — there is no IR
/// or emitted-binary impact at the three call sites.
struct PerFnRefs {
    string_new_ref: FuncRef,
    println_ref: FuncRef,
    panic_arith_ref: FuncRef,
    alloc_ref: FuncRef,
    int_to_string_ref: FuncRef,
    handler_frame_new_ref: FuncRef,
    handle_push_ref: FuncRef,
    handle_pop_ref: FuncRef,
    handler_frame_set_arm_ref: FuncRef,
    perform_ref: FuncRef,
    run_loop_ref: FuncRef,
    next_step_done_ref: FuncRef,
    next_step_call_ref: FuncRef,
    next_step_args_ptr_ref: FuncRef,
    continuation_identity_ref: FuncRef,
    handler_arm_refs_per_handle: BTreeMap<Span, Vec<FuncRef>>,
    user_fn_refs: BTreeMap<String, FuncRef>,
    lit_gvs: Vec<(Span, GlobalValue, usize)>,
    div_zero_gv: GlobalValue,
    mod_zero_gv: GlobalValue,
}

/// Plan B Task 55, Phase 4e — declare per-fn FuncRefs + DataRefs +
/// side-table reflections needed to lower a fn body.
///
/// Replaces ~70 LOC of duplicated FFI-ref + lit_gv + handler_arm_refs +
/// user_fn_refs setup at three sites in `emit_object`. The three sites
/// have slightly different needs — the user-fn site doesn't use the
/// `next_step_done_ref` / `next_step_call_ref` / `next_step_args_ptr_ref`
/// trio, and the synth-cont site doesn't use the tail-`k` refs — but
/// over-declaring here is cheap: unreferenced FuncRefs sit in the
/// function's `dfg.ext_funcs` table without producing relocations, so
/// the emitted object code is unaffected.
fn prepare_per_fn_refs(
    module: &mut ObjectModule,
    builder: &mut FunctionBuilder<'_>,
    ctx: &PerFnRefsCtx<'_>,
) -> PerFnRefs {
    let string_new_ref = module.declare_func_in_func(ctx.string_new, builder.func);
    let println_ref = module.declare_func_in_func(ctx.println, builder.func);
    let panic_arith_ref = module.declare_func_in_func(ctx.panic_arith, builder.func);
    let alloc_ref = module.declare_func_in_func(ctx.alloc, builder.func);
    let int_to_string_ref = module.declare_func_in_func(ctx.int_to_string, builder.func);
    let handler_frame_new_ref = module.declare_func_in_func(ctx.handler_frame_new, builder.func);
    let handle_push_ref = module.declare_func_in_func(ctx.handle_push, builder.func);
    let handle_pop_ref = module.declare_func_in_func(ctx.handle_pop, builder.func);
    let handler_frame_set_arm_ref =
        module.declare_func_in_func(ctx.handler_frame_set_arm, builder.func);
    let perform_ref = module.declare_func_in_func(ctx.perform_func, builder.func);
    let run_loop_ref = module.declare_func_in_func(ctx.run_loop, builder.func);
    let next_step_done_ref = module.declare_func_in_func(ctx.next_step_done, builder.func);
    let next_step_call_ref = module.declare_func_in_func(ctx.next_step_call, builder.func);
    let next_step_args_ptr_ref = module.declare_func_in_func(ctx.next_step_args_ptr, builder.func);
    let continuation_identity_ref =
        module.declare_func_in_func(ctx.continuation_identity, builder.func);

    // Per-handle synth-arm-fn FuncRefs, keyed by handle span. Built
    // from the `handler_arm_indices` side-table (one entry per
    // handle expression in the program; each entry's Vec maps to
    // the arms in source declaration order).
    let handler_arm_refs_per_handle: BTreeMap<Span, Vec<FuncRef>> = ctx
        .handler_arm_indices
        .iter()
        .map(|(span, idx_vec)| {
            let refs: Vec<FuncRef> = idx_vec
                .iter()
                .map(|&i| {
                    module.declare_func_in_func(ctx.handler_arm_synth[i].func_id, builder.func)
                })
                .collect();
            (span.clone(), refs)
        })
        .collect();

    // Per-user-fn FuncRefs, keyed by fn name. Used for direct calls
    // and for `func_addr` when a `ClosureRecord` stores a synthetic
    // fn's address.
    let user_fn_refs: BTreeMap<String, FuncRef> = ctx
        .user_fns
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
    // source-order list; span-keyed linear-search is O(small) and
    // robust.
    let lit_gvs: Vec<(Span, GlobalValue, usize)> = ctx
        .string_literals
        .iter()
        .enumerate()
        .map(|(idx, (span, s))| {
            let gv = module.declare_data_in_func(ctx.lit_ids[idx], builder.func);
            (span.clone(), gv, s.len())
        })
        .collect();
    let div_zero_gv = module.declare_data_in_func(ctx.div_zero_msg_id, builder.func);
    let mod_zero_gv = module.declare_data_in_func(ctx.mod_zero_msg_id, builder.func);

    PerFnRefs {
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
        next_step_done_ref,
        next_step_call_ref,
        next_step_args_ptr_ref,
        continuation_identity_ref,
        handler_arm_refs_per_handle,
        user_fn_refs,
        lit_gvs,
        div_zero_gv,
        mod_zero_gv,
    }
}

/// Plan B Task 55, Phase 4e — does this fn body match the **simple
/// tail perform with pure args** shape that the first slice of CPS
/// body lowering supports?
///
/// A body matches iff:
///
/// 1. Its statement list is empty.
/// 2. Its tail is a non-IO [`crate::ast::Expr::Perform`].
/// 3. **Every arg of the perform is pure** — see [`expr_is_pure`].
///    Calls (which may target CPS-color callees that yield to the
///    trampoline), nested performs, lambdas, closure records, and
///    handle expressions are all rejected; literals, identifiers,
///    arithmetic, and conditionals over pure sub-expressions are
///    accepted.
///
/// This is the strictest CPS-color body shape: lowering emits a
/// single `sigil_perform(effect_id, op_id, args_ptr, args_len,
/// k_closure_loaded, k_fn_loaded)` and returns the resulting
/// `*mut NextStep` directly. No continuation closures, no lambda-
/// lifting, no synthetic post-yield fn IDs needed — the
/// caller-supplied `(k_closure, k_fn)` pair (loaded from the fn's
/// `args_ptr` slots) becomes the continuation for the perform site.
///
/// **Why arg purity matters.** Without this check, a body like
/// `perform E.op(other_cps_helper())` would classify as eligible.
/// But evaluating `other_cps_helper()` synchronously inside a fn
/// that's already returning `*mut NextStep` is impossible — the
/// callee yields to the trampoline before its result is available.
/// The arg-purity check rejects such bodies up front so codegen
/// never emits incomplete CPS-form output. Reviewer-flagged in
/// PR #26 mid-flight at `a2840b6` (must-fix #2): the prior
/// "args may be arbitrary pure expressions" doc-only contract
/// without an enforcement check was the kind of structural debt
/// that gets paid through bug bisects.
///
/// **Why this carve-out exists.** The full Phase 4e roadmap needs
/// lambda-lifting machinery (closure-convert side-table extension
/// for synthetic continuation closures) to handle bodies with
/// stmts before the tail or with non-tail yield points. That
/// machinery is its own commit. This classifier identifies the
/// strictest body subset that does NOT need lambda-lifting, so the
/// first codegen-consumes-color slice can land a working CPS-ABI
/// path end-to-end without dragging in the larger machinery. Future
/// Phase 4e commits relax this classifier to cover:
///
/// 1. Pure stmts (let-bindings of pure expressions) followed by a
///    tail perform — still no lambda-lifting because the bindings
///    flow into the perform's args without crossing a yield.
/// 2. Conditional / match in tail position whose every branch is
///    itself a simple-tail-perform body.
/// 3. Calls in args via lambda-lifting (the synthetic-continuation
///    closure pre-pass).
/// 4. Arbitrary CPS-color bodies via the same pre-pass.
///
/// The classifier is conservative — false negatives are acceptable
/// (force the body through the existing native-ABI path), false
/// positives are not (would cause codegen to emit incomplete
/// CPS-form output).
///
/// Used by [`compute_user_fn_abi`] (the user-fn pre-pass ABI-
/// selection helper) to decide whether to declare the fn with the
/// [`cps_signature`] CPS ABI or with the existing native ABI. The
/// transitional `#[allow(dead_code)]` from `76c17ae` is removed
/// at this commit — the function is now consumed.
fn is_simple_tail_perform_with_pure_args_body(body: &crate::ast::Block) -> bool {
    if !body.stmts.is_empty() {
        return false;
    }
    match &body.tail {
        Some(crate::ast::Expr::Perform(p)) => {
            if p.effect == "IO" {
                return false;
            }
            p.args.iter().all(expr_is_pure)
        }
        _ => false,
    }
}

/// Plan B Task 55, Phase 4e — is this expression pure for the
/// purposes of [`is_simple_tail_perform_with_pure_args_body`]'s
/// arg-purity check?
///
/// "Pure" here means: evaluating the expression does NOT require
/// yielding to the trampoline (so the synchronous CPS-ABI body
/// lowering can evaluate it inline before building the perform's
/// NextStep). Literals, identifiers, closure-env loads, and
/// recursive compounds over pure sub-expressions are pure. Calls
/// (may target CPS callees), performs (yield by definition), and
/// lambdas/closure records (allocate but also indicate first-class
/// fn values that may need their own CPS treatment downstream) are
/// not.
///
/// Conservative: `Expr::Call` is rejected unconditionally even
/// though calls to Native-color callees are technically safe. The
/// alternative (color-aware purity) requires the colored program
/// at the analysis site, which the classifier doesn't have access
/// to. False negatives are acceptable; rejecting `int_to_string`
/// in a perform's args means the surrounding fn falls through to
/// the native-ABI path, which lowers correctly via the existing
/// synchronous shape.
fn expr_is_pure(e: &crate::ast::Expr) -> bool {
    use crate::ast::Expr;
    match e {
        Expr::IntLit(..)
        | Expr::StringLit(..)
        | Expr::BoolLit(..)
        | Expr::CharLit(..)
        | Expr::Ident(..)
        | Expr::ClosureEnvLoad { .. } => true,
        Expr::Binary { lhs, rhs, .. } => expr_is_pure(lhs) && expr_is_pure(rhs),
        Expr::Unary { operand, .. } => expr_is_pure(operand),
        Expr::If {
            cond,
            then_block,
            else_block,
            ..
        } => expr_is_pure(cond) && block_is_pure(then_block) && block_is_pure(else_block),
        Expr::Match {
            scrutinee, arms, ..
        } => expr_is_pure(scrutinee) && arms.iter().all(|a| expr_is_pure(&a.body)),
        Expr::Block(b) => block_is_pure(b),
        Expr::RecordLit { fields, .. } => fields.iter().all(|f| expr_is_pure(&f.value)),
        // Reject any yield-able shape and any first-class-fn-value
        // construction (lambdas / closure records). Conservative; a
        // future commit could allow lambdas if it's clear they don't
        // need CPS treatment downstream.
        Expr::Call { .. }
        | Expr::Perform(_)
        | Expr::Handle { .. }
        | Expr::Lambda { .. }
        | Expr::ClosureRecord { .. } => false,
    }
}

/// Helper for [`expr_is_pure`]: a [`crate::ast::Block`] is pure iff
/// every stmt and the tail are pure. `Stmt::Perform` is always
/// rejected (yield by definition). Used recursively to cover the
/// `Expr::If` / `Expr::Match` / `Expr::Block` arms of `expr_is_pure`.
fn block_is_pure(b: &crate::ast::Block) -> bool {
    use crate::ast::Stmt;
    for s in &b.stmts {
        match s {
            Stmt::Let(l) => {
                if !expr_is_pure(&l.value) {
                    return false;
                }
            }
            Stmt::Expr(e) => {
                if !expr_is_pure(e) {
                    return false;
                }
            }
            Stmt::Perform(_) => return false,
        }
    }
    match &b.tail {
        Some(t) => expr_is_pure(t),
        None => true,
    }
}

/// Plan B Task 55, Phase 4e — does this fn body match the **simple
/// yield then constant tail** shape that the first slice of lambda-
/// lifting supports?
///
/// A body matches iff:
///
/// 1. Its statement list has exactly one stmt.
/// 2. That stmt is a non-IO [`crate::ast::Stmt::Perform`].
/// 3. Every arg of the perform is pure (per [`expr_is_pure`]).
/// 4. The tail expression is a constant literal (initially:
///    [`crate::ast::Expr::IntLit`] only — future widenings cover
///    BoolLit / CharLit / StringLit).
///
/// **Why this carve-out exists.** The full Phase 4e lambda-lifting
/// machinery needs synthetic continuation closures that capture
/// helper's k_closure / k_fn and any user params referenced by
/// the post-yield rest-of-body. Closure captures require a
/// closure-convert side-table extension (the "S1" item from prior
/// reviews), which is its own commit. This classifier identifies
/// the strictest stmt-then-tail subset that does NOT need closure
/// captures: the synth-cont's body is a constant literal so it
/// captures NOTHING, and the perform's args are pure so they can
/// be lowered synchronously at the parent fn's entry block.
///
/// **Synth-cont shape under this classifier.** For a body matching
/// `perform E.op(); 42`, codegen synthesises:
///
/// ```text
/// extern "C" fn synth_cont(closure_ptr, args_ptr, args_len) -> *NextStep {
///     // closure_ptr / args_ptr / args_len ignored — Stmt::Perform
///     // discards the perform's result, and the tail is constant.
///     return sigil_next_step_done(42)
/// }
/// ```
///
/// Helper's body emit builds `sigil_perform(eff, op, args, len,
/// k_closure=null, k_fn=&synth_cont)` and returns its NextStep.
/// When the trampoline eventually dispatches the arm:
///   - **Discard-k arm** (no `k` in arm body) returns Done(arm_value)
///     directly — the synth-cont never runs. The handle's overall
///     value is the arm's value. **This is the discard-k correctness
///     fix for stmt-form perform yields**, the load-bearing piece
///     for inverting `statement_form_non_io_perform_inside_handle_
///     compiles_and_runs` (`42` → `99`).
///   - **Use-k arm** (`k(value)` in arm body) builds Call(synth_cont,
///     [value]); trampoline runs synth_cont which returns Done(42)
///     ignoring the value. Tail-position `k(value)` arms produce
///     the same observable behavior they would have under the
///     synchronous Phase 4d MVP shape (the tail expression is
///     the value).
///
/// Future Phase 4e commits widen this classifier to:
///
/// 1. Tail expressions referencing user params or let-bindings —
///    requires synth-cont closure captures.
/// 2. Multi-stmt bodies with multiple yields — requires per-yield
///    synth-cont chaining.
/// 3. `Stmt::Let(name, perform)` — synth-cont takes the perform's
///    result as `name` in its env.
fn is_simple_yield_then_constant_tail_body(body: &crate::ast::Block) -> bool {
    use crate::ast::{Expr, Stmt};
    if body.stmts.len() != 1 {
        return false;
    }
    let yield_perform = match &body.stmts[0] {
        Stmt::Perform(p) if p.effect != "IO" => p,
        _ => return false,
    };
    if !yield_perform.args.iter().all(expr_is_pure) {
        return false;
    }
    matches!(&body.tail, Some(Expr::IntLit(_, _)))
}

/// Plan B Task 55, Phase 4e — does this fn body match the **simple
/// let-yield then pure tail** shape that the captures-free
/// lambda-lifting slice supports?
///
/// A body matches iff:
///
/// 1. Its statement list has exactly one stmt.
/// 2. That stmt is a [`crate::ast::Stmt::Let`] whose value is a
///    non-IO [`crate::ast::Expr::Perform`] with all pure args
///    (per [`expr_is_pure`]).
/// 3. The tail expression is pure (per [`expr_is_pure`]).
///
/// **The big difference from `is_simple_yield_then_constant_tail
/// _body`**: the perform's result is bound by name in the source,
/// and the tail expression can reference that name. The synth-cont
/// must bind `args_ptr[0]` (the value passed to `k(...)` by the
/// arm) as the let-binding's name in its env, then lower the tail
/// via [`Lowerer`] with that env.
///
/// **Captures-free constraint**: this slice doesn't yet handle
/// helpers whose tail expression references user params (which
/// would require a closure record capturing helper's params). The
/// `compute_user_fn_abi` selector enforces `params.is_empty()` for
/// this body shape. Helpers with user params + this body shape
/// fall through to `UserFnAbi::Sync` (synchronous run_loop path,
/// the Phase 4d MVP behavior). The captures-bearing slice (next
/// major commit) lifts the arity-0 restriction.
///
/// **Why this carve-out exists.** This shape is exactly what the
/// `discard_k_handler_does_not_abort_helper_phase_4e_pending`
/// e2e test exercises:
///
/// ```text
/// fn helper() -> Int ![Raise, IO] {
///   let x: Int = perform Raise.fail();
///   x + 100
/// }
/// ```
///
/// Under Phase 4d MVP synchronous shape, `sigil_run_loop` returned
/// the arm value (42) to the perform site, x got bound to 42,
/// helper computed 42 + 100 = 142, handle's overall = 142. Phase
/// 4e correctness: when the arm discards `k` (no reference to k
/// in the arm body), the synth-cont never runs, helper's rest-of-
/// body is dropped, the arm value (42) flows directly to the
/// handle site. **Inverts the discard_k test from `142` to
/// `42`.**
///
/// When the arm uses `k(value)`, the synth-cont fires, binds
/// `args_ptr[0] = value` as `x`, lowers `x + 100` via Lowerer,
/// returns `Done(value + 100)`. Trampoline returns that to the
/// wrapper.
///
/// Future widening from this slice (captures-bearing slice):
///   1. Helpers with user params + tail referencing them — synth-
///      cont closure record captures helper's params.
///   2. Multi-yield bodies (`perform; perform; tail`) — chained
///      synth-conts.
fn is_simple_let_yield_then_pure_tail_body(body: &crate::ast::Block) -> bool {
    use crate::ast::{Expr, Stmt};
    if body.stmts.len() != 1 {
        return false;
    }
    let let_stmt = match &body.stmts[0] {
        Stmt::Let(l) => l,
        _ => return false,
    };
    let yield_perform = match &let_stmt.value {
        Expr::Perform(p) if p.effect != "IO" => p,
        _ => return false,
    };
    if !yield_perform.args.iter().all(expr_is_pure) {
        return false;
    }
    match &body.tail {
        Some(t) => expr_is_pure(t),
        None => false,
    }
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

    // ---------------- Plan B Task 55, Phase 4e — body-shape classifier
    //
    // Pins `is_simple_tail_perform_with_pure_args_body` for the body shapes the
    // first slice of CPS body lowering supports vs. rejects. The
    // classifier identifies bodies that don't need lambda-lifting:
    // empty stmts + tail = non-IO `Expr::Perform`. Pure analysis at
    // HEAD; the next commit consumes it for ABI selection.

    #[test]
    fn simple_tail_perform_body_recognised() {
        use crate::ast::{Block, Expr, PerformExpr};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let body = Block {
            stmts: Vec::new(),
            tail: Some(Expr::Perform(PerformExpr {
                effect: "Raise".to_string(),
                op: "fail".to_string(),
                args: Vec::new(),
                span: span.clone(),
            })),
            span,
        };
        assert!(is_simple_tail_perform_with_pure_args_body(&body));
    }

    #[test]
    fn simple_tail_perform_with_args_recognised() {
        // The classifier doesn't inspect args — they may be arbitrary
        // pure expressions that get lowered alongside the perform's
        // own lowering. What matters is that the tail is the
        // `Expr::Perform` and there are no preceding stmts.
        use crate::ast::{Block, Expr, PerformExpr};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let body = Block {
            stmts: Vec::new(),
            tail: Some(Expr::Perform(PerformExpr {
                effect: "Raise".to_string(),
                op: "fail".to_string(),
                args: vec![Expr::IntLit(42, span.clone())],
                span: span.clone(),
            })),
            span,
        };
        assert!(is_simple_tail_perform_with_pure_args_body(&body));
    }

    #[test]
    fn io_perform_in_tail_is_not_simple_tail_perform() {
        // IO performs use the synchronous `lower_perform` path
        // regardless of color (special-cased; doesn't route through
        // `sigil_perform`). The classifier returns false so codegen
        // doesn't try to declare an IO-only fn with the CPS ABI.
        use crate::ast::{Block, Expr, PerformExpr};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let body = Block {
            stmts: Vec::new(),
            tail: Some(Expr::Perform(PerformExpr {
                effect: "IO".to_string(),
                op: "println".to_string(),
                args: vec![Expr::StringLit("hi".to_string(), span.clone())],
                span: span.clone(),
            })),
            span,
        };
        assert!(!is_simple_tail_perform_with_pure_args_body(&body));
    }

    #[test]
    fn body_with_stmt_before_tail_perform_is_not_simple() {
        // `{ let x: Int = 1; perform Raise.fail() }` — the let stmt
        // is pure but the classifier rejects it because lowering it
        // requires binding `x` in the env before the perform.
        // Subsequent commits widen to cover this shape (it doesn't
        // need lambda-lifting, just stmt prologue lowering); the
        // first slice keeps the strictest definition.
        use crate::ast::{Block, Expr, LetStmt, PerformExpr, Stmt, TypeExpr};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let body = Block {
            stmts: vec![Stmt::Let(LetStmt {
                name: "x".to_string(),
                ty: TypeExpr::Named("Int".to_string(), span.clone()),
                value: Expr::IntLit(1, span.clone()),
                span: span.clone(),
            })],
            tail: Some(Expr::Perform(PerformExpr {
                effect: "Raise".to_string(),
                op: "fail".to_string(),
                args: Vec::new(),
                span: span.clone(),
            })),
            span,
        };
        assert!(!is_simple_tail_perform_with_pure_args_body(&body));
    }

    #[test]
    fn pure_tail_value_is_not_simple_tail_perform() {
        use crate::ast::{Block, Expr};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let body = Block {
            stmts: Vec::new(),
            tail: Some(Expr::IntLit(42, span.clone())),
            span,
        };
        assert!(!is_simple_tail_perform_with_pure_args_body(&body));
    }

    // ---------------- Plan B Task 55, Phase 4e — yield-then-
    // constant-tail classifier (lambda-lifting first slice).

    #[test]
    fn yield_then_constant_tail_body_recognised() {
        use crate::ast::{Block, Expr, PerformExpr, Stmt};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let body = Block {
            stmts: vec![Stmt::Perform(PerformExpr {
                effect: "Raise".to_string(),
                op: "fail".to_string(),
                args: Vec::new(),
                span: span.clone(),
            })],
            tail: Some(Expr::IntLit(42, span.clone())),
            span,
        };
        assert!(is_simple_yield_then_constant_tail_body(&body));
    }

    #[test]
    fn yield_then_constant_tail_with_pure_perform_args_recognised() {
        // The classifier accepts pure perform args (Idents,
        // literals, pure compounds). They get lowered via Lowerer
        // at body-emit time; the env is populated from the user-
        // arg unpack at the parent fn's entry block.
        use crate::ast::{Block, Expr, PerformExpr, Stmt};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let body = Block {
            stmts: vec![Stmt::Perform(PerformExpr {
                effect: "Raise".to_string(),
                op: "fail".to_string(),
                args: vec![Expr::Ident("x".to_string(), span.clone())],
                span: span.clone(),
            })],
            tail: Some(Expr::IntLit(42, span.clone())),
            span,
        };
        assert!(is_simple_yield_then_constant_tail_body(&body));
    }

    #[test]
    fn yield_then_non_constant_tail_is_not_yield_then_constant() {
        // Tail is `Expr::Ident("x")` — not a literal constant.
        // Future widening (with closure captures) will support
        // this; first slice restricts to literal tail.
        use crate::ast::{Block, Expr, PerformExpr, Stmt};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let body = Block {
            stmts: vec![Stmt::Perform(PerformExpr {
                effect: "Raise".to_string(),
                op: "fail".to_string(),
                args: Vec::new(),
                span: span.clone(),
            })],
            tail: Some(Expr::Ident("x".to_string(), span.clone())),
            span,
        };
        assert!(!is_simple_yield_then_constant_tail_body(&body));
    }

    #[test]
    fn yield_with_io_perform_is_not_yield_then_constant() {
        // IO performs use the synchronous lower_perform path
        // regardless of color; classifier rejects.
        use crate::ast::{Block, Expr, PerformExpr, Stmt};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let body = Block {
            stmts: vec![Stmt::Perform(PerformExpr {
                effect: "IO".to_string(),
                op: "println".to_string(),
                args: vec![Expr::StringLit("hi".to_string(), span.clone())],
                span: span.clone(),
            })],
            tail: Some(Expr::IntLit(42, span.clone())),
            span,
        };
        assert!(!is_simple_yield_then_constant_tail_body(&body));
    }

    // ---------------- Plan B Task 55, Phase 4e — let-yield-then-
    // pure-tail classifier (lambda-lifting captures-free slice).

    #[test]
    fn let_yield_then_pure_tail_body_recognised() {
        // `let x: Int = perform Raise.fail(); 42` — happy path with
        // a literal tail. Even though a constant tail also matches
        // `is_simple_yield_then_constant_tail_body` for the
        // Stmt::Perform form, the let-form here goes through the
        // let-yield classifier.
        use crate::ast::{Block, Expr, LetStmt, PerformExpr, Stmt, TypeExpr};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let body = Block {
            stmts: vec![Stmt::Let(LetStmt {
                name: "x".to_string(),
                ty: TypeExpr::Named("Int".to_string(), span.clone()),
                value: Expr::Perform(PerformExpr {
                    effect: "Raise".to_string(),
                    op: "fail".to_string(),
                    args: Vec::new(),
                    span: span.clone(),
                }),
                span: span.clone(),
            })],
            tail: Some(Expr::IntLit(42, span.clone())),
            span,
        };
        assert!(is_simple_let_yield_then_pure_tail_body(&body));
    }

    #[test]
    fn let_yield_then_binary_using_binding_recognised() {
        // `let x: Int = perform Raise.fail(); x + 100` — the
        // `discard_k_handler_does_abort_helper_across_call_boundary`
        // e2e test's helper shape. Tail is a pure Binary referencing
        // the let-binding.
        use crate::ast::{BinOp, Block, Expr, LetStmt, PerformExpr, Stmt, TypeExpr};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let body = Block {
            stmts: vec![Stmt::Let(LetStmt {
                name: "x".to_string(),
                ty: TypeExpr::Named("Int".to_string(), span.clone()),
                value: Expr::Perform(PerformExpr {
                    effect: "Raise".to_string(),
                    op: "fail".to_string(),
                    args: Vec::new(),
                    span: span.clone(),
                }),
                span: span.clone(),
            })],
            tail: Some(Expr::Binary {
                op: BinOp::Add,
                lhs: Box::new(Expr::Ident("x".to_string(), span.clone())),
                rhs: Box::new(Expr::IntLit(100, span.clone())),
                span: span.clone(),
            }),
            span,
        };
        assert!(is_simple_let_yield_then_pure_tail_body(&body));
    }

    #[test]
    fn let_yield_then_call_in_tail_is_not_let_yield_then_pure() {
        // `let x: Int = perform Raise.fail(); helper(x)` — tail
        // contains a Call (impure per `expr_is_pure`). Classifier
        // rejects; helper falls through to Sync ABI cleanly.
        use crate::ast::{Block, Expr, LetStmt, PerformExpr, Stmt, TypeExpr};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let body = Block {
            stmts: vec![Stmt::Let(LetStmt {
                name: "x".to_string(),
                ty: TypeExpr::Named("Int".to_string(), span.clone()),
                value: Expr::Perform(PerformExpr {
                    effect: "Raise".to_string(),
                    op: "fail".to_string(),
                    args: Vec::new(),
                    span: span.clone(),
                }),
                span: span.clone(),
            })],
            tail: Some(Expr::Call {
                callee: Box::new(Expr::Ident("helper".to_string(), span.clone())),
                args: vec![Expr::Ident("x".to_string(), span.clone())],
                span: span.clone(),
            }),
            span,
        };
        assert!(!is_simple_let_yield_then_pure_tail_body(&body));
    }

    #[test]
    fn let_yield_with_io_perform_value_is_not_let_yield_then_pure() {
        // `let s: String = perform IO.println("hi"); ...` — IO
        // performs use the synchronous lower_perform path
        // regardless of color; the let-yield classifier rejects.
        // Mirrors the IO rejection from
        // `is_simple_yield_then_constant_tail_body`.
        use crate::ast::{Block, Expr, LetStmt, PerformExpr, Stmt, TypeExpr};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let body = Block {
            stmts: vec![Stmt::Let(LetStmt {
                name: "s".to_string(),
                ty: TypeExpr::Named("Unit".to_string(), span.clone()),
                value: Expr::Perform(PerformExpr {
                    effect: "IO".to_string(),
                    op: "println".to_string(),
                    args: vec![Expr::StringLit("hi".to_string(), span.clone())],
                    span: span.clone(),
                }),
                span: span.clone(),
            })],
            tail: Some(Expr::IntLit(0, span.clone())),
            span,
        };
        assert!(!is_simple_let_yield_then_pure_tail_body(&body));
    }

    #[test]
    fn multi_stmt_body_with_let_yield_first_is_not_let_yield_then_pure() {
        // Classifier requires exactly one stmt. Multi-stmt bodies
        // with a let-yield as stmts[0] (followed by other stmts)
        // need full lambda-lifting (chained synth-conts) — future
        // commit.
        use crate::ast::{Block, Expr, LetStmt, PerformExpr, Stmt, TypeExpr};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let body = Block {
            stmts: vec![
                Stmt::Let(LetStmt {
                    name: "x".to_string(),
                    ty: TypeExpr::Named("Int".to_string(), span.clone()),
                    value: Expr::Perform(PerformExpr {
                        effect: "Raise".to_string(),
                        op: "fail".to_string(),
                        args: Vec::new(),
                        span: span.clone(),
                    }),
                    span: span.clone(),
                }),
                // Second stmt — disqualifies the body from this
                // classifier's exactly-one-stmt requirement.
                Stmt::Expr(Expr::IntLit(1, span.clone())),
            ],
            tail: Some(Expr::Ident("x".to_string(), span.clone())),
            span,
        };
        assert!(!is_simple_let_yield_then_pure_tail_body(&body));
    }

    #[test]
    fn let_yield_with_match_tail_using_binding_recognised() {
        // Recursive purity: a Match expression over a pure scrutinee
        // (the binding) with pure arm bodies (literals) is pure.
        // The synth-cont's Lowerer handles Match correctly via
        // `lower_expr` → `lower_match`. Pin acceptance.
        use crate::ast::{Block, Expr, LetStmt, MatchArm, Pattern, PerformExpr, Stmt, TypeExpr};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let body = Block {
            stmts: vec![Stmt::Let(LetStmt {
                name: "x".to_string(),
                ty: TypeExpr::Named("Bool".to_string(), span.clone()),
                value: Expr::Perform(PerformExpr {
                    effect: "Raise".to_string(),
                    op: "fail".to_string(),
                    args: Vec::new(),
                    span: span.clone(),
                }),
                span: span.clone(),
            })],
            tail: Some(Expr::Match {
                scrutinee: Box::new(Expr::Ident("x".to_string(), span.clone())),
                arms: vec![
                    MatchArm {
                        pattern: Pattern::BoolLit(true, span.clone()),
                        body: Expr::IntLit(1, span.clone()),
                        span: span.clone(),
                    },
                    MatchArm {
                        pattern: Pattern::BoolLit(false, span.clone()),
                        body: Expr::IntLit(0, span.clone()),
                        span: span.clone(),
                    },
                ],
                span: span.clone(),
            }),
            span,
        };
        assert!(is_simple_let_yield_then_pure_tail_body(&body));
    }

    #[test]
    fn multi_stmt_body_is_not_yield_then_constant() {
        // Classifier requires exactly one stmt. Multi-stmt bodies
        // need full lambda-lifting (with chained synth-conts) —
        // future commit.
        use crate::ast::{Block, Expr, PerformExpr, Stmt};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let body = Block {
            stmts: vec![
                Stmt::Perform(PerformExpr {
                    effect: "Raise".to_string(),
                    op: "fail".to_string(),
                    args: Vec::new(),
                    span: span.clone(),
                }),
                Stmt::Perform(PerformExpr {
                    effect: "Raise".to_string(),
                    op: "fail".to_string(),
                    args: Vec::new(),
                    span: span.clone(),
                }),
            ],
            tail: Some(Expr::IntLit(42, span.clone())),
            span,
        };
        assert!(!is_simple_yield_then_constant_tail_body(&body));
    }

    #[test]
    fn empty_body_is_not_simple_tail_perform() {
        use crate::ast::Block;
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let body = Block {
            stmts: Vec::new(),
            tail: None,
            span,
        };
        assert!(!is_simple_tail_perform_with_pure_args_body(&body));
    }

    #[test]
    fn perform_with_call_arg_is_not_simple_tail_perform_with_pure_args() {
        // PR #26 mid-flight at a2840b6 must-fix #2: the classifier
        // must reject a perform whose args contain a call. Calls
        // may target CPS-color callees that yield to the
        // trampoline, which is incompatible with the synchronous
        // CPS body lowering this classifier gates. Synthetic AST
        // because we want to exercise the purity check directly,
        // independent of typecheck.
        use crate::ast::{Block, Expr, PerformExpr};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let body = Block {
            stmts: Vec::new(),
            tail: Some(Expr::Perform(PerformExpr {
                effect: "Raise".to_string(),
                op: "fail".to_string(),
                args: vec![Expr::Call {
                    callee: Box::new(Expr::Ident("some_helper".to_string(), span.clone())),
                    args: Vec::new(),
                    span: span.clone(),
                }],
                span: span.clone(),
            })),
            span,
        };
        assert!(!is_simple_tail_perform_with_pure_args_body(&body));
    }

    #[test]
    fn perform_with_perform_arg_is_not_simple_tail_perform_with_pure_args() {
        // Similar to above but with a perform inside another
        // perform's args. The inner perform yields, so the outer
        // can't be evaluated synchronously inside a CPS-ABI body.
        use crate::ast::{Block, Expr, PerformExpr};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let body = Block {
            stmts: Vec::new(),
            tail: Some(Expr::Perform(PerformExpr {
                effect: "Raise".to_string(),
                op: "fail".to_string(),
                args: vec![Expr::Perform(PerformExpr {
                    effect: "Other".to_string(),
                    op: "boom".to_string(),
                    args: Vec::new(),
                    span: span.clone(),
                })],
                span: span.clone(),
            })),
            span,
        };
        assert!(!is_simple_tail_perform_with_pure_args_body(&body));
    }

    #[test]
    fn perform_with_pure_compound_arg_is_simple_tail_perform_with_pure_args() {
        // Recursive purity: pure compound (binary, conditional)
        // over pure leaves is still pure. `perform E.op(if true {
        // 1 } else { 2 })` qualifies.
        use crate::ast::{Block, Expr, PerformExpr};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let then_block = Block {
            stmts: Vec::new(),
            tail: Some(Expr::IntLit(1, span.clone())),
            span: span.clone(),
        };
        let else_block = Block {
            stmts: Vec::new(),
            tail: Some(Expr::IntLit(2, span.clone())),
            span: span.clone(),
        };
        let body = Block {
            stmts: Vec::new(),
            tail: Some(Expr::Perform(PerformExpr {
                effect: "Raise".to_string(),
                op: "fail".to_string(),
                args: vec![Expr::If {
                    cond: Box::new(Expr::BoolLit(true, span.clone())),
                    then_block: Box::new(then_block),
                    else_block: Box::new(else_block),
                    span: span.clone(),
                }],
                span: span.clone(),
            })),
            span,
        };
        assert!(is_simple_tail_perform_with_pure_args_body(&body));
    }

    // ---------------- Plan B Task 55, Phase 4e — ABI selection
    //
    // Pins `compute_user_fn_abi`'s combination rule: a fn is
    // `UserFnAbi::Cps` iff the colorer marks it CPS AND its body
    // matches `is_simple_tail_perform_with_pure_args_body`. The selector exists so
    // future signature-declaration changes (next commit) consult it
    // rather than re-deriving the rule inline.
    //
    // Tests use the lex → parse → resolve → typecheck → elaborate
    // → monomorphize → infer_colors pipeline (matches the test
    // helper pattern in `color::tests::color_from_src`) so the
    // full real-program shape is exercised, not synthetic AST.

    fn colored_from_src(src: &str) -> (crate::ast::Program, ColoredProgram) {
        let file = "test.sigil";
        let (tokens, lex_errs) = crate::lexer::lex(file, src);
        assert!(lex_errs.is_empty(), "lex: {lex_errs:?}");
        let (prog, parse_errs) = crate::parser::parse(file, &tokens);
        assert!(parse_errs.is_empty(), "parse: {parse_errs:?}");
        let (resolved, resolve_errs) = crate::resolve::resolve(prog);
        assert!(resolve_errs.is_empty(), "resolve: {resolve_errs:?}");
        let (checked, tc_errs) = crate::typecheck::typecheck(resolved.program);
        let hard_errs: Vec<_> = tc_errs
            .iter()
            .filter(|e| matches!(e.severity, crate::errors::Severity::Error))
            .collect();
        assert!(hard_errs.is_empty(), "typecheck: {hard_errs:?}");
        let prog_clone = checked.program.clone();
        let anf = crate::elaborate::elaborate(checked);
        let mono = crate::monomorphize::monomorphize(anf);
        let colored = crate::color::infer_colors(mono);
        (prog_clone, colored)
    }

    fn body_of(prog: &crate::ast::Program, name: &str) -> crate::ast::Block {
        for item in &prog.items {
            if let crate::ast::Item::Fn(f) = item {
                if f.name == name {
                    return f.body.clone();
                }
            }
        }
        panic!("no fn `{name}` in program");
    }

    fn params_of(prog: &crate::ast::Program, name: &str) -> Vec<crate::ast::Param> {
        for item in &prog.items {
            if let crate::ast::Item::Fn(f) = item {
                if f.name == name {
                    return f.params.clone();
                }
            }
        }
        panic!("no fn `{name}` in program");
    }

    #[test]
    fn compute_user_fn_abi_native_for_pure_main() {
        let src = r#"
            fn main() -> Int ![] { 42 }
        "#;
        let (prog, colored) = colored_from_src(src);
        let body = body_of(&prog, "main");
        assert_eq!(
            compute_user_fn_abi("main", &body, &colored),
            UserFnAbi::Sync
        );
    }

    #[test]
    fn compute_user_fn_abi_cps_for_simple_tail_perform_helper() {
        // helper has a single tail-position perform; row contains
        // non-IO effect → intrinsic CPS color. Body shape matches
        // `is_simple_tail_perform_with_pure_args_body`. Combined: Cps.
        let src = "effect E { op: () -> Int }\n\
                   fn helper() -> Int ![E] { perform E.op() }\n\
                   fn main() -> Int ![IO] {\n  \
                     let n: Int = handle helper() with { E.op(k) => 42 };\n  \
                     perform IO.println(int_to_string(n));\n  \
                     0\n\
                   }\n";
        let (prog, colored) = colored_from_src(src);
        let helper_body = body_of(&prog, "helper");
        assert_eq!(
            compute_user_fn_abi("helper", &helper_body, &colored),
            UserFnAbi::Cps,
            "helper has CPS color + simple-tail-perform body + arity-0"
        );
    }

    #[test]
    fn compute_user_fn_abi_native_for_cps_color_main_with_complex_body() {
        // main is CPS via SCC bridge to helper, but its body has
        // multiple stmts (not simple-tail-perform). Result: Native
        // ABI (the conservative default — main keeps the
        // synchronous-`run_loop` shape until the lambda-lifting
        // commit covers complex bodies).
        let src = "effect E { op: () -> Int }\n\
                   fn helper() -> Int ![E] { perform E.op() }\n\
                   fn main() -> Int ![IO] {\n  \
                     let n: Int = handle helper() with { E.op(k) => 42 };\n  \
                     perform IO.println(int_to_string(n));\n  \
                     0\n\
                   }\n";
        let (prog, colored) = colored_from_src(src);
        let main_body = body_of(&prog, "main");
        // Sanity-check: main IS classified CPS by the colorer (bridge).
        assert!(
            colored.needs_cps_transform("main"),
            "colorer should classify main as CPS via bridge to helper"
        );
        // ABI selection still picks Sync because main's body shape
        // isn't supported by the simple-tail-perform classifier.
        assert_eq!(
            compute_user_fn_abi("main", &main_body, &colored),
            UserFnAbi::Sync,
            "main is CPS-color but has complex body → Sync ABI"
        );
    }

    #[test]
    fn compute_user_fn_abi_cps_for_intrinsic_perform_in_empty_row_synthetic() {
        // Renamed from the prior misleading
        // `..._native_for_native_color_with_simple_tail_perform_body`
        // (PR #26 mid-flight at a2840b6 must-fix #1a). Despite the
        // synthetic fn having empty effects row, the colorer
        // classifies it CPS because `find_non_io_perform_in_block`
        // walks the body and taints on the perform of `Raise.fail`.
        // The synthetic shape is unreachable through the real
        // pipeline (typecheck rejects with E0042: unhandled
        // effect), but the codegen-side accessor must remain
        // robust against post-mono synthetic programs.
        //
        // What this test pins: when the colorer says CPS (for any
        // reason — row, body taint, SCC bridge), `compute_user_fn_
        // abi` checks the body shape and returns Cps when both
        // gates pass. Combined with
        // `..._sync_when_color_native_short_circuits_regardless_of_
        // body_shape` below, the two together exercise both
        // positive arms of the `&&` in `compute_user_fn_abi`.
        use crate::ast::{Block, Expr, FnDecl, Item, PerformExpr, Program, TypeExpr};
        use crate::elaborate::AnfProgram;
        use crate::errors::Span;
        use crate::monomorphize::MonoProgram;
        use crate::typecheck::CheckedProgram;
        let span = Span::synthetic("test.sigil");
        let body = Block {
            stmts: Vec::new(),
            tail: Some(Expr::Perform(PerformExpr {
                effect: "Raise".to_string(),
                op: "fail".to_string(),
                args: Vec::new(),
                span: span.clone(),
            })),
            span: span.clone(),
        };
        let fn_decl = FnDecl {
            name: "f".to_string(),
            name_span: span.clone(),
            generic_params: Vec::new(),
            params: Vec::new(),
            return_type: TypeExpr::Named("Int".to_string(), span.clone()),
            effects: Vec::new(),
            effect_row_var: None,
            body: body.clone(),
            span: span.clone(),
        };
        let program = Program {
            items: vec![Item::Fn(Box::new(fn_decl))],
            file: "test.sigil".to_string(),
        };
        let checked = CheckedProgram {
            program,
            string_literals: Vec::new(),
            lambda_captures: Vec::new(),
            types: std::collections::BTreeMap::new(),
            match_scrut_tys: std::collections::BTreeMap::new(),
            fn_schemes: std::collections::BTreeMap::new(),
            call_site_instantiations: std::collections::BTreeMap::new(),
            ctor_site_instantiations: std::collections::BTreeMap::new(),
            effects: std::collections::BTreeMap::new(),
            effect_ids: std::collections::BTreeMap::new(),
            op_ids: std::collections::BTreeMap::new(),
            handle_arm_captures: std::collections::BTreeMap::new(),
        };
        let anf = AnfProgram { checked };
        let mono = MonoProgram { anf };
        let colored = crate::color::infer_colors(mono);
        assert_eq!(
            compute_user_fn_abi("f", &body, &colored),
            UserFnAbi::Cps,
            "intrinsic perform of non-IO effect taints the fn as CPS via \
             find_non_io_perform_in_block; combined with the simple-tail-\
             perform body shape AND arity-0 user params AND arity-0 \
             perform args, compute_user_fn_abi returns Cps"
        );
    }

    #[test]
    fn compute_user_fn_abi_sync_when_color_native_short_circuits_regardless_of_body_shape() {
        // PR #26 mid-flight at a2840b6 must-fix #1b: pin that
        // `compute_user_fn_abi` short-circuits on `Color::Native`
        // and returns `UserFnAbi::Sync` regardless of body shape.
        // The previous test infrastructure routes through
        // `infer_colors`, where any non-IO perform in a body taints
        // the fn CPS — leaving the Native short-circuit untested.
        //
        // Bypass `infer_colors` and construct a `ColoredProgram`
        // directly with a synthetic Native classification for a fn
        // whose body IS simple-tail-perform. If a future refactor
        // reordered the `&&` in `compute_user_fn_abi` (body-gate
        // before color-gate), this test would fail.
        use crate::ast::{Block, Expr, FnDecl, Item, PerformExpr, Program, TypeExpr};
        use crate::color::{Color, ColoredProgram};
        use crate::elaborate::AnfProgram;
        use crate::errors::Span;
        use crate::monomorphize::MonoProgram;
        use crate::typecheck::CheckedProgram;
        let span = Span::synthetic("test.sigil");
        let body = Block {
            stmts: Vec::new(),
            tail: Some(Expr::Perform(PerformExpr {
                effect: "Raise".to_string(),
                op: "fail".to_string(),
                args: Vec::new(),
                span: span.clone(),
            })),
            span: span.clone(),
        };
        let fn_decl = FnDecl {
            name: "f".to_string(),
            name_span: span.clone(),
            generic_params: Vec::new(),
            params: Vec::new(),
            return_type: TypeExpr::Named("Int".to_string(), span.clone()),
            effects: Vec::new(),
            effect_row_var: None,
            body: body.clone(),
            span: span.clone(),
        };
        let program = Program {
            items: vec![Item::Fn(Box::new(fn_decl))],
            file: "test.sigil".to_string(),
        };
        let checked = CheckedProgram {
            program,
            string_literals: Vec::new(),
            lambda_captures: Vec::new(),
            types: std::collections::BTreeMap::new(),
            match_scrut_tys: std::collections::BTreeMap::new(),
            fn_schemes: std::collections::BTreeMap::new(),
            call_site_instantiations: std::collections::BTreeMap::new(),
            ctor_site_instantiations: std::collections::BTreeMap::new(),
            effects: std::collections::BTreeMap::new(),
            effect_ids: std::collections::BTreeMap::new(),
            op_ids: std::collections::BTreeMap::new(),
            handle_arm_captures: std::collections::BTreeMap::new(),
        };
        let anf = AnfProgram { checked };
        let mono = MonoProgram { anf };
        // Build ColoredProgram manually with `colors[("f")] = Native`,
        // overriding what `infer_colors` would have returned. This is
        // the test setup that pins the short-circuit; reordering the
        // `&&` in `compute_user_fn_abi` to check body shape first
        // would make this test fail.
        let colored = ColoredProgram {
            mono,
            colors: vec![("f".to_string(), Color::Native)],
            reasons: vec![("f".to_string(), "test: forced Native".to_string())],
        };
        // Sanity: the body IS simple-tail-perform; if not for the
        // forced-Native classification, the fn would be Cps.
        assert!(is_simple_tail_perform_with_pure_args_body(&body));
        // The actual assertion: Sync, because color short-circuits.
        assert_eq!(
            compute_user_fn_abi("f", &body, &colored),
            UserFnAbi::Sync,
            "Color::Native must short-circuit `compute_user_fn_abi` \
             regardless of body shape eligibility"
        );
    }

    // ---------------- Plan B Task 55, Phase 4e — arity-widening
    // (D1 inversion).
    //
    // The prior commit (5a0459a) added arity gates to
    // `compute_user_fn_abi` that rejected arity-N user fns and
    // arity-N perform args, falling through to `Sync` ABI to
    // avoid crashing the body-emit `assert!`s. This commit
    // removes those gates AND widens the body emission + caller
    // wrapper to support arity-N. The two tests below were
    // negative regression guards (Sync) under the prior arity
    // gates; they are inverted now to positive guards (Cps)
    // pinning the widened classification.

    #[test]
    fn compute_user_fn_abi_cps_for_arity_n_intrinsic_cps_helper() {
        // helper has arity-1 (`x: Int`); body is a single tail
        // perform with the arg `x` (pure Ident). Pre-arity-widening
        // (5a0459a), the D1 user-fn-arity gate rejected → Sync.
        // Post-widening (this commit), the gate is gone → Cps. The
        // body emission unpacks `x` from `args_ptr[0]` and
        // forwards it to the perform's args buffer; the wrapper at
        // call sites packs the user arg before the trailing
        // (k_closure, k_fn) pair.
        let src = "effect E { op: (Int) -> Int }\n\
                   fn helper(x: Int) -> Int ![E] { perform E.op(x) }\n\
                   fn main() -> Int ![IO] {\n  \
                     let n: Int = handle helper(7) with { E.op(arg, k) => arg + 35 };\n  \
                     perform IO.println(int_to_string(n));\n  \
                     0\n\
                   }\n";
        let (prog, colored) = colored_from_src(src);
        let helper_body = body_of(&prog, "helper");
        let helper_params = params_of(&prog, "helper");
        // Sanity: classifier accepts the body shape (perform's
        // arg `x` is pure Ident); color taints CPS via row.
        assert!(is_simple_tail_perform_with_pure_args_body(&helper_body));
        assert!(colored.needs_cps_transform("helper"));
        assert_eq!(helper_params.len(), 1);
        // Inverted from prior commit's Sync expectation — the
        // arity gate is gone.
        assert_eq!(
            compute_user_fn_abi("helper", &helper_body, &colored),
            UserFnAbi::Cps,
            "arity-N intrinsic-CPS helper now classifies Cps after the \
             D1 arity gate's removal in this commit"
        );
    }

    #[test]
    fn compute_user_fn_abi_cps_for_arity_0_helper_with_perform_args() {
        // helper has arity-0 user params, but the tail perform has
        // a literal arg. Pre-arity-widening, the D1 perform-args
        // gate rejected → Sync. Post-widening, the gate is gone
        // → Cps. The body emission packs the literal `7` into a
        // stack slot for `sigil_perform`.
        let src = "effect E { op: (Int) -> Int }\n\
                   fn helper() -> Int ![E] { perform E.op(7) }\n\
                   fn main() -> Int ![IO] {\n  \
                     let n: Int = handle helper() with { E.op(arg, k) => arg + 35 };\n  \
                     perform IO.println(int_to_string(n));\n  \
                     0\n\
                   }\n";
        let (prog, colored) = colored_from_src(src);
        let helper_body = body_of(&prog, "helper");
        let helper_params = params_of(&prog, "helper");
        assert!(is_simple_tail_perform_with_pure_args_body(&helper_body));
        assert!(colored.needs_cps_transform("helper"));
        assert!(helper_params.is_empty());
        assert_eq!(
            compute_user_fn_abi("helper", &helper_body, &colored),
            UserFnAbi::Cps,
            "arity-0 helper with arity-N perform now classifies Cps after \
             the D1 perform-args gate's removal in this commit"
        );
    }

    // ---------------- Plan B Task 55, Phase 4e — captures-bearing
    // synth-cont slice (this commit): the let-yield-then-pure-tail
    // shape now accepts arity-N helpers, with the synth-cont
    // capturing helper's user params referenced in the tail.

    #[test]
    fn compute_user_fn_abi_cps_for_arity_n_let_yield_helper_with_capture() {
        // helper takes `threshold: Int`; body is `let x = perform
        // Raise.fail(); x + threshold`. Pre-captures-slice
        // (`f911a0b`), the arity-0 gate in `compute_user_fn_abi`
        // rejected → Sync. Post-captures-slice (this commit), the
        // gate is removed → Cps. The pre-pass populates
        // `LetBindThenTail.captures = [SynthContCapture { name:
        // "threshold", kind: Int }]`; helper's body emit allocates
        // a closure record holding `threshold`, passes its pointer
        // as `k_closure`; synth-cont reads `threshold` from
        // `closure_ptr + 16` at fn entry.
        let src = "effect Raise { fail: () -> Int }\n\
                   fn helper(threshold: Int) -> Int ![Raise, IO] {\n  \
                     let x: Int = perform Raise.fail();\n  \
                     x + threshold\n\
                   }\n\
                   fn main() -> Int ![IO] {\n  \
                     let n: Int = handle helper(10) with { Raise.fail(k) => 42 };\n  \
                     perform IO.println(int_to_string(n));\n  \
                     0\n\
                   }\n";
        let (prog, colored) = colored_from_src(src);
        let helper_body = body_of(&prog, "helper");
        let helper_params = params_of(&prog, "helper");
        // Sanity: classifier accepts the body shape; color taints
        // CPS via row.
        assert!(is_simple_let_yield_then_pure_tail_body(&helper_body));
        assert!(colored.needs_cps_transform("helper"));
        assert_eq!(helper_params.len(), 1);
        // Inverted from prior arity-0 restriction — the gate is
        // gone in this commit.
        assert_eq!(
            compute_user_fn_abi("helper", &helper_body, &colored),
            UserFnAbi::Cps,
            "arity-N let-yield-helper with capture now classifies Cps after \
             the captures-bearing slice's arity gate lift"
        );
    }

    #[test]
    fn collect_synth_cont_captures_finds_helper_param_in_tail() {
        // Free-var analysis on `x + threshold` with let-binding
        // `x` and helper params `[threshold]` returns
        // `[threshold]`. Pin the analysis surface directly.
        use crate::ast::{BinOp, Expr, Param, TypeExpr};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let tail = Expr::Binary {
            op: BinOp::Add,
            lhs: Box::new(Expr::Ident("x".to_string(), span.clone())),
            rhs: Box::new(Expr::Ident("threshold".to_string(), span.clone())),
            span: span.clone(),
        };
        let helper_params = vec![Param {
            name: "threshold".to_string(),
            ty: TypeExpr::Named("Int".to_string(), span.clone()),
            span: span.clone(),
        }];
        let captures = collect_synth_cont_captures(&tail, "x", &helper_params);
        assert_eq!(captures.len(), 1);
        assert_eq!(captures[0].name, "threshold");
        assert_eq!(captures[0].kind, EnvSlotKind::Int);
    }

    #[test]
    fn collect_synth_cont_captures_excludes_let_binding_name() {
        // Free-var analysis on `x + 100` — the let-binding `x` is
        // shadowed; `100` is a literal. No captures.
        use crate::ast::{BinOp, Expr, Param, TypeExpr};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let tail = Expr::Binary {
            op: BinOp::Add,
            lhs: Box::new(Expr::Ident("x".to_string(), span.clone())),
            rhs: Box::new(Expr::IntLit(100, span.clone())),
            span: span.clone(),
        };
        // Even with a `threshold` param, if the tail doesn't use it,
        // captures stays empty.
        let helper_params = vec![Param {
            name: "threshold".to_string(),
            ty: TypeExpr::Named("Int".to_string(), span.clone()),
            span: span.clone(),
        }];
        let captures = collect_synth_cont_captures(&tail, "x", &helper_params);
        assert_eq!(captures.len(), 0);
    }

    #[test]
    fn collect_synth_cont_captures_match_pattern_var_shadows_param() {
        // PR #26 mid-flight at a5ee4c6 item #1: when a match arm's
        // pattern binds a name (`Pattern::Var`) that shadows a
        // helper param, the walker must NOT capture the param —
        // the arm body's reference resolves to the pattern
        // binding, not the outer scope.
        //
        // Source-equivalent body:
        //   `match x { threshold => threshold + 1 }`
        // Helper has param `threshold: Int`; the pattern binds
        // a fresh `threshold` to the scrutinee value, so the
        // `threshold` in the arm body refers to the binding,
        // not helper's param. Walker should return empty
        // captures.
        //
        // Pre-fix, the walker recursed into arm bodies without
        // adding pattern-bound names to `bound`, so `threshold`
        // (helper param) would be captured. At runtime the
        // synth-cont would load helper's `threshold` from the
        // closure record and use it as `threshold + 1`, ignoring
        // the scrutinee — wrong result.
        use crate::ast::{BinOp, Expr, MatchArm, Param, Pattern, TypeExpr};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let tail = Expr::Match {
            scrutinee: Box::new(Expr::Ident("x".to_string(), span.clone())),
            arms: vec![MatchArm {
                pattern: Pattern::Var("threshold".to_string(), span.clone()),
                body: Expr::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(Expr::Ident("threshold".to_string(), span.clone())),
                    rhs: Box::new(Expr::IntLit(1, span.clone())),
                    span: span.clone(),
                },
                span: span.clone(),
            }],
            span: span.clone(),
        };
        let helper_params = vec![Param {
            name: "threshold".to_string(),
            ty: TypeExpr::Named("Int".to_string(), span.clone()),
            span: span.clone(),
        }];
        let captures = collect_synth_cont_captures(&tail, "x", &helper_params);
        assert_eq!(
            captures.len(),
            0,
            "pattern-bound `threshold` shadows helper param; walker must \
             not capture. Pre-fix this returned [threshold] (incorrect)."
        );
    }

    #[test]
    fn collect_synth_cont_captures_match_with_tuple_pattern_var_shadows_param() {
        // PR #26 mid-flight at 2be70ce review item #1:
        // Pattern::Tuple binding shape was unpinned — the
        // `Tuple(patterns)` arm of `collect_pattern_bindings`
        // recurses into each sub-pattern; without a test, a
        // future refactor could break tuple recursion silently.
        //
        // Source-equivalent body: `match (x, x) { (threshold, _)
        // => threshold + 1 }`. The pattern's first component
        // binds `threshold`, shadowing helper's user-param.
        // Walker must not capture `threshold`.
        use crate::ast::{BinOp, Expr, MatchArm, Param, Pattern, TypeExpr};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let tail = Expr::Match {
            scrutinee: Box::new(Expr::Ident("x".to_string(), span.clone())),
            arms: vec![MatchArm {
                pattern: Pattern::Tuple(
                    vec![
                        Pattern::Var("threshold".to_string(), span.clone()),
                        Pattern::Wildcard(span.clone()),
                    ],
                    span.clone(),
                ),
                body: Expr::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(Expr::Ident("threshold".to_string(), span.clone())),
                    rhs: Box::new(Expr::IntLit(1, span.clone())),
                    span: span.clone(),
                },
                span: span.clone(),
            }],
            span: span.clone(),
        };
        let helper_params = vec![Param {
            name: "threshold".to_string(),
            ty: TypeExpr::Named("Int".to_string(), span.clone()),
            span: span.clone(),
        }];
        let captures = collect_synth_cont_captures(&tail, "x", &helper_params);
        assert_eq!(
            captures.len(),
            0,
            "Tuple pattern's first-component binding `threshold` shadows \
             helper param; walker must not capture"
        );
    }

    #[test]
    fn collect_synth_cont_captures_match_with_ctor_positional_pattern_var_shadows_param() {
        // PR #26 mid-flight at 2be70ce review item #1:
        // Pattern::Ctor::Positional binding shape was unpinned.
        // Source-equivalent: `match opt { Some(threshold) =>
        // threshold + 1, None => 0 }`. The Some's positional
        // binding shadows helper's user-param.
        use crate::ast::{BinOp, CtorPatternFields, Expr, MatchArm, Param, Pattern, TypeExpr};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let tail = Expr::Match {
            scrutinee: Box::new(Expr::Ident("x".to_string(), span.clone())),
            arms: vec![
                MatchArm {
                    pattern: Pattern::Ctor {
                        name: "Some".to_string(),
                        fields: CtorPatternFields::Positional(vec![Pattern::Var(
                            "threshold".to_string(),
                            span.clone(),
                        )]),
                        span: span.clone(),
                    },
                    body: Expr::Binary {
                        op: BinOp::Add,
                        lhs: Box::new(Expr::Ident("threshold".to_string(), span.clone())),
                        rhs: Box::new(Expr::IntLit(1, span.clone())),
                        span: span.clone(),
                    },
                    span: span.clone(),
                },
                MatchArm {
                    pattern: Pattern::Ctor {
                        name: "None".to_string(),
                        fields: CtorPatternFields::Unit,
                        span: span.clone(),
                    },
                    body: Expr::IntLit(0, span.clone()),
                    span: span.clone(),
                },
            ],
            span: span.clone(),
        };
        let helper_params = vec![Param {
            name: "threshold".to_string(),
            ty: TypeExpr::Named("Int".to_string(), span.clone()),
            span: span.clone(),
        }];
        let captures = collect_synth_cont_captures(&tail, "x", &helper_params);
        assert_eq!(
            captures.len(),
            0,
            "Ctor::Positional pattern's binding `threshold` shadows helper \
             param in Some arm; None arm has no binding. Walker must not \
             capture in either arm."
        );
    }

    #[test]
    fn collect_synth_cont_captures_match_with_ctor_pattern_handles_record_field_bindings() {
        // The Pattern::Ctor with Record fields shape introduces
        // bindings via field-pun (`Point { x, y }`) or rename
        // (`Point { x: px }`). `collect_pattern_bindings` recurses
        // into each field's pattern. Pin acceptance of a record-
        // pattern shadowing a helper param.
        //
        // Source-equivalent: `match scrut { Point { threshold } =>
        // threshold + 1 }`. The field-pun binds `threshold` from
        // the Point's `threshold` field. Helper's `threshold`
        // param is shadowed inside the arm body.
        use crate::ast::{
            BinOp, CtorPatternField, CtorPatternFields, Expr, MatchArm, Param, Pattern, TypeExpr,
        };
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let tail = Expr::Match {
            scrutinee: Box::new(Expr::Ident("x".to_string(), span.clone())),
            arms: vec![MatchArm {
                pattern: Pattern::Ctor {
                    name: "Point".to_string(),
                    fields: CtorPatternFields::Record(vec![CtorPatternField {
                        name: "threshold".to_string(),
                        pattern: Pattern::Var("threshold".to_string(), span.clone()),
                        span: span.clone(),
                    }]),
                    span: span.clone(),
                },
                body: Expr::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(Expr::Ident("threshold".to_string(), span.clone())),
                    rhs: Box::new(Expr::IntLit(1, span.clone())),
                    span: span.clone(),
                },
                span: span.clone(),
            }],
            span: span.clone(),
        };
        let helper_params = vec![Param {
            name: "threshold".to_string(),
            ty: TypeExpr::Named("Int".to_string(), span.clone()),
            span: span.clone(),
        }];
        let captures = collect_synth_cont_captures(&tail, "x", &helper_params);
        assert_eq!(
            captures.len(),
            0,
            "Ctor::Record pattern field-binding `threshold` shadows helper \
             param; walker must not capture"
        );
    }

    #[test]
    fn collect_synth_cont_captures_does_not_recurse_into_call_in_tail() {
        // Renamed from the prior misleading
        // `..._skips_globals` (PR #26 mid-flight at a5ee4c6 item
        // #7). The walker treats `Expr::Call` as a yield-able
        // shape and defensively skips recursion into it; the
        // classifier already rejects bodies with calls in tail,
        // so the walker never has to make a global-vs-param
        // resolution decision. This test pins the defensive skip,
        // not a global-resolution rule.
        use crate::ast::{Expr, Param, TypeExpr};
        use crate::errors::Span;
        let span = Span::synthetic("x.sigil");
        let tail = Expr::Call {
            callee: Box::new(Expr::Ident("not_a_param".to_string(), span.clone())),
            args: vec![Expr::Ident("threshold".to_string(), span.clone())],
            span: span.clone(),
        };
        let helper_params = vec![Param {
            name: "threshold".to_string(),
            ty: TypeExpr::Named("Int".to_string(), span.clone()),
            span: span.clone(),
        }];
        let captures = collect_synth_cont_captures(&tail, "x", &helper_params);
        // The analysis defensively returns empty for Call (yield-able
        // shape); even if it descended, `not_a_param` isn't in
        // helper_params and would be skipped.
        // We get 0 captures because the walker treats Expr::Call as
        // a yield-able shape and doesn't recurse (defensive — the
        // classifier already rejects Call in tail).
        assert_eq!(captures.len(), 0);
    }
}
