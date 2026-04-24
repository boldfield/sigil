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

use sigil_header_constants::{header_word, TAG_CLOSURE};

use crate::ast::{EnvSlotKind, TypeExpr};
use crate::closure_convert::ClosureConvertedProgram;
use crate::errors::Span;
use crate::typecheck::CheckedProgram;

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
fn cranelift_ty_for_type_expr(te: &TypeExpr, pointer_ty: Type) -> Type {
    match te {
        TypeExpr::Named(name, _) => match name.as_str() {
            "Int" => types::I64,
            "String" => pointer_ty,
            "Bool" | "Byte" | "Unit" => types::I8,
            "Char" => types::I32,
            other => unreachable!("codegen: unknown named type `{other}`"),
        },
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

/// Stackmap section layout. Plan A1 emits **version 0 (placeholder)**:
///
/// ```text
/// header  = magic:4 "SGST" | version:4 | record_count:4           // 12 bytes
/// record  = pc_offset:4    | live_count:2 (always 0 in v0) | flags:2   // 8 bytes
/// ```
///
/// `flags` has bit 0 (`STACKMAP_FLAG_PLACEHOLDER`) set in v0 so a v2
/// reader that only understands version 1 can detect stale placeholder
/// records on a per-record basis as well as via the version field.
///
/// Version 1 (Plan B) will reuse the same header; the record format
/// gains a live-value list per record and `pc_offset` becomes a real
/// post-regalloc code offset via Cranelift's safepoint API.
pub const STACKMAP_MAGIC: &[u8; 4] = b"SGST";
pub const STACKMAP_VERSION_PLACEHOLDER: u32 = 0;
pub const STACKMAP_HEADER_SIZE: usize = 12;
pub const STACKMAP_RECORD_SIZE: usize = 8;
pub const STACKMAP_FLAG_PLACEHOLDER: u16 = 0x0001;

/// Accumulator for safepoint records emitted during function lowering.
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
    let string_literals = &checked.string_literals;

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
            };

            let tail_val = lowerer.lower_block(&f.body);

            // main tags its Int return with `ishl_imm 1` so the C-main
            // shim can `sshr_imm 1` → i32. Other user fns return their
            // raw Cranelift value; callers (user code) use it directly.
            let ret_val = match tail_val {
                Some(v) if is_main => lowerer.builder.ins().ishl_imm(v, 1),
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
        // v1 (main returns Int, and hello-world returns 0).
        let tagged = builder.inst_results(um_call)[0];
        let untagged = builder.ins().sshr_imm(tagged, 1);
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
            Expr::Ident(name, _) => *self
                .env
                .get(name)
                .unwrap_or_else(|| unreachable!("codegen: unknown ident `{name}`")),
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
                scrutinee, arms, ..
            } => self.lower_match(scrutinee, arms),
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
            // Plan A3 task 41 replaces this stub with record-literal
            // allocation (heap, user type-tag, fields written per the
            // registered layout). For task 37 (parser), this arm is an
            // `unreachable!` because no well-typed Plan A3 program can
            // reach codegen with a `RecordLit` until task 38's nominal-
            // types symbol table lands — the typechecker will reject
            // any program using the new surface syntax until then.
            Expr::RecordLit { name, .. } => {
                unreachable!(
                    "codegen: Expr::RecordLit `{name}` requires Plan A3 task 41's record-allocation lowering; unreachable pre-task-41"
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
                EnvSlotKind::String | EnvSlotKind::Closure => *raw,
            };
            let offset: i32 = 16 + 8 * i as i32;
            self.builder
                .ins()
                .store(MemFlags::trusted(), slot_val, closure_ptr, offset);
        }

        closure_ptr
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
            EnvSlotKind::String | EnvSlotKind::Closure => {
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

    /// Lower `match scrutinee { pat => body, ... }` to a linear chain
    /// of compare-and-branch blocks, terminated by a continue block
    /// whose single parameter carries the match's result value.
    ///
    /// Exhaustiveness is enforced at typecheck (E0066); the tail-of-
    /// the-chain block emits an `unreachable` trap as a safety net.
    fn lower_match(
        &mut self,
        scrutinee: &crate::ast::Expr,
        arms: &[crate::ast::MatchArm],
    ) -> Value {
        let s = self.lower_expr(scrutinee);
        let s_ty = self.builder.func.dfg.value_type(s);

        // Predict the result type from the first arm's body by peeking
        // at its shape. We need the type *before* we emit the arm
        // bodies because the continue block's single param is created
        // up front.
        let result_ty = self.type_of_expr(&arms[0].body);
        let cont = self.builder.create_block();
        self.builder.append_block_param(cont, result_ty);

        // Tracks whether the chain's final fall-through block still
        // needs a terminator. A wildcard arm jumps to `cont` and sets
        // this to true; if the loop exits without that, we emit an
        // unreachable trap below.
        let mut chain_terminated = false;
        for arm in arms.iter() {
            match pattern_as_immediate(&arm.pattern) {
                None => {
                    // Wildcard arm: unconditional jump to the continue
                    // block. Typecheck accepts trailing arms after a
                    // wildcard but they're dead; break out of the
                    // chain unconditionally.
                    let v = self.lower_expr(&arm.body);
                    self.builder.ins().jump(cont, &[BlockArg::Value(v)]);
                    chain_terminated = true;
                    break;
                }
                Some(imm) => {
                    // Literal arm: compare scrutinee to the pattern's
                    // immediate value; on equality enter the body and
                    // jump to `cont`, otherwise fall through to the
                    // next arm.
                    let lit = self.builder.ins().iconst(s_ty, imm);
                    let eq = self.builder.ins().icmp(IntCC::Equal, s, lit);
                    let body = self.builder.create_block();
                    let next = self.builder.create_block();
                    self.builder.ins().brif(eq, body, &[], next, &[]);
                    self.builder.switch_to_block(body);
                    self.builder.seal_block(body);
                    let v = self.lower_expr(&arm.body);
                    self.builder.ins().jump(cont, &[BlockArg::Value(v)]);
                    self.builder.switch_to_block(next);
                    self.builder.seal_block(next);
                }
            }
        }

        // If the chain exited without a wildcard — which only happens
        // for a `Bool`-scrutinee match that enumerates both polarities,
        // or an `Int`/`Char`/`Byte` match whose wildcard was not reached
        // due to a codegen bug — emit an unreachable trap in the fall-
        // through block. Typecheck's exhaustiveness rule (E0066)
        // guarantees every well-typed program either hits a wildcard
        // or covers both Bool polarities; this trap is a defensive
        // contract, not a real exit path.
        if !chain_terminated {
            self.builder
                .ins()
                .trap(TrapCode::unwrap_user(TRAP_NONEXHAUSTIVE_MATCH));
        }

        self.builder.switch_to_block(cont);
        self.builder.seal_block(cont);
        self.builder.block_params(cont)[0]
    }

    /// Structural Cranelift-type predictor. Used by `lower_match` to
    /// size the continue-block parameter before any arm body is
    /// emitted. Agrees with `lower_expr`'s emitted types by
    /// construction.
    fn type_of_expr(&self, e: &crate::ast::Expr) -> Type {
        use crate::ast::{BinOp, Expr, UnOp};
        match e {
            Expr::IntLit(..) => types::I64,
            Expr::BoolLit(..) | Expr::Perform(_) => types::I8,
            Expr::CharLit(..) => types::I32,
            Expr::StringLit(..) | Expr::RecordLit { .. } => self.pointer_ty,
            Expr::Ident(name, _) => {
                let v = *self
                    .env
                    .get(name)
                    .unwrap_or_else(|| unreachable!("type_of_expr: unknown ident `{name}`"));
                self.builder.func.dfg.value_type(v)
            }
            Expr::Binary { op, .. } => match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => types::I64,
                _ => types::I8, // comparison / logic → Bool
            },
            Expr::Unary { op, .. } => match op {
                UnOp::Neg => types::I64,
                UnOp::Not => types::I8,
            },
            Expr::Match { arms, .. } => self.type_of_expr(&arms[0].body),
            Expr::If { then_block, .. } => match &then_block.tail {
                Some(t) => self.type_of_expr(t),
                None => types::I8,
            },
            Expr::Block(b) => match &b.tail {
                Some(t) => self.type_of_expr(t),
                None => types::I8,
            },
            Expr::Call { callee, .. } => match callee.as_ref() {
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
                crate::ast::EnvSlotKind::String | crate::ast::EnvSlotKind::Closure => {
                    self.pointer_ty
                }
            },
        }
    }
}

