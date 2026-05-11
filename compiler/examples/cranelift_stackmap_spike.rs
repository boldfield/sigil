// Plan E2 Phase 1 Task 1 — Cranelift 0.131.0 stack-map API spike.
//
// `unwrap` / `expect` are clippy-disallowed in `sigil-compiler` because
// production code paths route every error through `CompilerError`. This
// is a one-shot API spike — there is no error-recovery contract to honor
// and the script's failure mode is "panic with the underlying message,"
// which is exactly what `unwrap` does.
#![allow(clippy::disallowed_methods)]

//
// Demonstrates the minimum end-to-end use of the user-stack-map API:
//   1. `FunctionBuilder::declare_value_needs_stack_map(v)` flags a value
//      as a GC reference. Cranelift then spills it to a sized stack slot
//      around every safepoint and records the slot's offset in the
//      `UserStackMap` for that safepoint.
//   2. Every non-tail `call` (and `call_indirect`) is automatically a
//      safepoint in 0.131 — there is no per-instruction safepoint flag.
//   3. After `Context::compile`, `code.buffer.user_stack_maps()` returns
//      a slice of `(CodeOffset, frame_size, UserStackMap)` triples. The
//      `UserStackMap` exposes `(ir::Type, sp_offset)` entries.
//
// Run: `cargo run --example cranelift_stackmap_spike -p sigil-compiler`.
// Expected: one stack-map record covering the call to `tickle`, with a
// single i64 entry at an SP-relative offset matching the spill slot.

use cranelift::codegen::control::ControlPlane;
use cranelift::codegen::ir::{
    AbiParam, ExtFuncData, ExternalName, Function, Signature, UserExternalName, UserFuncName,
};
use cranelift::codegen::isa::{self, CallConv};
use cranelift::codegen::settings::{self, Flags};
use cranelift::codegen::Context;
use cranelift::prelude::*;
use target_lexicon::Triple;

fn main() {
    let triple = Triple::host();
    let isa = isa::lookup(triple.clone())
        .unwrap()
        .finish(Flags::new(settings::builder()))
        .unwrap();

    let cc = CallConv::triple_default(&triple);
    let mut tickle_sig = Signature::new(cc);
    tickle_sig.params.push(AbiParam::new(types::I64));
    let mut entry_sig = Signature::new(cc);
    entry_sig.returns.push(AbiParam::new(types::I64));

    let mut ctx = Context::new();
    ctx.func = Function::with_name_signature(UserFuncName::user(0, 0), entry_sig);

    let mut fbc = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbc);

    let sig = b.func.import_signature(tickle_sig);
    let name_ref = b.func.declare_imported_user_function(UserExternalName {
        namespace: 0,
        index: 1,
    });
    let callee = b.func.import_function(ExtFuncData {
        name: ExternalName::user(name_ref),
        signature: sig,
        colocated: false,
        patchable: false,
    });

    let blk = b.create_block();
    b.switch_to_block(blk);
    b.seal_block(blk);

    let v = b.ins().iconst(types::I64, 42);
    b.declare_value_needs_stack_map(v); // mark v as a GC reference
    b.ins().call(callee, &[v]); // implicit safepoint at every non-tail call
    b.ins().return_(&[v]);
    b.finalize();

    let code = ctx
        .compile(&*isa, &mut ControlPlane::default())
        .expect("compile");
    let maps = code.buffer.user_stack_maps();
    println!("user stack maps: {}", maps.len());
    for (pc_off, frame, sm) in maps {
        println!("  PC offset {pc_off:#x} | frame {frame} bytes");
        for (ty, sp_off) in sm.entries() {
            println!("    entry: ty={ty}, sp+{sp_off:#x}");
        }
    }
    assert!(!maps.is_empty(), "expected at least one stack map entry");
}
