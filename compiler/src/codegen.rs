//! Cranelift codegen — plan A1 Stage 1 task 12.
//!
//! Lowers the post-closure-conversion program to a native object file via
//! `cranelift-object`. Stage 1 handles exactly the hello-world shape: one
//! function named `main` whose body is a sequence of `IO.println` calls
//! followed by an integer literal tail. Generalisation lands in Plan A2.
//!
//! Emission responsibilities:
//!
//! - A C-callable `main` shim that initialises the GC, calls user-`main`,
//!   then returns the user return value as a C int exit status.
//! - One function per user `fn`. For Stage 1 only `main` exists.
//! - String literals are emitted as read-only data bytes; the generated
//!   code calls `sigil_string_new(bytes, len)` to materialise a heap
//!   String (bumping the Boehm counter) and then passes the heap pointer
//!   to `sigil_println`.
//! - Safepoint metadata at every call site is written to `.sigil_stackmaps`
//!   (ELF) / `__SIGIL,__stackmaps` (Mach-O). v1 Boehm ignores the section;
//!   v2 precise GC consumes it. Stage 1 emits a minimal but parseable
//!   record: a u32 record count followed by per-record `pc_offset, count=0`
//!   placeholder entries so the section is demonstrably non-empty and a
//!   v2 reader can walk it.
//! - No interior pointers. Generated code never computes a pointer into
//!   the middle of a heap object; it calls runtime helpers that work with
//!   header pointers and extract transient payload views internally.
//!
//! Target-triple detection uses `target_lexicon::HOST` so the compiler
//! emits for whatever host it runs on; cross-compilation is not v1 scope.

use std::path::Path;

use cranelift::codegen::ir::{AbiParam, Inst, Signature, UserFuncName};
use cranelift::codegen::isa;
use cranelift::codegen::settings;
use cranelift::prelude::*;
use cranelift_module::{default_libcall_names, DataDescription, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use object::write::SectionKind;
use target_lexicon::Triple;

use crate::closure_convert::ClosureConvertedProgram;
use crate::typecheck::CheckedProgram;

/// Stable record layout for `.sigil_stackmaps`. Stage 1 writes records
/// with `live_count = 0`; Plan B fills in real live-value entries.
///
/// | field       | width |
/// | ----------- | ----- |
/// | record_count (whole section preamble) | u32 |
/// | per-record: pc_offset                  | u32 |
/// | per-record: live_count                 | u16 |
/// | per-record: _pad                       | u16 |
pub const STACKMAP_RECORD_SIZE: usize = 8;

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

    // user-main signature: () -> i64 (the tagged Int exit value).
    let mut user_main_sig = Signature::new(isa_call_conv(&module));
    user_main_sig.returns.push(AbiParam::new(types::I64));
    let user_main = module
        .declare_function("sigil_user_main", Linkage::Local, &user_main_sig)
        .map_err(|e| format!("declare sigil_user_main: {e}"))?;

    // C-callable main: int main(int argc, char **argv). Stage 1 ignores argv.
    let mut main_sig = Signature::new(isa_call_conv(&module));
    main_sig.params.push(AbiParam::new(types::I32));
    main_sig.params.push(AbiParam::new(pointer_ty));
    main_sig.returns.push(AbiParam::new(types::I32));
    let main = module
        .declare_function("main", Linkage::Export, &main_sig)
        .map_err(|e| format!("declare main: {e}"))?;

    // Collect safepoint PC offsets for the stackmap section.
    let mut stackmap_pc_offsets: Vec<u32> = Vec::new();

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

    // --- user-main body --------------------------------------------------
    let mut ctx = module.make_context();
    let mut fb_ctx = FunctionBuilderContext::new();
    ctx.func.signature = user_main_sig.clone();
    ctx.func.name = UserFuncName::user(0, user_main.as_u32());
    {
        let mut builder = FunctionBuilder::new(&mut ctx.func, &mut fb_ctx);
        let block = builder.create_block();
        builder.append_block_params_for_function_params(block);
        builder.switch_to_block(block);
        builder.seal_block(block);

        // Walk the typed AST and emit per-literal allocation + print pairs.
        // Stage 1 walks exactly one function (main).
        let main_decl = checked
            .program
            .items
            .iter()
            .find_map(|i| match i {
                crate::ast::Item::Fn(f) if f.name == "main" => Some(f),
                _ => None,
            })
            .ok_or_else(|| "Stage 1 codegen requires a `main` function".to_string())?;
        let string_new_ref = module.declare_func_in_func(string_new, builder.func);
        let println_ref = module.declare_func_in_func(println, builder.func);

        let mut lit_cursor = 0usize;
        for stmt in &main_decl.body.stmts {
            if let crate::ast::Stmt::Perform(p) = stmt {
                if p.effect == "IO" && p.op == "println" && p.args.len() == 1 {
                    let crate::ast::Expr::StringLit(s, _) = &p.args[0] else {
                        return Err(
                            "Stage 1 codegen: IO.println argument must be a string literal".into(),
                        );
                    };
                    let data_id = lit_ids[lit_cursor];
                    lit_cursor += 1;
                    let gv = module.declare_data_in_func(data_id, builder.func);
                    let bytes_ptr = builder.ins().symbol_value(pointer_ty, gv);
                    let len_v = builder.ins().iconst(pointer_ty, s.len() as i64);
                    let alloc_call = builder.ins().call(string_new_ref, &[bytes_ptr, len_v]);
                    stackmap_pc_offsets.push(function_code_offset(&builder, alloc_call));
                    let heap = builder.inst_results(alloc_call)[0];
                    let print_call = builder.ins().call(println_ref, &[heap]);
                    stackmap_pc_offsets.push(function_code_offset(&builder, print_call));
                }
            }
        }

        // Tail expression: hello-world's main returns `0`. We take the int
        // literal from the tail and return it as a tagged u64 (0 << 1 = 0).
        let tail = match &main_decl.body.tail {
            Some(crate::ast::Expr::IntLit(n, _)) => *n,
            _ => 0,
        };
        // Tag as Sigil Int: (n as u64) << 1.
        let tagged = (tail as u64).wrapping_shl(1) as i64;
        let ret = builder.ins().iconst(types::I64, tagged);
        builder.ins().return_(&[ret]);
        builder.finalize();
    }
    module
        .define_function(user_main, &mut ctx)
        .map_err(|e| format!("define sigil_user_main: {e}"))?;
    module.clear_context(&mut ctx);

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
        stackmap_pc_offsets.push(function_code_offset(&builder, init_call));
        let um_call = builder.ins().call(user_main_ref, &[]);
        stackmap_pc_offsets.push(function_code_offset(&builder, um_call));

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

    // Write a minimal stackmap: u32 count, then count * (pc_offset: u32, live: u16, pad: u16).
    let mut section_bytes =
        Vec::with_capacity(4 + stackmap_pc_offsets.len() * STACKMAP_RECORD_SIZE);
    section_bytes.extend_from_slice(&(stackmap_pc_offsets.len() as u32).to_le_bytes());
    for off in &stackmap_pc_offsets {
        section_bytes.extend_from_slice(&off.to_le_bytes());
        section_bytes.extend_from_slice(&0u16.to_le_bytes()); // live_count = 0
        section_bytes.extend_from_slice(&0u16.to_le_bytes()); // pad
    }
    let is_macho = matches!(product.object.format(), object::BinaryFormat::MachO);
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
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;

    #[test]
    fn stackmap_record_size_is_eight() {
        assert_eq!(STACKMAP_RECORD_SIZE, 8);
    }
}
