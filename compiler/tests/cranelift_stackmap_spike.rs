// Plan E2 Phase 1 Task 1 — Cranelift 0.131.0 stack-map API spike, as an
// integration test so CI exercises both API paths on every push.
//
// Two tests pin the API surface Plan E2 will use:
//
//   1. `value_variant_emits_stackmap_at_call_safepoint` — the Value-variant
//      path. `declare_value_needs_stack_map(v)` flags a single SSA value as
//      a GC reference; Cranelift spills it across the safepoint at the
//      synthetic call site and records the slot in the per-call
//      `UserStackMap`.
//
//   2. `var_variant_emits_stackmap_for_phi_confluence` — the Variable-variant
//      path. `declare_var_needs_stack_map(var)` flags a frontend Variable;
//      every value defined via `def_var(var, _)` (from each predecessor of
//      a join block, in this test) is propagated through the safepoint
//      pass during `finalize`, then spilled around the safepoint that
//      reads the variable past the join. This is the path Plan E2 Phase 1
//      Task 2 will use for phi confluences (heap-pointer-bearing locals
//      defined in multiple predecessor blocks), since per-Value flagging
//      at each phi input edge is fragile.
//
// Each test prints a `--- IR dump ---` from `ctx.func.display()` before
// the assert!() trip if the stack-map list is empty — saves a debug
// session next time the spike fails after an upstream Cranelift change.

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

fn assert_one_safepoint(ir_dump: &str, maps: &[(u32, u32, ir::UserStackMap)]) {
    if maps.is_empty() {
        eprintln!("--- IR dump ---\n{ir_dump}");
    }
    assert!(
        !maps.is_empty(),
        "expected at least one stack map entry — IR dump on stderr. \
         Likely upstream Cranelift behaviour change in `declare_*_needs_stack_map` \
         or in the automatic safepoint pass.",
    );
}

#[test]
fn value_variant_emits_stackmap_at_call_safepoint() -> SpikeResult {
    let isa = make_isa()?;
    let cc = CallConv::triple_default(&Triple::host());

    // Signatures: `tickle(i64)` external; `entry() -> i64`.
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

    // Body: iconst v0; call tickle(v0); return v0.
    // `v0` is live across the call and flagged needs-stack-map.
    let v0 = b.ins().iconst(types::I64, 42);
    b.declare_value_needs_stack_map(v0);
    b.ins().call(callee, &[v0]);
    b.ins().return_(&[v0]);
    b.finalize();

    // Snapshot the post-finalize IR before `compile` mutably borrows
    // `ctx` so the assertion helper can dump it on failure.
    let ir_dump = ctx.func.display().to_string();
    let code = ctx
        .compile(&*isa, &mut ControlPlane::default())
        .map_err(|e| format!("compile: {e:?}"))?;
    let maps = code.buffer.user_stack_maps();
    assert_one_safepoint(&ir_dump, maps);

    // Exactly one safepoint (the single call site); the live value v0
    // is i64 (pointer-width on x86_64 + aarch64), so one entry.
    assert_eq!(maps.len(), 1, "expected exactly one safepoint");
    let (_pc, _frame, sm) = &maps[0];
    let entries: Vec<_> = sm.entries().collect();
    assert_eq!(entries.len(), 1, "expected exactly one live GC ref");
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
    let code = ctx
        .compile(&*isa, &mut ControlPlane::default())
        .map_err(|e| format!("compile: {e:?}"))?;
    let maps = code.buffer.user_stack_maps();
    assert_one_safepoint(&ir_dump, maps);

    // One safepoint (the call in merge_blk). The phi value at that PC
    // is the gc_ref variable. `declare_var_needs_stack_map` should have
    // propagated needs-stack-map to whichever SSA value the frontend
    // chose as the phi result, so we expect one entry.
    assert_eq!(maps.len(), 1, "expected exactly one safepoint");
    let (_pc, _frame, sm) = &maps[0];
    let entries: Vec<_> = sm.entries().collect();
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one live GC ref through phi confluence"
    );
    assert_eq!(entries[0].0, types::I64);
    Ok(())
}
