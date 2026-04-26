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
                self.lower_perform(p);
            }
        }
    }

    fn lower_perform(&mut self, p: &crate::ast::PerformExpr) {
        // Plan A2 only recognises `IO.println(String)`; typecheck
        // (E0042/E0043/E0044) rejects every other shape before codegen
        // sees the program, so we assume the happy path.
        assert_eq!(p.effect, "IO", "non-IO effect reached codegen");
        assert_eq!(p.op, "println", "non-println IO op reached codegen");
        assert_eq!(p.args.len(), 1, "IO.println arg count is not 1");

        let heap = self.lower_expr(&p.args[0]);
        let call = self.builder.ins().call(self.println_ref, &[heap]);
        self.stackmap
            .push_placeholder(function_code_offset(&self.builder, call));
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
                self.lower_perform(p);
                // Perform returns Unit — represented as `i8 0`.
                self.builder.ins().iconst(types::I8, 0)
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
            Expr::Handle { .. } => {
                // Typecheck (Plan B Task 53) emits E0134 for any
                // `handle` expression; the codegen-entry walker
                // additionally hard-rejects programs that contain one.
                // Reaching this arm means an upstream invariant broke.
                // Plan B Task 55 replaces this `unreachable!` with the
                // CPS transform's expansion to `sigil_perform` /
                // `sigil_handle_push` calls.
                unreachable!(
                    "codegen: Expr::Handle should be rejected by typecheck E0134 + entry walker before codegen"
                )
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
            Expr::BoolLit(..) | Expr::Perform(_) => types::I8,
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
            // Plan B task 53 — handler expressions are rejected by
            // typecheck E0134 + the codegen-entry walker before this
            // type predictor runs. Reaching this arm means an upstream
            // invariant broke; trip `unreachable!` rather than guess
            // a Cranelift type for a node Task 55's CPS transform
            // hasn't yet expanded.
            Expr::Handle { .. } => unreachable!(
                "type_of_expr: Expr::Handle is rejected by typecheck E0134 + codegen entry walker"
            ),
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
