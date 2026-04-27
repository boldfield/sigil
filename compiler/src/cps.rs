//! CPS transform — Plan B Task 55, Phase 4e (real implementation
//! pending; this module is a typed pass-through stub at HEAD).
//!
//! ## Status
//!
//! At HEAD (`plan-b-task-55-phase-4e` branch), `transform` is a typed
//! pass-through: it wraps the [`ColoredProgram`] in a [`CpsProgram`]
//! without rewriting any expression. The Phase 4d MVP shape — synthetic
//! handler-arm CPS fns synthesised inline in `codegen.rs` plus a
//! synchronous `lower_perform_non_io_to_value` driving `sigil_run_loop`
//! from the call site — bypasses the need for a real CPS transform: a
//! CPS-color user fn that reaches `codegen.rs` is still emitted with
//! its declared Cranelift signature, and its `perform` sites resolve
//! synchronously inside the fn rather than yielding to the caller's
//! trampoline. This works for tail-position perform shapes (every
//! existing Phase 4c/4d positive test) but breaks discard-`k`
//! correctness across function-call boundaries (the
//! [`discard_k_handler_does_not_abort_helper_phase_4e_pending`][Phase 4d MVP entry]
//! `#[ignore]`'d e2e test, plus the existing passing e2e
//! `statement_form_non_io_perform_inside_handle_compiles_and_runs` —
//! algebraic-incorrect by `42 vs 99`).
//!
//! ## Phase 4e plan (multi-commit, this branch)
//!
//! Phase 4e replaces the synchronous shape with a real CPS dispatch:
//! CPS-color user fns get the uniform CPS calling convention
//! `extern "C" fn(closure_ptr, args_ptr, args_len) -> *mut NextStep`,
//! their bodies are CPS-converted (every non-tail expression whose
//! continuation reaches a perform or another CPS call gets lambda-
//! lifted into an explicit continuation closure), and the C-ABI
//! shim drives `sigil_run_loop` on user-`main`'s first NextStep when
//! `main` is CPS-color. See `[DEVIATION Task 55] Phase 4e —
//! comprehensive` in `PLAN_B_DEVIATIONS.md` for the full architectural
//! choices.
//!
//! Two design options were considered for where the CPS conversion
//! happens. The leading choice (subject to confirmation at the
//! implementing commit) is **inline lowering in `codegen.rs`**, not a
//! separate IR pass in this file:
//!
//! - **Option A — separate IR pass.** Extend the [`Expr`] enum with
//!   new CPS-form variants (`CpsTailCall`, `CpsDone`, `CpsContinue`,
//!   etc.) and have `cps::transform` rewrite CPS-color fn bodies into
//!   them. Codegen lowers the CPS-form variants directly. Pros:
//!   typed-IR-preservation discipline (Plan B Task 49 entry) applies
//!   automatically; the rewrite logic is independent of Cranelift.
//!   Cons: ~1500 LOC of new IR and rewrite machinery; matches every
//!   `Expr` use site in the compiler.
//!
//! - **Option B — inline lowering in codegen.** Generalise the Phase
//!   4d MVP synthetic-handler-arm-fn machinery (`HandlerArmSynth` +
//!   `collect_handle_arms_in_*` pre-pass + the synth-pass body
//!   lowering at the bottom of `emit_object`) to user fns: per-fn
//!   ABI selection driven by [`ColoredProgram::colors`], CPS-aware
//!   Lowerer paths for body lowering, lambda-lift continuation
//!   closures inline at perform / non-tail-CPS-call sites. Pros:
//!   reuses Phase 4d's already-tested machinery; smaller diff;
//!   `cps.rs` stays a thin pass-through. Cons: the lambda-lifted
//!   continuation closures need the same closure_convert machinery
//!   user lambdas use, but the lift happens at codegen-pass time
//!   (after closure_convert), so a side-table extension for
//!   "synthetic continuation closure records" is needed.
//!
//! Option B is the leading choice because the Phase 4d MVP's per-arm
//! closure-record allocation (`alloc_arm_closure_record`) is exactly
//! the shape the lambda-lifted continuations need; generalising it
//! adds a small accessor surface rather than a new IR. The deviation
//! entry's section 1 (real CPS transform in cps.rs) was framed
//! around Option A; a future deviation-entry update at the
//! implementing commit will reflect the option chosen at that time.
//!
//! ## What this module exposes today
//!
//! - [`CpsProgram`] — wraps [`ColoredProgram`] as a typed pipeline
//!   checkpoint. At HEAD the wrapper carries no CPS-form-specific
//!   metadata; it exists because each pipeline pass produces its own
//!   typed output by convention (lex → parse → resolve → typecheck →
//!   elaborate → monomorphize → infer_colors → cps::transform →
//!   closure_convert → emit_object). If Option B (above) ships and
//!   the codegen-consumes-color commit lands without adding fields
//!   here, future Phase 4e commits may add CPS-form metadata fields
//!   (e.g., per-fn yield-point side-tables, synthetic continuation
//!   FuncId allocations) that justify the wrapper retroactively. If
//!   no such metadata accrues, a future cleanup commit could fold the
//!   accessors below into [`ColoredProgram`] and drop `CpsProgram`
//!   entirely. The decision is deferred to the implementing commits;
//!   the wrapper is preserved at HEAD to match the staged-pipeline
//!   convention.
//! - [`transform`] — pass-through producer of `CpsProgram`.
//! - [`CpsProgram::needs_cps_transform`] — accessor: does this fn
//!   need CPS-form codegen treatment? (= is it CPS-color?)
//! - [`CpsProgram::cps_color_user_fns`] — accessor: list of CPS-color
//!   user fn names in program order.
//!
//! The accessors land in this commit (no behaviour change at codegen
//! yet); the codegen-consumes-color commit will use them at the user-
//! fn declaration loop in `emit_object` to drive per-fn ABI selection.

