// Plan E2 Phase 1 Task 1 — Cranelift 0.131.0 stack-map API spike, as an
// integration test so CI exercises both API paths on every push.
//
// Two tests pin the API surface Plan E2 will use:
//
//   1. `value_variant_flag_filters_live_set_at_safepoint` — the
//      Value-variant path. The function has *two* i64 values live across
//      the call safepoint; only one is flagged via
//      `declare_value_needs_stack_map`. Asserts the resulting stack map
//      has exactly one entry. This proves the flag actually filters the
//      live set: a regression where Cranelift starts flagging every
//      live value (or the declare quietly becomes a no-op) would change
//      the entry count and trip the test.
//
//   2. `var_variant_emits_stackmap_for_phi_confluence` — the Variable-variant
//      path. `declare_var_needs_stack_map(var)` flags a frontend Variable;
//      every value defined via `def_var(var, _)` (from each predecessor of
//      a join block, in this test) is propagated through the safepoint
//      pass during `finalize`, then spilled around the safepoint that
//      reads the variable past the join. This is the path Plan E2 Phase 1
//      Task 2 will use for phi confluences (heap-pointer-bearing locals
//      defined in multiple predecessor blocks). We have NOT validated
//      per-Value flagging across phi confluences — the Variable path is
//      the documented frontend-supported route, that's the verified
//      claim.
//
// Both tests dump the post-`finalize()` IR via `Function::display` to
// stderr before the asserts run, so any failed assertion (empty stack
// map list, wrong entry count, wrong entry type) lands with the IR
// already visible — saves a debug session next time the spike fails
// after an upstream Cranelift change.

use cranelift::codegen::control::ControlPlane;
use cranelift::codegen::ir::{
    self, AbiParam, ExtFuncData, ExternalName, Function, Signature, UserExternalName, UserFuncName,
};
use cranelift::codegen::isa::{self, CallConv};
use cranelift::codegen::settings::{self, Flags};
use cranelift::codegen::Context;
use cranelift::prelude::*;
use target_lexicon::Triple;

type SpikeResult = Result<(), String>;

fn make_isa() -> Result<std::sync::Arc<dyn isa::TargetIsa>, String> {
    let triple = Triple::host();
    let isa = isa::lookup(triple)
        .map_err(|e| format!("isa::lookup: {e}"))?
        .finish(Flags::new(settings::builder()))
        .map_err(|e| format!("isa finish: {e}"))?;
    Ok(isa)
}

fn assert_stackmap_nonempty(ir_dump: &str, maps: &[(u32, u32, ir::UserStackMap)]) {
    assert!(
        !maps.is_empty(),
        "expected at least one stack map entry — IR dump on stderr. \
         Likely upstream Cranelift behaviour change in `declare_*_needs_stack_map` \
         or in the automatic safepoint pass. Got 0 maps.",
    );
    let _ = ir_dump; // dump happens unconditionally pre-asserts in each test
}

#[test]
fn value_variant_flag_filters_live_set_at_safepoint() -> SpikeResult {
    let isa = make_isa()?;
    let cc = CallConv::triple_default(&Triple::host());

    // Signatures: `tickle(i64, i64)` external; `entry() -> i64`.
    // Two-arg callee so the test can pass both live values into the
    // safepoint; the call site is the safepoint.
    let mut tickle_sig = Signature::new(cc);
    tickle_sig.params.push(AbiParam::new(types::I64));
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

    // Body:
    //   v0 = iconst.i64 42       ; flagged needs-stack-map
    //   v1 = iconst.i64 7        ; NOT flagged
    //   call tickle(v0, v1)      ; safepoint
    //   sum = iadd v0, v1        ; forces both v0 and v1 live across the call
    //   return sum
    //
    // Both v0 and v1 are live at the safepoint (used by `iadd` after the
    // call). Only v0 is flagged. If `declare_value_needs_stack_map` is
    // the filter, the stack map has exactly one entry. If Cranelift
    // flags every live value at every safepoint, the entry count would
    // be 2 — and that's what this test rejects.
    let v0 = b.ins().iconst(types::I64, 42);
    let v1 = b.ins().iconst(types::I64, 7);
    b.declare_value_needs_stack_map(v0);
    b.ins().call(callee, &[v0, v1]);
    let sum = b.ins().iadd(v0, v1);
    b.ins().return_(&[sum]);
    b.finalize();

    // Snapshot the post-finalize IR before `compile` mutably borrows
    // `ctx`, then dump unconditionally so any failed assertion below
    // has the IR right above it on stderr.
    let ir_dump = ctx.func.display().to_string();
    eprintln!("--- IR dump (value_variant) ---\n{ir_dump}");
    let code = ctx
        .compile(&*isa, &mut ControlPlane::default())
        .map_err(|e| format!("compile: {e:?}"))?;
    let maps = code.buffer.user_stack_maps();
    assert_stackmap_nonempty(&ir_dump, maps);

    assert_eq!(
        maps.len(),
        1,
        "expected exactly one safepoint, got {maps:?}"
    );
    let (_pc, _frame, sm) = &maps[0];
    let entries: Vec<_> = sm.entries().collect();
    assert_eq!(
        entries.len(),
        1,
        "expected the flag to filter the live set to exactly one entry \
         (v0 only). Got {entries:?}, which would mean Cranelift is \
         flagging every live value at the safepoint, not honoring \
         `declare_value_needs_stack_map`.",
    );
    assert_eq!(entries[0].0, types::I64, "expected i64 entry type");
    Ok(())
}

