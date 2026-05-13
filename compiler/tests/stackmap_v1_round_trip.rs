// Plan E2 Phase 1 Task 4 — IR-level round-trip test for the v1
// stackmap section writer + parser.
//
// Per Plan E2 Task 4's plan-body unit-test specification:
//
//     Unit: hand-build a known IR with one alloc + one fn call;
//     emit; parse the section back; assert structure.
//
// The G1 e2e test (`compiler/tests/e2e.rs::stackmap_section_parses_v1_
// with_real_safepoints`) covers the full Sigil-source → emit_object →
// parse-back path. This integration test covers the **same wire format
// contract** at the IR level: one Cranelift function, hand-built via
// FunctionBuilder + ObjectModule::define_function, with one allocator
// call whose result is `declare_value_needs_stack_map`'d and live
// across one consumer call (the canonical safepoint-spanning shape).
//
// The two tests share assertions but differ in how they reach the
// section bytes:
//   - G1 e2e: codegen::emit_object → object file → magic-anchored
//     section locator → parse_section.
//   - This test: ObjectModule::define_function →
//     ctx.compiled_code().buffer.user_stack_maps() →
//     StackMapV1Builder::push_function → ::serialize → parse_section.
//
// Both end with the same `parse_section` invariant assertions so a
// regression at either layer (the in-memory builder OR the
// object-file integration) surfaces independently.

use cranelift::codegen::ir::{AbiParam, Function, Signature, UserFuncName};
use cranelift::codegen::isa::{self, CallConv};
use cranelift::codegen::settings::{self, Flags};
use cranelift::codegen::Context;
use cranelift::prelude::*;
use cranelift_module::{Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use target_lexicon::Triple;

use sigil_compiler::codegen::StackMapV1Builder;
use sigil_runtime::stackmap::{
    parse_section, STACKMAP_ENTRY_KIND_HEAP_POINTER, STACKMAP_VERSION_V1,
};

type IntegrationResult = Result<(), String>;

fn make_isa() -> Result<std::sync::Arc<dyn isa::TargetIsa>, String> {
    let triple = Triple::host();
    let isa = isa::lookup(triple)
        .map_err(|e| format!("isa::lookup: {e}"))?
        .finish(Flags::new(settings::builder()))
        .map_err(|e| format!("isa finish: {e}"))?;
    Ok(isa)
}

#[test]
fn hand_built_alloc_and_call_round_trips_through_v1_section() -> IntegrationResult {
    let isa = make_isa()?;
    let cc = CallConv::triple_default(&Triple::host());

    let obj_builder = ObjectBuilder::new(
        isa.clone(),
        b"test_stackmap_v1_round_trip".to_vec(),
        cranelift_module::default_libcall_names(),
    )
    .map_err(|e| format!("ObjectBuilder::new: {e}"))?;
    let mut module = ObjectModule::new(obj_builder);

    let pointer_ty = isa.pointer_type();

    // External "alloc" — `() -> *mut u8` (heap pointer).
    let mut alloc_sig = Signature::new(cc);
    alloc_sig.returns.push(AbiParam::new(pointer_ty));
    let alloc_id = module
        .declare_function("test_stackmap_alloc", Linkage::Import, &alloc_sig)
        .map_err(|e| format!("declare alloc: {e:?}"))?;

    // External "consume" — `(*mut u8, i64) -> ()`. Two args so the
    // safepoint-spanning value is a real arg position, not just a
    // call-by-coincidence.
    let mut consume_sig = Signature::new(cc);
    consume_sig.params.push(AbiParam::new(pointer_ty));
    consume_sig.params.push(AbiParam::new(types::I64));
    let consume_id = module
        .declare_function("test_stackmap_consume", Linkage::Import, &consume_sig)
        .map_err(|e| format!("declare consume: {e:?}"))?;

    // Entry — `() -> *mut u8`. Returning `heap_ptr` keeps it live
    // past the consume call, matching the spike test's
    // `live-across-safepoint` shape.
    let mut entry_sig = Signature::new(cc);
    entry_sig.returns.push(AbiParam::new(pointer_ty));
    let entry_id = module
        .declare_function("test_stackmap_entry", Linkage::Export, &entry_sig)
        .map_err(|e| format!("declare entry: {e:?}"))?;

    let mut ctx = Context::new();
    ctx.func = Function::with_name_signature(UserFuncName::user(0, 0), entry_sig);

    let mut fbc = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbc);

    let alloc_ref = module.declare_func_in_func(alloc_id, b.func);
    let consume_ref = module.declare_func_in_func(consume_id, b.func);

    let blk = b.create_block();
    b.switch_to_block(blk);
    b.seal_block(blk);

    // Body:
    //   heap_ptr = call test_stackmap_alloc()    ; flagged needs-stack-map
    //   call test_stackmap_consume(heap_ptr, 0)  ; safepoint — heap_ptr live
    //   return heap_ptr                          ; live past safepoint via return
    let alloc_call = b.ins().call(alloc_ref, &[]);
    let heap_ptr = b.inst_results(alloc_call)[0];
    b.declare_value_needs_stack_map(heap_ptr);
    let zero = b.ins().iconst(types::I64, 0);
    b.ins().call(consume_ref, &[heap_ptr, zero]);
    b.ins().return_(&[heap_ptr]);
    b.finalize();

    module
        .define_function(entry_id, &mut ctx)
        .map_err(|e| format!("define_function: {e:?}"))?;

    let mut stackmap = StackMapV1Builder::new();
    let compiled = ctx
        .compiled_code()
        .ok_or_else(|| "ctx.compiled_code() returned None after define_function".to_string())?;
    stackmap.push_function(
        "test_stackmap_entry".to_string(),
        compiled.buffer.user_stack_maps(),
    );

    let bytes = stackmap.serialize();
    let parsed = parse_section(&bytes).map_err(|e| format!("v1 parse: {e:?}"))?;

    assert_eq!(parsed.version, STACKMAP_VERSION_V1);
    assert_eq!(parsed.functions.len(), 1);
    let f = &parsed.functions[0];
    assert_eq!(f.symbol_name, "test_stackmap_entry");
    assert_eq!(f.text_offset, 0, "v1 reserves text_offset = 0");
    assert!(
        !f.records.is_empty(),
        "expected ≥1 safepoint record (the consume call) — got 0; \
         declare_value_needs_stack_map may have stopped propagating",
    );
    let total_entries: usize = f.records.iter().map(|r| r.entries.len()).sum();
    assert!(
        total_entries >= 1,
        "expected ≥1 entry across {} record(s); got 0 — flagged value did \
         not survive the safepoint pass",
        f.records.len(),
    );
    for r in &f.records {
        for e in &r.entries {
            assert_eq!(
                e.kind, STACKMAP_ENTRY_KIND_HEAP_POINTER,
                "v1 invariant: every entry must be heap-pointer kind",
            );
        }
    }
    Ok(())
}