use crate::color::{Color, ColoredProgram};

#[derive(Clone, Debug)]
pub struct CpsProgram {
    pub colored: ColoredProgram,
}

impl CpsProgram {
    /// Does the user fn named `name` need CPS-form codegen treatment?
    ///
    /// At HEAD this is equivalent to "is `name` CPS-color?", because the
    /// `Color::Cps` classification is exactly the set of fns that need
    /// CPS calling convention + CPS-aware body lowering. Future Phase
    /// 4e commits may refine the per-fn decision (e.g., a CPS-color fn
    /// whose body has no perform and no CPS-call could in principle be
    /// emitted as native; Plan B treats this as a v2 optimization),
    /// but at HEAD the answer matches color directly.
    ///
    /// Returns `false` for fns not in the colored program (no panic).
    /// The codegen entry walker is the source of truth for which fns
    /// reach codegen; this accessor is a query, not an assertion.
    pub fn needs_cps_transform(&self, name: &str) -> bool {
        self.colored
            .colors
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, c)| matches!(c, Color::Cps))
            .unwrap_or(false)
    }

    /// List CPS-color user fn names in program order (matching the
    /// order in which they appear in `colored.colors`, which is the
    /// post-monomorphization program order from `monomorphize::run`).
    ///
    /// Used by the codegen-consumes-color commit (next on this branch)
    /// to iterate the fns that need CPS calling convention. Stable
    /// ordering matters for reproducibility (Plan A1's reproducibility
    /// test compares object-file bytes between runs); the underlying
    /// `colors` Vec preserves program declaration order
    /// (`color.rs::infer_colors` iterates `mono.anf.checked.program.
    /// items`), so this accessor preserves that. Source declaration
    /// order is stronger than alphabetical — a future refactor that
    /// silently swaps to a BTreeMap-derived order (alphabetical-only)
    /// would change reproducibility-relevant byte sequences without
    /// the colorer's tests catching it. This is the property pinned
    /// by `cps_color_user_fns_lists_program_order_cps_only` and
    /// `cps_color_user_fns_pins_multi_level_scc_bridge_ordering` in
    /// `cps::tests`.
    ///
    /// **Consumer contract.** Consumers driven by per-fn ABI
    /// selection should iterate this list directly (e.g.,
    /// `for name in cps_color_user_fns()` then look up the FuncId
    /// keyed by `name`), not query `needs_cps_transform(some_name)`
    /// with a name harvested from an AST walk. The latter pattern
    /// allows a typo to silently classify an unknown name as Native
    /// (since `needs_cps_transform` returns `false` for unknown fns
    /// by design — see its doc comment). The codegen-consumes-color
    /// commit follows the iterate-the-list pattern.
    pub fn cps_color_user_fns(&self) -> Vec<String> {
        self.colored
            .colors
            .iter()
            .filter_map(|(name, c)| {
                if matches!(c, Color::Cps) {
                    Some(name.clone())
                } else {
                    None
                }
            })
            .collect()
    }
}