#[test]
fn var_variant_emits_stackmap_for_phi_confluence() -> SpikeResult {
    let isa = make_isa()?;
    let cc = CallConv::triple_default(&Triple::host());

    // Signatures: `tickle(i64)`; `entry(i64) -> i64` — param is the
    // branch selector.
    let mut tickle_sig = Signature::new(cc);
    tickle_sig.params.push(AbiParam::new(types::I64));
    let mut entry_sig = Signature::new(cc);
    entry_sig.params.push(AbiParam::new(types::I64));
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

    // Variable holding a GC-managed pointer-typed value. The variable
    // is defined in two predecessor blocks, observed past the safepoint
    // in a merge block — the canonical phi-confluence shape Phase 1
    // Task 2 needs to handle for heap-pointer-bearing locals.
    let gc_ref: Variable = b.declare_var(types::I64);
    b.declare_var_needs_stack_map(gc_ref);

    let entry_blk = b.create_block();
    let then_blk = b.create_block();
    let else_blk = b.create_block();
    let merge_blk = b.create_block();
    b.append_block_params_for_function_params(entry_blk);

    // entry: brif(selector, then, else)
    b.switch_to_block(entry_blk);
    b.seal_block(entry_blk);
    let selector = b.block_params(entry_blk)[0];
    b.ins().brif(selector, then_blk, &[], else_blk, &[]);

    // then: def_var(gc_ref, 111); jump merge
    b.switch_to_block(then_blk);
    b.seal_block(then_blk);
    let v_then = b.ins().iconst(types::I64, 111);
    b.def_var(gc_ref, v_then);
    b.ins().jump(merge_blk, &[]);

    // else: def_var(gc_ref, 222); jump merge
    b.switch_to_block(else_blk);
    b.seal_block(else_blk);
    let v_else = b.ins().iconst(types::I64, 222);
    b.def_var(gc_ref, v_else);
    b.ins().jump(merge_blk, &[]);

    // merge: use_var(gc_ref) past safepoint; call tickle; return.
    b.switch_to_block(merge_blk);
    b.seal_block(merge_blk);
    let live = b.use_var(gc_ref);
    b.ins().call(callee, &[live]);
    let live_again = b.use_var(gc_ref);
    b.ins().return_(&[live_again]);
    b.finalize();

    let ir_dump = ctx.func.display().to_string();
    eprintln!("--- IR dump (var_variant) ---\n{ir_dump}");
    let code = ctx
        .compile(&*isa, &mut ControlPlane::default())
        .map_err(|e| format!("compile: {e:?}"))?;
    let maps = code.buffer.user_stack_maps();
    assert_stackmap_nonempty(&ir_dump, maps);

    // One safepoint (the call in merge_blk). The phi value at that PC
    // is the gc_ref variable. `declare_var_needs_stack_map` should have
    // propagated needs-stack-map to whichever SSA value the frontend
    // chose as the phi result, so we expect one entry.
    assert_eq!(
        maps.len(),
        1,
        "expected exactly one safepoint, got {maps:?}"
    );
    let (_pc, _frame, sm) = &maps[0];
    let entries: Vec<_> = sm.entries().collect();
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one live GC ref through phi confluence, got {entries:?}",
    );
    assert_eq!(entries[0].0, types::I64);
    Ok(())
}