/// Cranelift trap codes Plan A2 uses. Values are in the user-trap
/// range (`TrapCode::user`). Plan B will rename these when the effect
/// runtime has a richer trap catalogue.
const TRAP_ARITH_ABORT: u8 = 0x40;
const TRAP_NONEXHAUSTIVE_MATCH: u8 = 0x41;

/// Reduce a pattern to the `i64` immediate that codegen needs to
/// compare the scrutinee against. Returns `None` for `Wildcard` — the
/// lowerer treats that as an unconditional branch target. This helper
/// unifies the compare-and-branch logic for `IntLit` / `BoolLit` /
/// `CharLit` patterns, which otherwise differ only in the immediate's
/// source.
///
/// Plan A3 will introduce constructor patterns (sum types); when that
/// lands the return type must change — `Option<i64>` no longer spans
/// the full pattern space. The lowerer's callers will need a richer
/// classification (tag-then-compare for sum-type tags, structural
/// match for records). Until then, this helper is a faithful Stage-2
/// surface.
fn pattern_as_immediate(p: &crate::ast::Pattern) -> Option<i64> {
    use crate::ast::Pattern;
    match p {
        Pattern::IntLit(n, _) => Some(*n),
        Pattern::BoolLit(b, _) => Some(i64::from(*b)),
        Pattern::CharLit(c, _) => Some(*c as i64),
        Pattern::Wildcard(_) => None,
        // Plan A3 task 37: Var / Tuple / Ctor patterns land in task
        // 41's codegen rewrite, which replaces this primitive
        // "pattern as scalar" predicate with a full decision-tree
        // lowerer. Until then, no program that reaches codegen
        // carries these patterns (task 38 will either typecheck-
        // reject or, once 41 lands, lower them via a dedicated
        // path that does not consult `pattern_as_immediate`).
        Pattern::Var(..) | Pattern::Tuple(..) | Pattern::Ctor { .. } => {
            unreachable!(
                "codegen: Pattern::{{Var,Tuple,Ctor}} reaches pattern_as_immediate pre-task-41"
            )
        }
    }
}

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
        // Constants pin the shipped format. Bumping STACKMAP_VERSION_PLACEHOLDER
        // from 0 should be paired with a corresponding change in
        // runtime/src/stackmap.rs so both crates agree on what v1 looks like.
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
}