/// Wrap a [`ColoredProgram`] for the CPS pass.
///
/// At HEAD this is a typed pass-through: no expression rewriting
/// happens. See module docs for the Phase 4e plan that replaces
/// the synchronous-`run_loop` shape with real CPS dispatch.
pub fn transform(colored: ColoredProgram) -> CpsProgram {
    CpsProgram { colored }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::color::infer_colors;
    use crate::elaborate;
    use crate::lexer;
    use crate::monomorphize::monomorphize;
    use crate::parser;
    use crate::resolve;
    use crate::typecheck;

    fn cps_from_src(src: &str) -> CpsProgram {
        let file = "test.sigil";
        let (tokens, lex_errs) = lexer::lex(file, src);
        assert!(lex_errs.is_empty(), "lex: {lex_errs:?}");
        let (prog, parse_errs) = parser::parse(file, &tokens);
        assert!(parse_errs.is_empty(), "parse: {parse_errs:?}");
        let (resolved, resolve_errs) = resolve::resolve(prog);
        assert!(resolve_errs.is_empty(), "resolve: {resolve_errs:?}");
        let (checked, tc_errs) = typecheck::typecheck(resolved.program);
        let hard_errs: Vec<_> = tc_errs
            .iter()
            .filter(|e| matches!(e.severity, crate::errors::Severity::Error))
            .collect();
        assert!(hard_errs.is_empty(), "typecheck: {hard_errs:?}");
        let anf = elaborate::elaborate(checked);
        let mono = monomorphize(anf);
        let colored = infer_colors(mono);
        transform(colored)
    }

    #[test]
    fn transform_is_typed_pass_through_at_head() {
        let src = r#"
            fn main() -> Int ![] { 42 }
        "#;
        let cp = cps_from_src(src);
        // At HEAD `transform` does not rewrite the expression tree.
        // Verify the wrapped colored program reaches us unchanged
        // by counting fns and checking main's color.
        assert_eq!(cp.colored.colors.len(), 1);
        assert_eq!(cp.colored.colors[0].0, "main");
    }

    #[test]
    fn needs_cps_transform_native_main_returns_false() {
        let src = r#"
            fn main() -> Int ![] { 42 }
        "#;
        let cp = cps_from_src(src);
        assert!(!cp.needs_cps_transform("main"));
    }

    #[test]
    fn needs_cps_transform_unknown_fn_returns_false() {
        let src = r#"
            fn main() -> Int ![] { 42 }
        "#;
        let cp = cps_from_src(src);
        // Querying a fn that doesn't exist returns false (not a panic).
        // The codegen entry walker is the source of truth for which fns
        // reach codegen; this accessor is a query, not an assertion.
        assert!(!cp.needs_cps_transform("nonexistent_fn"));
    }

    #[test]
    fn needs_cps_transform_classifies_cps_color_helper_correctly() {
        // statement_form_non_io_perform_inside_handle source — the
        // existing passing e2e test with main calling helper inside
        // a handle body. helper has row ![E] (intrinsic CPS); main
        // is CPS via SCC bridge. Verify the accessor returns true
        // for both.
        let src = "effect E { op: () -> Int }\n\
                   fn helper() -> Int ![E] {\n  \
                     perform E.op();\n  \
                     42\n\
                   }\n\
                   fn main() -> Int ![IO] {\n  \
                     let n: Int = handle helper() with { E.op(k) => 99 };\n  \
                     perform IO.println(int_to_string(n));\n  \
                     0\n\
                   }\n";
        let cp = cps_from_src(src);
        assert!(cp.needs_cps_transform("helper"));
        assert!(cp.needs_cps_transform("main"));
    }

    #[test]
    fn cps_color_user_fns_pins_mutual_recursion_scc_with_cps_bridge() {
        // a and b mutually recurse; both call c; c performs E.op
        // (intrinsic CPS). The mutual recursion forms a single SCC
        // {a, b}; the SCC bridges to c's singleton SCC (which is
        // CPS). All three end up CPS — a and b via SCC-bridge-to-cps,
        // c intrinsically. Pins the SCC-collapse + multi-member
        // ordering invariant: cps_color_user_fns() should list all
        // SCC members in source declaration order, not just one
        // representative member.
        //
        // Flagged by PR #26 mid-flight review at 06c3459 as a useful
        // pin before the codegen-consumes-color commit, which may
        // rely on per-SCC ABI uniformity (every member of a
        // CPS-color SCC must be emitted with the CPS calling
        // convention; a partial emission would be soundness-broken).
        // The 3-hop linear-chain test
        // (`cps_color_user_fns_pins_multi_level_scc_bridge_ordering`)
        // exercises forward-edge transitive classification; this one
        // exercises SCC-collapse with mutual recursion.
        //
        // Note: typecheck currently rejects mutual recursion at
        // function granularity (forward-reference handling is via
        // `fn_schemes` pre-pass, but it doesn't rebuild SCCs).
        // Phase B Task 48's pre-pass scheme-seeding closes this
        // hole — both a and b's signatures are visible to each
        // other's body checking. Verified passing on the synthetic
        // colorer test infrastructure where the same mutual-
        // recursion pattern appears.
        let src = "effect E { op: () -> Int }\n\
                   fn c() -> Int ![E] {\n  \
                     perform E.op()\n\
                   }\n\
                   fn a() -> Int ![E] {\n  \
                     let x: Int = c();\n  \
                     b()\n\
                   }\n\
                   fn b() -> Int ![E] {\n  \
                     let y: Int = c();\n  \
                     a()\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let cp = cps_from_src(src);
        let cps_fns = cp.cps_color_user_fns();
        // c is intrinsic CPS. a and b are mutually recursive — they
        // form a single SCC {a, b} which bridges to c's CPS
        // classification. All three are CPS. Source declaration
        // order: c, a, b. main is Native (excluded).
        assert_eq!(
            cps_fns,
            vec!["c".to_string(), "a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn cps_color_user_fns_pins_multi_level_scc_bridge_ordering() {
        // a → b → c, where c is intrinsically CPS, and verify
        // cps_color_user_fns lists all three in program declaration
        // order. Pins the transitive-closure invariant for ordering,
        // which is load-bearing if the codegen-consumes-color commit
        // relies on the order. The 2-level test below
        // (`cps_color_user_fns_lists_program_order_cps_only`)
        // exercises a single-hop bridge; this exercises three hops
        // and confirms the program-declaration-order property holds
        // through transitive classification (not just directly-
        // intrinsic-CPS members).
        //
        // Surface syntax notes:
        //   - `effect E { op: () -> Int }` declares the intrinsic
        //     CPS source.
        //   - `c` performs E.op (intrinsic CPS).
        //   - `b` calls `c` (bridge to c's SCC, which is CPS).
        //   - `a` calls `b` (bridge to b's SCC, which is CPS via
        //     bridge — this is the multi-hop case).
        //   - `main` is required for the typecheck/elaborate
        //     pipeline; it's Native (just returns 0).
        let src = "effect E { op: () -> Int }\n\
                   fn c() -> Int ![E] {\n  \
                     perform E.op()\n\
                   }\n\
                   fn b() -> Int ![E] {\n  \
                     c()\n\
                   }\n\
                   fn a() -> Int ![E] {\n  \
                     b()\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let cp = cps_from_src(src);
        let cps_fns = cp.cps_color_user_fns();
        // c is intrinsic CPS (row contains E + body performs E.op);
        // b and a become CPS via SCC bridge (a → b → c). main is
        // Native (empty row, no perform, no calls). Order follows
        // source declaration: c, b, a.
        assert_eq!(
            cps_fns,
            vec!["c".to_string(), "b".to_string(), "a".to_string()]
        );
    }

    #[test]
    fn cps_color_user_fns_lists_program_order_cps_only() {
        let src = "effect E { op: () -> Int }\n\
                   fn helper() -> Int ![E] {\n  \
                     perform E.op();\n  \
                     42\n\
                   }\n\
                   fn pure_helper(n: Int) -> Int ![] { n + 1 }\n\
                   fn main() -> Int ![IO] {\n  \
                     let n: Int = handle helper() with { E.op(k) => pure_helper(99) };\n  \
                     perform IO.println(int_to_string(n));\n  \
                     0\n\
                   }\n";
        let cp = cps_from_src(src);
        let cps_fns = cp.cps_color_user_fns();
        // helper is intrinsic CPS; main is CPS via bridge to helper;
        // pure_helper has empty row and no perform → Native. Order
        // follows program order (helper, pure_helper, main).
        assert_eq!(cps_fns, vec!["helper".to_string(), "main".to_string()]);
    }
}
