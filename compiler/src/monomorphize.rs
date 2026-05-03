//! Monomorphization — Plan B task 49.
//!
//! Whole-program specialisation pass that runs after type-check and
//! before color inference. For every reachable use of a generic
//! top-level function or generic user type, produce a concrete clone
//! at the inferred type-argument tuple and rewrite the use site to
//! reference the clone by its canonically-mangled name. Generic
//! decls themselves are dropped from the post-pass IR — codegen only
//! sees fully-concrete `FnDecl`s and `TypeDecl`s.
//!
//! ## Inputs
//!
//! - The post-elaborate AST (`AnfProgram::checked::program`).
//! - Per-fn polymorphic schemes recorded by typecheck
//!   (`CheckedProgram::fn_schemes`).
//! - Per-use-site instantiation indices
//!   (`call_site_instantiations`, `ctor_site_instantiations`).
//! - Per-match scrutinee types (`match_scrut_tys`) for resolving
//!   pattern-ctor instantiations through the scrutinee's type.
//!
//! ## Reachability
//!
//! Reachability is rooted at `main` (the only fn directly invoked by
//! the runtime entry shim) and proceeds depth-first through call /
//! value-reference / construction sites. Unreachable generic
//! declarations and unreachable concrete declarations alike are
//! dropped — keeping binary size tight and giving v2 whole-program
//! optimization passes a smaller graph to chew on. Plan A1's
//! single-fn `main`-only programs still produce a single clone
//! identical (modulo identity rewriting) to the input.
//!
//! ## Effect rows are NOT monomorphized
//!
//! Plan B v1 reserves row-specialised monomorphs for v2. Effect rows
//! remain polymorphic through this pass — `FnDecl::effect_row_var`
//! is preserved on each clone, and codegen erases the row variable
//! at lowering (effect dispatch is runtime-indirect). The codegen-
//! entry guard's check for `f.effect_row_var.is_some()` is relaxed
//! correspondingly.
//!
//! ## Typed IR preserved
//!
//! Every concrete clone carries fully-resolved `TypeExpr::Named(...)`
//! references on its `Param.ty`, `return_type`, and let-binding
//! `LetStmt.ty` slots. `TypeExpr::Apply` does not appear in the
//! post-pass IR (resolved into mangled `Named` references); generic-
//! parameter references like `Named("A", _)` are likewise resolved
//! into concrete primitive or user-type names.
//!
//! ## Canonical specialization names
//!
//! For a generic fn `f[T1, T2, ...]` instantiated at concrete
//! `Ty(T1) = TA, Ty(T2) = TB, ...`:
//!
//! - Mangled name: `f$$<canon(TA)>$$<canon(TB)>$$...`.
//!
//! For a generic type `Foo[T1, T2, ...]` instantiated at the same
//! tuple: `Foo$$<canon(TA)>$$<canon(TB)>$$...`. Each ctor `C` of
//! `Foo[T1, T2]` is renamed to `C$$<canon(TA)>$$<canon(TB)>` so
//! the global ctor namespace stays unique across instantiations.
//!
//! `canon(Ty)` is recursive:
//! - Primitives render as themselves: `Int`, `String`, `Bool`,
//!   `Char`, `Byte`, `Unit`.
//! - `User(name, [])` renders as `name`.
//! - `User(name, [a1, a2, ...])` renders as
//!   `name$<canon(a1)>$<canon(a2)>$...` — single `$` between parts
//!   within a single type-arg, double `$$` between top-level
//!   type-args.
//!
//! `$` is the separator because the lexer rejects it as an
//! identifier character (same constraint Plan A2 relied on for
//! `$lambda_N` synthetic names from closure conversion). This makes
//! the format **structurally unambiguous regardless of underscore
//! density in user identifiers**: a user-declared `type List_Option[A]`
//! cannot collide with the canonical render of `List[Option[Int]]`,
//! because the former uses underscores in the type *name* (`List_Option`)
//! while the latter inserts `$` separators between the type and its
//! args (`List$Option$Int`). Codegen's `mangle_user_fn` rewrites `$`
//! to `__` for ELF / Mach-O linker compatibility — that rewrite
//! preserves the unambiguity since at the AST level (where uniqueness
//! is enforced) we always carry the `$`-form.
//!
//! Type arguments are emitted in the callee's *declared* generic-
//! parameter order, not lex-sorted. This is a deliberate choice for
//! v1 — sort-by-name would lose the positional binding between a
//! type-var's declared name (`A`, `B`, ...) and its concrete arg.
//! Determinism flows from BTreeMap iteration order over the
//! reachability worklist, not from argument-ordering.
//!
//! Non-generic fns and types pass through unchanged: their names
//! retain the original form (`f`, `Option`, `Some`), no mangling
//! applied. The pre-pass guard returns the input unchanged when no
//! generic decls exist, so plan A1 / A2 / A3 programs incur zero
//! mangling overhead.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::ast::{
    Block, CtorPatternField, CtorPatternFields, Expr, FnDecl, Item, LetStmt, MatchArm, Param,
    Pattern, PerformExpr, Program, RecordFieldDecl, RecordFieldLit, Stmt, TypeDecl, TypeExpr,
    Variant, VariantFields,
};
use crate::elaborate::AnfProgram;
use crate::errors::Span;
use crate::typecheck::{GenericInstantiation, Scheme, Ty};

/// Plan B' Stage 6.8 Phase C++ — per-clone resolved
/// `lambda_captures`. See `MonoProgram::lambda_captures_resolved`.
pub type LambdaCapturesResolved = BTreeMap<(String, Span), Vec<(String, Ty)>>;

/// Plan D Task 113 R1 finding 2 — per-clone resolved
/// `match_scrut_tys`. See `MonoProgram::match_scrut_tys_resolved`.
pub type MatchScrutTysResolved = BTreeMap<(String, Span), Ty>;

/// Plan D Task 119b — per-clone resolved `handle_body_ty`. See
/// `MonoProgram::handle_body_ty_resolved`.
pub type HandleBodyTyResolved = BTreeMap<(String, Span), Ty>;

/// Plan D Task 119b — per-clone resolved `call_callee_tys`. See
/// `MonoProgram::call_callee_tys_resolved`.
pub type CallCalleeTysResolved = BTreeMap<(String, Span), Ty>;

#[derive(Clone, Debug)]
pub struct MonoProgram {
    pub anf: AnfProgram,
    /// Plan B' Stage 6.8 Phase C++ — per-clone resolved
    /// `lambda_captures` keyed by (clone_fn_name, lambda_span).
    /// closure_convert consumes this to source captures' Tys post-
    /// monomorphize-substitution. For non-generic programs (the
    /// fast-path skip), this map is empty and closure_convert
    /// falls back to `CheckedProgram.lambda_captures` (the
    /// pre-mono typecheck side-table); for generic programs each
    /// fn clone populates per-(fn_name, span) entries with
    /// `Ty::Var` resolved through the active substitution.
    pub lambda_captures_resolved: LambdaCapturesResolved,
    /// Plan D Task 113 R1 finding 2 — per-clone resolved
    /// `match_scrut_tys` keyed by (clone_fn_name, match_span). The
    /// span-keyed `CheckedProgram::match_scrut_tys` side-table is
    /// shared across every clone of a generic fn (spans are
    /// preserved across cloning by design, so a single source
    /// `match` expression has one span regardless of which clone is
    /// being lowered). For generic clones, the entry holds
    /// `Ty::Var(_)`-bearing types from the pre-mono parent; codegen
    /// reading those would trip `cranelift_ty_of_ty`'s `Ty::Var`
    /// guard. This per-clone map records the post-substitution
    /// scrutinee `Ty` keyed by `(clone_fn_name, match_span)` so
    /// codegen's `lower_match` can recover the concrete type
    /// regardless of scrutinee shape (Ident, Call, nested Match,
    /// etc.). For non-generic programs (the fast-path skip), this
    /// map is empty and codegen falls back to the span-keyed
    /// `CheckedProgram` side-table.
    pub match_scrut_tys_resolved: MatchScrutTysResolved,
    /// Plan D Task 119b — per-clone resolved `handle_body_ty` keyed
    /// by (clone_fn_name, handle_span). Mirrors
    /// `match_scrut_tys_resolved` for `Expr::Handle`. Codegen's
    /// pre-pass at the return-arm dispatch reads from this map to
    /// recover the concrete body Ty for generic clones (the
    /// span-keyed `CheckedProgram::handle_body_ty` is shared across
    /// every clone of a generic source fn and leaks `Ty::Var(_)` for
    /// generic clones).
    pub handle_body_ty_resolved: HandleBodyTyResolved,
    /// Plan D Task 119b — per-clone resolved `call_callee_tys` keyed
    /// by (clone_fn_name, call_span). Mirrors
    /// `match_scrut_tys_resolved` for indirect-call sites whose
    /// callee is `Expr::Call(..)` (the inner Call's return Ty
    /// resolves the outer Call's signature in `lower_call`'s
    /// indirect path). The pre-mono `CheckedProgram::call_callee_tys`
    /// is span-keyed and leaks `Ty::Var(_)` for calls inside generic
    /// clones (the source side-table is shared across every clone).
    pub call_callee_tys_resolved: CallCalleeTysResolved,
}

/// Run monomorphization on the post-elaborate IR. Returns the input
/// unchanged when no generic decls exist (the common case for plan
/// A1/A2/A3 programs); otherwise produces a fully-specialised
/// `Program` and updates the types registry to match.
pub fn monomorphize(mut anf: AnfProgram) -> MonoProgram {
    let needs_mono = program_has_generics(&anf.checked.program);
    if !needs_mono {
        return MonoProgram {
            anf,
            lambda_captures_resolved: BTreeMap::new(),
            match_scrut_tys_resolved: BTreeMap::new(),
            handle_body_ty_resolved: BTreeMap::new(),
            call_callee_tys_resolved: BTreeMap::new(),
        };
    }

    let (
        new_items,
        lambda_captures_resolved,
        match_scrut_tys_resolved,
        handle_body_ty_resolved,
        call_callee_tys_resolved,
        builtin_specializations,
    ) = {
        let mono = Monomorphizer::new(&anf.checked);
        mono.run_with_lambda_captures()
    };

    // Replace the AST and rebuild the types registry from the new
    // (concrete-only) item list so downstream passes see the
    // monomorphized type set rather than the original generic decls.
    anf.checked.program.items = new_items;
    let mut new_types: BTreeMap<String, TypeDecl> = BTreeMap::new();
    for item in &anf.checked.program.items {
        if let Item::Type(td) = item {
            new_types.insert(td.name.clone(), (**td).clone());
        }
    }
    // Plan D Task 117 (a) — inject synthetic TypeDecls for builtin
    // generic specializations encountered during rewriting. These
    // entries (e.g. `Array$$Int`) carry empty `generic_params` and
    // empty `variants`; they're never instantiated as user types,
    // but their presence in `tc.types` lets `ty_from_type_expr`
    // resolve mangled `Named("Array$$Int")` references that
    // monomorphize emits for Apply-form builtin generic uses.
    // Empty variants ensure `build_ctor_index` registers no spurious
    // ctors and `build_layouts`'s inner variant loop produces an
    // empty layout entry. Synthetic TypeDecls are NOT pushed into
    // `program.items` — that would re-trigger `clone_type` and
    // double-mangle on subsequent passes; injecting directly into
    // the rebuilt types map keeps them registry-only.
    let synth_span = crate::errors::Span::synthetic("monomorphize.synthetic");
    for mangled in &builtin_specializations {
        new_types
            .entry(mangled.clone())
            .or_insert_with(|| TypeDecl {
                name: mangled.clone(),
                name_span: synth_span.clone(),
                generic_params: Vec::new(),
                variants: Vec::new(),
                span: synth_span.clone(),
            });
    }
    anf.checked.types = new_types;
    MonoProgram {
        anf,
        lambda_captures_resolved,
        match_scrut_tys_resolved,
        handle_body_ty_resolved,
        call_callee_tys_resolved,
    }
}

/// Quick check: does the program contain any generic decls or any
/// `TypeExpr::Apply` use sites? Skipping the entire pass on non-
/// generic programs keeps plan A1/A2/A3 codegen budget unchanged and
/// avoids touching the AST at all.
///
/// Plan C Task 65 — also returns true when any `TypeExpr::Apply` node
/// exists anywhere in the program (delegated to `codegen::
/// contains_apply_or_generic_ref`). Programs that use builtin generic
/// types (e.g. `Array[Int]` in a let binding) carry Apply nodes even
/// without user-declared generic decls; those Apply nodes must be
/// rewritten to mangled `Named` so the codegen-entry assertion
/// doesn't trip.
pub(crate) fn program_has_generics(program: &Program) -> bool {
    if crate::codegen::contains_apply_or_generic_ref(program) {
        return true;
    }
    for item in &program.items {
        match item {
            Item::Fn(f) => {
                if !f.generic_params.is_empty() {
                    return true;
                }
            }
            Item::Type(td) => {
                if !td.generic_params.is_empty() {
                    return true;
                }
            }
            Item::Import(_) => {}
            Item::Effect(_) => {}
        }
    }
    false
}

// ---------------------------------------------------------------- name mangling

/// Canonical render of a `Ty` for use inside a mangled symbol name.
/// See module-level docs for the exact format. Public so unit tests
/// in this module can pin the contract.
///
/// `Ty::Var(_)` is *unreachable* here: the E0132 ambiguous-polymorphism
/// diagnostic in typecheck rejects any `Ty::Var(_)` that would survive
/// substitution to a use site. Hitting that arm means an upstream
/// invariant broke; we trip `unreachable!` rather than silently
/// rendering a placeholder that two distinct vars would both collide
/// to.
///
/// `Ty::Fn(_)` (Plan B' Stage 6.8 Task 103) renders as
/// `Fn$<P1>$..$<Pn>$Ret$<R>$Eff$<E1>$..$<Em>` — params first, then
/// `Ret$<R>`, then `Eff$<E1>$..` for the effect set. The `Fn` /
/// `Ret` / `Eff` segment markers fence each component so a 0-param
/// fn-type still has a syntactically distinct mangle from a unit-
/// returning user type. Closed rows only — `effect_row_var` is
/// rejected upstream by E0137.
pub fn canon_ty(ty: &Ty) -> String {
    match ty {
        Ty::Int => "Int".to_string(),
        Ty::String => "String".to_string(),
        Ty::Bool => "Bool".to_string(),
        Ty::Char => "Char".to_string(),
        Ty::Byte => "Byte".to_string(),
        Ty::Unit => "Unit".to_string(),
        Ty::User(name, args) => {
            if args.is_empty() {
                name.clone()
            } else {
                let mut s = name.clone();
                for a in args {
                    s.push('$');
                    s.push_str(&canon_ty(a));
                }
                s
            }
        }
        Ty::Tuple(elems) => {
            let mut s = String::from("Tuple");
            for e in elems {
                s.push('$');
                s.push_str(&canon_ty(e));
            }
            s
        }
        Ty::Var(id) => {
            // Reachability-bounded mono never sees an unresolved var
            // because (a) the E0132 ambiguous-polymorphism check at
            // end-of-typecheck rejects unconstrained type parameters,
            // and (b) descent into a generic clone substitutes outer-
            // fn vars to concrete types via `Substitution::by_var`. If
            // we reach this arm, an invariant in either of those two
            // mechanisms broke; surface it loudly.
            unreachable!(
                "monomorphize::canon_ty: Ty::Var({id}) escaped substitution — \
                 typecheck E0132 should have rejected this call site, or \
                 monomorph descent missed an outer-fn var binding"
            )
        }
        Ty::Fn(sig) => {
            // Plan B' Stage 6.8 Task 103 fixup (R1 finding 1): render
            // `Ty::Fn` to a stable mangled string so generic helpers
            // instantiated with fn-typed args don't trip a panic in
            // monomorphize. Closed rows only (typecheck E0137 rejects
            // row-variable-bearing fn-types upstream).
            let mut s = String::from("Fn");
            for p in &sig.params {
                s.push('$');
                s.push_str(&canon_ty(p));
            }
            s.push_str("$Ret$");
            s.push_str(&canon_ty(&sig.ret));
            if !sig.effects.is_empty() {
                s.push_str("$Eff");
                // Sort for stable mangling regardless of declaration
                // order (parser keeps source order; effect rows are
                // semantically a set). Plan D Task 114 — mangle as
                // `Eff$<name>` (bare) or `Eff$<name>$<arg1>$<arg2>$...`
                // (type-parameterized) so distinct instantiations of
                // a generic effect-decl produce distinct symbols.
                let mut effs: Vec<&crate::typecheck::EffectInst> = sig.effects.iter().collect();
                effs.sort_by(|a, b| a.name.cmp(&b.name));
                for e in effs {
                    s.push('$');
                    s.push_str(&e.name);
                    for a in &e.args {
                        s.push('$');
                        s.push_str(&canon_ty(a));
                    }
                }
            }
            s
        }
        Ty::Continuation(_) => {
            // Plan D Task 117 — Continuation values cannot reach
            // mono. The escape barrier rejects them from any
            // cross-fn position (return / heap-store / fn-arg via
            // unify_ty's broad arm; generic-instantiation via
            // check_call's bind_ty_var bypass closure with
            // E0145 — see typecheck.rs:4403-4445 for the
            // precision check). Mono runs only on programs whose
            // typecheck succeeded, so a Continuation reaching
            // canon_ty means an escape path slipped through the
            // typecheck barrier.
            //
            // Restored to `unreachable!()` after PR #60 review #3
            // closed the bind_ty_var bypass; defensive mangling
            // was the temporary cover for that bypass. Walker
            // consistency: matches the Var unreachable!()s in
            // typecheck.rs (`apply_ty_inner`, `rename_ty`,
            // `unify_ty`) and `Substitution::apply_to_ty` /
            // `ty_to_type_expr` below.
            unreachable!(
                "monomorphize::canon_ty: Ty::Continuation reached the canon-mangler — \
                 typecheck E0145 should have rejected the cross-fn / cross-storage \
                 site that allowed a Continuation to escape"
            )
        }
    }
}

/// Mangled name for a fn instantiation `f[args...]`. Non-generic
/// callees (empty `args`) keep the original name.
pub fn mangle_fn(name: &str, args: &[Ty]) -> String {
    if args.is_empty() {
        return name.to_string();
    }
    let mut s = name.to_string();
    for a in args {
        s.push_str("$$");
        s.push_str(&canon_ty(a));
    }
    s
}

/// Mangled name for a type instantiation `Foo[args...]`. Non-generic
/// types (empty `args`) keep the original name.
pub fn mangle_type(name: &str, args: &[Ty]) -> String {
    if args.is_empty() {
        return name.to_string();
    }
    let mut s = name.to_string();
    for a in args {
        s.push_str("$$");
        s.push_str(&canon_ty(a));
    }
    s
}

/// Mangled name for a ctor `C` of `Foo[args...]`. Non-generic types
/// keep the ctor's original name; generic types get a per-tuple
/// suffix so the ctor namespace stays globally unique post-mono.
pub fn mangle_ctor(ctor: &str, type_args: &[Ty]) -> String {
    if type_args.is_empty() {
        return ctor.to_string();
    }
    let mut s = ctor.to_string();
    for a in type_args {
        s.push_str("$$");
        s.push_str(&canon_ty(a));
    }
    s
}

// ---------------------------------------------------------------- monomorphizer

/// Worklist key — a generic-fn instantiation pending cloning.
type FnKey = (String, Vec<Ty>);
/// Worklist key — a generic-type instantiation pending cloning.
type TypeKey = (String, Vec<Ty>);

struct Monomorphizer<'a> {
    /// Reference to the original `Program::items` so imports can be
    /// preserved in the output without re-cloning.
    original_items: &'a [Item],
    /// Source items, indexed by name for O(log n) lookup during
    /// reachability traversal.
    fn_decls: BTreeMap<String, &'a FnDecl>,
    type_decls: BTreeMap<String, &'a TypeDecl>,
    /// Reverse index: ctor name → owning type name. Built from the
    /// original (pre-mono) types registry. Used at pattern-rewriting
    /// time to identify which type a pattern ctor belongs to.
    ctor_to_type: BTreeMap<String, String>,
    /// Per-fn schemes from typecheck. Lookups during cloning use
    /// `scheme.type_vars` (in declared order) to build the surface-
    /// name → concrete-Ty substitution for each clone.
    fn_schemes: &'a BTreeMap<String, Scheme>,
    /// Per-use-site fn instantiations. Keyed by `Expr::Ident` span.
    call_sites: &'a BTreeMap<Span, GenericInstantiation>,
    /// Per-construction-site ctor instantiations. Keyed by the
    /// construction expression's span (call / ident / record-lit).
    ctor_sites: &'a BTreeMap<Span, GenericInstantiation>,
    /// Per-match scrutinee `Ty`. Used to resolve pattern-ctor
    /// instantiations from the surrounding match's scrutinee type.
    match_scrut_tys: &'a BTreeMap<Span, Ty>,
    /// Pending fn instantiations to clone.
    fn_worklist: VecDeque<FnKey>,
    /// Pending type instantiations to clone.
    type_worklist: VecDeque<TypeKey>,
    /// Already-enqueued fn instantiations, deduped by mangled name
    /// (the canonical-form String produced by `mangle_fn`). `Ty`
    /// itself doesn't impl `Ord` (deliberately — equality is
    /// well-defined but a total order would require choosing one
    /// among many for `FnSig` shapes), so we key the seen-set by
    /// the same string codegen will use.
    fn_seen: BTreeSet<String>,
    /// Already-enqueued type instantiations, same dedup approach.
    type_seen: BTreeSet<String>,
    /// Fully-cloned fn decls in production order (BFS-driven).
    output_fns: Vec<FnDecl>,
    /// Fully-cloned type decls in production order.
    output_types: Vec<TypeDecl>,
    /// Plan B' Stage 6.8 Phase C++ — typecheck's per-Lambda-span
    /// captures table. Read at clone time to populate
    /// `lambda_captures_resolved` per (clone_fn_name, span). The
    /// original is retained because non-generic fn paths (and the
    /// fast-path skip for non-generic programs) consult it
    /// directly via closure_convert; the per-clone resolved view
    /// only matters for generic clones whose captures contain
    /// `Ty::Var` references that need substitution.
    lambda_captures: &'a Vec<(Span, Vec<(String, Ty)>)>,
    /// Plan B' Stage 6.8 Phase C++ — per-clone resolved
    /// `lambda_captures` keyed by (clone_fn_name, lambda_span).
    /// Each entry's `Vec<(String, Ty)>` has the substitution
    /// applied — for non-generic clones this is identity-equal to
    /// the original entry; for generic clones it resolves
    /// `Ty::Var(_)` to the concrete type-arg.
    lambda_captures_resolved: LambdaCapturesResolved,
    /// Plan B' Stage 6.8 Phase C++ — set in `clone_fn` before
    /// rewriting the body, cleared after. The Lambda arm of
    /// `rewrite_expr` reads this to record (current_clone_fn_name,
    /// lambda_span) → resolved-captures entries in
    /// `lambda_captures_resolved`.
    current_clone_fn_name: Option<String>,
    /// Plan D Task 113 R1 finding 2 — per-clone resolved
    /// `match_scrut_tys` populated by the Match arm of `rewrite_expr`,
    /// keyed by (clone_fn_name, match_span). Lifted into
    /// `MonoProgram` at the end of monomorphize so codegen can read
    /// concrete (post-substitution) scrutinee types regardless of
    /// scrutinee expression shape.
    match_scrut_tys_resolved: MatchScrutTysResolved,
    /// Plan D Task 119b — typecheck's per-`Expr::Handle`-span body
    /// type (the Ty of the handle's `body` expression). Read at
    /// clone time to populate `handle_body_ty_resolved` per
    /// (clone_fn_name, handle_span). Mirrors `match_scrut_tys` —
    /// codegen needs the post-substitution body Ty when sizing the
    /// return arm's `v` binding's Cranelift type, and the span-keyed
    /// CheckedProgram entry leaks `Ty::Var` from the pre-mono
    /// parent for generic clones.
    handle_body_ty: &'a BTreeMap<Span, Ty>,
    /// Plan D Task 119b — per-clone resolved `handle_body_ty`
    /// populated by the Handle arm of `rewrite_expr`, keyed by
    /// (clone_fn_name, handle_span). Lifted into `MonoProgram` so
    /// codegen's pre-pass can recover the concrete body Ty without
    /// re-substituting from the span-keyed source side-table.
    handle_body_ty_resolved: HandleBodyTyResolved,
    /// Plan D Task 119b — typecheck's per-Call-span return Ty.
    /// Read at clone time to populate `call_callee_tys_resolved`
    /// per (clone_fn_name, call_span). Codegen reads the resolved
    /// map for indirect-call signature derivation in generic clones.
    call_callee_tys: &'a BTreeMap<Span, Ty>,
    /// Plan D Task 119b — per-clone resolved `call_callee_tys`
    /// populated by the Call arm of `rewrite_expr`, keyed by
    /// (clone_fn_name, call_span).
    call_callee_tys_resolved: CallCalleeTysResolved,
    /// Plan D Task 117 (a) — mangled names of builtin generic
    /// specializations encountered during `rewrite_type_expr`'s
    /// `Apply` rewrite. Builtin generic types (`Array`, `MutArray`)
    /// have synthetic TypeDecls in `tc.types` (`builtin_types()`)
    /// but no clone target — `enqueue_type` skips them. The Apply
    /// rewrite still produces a mangled `Named("Array$$Int")` for
    /// downstream IR consistency. Without a corresponding TypeDecl
    /// in `tc.types`, sites like `build_layouts` /
    /// `ty_from_type_expr` can't resolve the mangled name. The set
    /// here is consumed at the top-level `monomorphize()` to
    /// inject synthetic empty-variants TypeDecls into the rebuilt
    /// `tc.types`. Surfaced by Sudoku's `Option[Array[Int]]`
    /// pattern destructure.
    builtin_specializations: BTreeSet<String>,
}

impl<'a> Monomorphizer<'a> {
    fn new(checked: &'a crate::typecheck::CheckedProgram) -> Self {
        let mut fn_decls: BTreeMap<String, &'a FnDecl> = BTreeMap::new();
        let mut type_decls: BTreeMap<String, &'a TypeDecl> = BTreeMap::new();
        let mut ctor_to_type: BTreeMap<String, String> = BTreeMap::new();
        for item in &checked.program.items {
            match item {
                Item::Fn(f) => {
                    fn_decls.insert(f.name.clone(), f.as_ref());
                }
                Item::Type(td) => {
                    type_decls.insert(td.name.clone(), td.as_ref());
                    for v in &td.variants {
                        ctor_to_type.insert(v.name.clone(), td.name.clone());
                    }
                }
                Item::Import(_) => {}
                // Plan B task 53 — effect decls do not yet have an
                // entry in the registry; Task 54 adds it. Skipping
                // here keeps Task 53's parser-only commit compile-
                // clean without prematurely committing to a registry
                // shape Task 54 might revise.
                Item::Effect(_) => {}
            }
        }
        Self {
            original_items: &checked.program.items,
            fn_decls,
            type_decls,
            ctor_to_type,
            fn_schemes: &checked.fn_schemes,
            call_sites: &checked.call_site_instantiations,
            ctor_sites: &checked.ctor_site_instantiations,
            match_scrut_tys: &checked.match_scrut_tys,
            fn_worklist: VecDeque::new(),
            type_worklist: VecDeque::new(),
            fn_seen: BTreeSet::new(),
            type_seen: BTreeSet::new(),
            output_fns: Vec::new(),
            output_types: Vec::new(),
            lambda_captures: &checked.lambda_captures,
            lambda_captures_resolved: BTreeMap::new(),
            current_clone_fn_name: None,
            match_scrut_tys_resolved: BTreeMap::new(),
            handle_body_ty: &checked.handle_body_ty,
            handle_body_ty_resolved: BTreeMap::new(),
            call_callee_tys: &checked.call_callee_tys,
            call_callee_tys_resolved: BTreeMap::new(),
            builtin_specializations: BTreeSet::new(),
        }
    }

    /// Plan B' Stage 6.8 Phase C++ — same as `run` but also returns
    /// the per-clone-resolved `lambda_captures` map populated during
    /// fn cloning. Used by the slow path of `monomorphize()` so the
    /// downstream `closure_convert` pass can read substituted Tys
    /// for each clone's lambdas.
    fn run_with_lambda_captures(
        self,
    ) -> (
        Vec<Item>,
        LambdaCapturesResolved,
        MatchScrutTysResolved,
        HandleBodyTyResolved,
        CallCalleeTysResolved,
        BTreeSet<String>,
    ) {
        let mut this = self;
        let items = this.run_inner_borrowed();
        (
            items,
            this.lambda_captures_resolved,
            this.match_scrut_tys_resolved,
            this.handle_body_ty_resolved,
            this.call_callee_tys_resolved,
            this.builtin_specializations,
        )
    }

    fn run_inner_borrowed(&mut self) -> Vec<Item> {
        // Seed the worklist with main (zero type-args; main is non-
        // generic by construction — typecheck E0040 guards an absent
        // main). Main's reachability transitively pulls in every
        // call / ctor / pattern site we care about.
        if self.fn_decls.contains_key("main") {
            self.enqueue_fn("main".to_string(), Vec::new());
        }

        // Fixpoint loop: cloning a fn body may enqueue more fns *and*
        // more types (via TypeExpr::Apply in let-bindings / signatures);
        // cloning a type may enqueue more types (via generic field
        // types). Drain both worklists until neither has work.
        loop {
            let any_pending = !self.fn_worklist.is_empty() || !self.type_worklist.is_empty();
            if !any_pending {
                break;
            }
            while let Some(key) = self.fn_worklist.pop_front() {
                let cloned = self.clone_fn(&key);
                self.output_fns.push(cloned);
            }
            while let Some(key) = self.type_worklist.pop_front() {
                let cloned = self.clone_type(&key);
                self.output_types.push(cloned);
            }
        }

        // Assemble the post-pass items: imports preserved (original
        // order), then types in worklist BFS order, then fns in BFS
        // order from main. Imports are pure compile-time directives;
        // their relative ordering doesn't matter for codegen but
        // preserving it keeps the output diff-readable against input.
        let mut out: Vec<Item> = Vec::new();
        // Imports first; effect decls (Plan B task 53) preserved
        // alongside imports so a future Task 54 registry build sees
        // the full surface set even after monomorphization clones
        // generic fns / types.
        for item in self.original_items {
            match item {
                Item::Import(_) | Item::Effect(_) => out.push(item.clone()),
                _ => {}
            }
        }
        for td in std::mem::take(&mut self.output_types) {
            out.push(Item::Type(Box::new(td)));
        }
        for f in std::mem::take(&mut self.output_fns) {
            out.push(Item::Fn(Box::new(f)));
        }
        out
    }

    fn enqueue_fn(&mut self, name: String, type_args: Vec<Ty>) {
        let mangled = mangle_fn(&name, &type_args);
        if self.fn_seen.insert(mangled) {
            self.fn_worklist.push_back((name, type_args));
        }
    }

    fn enqueue_type(&mut self, name: String, type_args: Vec<Ty>) {
        let mangled = mangle_type(&name, &type_args);
        if self.type_seen.insert(mangled) {
            self.type_worklist.push_back((name, type_args));
        }
    }

    /// Build the surface-name → concrete-Ty substitution for cloning
    /// a generic fn at a specific instantiation. Pulls the fn's
    /// scheme to recover the parallel ordering of `generic_params`
    /// (declared names) and `scheme.type_vars` (allocated ids — used
    /// when resolving captured instantiations whose type-args still
    /// reference outer-fn vars).
    fn fn_subst(&self, name: &str, type_args: &[Ty]) -> Substitution {
        let f = self.fn_decls.get(name);
        let scheme = self.fn_schemes.get(name);
        let mut name_subst: BTreeMap<String, Ty> = BTreeMap::new();
        let mut var_subst: BTreeMap<u32, Ty> = BTreeMap::new();
        if let (Some(f), Some(scheme)) = (f, scheme) {
            for (i, gp) in f.generic_params.iter().enumerate() {
                if let Some(arg) = type_args.get(i) {
                    name_subst.insert(gp.name.clone(), arg.clone());
                }
            }
            for (i, var_id) in scheme.type_vars.iter().enumerate() {
                if let Some(arg) = type_args.get(i) {
                    var_subst.insert(*var_id, arg.clone());
                }
            }
        }
        Substitution {
            by_name: name_subst,
            by_var: var_subst,
        }
    }

    /// Build the surface-name substitution for cloning a generic
    /// type. Types' generic params don't have a global scheme entry,
    /// so we only build the by-name map; var ids on a type's
    /// generic params are not consumed downstream.
    fn type_subst(&self, name: &str, type_args: &[Ty]) -> Substitution {
        let td = self.type_decls.get(name);
        let mut name_subst: BTreeMap<String, Ty> = BTreeMap::new();
        if let Some(td) = td {
            for (i, gp) in td.generic_params.iter().enumerate() {
                if let Some(arg) = type_args.get(i) {
                    name_subst.insert(gp.name.clone(), arg.clone());
                }
            }
        }
        Substitution {
            by_name: name_subst,
            by_var: BTreeMap::new(),
        }
    }

    fn clone_fn(&mut self, key: &FnKey) -> FnDecl {
        let (name, type_args) = key;
        // Worklist invariant: every key was inserted via `enqueue_fn`,
        // and `enqueue_fn` only fires for names that already exist in
        // `self.fn_decls` (the call/value-ref rewriter checks
        // `self.fn_decls.contains_key` before enqueueing).
        let original = self.fn_decls.get(name).copied().unwrap_or_else(|| {
            unreachable!("monomorphize: fn worklist entry `{name}` missing from fn_decls")
        });
        let subst = self.fn_subst(name, type_args);
        let mangled_name = mangle_fn(name, type_args);

        // Plan B' Stage 6.8 Phase C++ — track the current clone's
        // fn name so the Lambda arm of `rewrite_expr` can record
        // resolved captures keyed by (mangled_name, lambda_span).
        let saved_clone_name = self.current_clone_fn_name.replace(mangled_name.clone());

        let new_params: Vec<Param> = original
            .params
            .iter()
            .map(|p| Param {
                name: p.name.clone(),
                ty: self.rewrite_type_expr(&p.ty, &subst),
                span: p.span.clone(),
            })
            .collect();
        let new_return = self.rewrite_type_expr(&original.return_type, &subst);
        let new_body = self.rewrite_block(&original.body, &subst);

        self.current_clone_fn_name = saved_clone_name;

        // Generic-params on the clone become empty — it's no longer
        // generic. Effect row variable is preserved per Plan B v1
        // (rows aren't monomorphized; codegen erases them).
        FnDecl {
            name: mangled_name,
            name_span: original.name_span.clone(),
            generic_params: Vec::new(),
            params: new_params,
            return_type: new_return,
            effects: original.effects.clone(),
            effect_row_var: original.effect_row_var.clone(),
            body: new_body,
            span: original.span.clone(),
        }
    }

    fn clone_type(&mut self, key: &TypeKey) -> TypeDecl {
        let (name, type_args) = key;
        // Worklist invariant: every key was inserted via `enqueue_type`,
        // and `enqueue_type` only fires for names already in
        // `self.type_decls` (the rewriter checks
        // `self.type_decls.contains_key` before enqueueing).
        let original = self.type_decls.get(name).copied().unwrap_or_else(|| {
            unreachable!("monomorphize: type worklist entry `{name}` missing from type_decls")
        });
        let subst = self.type_subst(name, type_args);
        let mangled_name = mangle_type(name, type_args);

        let new_variants: Vec<Variant> = original
            .variants
            .iter()
            .map(|v| {
                let new_fields = match &v.fields {
                    VariantFields::Unit => VariantFields::Unit,
                    VariantFields::Positional(ts) => VariantFields::Positional(
                        ts.iter()
                            .map(|t| self.rewrite_type_expr(t, &subst))
                            .collect(),
                    ),
                    VariantFields::Record(fs) => VariantFields::Record(
                        fs.iter()
                            .map(|f| RecordFieldDecl {
                                name: f.name.clone(),
                                ty: self.rewrite_type_expr(&f.ty, &subst),
                                span: f.span.clone(),
                            })
                            .collect(),
                    ),
                };
                let mangled_ctor = mangle_ctor(&v.name, type_args);
                Variant {
                    name: mangled_ctor,
                    name_span: v.name_span.clone(),
                    fields: new_fields,
                    span: v.span.clone(),
                }
            })
            .collect();

        TypeDecl {
            name: mangled_name,
            name_span: original.name_span.clone(),
            generic_params: Vec::new(),
            variants: new_variants,
            span: original.span.clone(),
        }
    }

    /// Rewrite a `TypeExpr` under the current substitution. Resolves
    /// `TypeExpr::Apply` to a `TypeExpr::Named(mangled_name)` after
    /// recursively rewriting its args; resolves
    /// `TypeExpr::Named(generic_param_name)` to the concrete
    /// substituted type's surface name. Concrete-type names (`Int`,
    /// registered user types) pass through.
    fn rewrite_type_expr(&mut self, t: &TypeExpr, subst: &Substitution) -> TypeExpr {
        match t {
            TypeExpr::Named(n, span) => {
                if let Some(concrete) = subst.by_name.get(n) {
                    return ty_to_type_expr(concrete, span);
                }
                // If `n` is a registered type (non-generic), keep as-is.
                // If `n` is unknown and not a generic-param substitution
                // target, leave it alone — typecheck would have rejected
                // this case via E0112 already.
                TypeExpr::Named(n.clone(), span.clone())
            }
            TypeExpr::Apply { name, args, span } => {
                let resolved_args: Vec<Ty> = args
                    .iter()
                    .map(|a| {
                        let te = self.rewrite_type_expr(a, subst);
                        type_expr_to_ty(&te)
                    })
                    .collect();
                // The application is over a generic type — clone it.
                if self.type_decls.contains_key(name) {
                    self.enqueue_type(name.clone(), resolved_args.clone());
                }
                let mangled = mangle_type(name, &resolved_args);
                // Plan D Task 117 (a) — record builtin generic
                // specializations for synthetic-TypeDecl injection at
                // top-level `monomorphize()`. User generic types get
                // clones via `enqueue_type` above; builtins
                // (Array/MutArray) have no user TypeDecl to clone, so
                // their mangled `Named("Array$$Int")` would have no
                // resolution target in `tc.types` post-mono. Tracking
                // here lets the wrapper inject a synthetic empty-
                // variants TypeDecl post-rebuild.
                if !self.type_decls.contains_key(name) {
                    self.builtin_specializations.insert(mangled.clone());
                }
                TypeExpr::Named(mangled, span.clone())
            }
            // Plan B' Stage 6.8 Task 102 — rewrite a fn-type by
            // recursively substituting its params + ret. Effects
            // and `effect_row_var` are surface text (no generic
            // substitution at type-name level — row polymorphism
            // handles row-vars at typecheck/monomorphize via the
            // existing `Tc` row machinery, not at this surface
            // rewrite). Phase B (Task 103) integrates `Ty::Fn` for
            // typechecking the result; this rewrite produces a
            // substituted surface that downstream Phase B sees.
            TypeExpr::Fn(fty) => TypeExpr::Fn(Box::new(crate::ast::FnTypeExpr {
                params: fty
                    .params
                    .iter()
                    .map(|p| self.rewrite_type_expr(p, subst))
                    .collect(),
                ret: self.rewrite_type_expr(&fty.ret, subst),
                effects: fty.effects.clone(),
                effect_row_var: fty.effect_row_var.clone(),
                span: fty.span.clone(),
            })),
            TypeExpr::Tuple { elems, span } => TypeExpr::Tuple {
                elems: elems
                    .iter()
                    .map(|e| self.rewrite_type_expr(e, subst))
                    .collect(),
                span: span.clone(),
            },
        }
    }

    fn rewrite_block(&mut self, b: &Block, subst: &Substitution) -> Block {
        Block {
            stmts: b
                .stmts
                .iter()
                .map(|s| self.rewrite_stmt(s, subst))
                .collect(),
            tail: b.tail.as_ref().map(|e| self.rewrite_expr(e, subst)),
            span: b.span.clone(),
        }
    }

    fn rewrite_stmt(&mut self, s: &Stmt, subst: &Substitution) -> Stmt {
        match s {
            Stmt::Let(l) => Stmt::Let(LetStmt {
                name: l.name.clone(),
                ty: self.rewrite_type_expr(&l.ty, subst),
                value: self.rewrite_expr(&l.value, subst),
                span: l.span.clone(),
            }),
            Stmt::Expr(e) => Stmt::Expr(self.rewrite_expr(e, subst)),
            Stmt::Perform(p) => Stmt::Perform(PerformExpr {
                effect: p.effect.clone(),
                op: p.op.clone(),
                args: p.args.iter().map(|a| self.rewrite_expr(a, subst)).collect(),
                span: p.span.clone(),
            }),
        }
    }

    fn rewrite_expr(&mut self, e: &Expr, subst: &Substitution) -> Expr {
        match e {
            Expr::IntLit(_, _)
            | Expr::StringLit(_, _)
            | Expr::BoolLit(_, _)
            | Expr::CharLit(_, _) => e.clone(),
            Expr::Ident(name, span) => {
                // Three name-resolution categories:
                // 1. Captured fn instantiation — rewrite to mangled
                //    name and enqueue the callee for cloning.
                // 2. Captured ctor instantiation (unit ctor) — same,
                //    but type-side; rewrite to the mangled ctor
                //    name and enqueue the type for cloning.
                // 3. Local binding — pass through unchanged.
                if let Some(inst) = self.call_sites.get(span) {
                    let resolved = subst.resolve_instantiation(inst);
                    if self.fn_decls.contains_key(&resolved.name) {
                        self.enqueue_fn(resolved.name.clone(), resolved.type_args.clone());
                        let mangled = mangle_fn(&resolved.name, &resolved.type_args);
                        return Expr::Ident(mangled, span.clone());
                    }
                }
                if let Some(inst) = self.ctor_sites.get(span) {
                    let resolved = subst.resolve_instantiation(inst);
                    if self.type_decls.contains_key(&resolved.name) {
                        self.enqueue_type(resolved.name.clone(), resolved.type_args.clone());
                        let mangled = mangle_ctor(name, &resolved.type_args);
                        return Expr::Ident(mangled, span.clone());
                    }
                }
                Expr::Ident(name.clone(), span.clone())
            }
            Expr::Call { callee, args, span } => {
                // Reviewer Comment 1 #1 closure: ctor positional-call
                // instantiations are recorded in `ctor_sites` keyed by
                // the *Call's* span, while fn-callee instantiations in
                // `call_sites` are keyed by the *Ident's* span. Both
                // lookups are performed explicitly here so a future
                // parser change to the Call.span boundary doesn't
                // silently break this path.
                //
                // Order: try ctor-site by Call span first (positional
                // ctor application like `Cons(1, Nil)`); if miss, fall
                // through to recursive `rewrite_expr` on the callee
                // which catches Ident-keyed fn calls and unit-ctor
                // bare-ident references.
                if let Expr::Ident(callee_name, _) = callee.as_ref() {
                    if let Some(inst) = self.ctor_sites.get(span) {
                        let resolved = subst.resolve_instantiation(inst);
                        if self.type_decls.contains_key(&resolved.name) {
                            self.enqueue_type(resolved.name.clone(), resolved.type_args.clone());
                            let mangled = mangle_ctor(callee_name, &resolved.type_args);
                            return Expr::Call {
                                callee: Box::new(Expr::Ident(mangled, callee.span())),
                                args: args.iter().map(|a| self.rewrite_expr(a, subst)).collect(),
                                span: span.clone(),
                            };
                        }
                    }
                }
                // Plan D Task 119b — record the post-substitution
                // call-callee Ty into the per-clone resolved map so
                // codegen's indirect-call signature builder can
                // recover the concrete return Ty for generic
                // clones. Mirrors `match_scrut_tys_resolved` /
                // `handle_body_ty_resolved`.
                let callee_ty_concrete: Option<Ty> =
                    self.call_callee_tys.get(span).map(|t| subst.apply_to_ty(t));
                if let (Some(fn_name), Some(callee_ty)) =
                    (&self.current_clone_fn_name, &callee_ty_concrete)
                {
                    self.call_callee_tys_resolved
                        .insert((fn_name.clone(), span.clone()), callee_ty.clone());
                }
                let new_callee = self.rewrite_expr(callee, subst);
                Expr::Call {
                    callee: Box::new(new_callee),
                    args: args.iter().map(|a| self.rewrite_expr(a, subst)).collect(),
                    span: span.clone(),
                }
            }
            Expr::Perform(p) => Expr::Perform(PerformExpr {
                effect: p.effect.clone(),
                op: p.op.clone(),
                args: p.args.iter().map(|a| self.rewrite_expr(a, subst)).collect(),
                span: p.span.clone(),
            }),
            Expr::Binary { op, lhs, rhs, span } => Expr::Binary {
                op: *op,
                lhs: Box::new(self.rewrite_expr(lhs, subst)),
                rhs: Box::new(self.rewrite_expr(rhs, subst)),
                span: span.clone(),
            },
            Expr::Unary { op, operand, span } => Expr::Unary {
                op: *op,
                operand: Box::new(self.rewrite_expr(operand, subst)),
                span: span.clone(),
            },
            Expr::If {
                cond,
                then_block,
                else_block,
                span,
            } => Expr::If {
                cond: Box::new(self.rewrite_expr(cond, subst)),
                then_block: Box::new(self.rewrite_block(then_block, subst)),
                else_block: Box::new(self.rewrite_block(else_block, subst)),
                span: span.clone(),
            },
            Expr::Match {
                scrutinee,
                arms,
                span,
            } => {
                // Resolve the scrutinee's type under the current
                // substitution so each arm's pattern-ctor rewrites
                // see concrete type-args (the per-match scrutinee_ty
                // entry might still reference outer-fn vars when
                // monomorph runs inside a generic clone).
                let scrut_ty_concrete: Option<Ty> =
                    self.match_scrut_tys.get(span).map(|t| subst.apply_to_ty(t));
                // Plan D Task 113 R1 finding 2 — record the post-
                // substitution scrutinee Ty into the per-clone
                // resolved map so codegen can recover the concrete
                // type at the same span without requiring scrutinee-
                // shape-specific fallbacks (`local_var_tys` only
                // worked for `Expr::Ident` scrutinees; non-Ident
                // scrutinees in generic fns would still leak
                // `Ty::Var(_)` into codegen). Spans are preserved
                // across cloning, so per-(clone_fn_name, span)
                // keying disambiguates between independent
                // instantiations of the same generic source fn.
                if let (Some(fn_name), Some(scrut_ty)) =
                    (&self.current_clone_fn_name, &scrut_ty_concrete)
                {
                    self.match_scrut_tys_resolved
                        .insert((fn_name.clone(), span.clone()), scrut_ty.clone());
                }
                Expr::Match {
                    scrutinee: Box::new(self.rewrite_expr(scrutinee, subst)),
                    arms: arms
                        .iter()
                        .map(|a| MatchArm {
                            pattern: self.rewrite_pattern(&a.pattern, &scrut_ty_concrete),
                            body: self.rewrite_expr(&a.body, subst),
                            span: a.span.clone(),
                        })
                        .collect(),
                    span: span.clone(),
                }
            }
            Expr::Block(b) => Expr::Block(Box::new(self.rewrite_block(b, subst))),
            Expr::Lambda {
                params,
                return_type,
                effects,
                effect_row_var,
                body,
                span,
            } => {
                // Plan B' Stage 6.8 Phase C++ — record per-clone-
                // resolved captures for this lambda. closure_convert
                // consumes via `lambda_captures_resolved[(fn_name,
                // span)]` so generic clones see substituted Tys
                // instead of the original `Ty::Var`s recorded by
                // typecheck.
                if let Some(fn_name) = &self.current_clone_fn_name {
                    if let Some((_, original_caps)) =
                        self.lambda_captures.iter().find(|(s, _)| s == span)
                    {
                        let resolved_caps: Vec<(String, Ty)> = original_caps
                            .iter()
                            .map(|(n, t)| (n.clone(), subst.apply_to_ty(t)))
                            .collect();
                        self.lambda_captures_resolved
                            .insert((fn_name.clone(), span.clone()), resolved_caps);
                    }
                }
                Expr::Lambda {
                    params: params
                        .iter()
                        .map(|p| Param {
                            name: p.name.clone(),
                            ty: self.rewrite_type_expr(&p.ty, subst),
                            span: p.span.clone(),
                        })
                        .collect(),
                    return_type: self.rewrite_type_expr(return_type, subst),
                    effects: effects.clone(),
                    effect_row_var: effect_row_var.clone(),
                    body: Box::new(self.rewrite_expr(body, subst)),
                    span: span.clone(),
                }
            }
            Expr::ClosureRecord {
                code_fn_name,
                env_exprs,
                env_slot_kinds,
                span,
            } => Expr::ClosureRecord {
                code_fn_name: code_fn_name.clone(),
                env_exprs: env_exprs
                    .iter()
                    .map(|e| self.rewrite_expr(e, subst))
                    .collect(),
                env_slot_kinds: env_slot_kinds.clone(),
                span: span.clone(),
            },
            Expr::ClosureEnvLoad {
                name,
                index,
                kind,
                span,
            } => Expr::ClosureEnvLoad {
                name: name.clone(),
                index: *index,
                kind: *kind,
                span: span.clone(),
            },
            Expr::RecordLit { name, fields, span } => {
                if let Some(inst) = self.ctor_sites.get(span) {
                    let resolved = subst.resolve_instantiation(inst);
                    if self.type_decls.contains_key(&resolved.name) {
                        self.enqueue_type(resolved.name.clone(), resolved.type_args.clone());
                        let mangled = mangle_ctor(name, &resolved.type_args);
                        return Expr::RecordLit {
                            name: mangled,
                            fields: fields
                                .iter()
                                .map(|f| RecordFieldLit {
                                    name: f.name.clone(),
                                    value: self.rewrite_expr(&f.value, subst),
                                    span: f.span.clone(),
                                })
                                .collect(),
                            span: span.clone(),
                        };
                    }
                }
                Expr::RecordLit {
                    name: name.clone(),
                    fields: fields
                        .iter()
                        .map(|f| RecordFieldLit {
                            name: f.name.clone(),
                            value: self.rewrite_expr(&f.value, subst),
                            span: f.span.clone(),
                        })
                        .collect(),
                    span: span.clone(),
                }
            }
            // Plan B task 53 — handler expressions pass through
            // monomorphize with substitution applied to their body
            // and arm bodies. The handler arms' op-parameter types
            // come from the matching effect decl (Task 54), not from
            // the AST shape, so this layer doesn't need to rewrite
            // type expressions inside the arms themselves.
            Expr::Handle {
                body,
                return_arm,
                op_arms,
                span,
            } => {
                // Plan D Task 119b — record the post-substitution
                // handle body type into the per-clone resolved map
                // so codegen's pre-pass can recover the concrete
                // body Ty at the same span without leaking
                // `Ty::Var(_)` from the pre-mono parent's
                // `handle_body_ty` side-table. The span-keyed
                // `CheckedProgram::handle_body_ty` is shared across
                // every clone of a generic fn (spans are preserved
                // by design); for generic clones the source entry
                // holds outer-fn `Ty::Var`s. Mirrors the
                // `match_scrut_tys_resolved` discipline introduced
                // by Plan D Task 113 R1 finding 2.
                let body_ty_concrete: Option<Ty> =
                    self.handle_body_ty.get(span).map(|t| subst.apply_to_ty(t));
                if let (Some(fn_name), Some(body_ty)) =
                    (&self.current_clone_fn_name, &body_ty_concrete)
                {
                    self.handle_body_ty_resolved
                        .insert((fn_name.clone(), span.clone()), body_ty.clone());
                }
                let new_body = self.rewrite_expr(body, subst);
                let new_return_arm = return_arm.as_ref().map(|ra| {
                    Box::new(crate::ast::HandleReturnArm {
                        binding: ra.binding.clone(),
                        binding_span: ra.binding_span.clone(),
                        body: self.rewrite_expr(&ra.body, subst),
                        span: ra.span.clone(),
                    })
                });
                let new_op_arms = op_arms
                    .iter()
                    .map(|arm| crate::ast::HandleOpArm {
                        effect: arm.effect.clone(),
                        effect_span: arm.effect_span.clone(),
                        op: arm.op.clone(),
                        op_span: arm.op_span.clone(),
                        params: arm.params.clone(),
                        k_name: arm.k_name.clone(),
                        k_span: arm.k_span.clone(),
                        body: self.rewrite_expr(&arm.body, subst),
                        span: arm.span.clone(),
                    })
                    .collect();
                Expr::Handle {
                    body: Box::new(new_body),
                    return_arm: new_return_arm,
                    op_arms: new_op_arms,
                    span: span.clone(),
                }
            }
            Expr::Tuple { elems, span } => Expr::Tuple {
                elems: elems.iter().map(|e| self.rewrite_expr(e, subst)).collect(),
                span: span.clone(),
            },
        }
    }

    fn rewrite_pattern(&mut self, p: &Pattern, scrut_ty: &Option<Ty>) -> Pattern {
        match p {
            Pattern::IntLit(_, _) | Pattern::BoolLit(_, _) | Pattern::CharLit(_, _) => p.clone(),
            Pattern::Wildcard(_) => p.clone(),
            Pattern::Var(name, span) => {
                // Plan A3 task 38.3 nullary-ctor promotion: a bare
                // identifier in pattern position whose name matches a
                // Unit variant of the scrutinee's user type is a
                // nullary ctor, not a fresh binding. Codegen's
                // `nullary_ctor_promotion` does this dispatch from
                // ctor name + scrut_ty; for monomorphization we need
                // the post-mono ctor name (`Nada$$Int`, not `Nada`)
                // to flow into the AST so codegen's `ctor_index`
                // lookup succeeds. Rewrite to a `Pattern::Ctor` with
                // mangled name when both the ctor exists in the
                // ctor-to-type registry and the scrutinee type
                // matches.
                if let (Some(owning_type), Some(Ty::User(scrut_name, args))) =
                    (self.ctor_to_type.get(name), scrut_ty)
                {
                    if owning_type == scrut_name {
                        // Verify it really is a Unit variant of that
                        // type before promoting.
                        let is_unit_variant = self
                            .type_decls
                            .get(owning_type.as_str())
                            .map(|td| {
                                td.variants.iter().any(|v| {
                                    v.name == *name && matches!(v.fields, VariantFields::Unit)
                                })
                            })
                            .unwrap_or(false);
                        if is_unit_variant {
                            let mangled = if args.is_empty() {
                                name.clone()
                            } else {
                                mangle_ctor(name, args)
                            };
                            if !args.is_empty() {
                                self.enqueue_type(owning_type.clone(), args.clone());
                            }
                            return Pattern::Ctor {
                                name: mangled,
                                fields: CtorPatternFields::Unit,
                                span: span.clone(),
                            };
                        }
                    }
                }
                p.clone()
            }
            Pattern::Tuple(ps, span) => Pattern::Tuple(
                ps.iter()
                    .map(|sp| self.rewrite_pattern(sp, scrut_ty))
                    .collect(),
                span.clone(),
            ),
            Pattern::Ctor { name, fields, span } => {
                // Resolve the ctor's owning type and determine the
                // type-arg tuple to mangle the ctor name with.
                //
                // Common case: the ctor belongs to the scrutinee's
                // outermost User type. The scrutinee's type-args
                // pin the instantiation directly.
                //
                // Nested case (e.g. `match opt: Option[List[Int]] {
                //   Some(Cons(h, t)) => ... }`): the inner pattern
                // `Cons(h, t)` belongs to `List`, not `Option`. The
                // recursive descent into the variant's field types
                // (below, in the field-type computation for
                // sub-patterns) propagates the *inner* scrutinee
                // type so this branch sees `Ty::User("List", [Int])`
                // when rewriting `Cons(h, t)`.
                //
                // Reviewer round-3 (Comment 4318208... regression
                // closure): an earlier version of this arm
                // `unreachable!`d when the ctor's owning type didn't
                // match the scrutinee's User type, on the false
                // assumption that v1 surface couldn't construct the
                // case. Nested generic ctor patterns construct it
                // routinely; the per-sub-pattern field-type
                // threading below is the correct mechanism.
                let owning_type = self.ctor_to_type.get(name).cloned();
                let type_args: Vec<Ty> = match (owning_type.as_deref(), scrut_ty) {
                    (Some(t), Some(Ty::User(scrut_name, args))) if t == scrut_name => args.clone(),
                    _ => Vec::new(),
                };
                let new_name = if type_args.is_empty() {
                    name.clone()
                } else {
                    mangle_ctor(name, &type_args)
                };
                if !type_args.is_empty() {
                    if let Some(t) = owning_type.as_deref() {
                        if self.type_decls.contains_key(t) {
                            self.enqueue_type(t.to_string(), type_args.clone());
                        }
                    }
                }
                // Compute the per-sub-pattern field Ty under the
                // owning type's generic-param substitution. For a
                // sub-pattern at field index `i`, the scrut_ty
                // becomes the field's declared `TypeExpr` resolved
                // under (owning_type.generic_params -> type_args).
                let field_tys = self.variant_field_types(owning_type.as_deref(), name, &type_args);
                let new_fields = match fields {
                    CtorPatternFields::Unit => CtorPatternFields::Unit,
                    CtorPatternFields::Positional(ps) => CtorPatternFields::Positional(
                        ps.iter()
                            .enumerate()
                            .map(|(i, sp)| {
                                let inner_scrut =
                                    field_tys.as_ref().and_then(|fts| fts.get_positional(i));
                                self.rewrite_pattern(sp, &inner_scrut)
                            })
                            .collect(),
                    ),
                    CtorPatternFields::Record(fs) => CtorPatternFields::Record(
                        fs.iter()
                            .map(|f| {
                                let inner_scrut =
                                    field_tys.as_ref().and_then(|fts| fts.get_record(&f.name));
                                CtorPatternField {
                                    name: f.name.clone(),
                                    pattern: self.rewrite_pattern(&f.pattern, &inner_scrut),
                                    span: f.span.clone(),
                                }
                            })
                            .collect(),
                    ),
                };
                Pattern::Ctor {
                    name: new_name,
                    fields: new_fields,
                    span: span.clone(),
                }
            }
        }
    }

    /// Resolve a constructor's field types under the owning type's
    /// per-pattern instantiation, so nested patterns can be rewritten
    /// against the right inner-scrutinee type.
    ///
    /// Returns `None` when the ctor's owning type can't be resolved
    /// (foreign ctor, malformed input — defensive for the rewrite
    /// pass even though typecheck would have caught these earlier).
    fn variant_field_types(
        &self,
        owning_type: Option<&str>,
        ctor_name: &str,
        type_args: &[Ty],
    ) -> Option<VariantFieldTys> {
        let type_name = owning_type?;
        let td = self.type_decls.get(type_name)?;
        let variant = td.variants.iter().find(|v| v.name == ctor_name)?;
        // Build the surface-name → Ty substitution from the type's
        // declared generic_params and the per-instantiation type_args.
        // Empty type_args (non-generic owning type) yields an empty
        // subst; field types resolve as-is.
        let mut subst: BTreeMap<String, Ty> = BTreeMap::new();
        for (i, gp) in td.generic_params.iter().enumerate() {
            if let Some(arg) = type_args.get(i) {
                subst.insert(gp.name.clone(), arg.clone());
            }
        }
        let resolve = |te: &TypeExpr| ty_from_type_expr_under_subst(te, &subst);
        match &variant.fields {
            VariantFields::Unit => Some(VariantFieldTys::Unit),
            VariantFields::Positional(ts) => Some(VariantFieldTys::Positional(
                ts.iter().map(resolve).collect(),
            )),
            VariantFields::Record(fs) => Some(VariantFieldTys::Record(
                fs.iter()
                    .map(|f| (f.name.clone(), resolve(&f.ty)))
                    .collect(),
            )),
        }
    }
}

/// Resolved per-pattern field types for a single ctor under one
/// per-instantiation substitution. Mirrors `VariantFields` shape but
/// carries `Ty` (resolved) rather than `TypeExpr` (unresolved).
#[derive(Clone, Debug)]
enum VariantFieldTys {
    Unit,
    Positional(Vec<Ty>),
    Record(Vec<(String, Ty)>),
}

impl VariantFieldTys {
    fn get_positional(&self, i: usize) -> Option<Ty> {
        match self {
            VariantFieldTys::Positional(ts) => ts.get(i).cloned(),
            _ => None,
        }
    }

    fn get_record(&self, name: &str) -> Option<Ty> {
        match self {
            VariantFieldTys::Record(fs) => {
                fs.iter().find(|(n, _)| n == name).map(|(_, t)| t.clone())
            }
            _ => None,
        }
    }
}

/// Resolve a `TypeExpr` to a concrete `Ty` under a surface-name →
/// `Ty` substitution. Mirrors `typecheck::ty_from_type_expr` but
/// without typecheck's error-collecting context — we know the input
/// is well-formed because typecheck already accepted it.
fn ty_from_type_expr_under_subst(te: &TypeExpr, subst: &BTreeMap<String, Ty>) -> Ty {
    match te {
        TypeExpr::Named(name, _) => match name.as_str() {
            "Int" => Ty::Int,
            "String" => Ty::String,
            "Bool" => Ty::Bool,
            "Char" => Ty::Char,
            "Byte" => Ty::Byte,
            "Unit" => Ty::Unit,
            other => {
                if let Some(resolved) = subst.get(other) {
                    resolved.clone()
                } else {
                    Ty::User(other.to_string(), Vec::new())
                }
            }
        },
        TypeExpr::Apply { name, args, .. } => Ty::User(
            name.clone(),
            args.iter()
                .map(|a| ty_from_type_expr_under_subst(a, subst))
                .collect(),
        ),
        // Plan B' Stage 6.8 Task 103 — TypeExpr::Fn → Ty::Fn under
        // monomorphize's substitution. Closed rows only in v1
        // (typecheck E0137 rejects row-variable-bearing fn-types),
        // so `effect_row_var` is always None here.
        TypeExpr::Fn(fty) => {
            let params = fty
                .params
                .iter()
                .map(|p| ty_from_type_expr_under_subst(p, subst))
                .collect();
            let ret = ty_from_type_expr_under_subst(&fty.ret, subst);
            // Plan D Task 114 — substitute row-arg type vars
            // through the active mono substitution.
            let effects: Vec<crate::typecheck::EffectInst> = fty
                .effects
                .iter()
                .map(|r| {
                    let args = r
                        .args
                        .iter()
                        .map(|t| ty_from_type_expr_under_subst(t, subst))
                        .collect();
                    crate::typecheck::EffectInst {
                        name: r.name.clone(),
                        args,
                    }
                })
                .collect();
            Ty::Fn(Box::new(crate::typecheck::FnSig {
                params,
                ret,
                effects,
                effect_row_var: None,
            }))
        }
        TypeExpr::Tuple { elems, .. } => Ty::Tuple(
            elems
                .iter()
                .map(|e| ty_from_type_expr_under_subst(e, subst))
                .collect(),
        ),
    }
}

// ---------------------------------------------------------------- Substitution

/// The active surface-name and var-id substitution used while cloning
/// a single fn or type instantiation. `by_name` maps generic-parameter
/// surface names (`A`, `B`) to the concrete `Ty`; `by_var` maps the
/// fresh-var ids the typechecker allocated for those names to the
/// same concrete types — used to resolve captured instantiations
/// whose type-args contain `Ty::Var(_)` references to outer-fn vars.
#[derive(Clone, Debug, Default)]
struct Substitution {
    by_name: BTreeMap<String, Ty>,
    by_var: BTreeMap<u32, Ty>,
}

impl Substitution {
    /// Apply this substitution to a `Ty`, recursively resolving
    /// `Ty::Var(id)` references through `by_var`.
    fn apply_to_ty(&self, ty: &Ty) -> Ty {
        match ty {
            Ty::Int | Ty::String | Ty::Bool | Ty::Char | Ty::Byte | Ty::Unit => ty.clone(),
            Ty::Var(id) => match self.by_var.get(id) {
                Some(resolved) => self.apply_to_ty(resolved),
                None => ty.clone(),
            },
            Ty::User(name, args) => Ty::User(
                name.clone(),
                args.iter().map(|a| self.apply_to_ty(a)).collect(),
            ),
            Ty::Tuple(elems) => Ty::Tuple(elems.iter().map(|e| self.apply_to_ty(e)).collect()),
            Ty::Fn(sig) => Ty::Fn(Box::new(crate::typecheck::FnSig {
                params: sig.params.iter().map(|p| self.apply_to_ty(p)).collect(),
                ret: self.apply_to_ty(&sig.ret),
                effects: sig.effects.clone(),
                // `effect_row_var` is intentionally copied unchanged
                // — Plan B v1's "Effect rows are not monomorphized"
                // deviation (PLAN_B_DEVIATIONS.md Deviation #2):
                // rows pass through this pass and are erased at
                // codegen, not specialised. This arm is reachable
                // only via `Ty::Fn` which itself currently can't
                // appear at any mono use site (`canon_ty`'s Ty::Fn
                // arm is `unreachable!`); kept here for forward
                // structural correctness when Plan C+ adds the
                // surface for first-class function values.
                effect_row_var: sig.effect_row_var,
            })),
            // Plan D Task 119b — lift Continuation values through
            // monomorphization. The op_ret and ret types substitute
            // recursively; the scope_id is a static handler-location
            // identifier (not a type-level binding) and stays
            // unchanged across instantiations. Runtime ScopeId
            // enforcement (RELINK_STACK + dynamic checks) is the
            // load-bearing soundness path for actual escapes.
            //
            // R1 tripwire: the standing invariant is that mono never
            // sees `ScopeId::Var` (typecheck-side `apply_ty_inner`
            // already enforces this). If a future region-polymorphic
            // scheme ever wires `Var` producers without simultaneously
            // wiring `Subst`-of-`ScopeId`, this arm would silently
            // keep stale `Var` ids; the debug_assert catches that.
            Ty::Continuation(c) => {
                debug_assert!(
                    matches!(c.scope_id, crate::typecheck::ScopeId::Concrete(_)),
                    "monomorphize::apply_to_ty: Ty::Continuation carries a \
                     non-Concrete ScopeId ({:?}) — typecheck must have \
                     resolved it before reaching mono",
                    c.scope_id,
                );
                Ty::Continuation(Box::new(crate::typecheck::ContinuationTy {
                    op_ret: self.apply_to_ty(&c.op_ret),
                    ret: self.apply_to_ty(&c.ret),
                    scope_id: c.scope_id.clone(),
                }))
            }
        }
    }

    /// Resolve a captured instantiation by applying the current
    /// substitution to its type-args. Used at clone-time when
    /// descending into a generic body whose inner instantiation's
    /// type-args still reference the *outer* fn's generic vars.
    fn resolve_instantiation(&self, inst: &GenericInstantiation) -> GenericInstantiation {
        GenericInstantiation {
            name: inst.name.clone(),
            type_args: inst.type_args.iter().map(|a| self.apply_to_ty(a)).collect(),
        }
    }
}

// ---------------------------------------------------------------- Ty ↔ TypeExpr

/// Render a `Ty` back into a `TypeExpr` so cloned signatures and
/// let-bindings carry concrete surface-level type annotations rather
/// than `TypeExpr::Apply` / generic-param `TypeExpr::Named` nodes.
/// For generic user-type instantiations, renders as `Named(mangled,
/// span)` — the same name the type-cloning pass produces, so codegen
/// looks up the concrete `TypeDecl` directly.
pub(crate) fn ty_to_type_expr(ty: &Ty, span: &Span) -> TypeExpr {
    match ty {
        Ty::Int => TypeExpr::Named("Int".to_string(), span.clone()),
        Ty::String => TypeExpr::Named("String".to_string(), span.clone()),
        Ty::Bool => TypeExpr::Named("Bool".to_string(), span.clone()),
        Ty::Char => TypeExpr::Named("Char".to_string(), span.clone()),
        Ty::Byte => TypeExpr::Named("Byte".to_string(), span.clone()),
        Ty::Unit => TypeExpr::Named("Unit".to_string(), span.clone()),
        Ty::User(name, args) => {
            let mangled = mangle_type(name, args);
            TypeExpr::Named(mangled, span.clone())
        }
        Ty::Var(id) => {
            // Symmetric with `canon_ty`'s `Ty::Var` arm — if a
            // var escaped substitution, an upstream invariant broke.
            // E0132 should have rejected the call site at typecheck.
            unreachable!(
                "monomorphize::ty_to_type_expr: Ty::Var({id}) reached \
                 TypeExpr rendering — typecheck E0132 should have rejected \
                 this site"
            )
        }
        Ty::Fn(sig) => {
            // Plan B' Stage 6.8 Task 103 — render Ty::Fn back into
            // TypeExpr::Fn so a generic-parameter substitution that
            // binds `A` to a fn-typed concrete still produces a
            // valid surface. Closed rows only in v1 (typecheck
            // E0137 rejects row-var-bearing fn-types upstream).
            let params = sig
                .params
                .iter()
                .map(|p| ty_to_type_expr(p, span))
                .collect();
            let ret = ty_to_type_expr(&sig.ret, span);
            TypeExpr::Fn(Box::new(crate::ast::FnTypeExpr {
                params,
                ret,
                effects: crate::typecheck::insts_to_effect_refs(&sig.effects, span),
                effect_row_var: None,
                span: span.clone(),
            }))
        }
        Ty::Tuple(elems) => TypeExpr::Tuple {
            elems: elems.iter().map(|e| ty_to_type_expr(e, span)).collect(),
            span: span.clone(),
        },
        Ty::Continuation(_) => {
            // Plan D Task 117 — Continuation has no surface
            // TypeExpr. PR #60 review #3 closed the bind_ty_var
            // bypass at check_call (typecheck.rs:4403-4445) via
            // a precision check that fires E0145 when an arg of
            // type Ty::Continuation would unify against an
            // unresolved generic-param Var. With that gate in
            // place, mono never sees Continuation, so this
            // unreachable!() is now actually unreachable on user
            // code. Walker consistency: matches canon_ty +
            // apply_to_ty above and the Var unreachable!()s in
            // typecheck.rs walkers.
            unreachable!(
                "monomorphize::ty_to_type_expr: Ty::Continuation has no surface TypeExpr — \
                 typecheck E0145 (escape barrier broad arm + check_call generic-\
                 instantiation precision check) should have rejected the cross-fn / \
                 cross-storage / generic-instantiation site upstream"
            )
        }
    }
}

/// Convert a fully-rewritten `TypeExpr` (no `Apply`, no generic-param
/// references, no `Var`) into a `Ty`. Used when resolving the args
/// of a `TypeExpr::Apply` recursively.
pub(crate) fn type_expr_to_ty(te: &TypeExpr) -> Ty {
    match te {
        TypeExpr::Named(n, _) => match n.as_str() {
            "Int" => Ty::Int,
            "String" => Ty::String,
            "Bool" => Ty::Bool,
            "Char" => Ty::Char,
            "Byte" => Ty::Byte,
            "Unit" => Ty::Unit,
            other => Ty::User(other.to_string(), Vec::new()),
        },
        TypeExpr::Apply { name, args, .. } => {
            Ty::User(name.clone(), args.iter().map(type_expr_to_ty).collect())
        }
        // Plan B' Stage 6.8 Task 103 — TypeExpr::Fn → Ty::Fn for
        // already-rewritten (no Apply, no generic-param refs)
        // surfaces. Closed rows only (typecheck E0137 enforces).
        TypeExpr::Fn(fty) => {
            let params = fty.params.iter().map(type_expr_to_ty).collect();
            let ret = type_expr_to_ty(&fty.ret);
            // Plan D Task 114 — type_expr_to_ty runs *after* monomorphize
            // has substituted all generic params; rewrite_type_expr's
            // outputs feed in here so effect-row args are already
            // concrete (no Ty::Var residue). Build EffectInst directly
            // from the AST's args via type_expr_to_ty.
            let effects: Vec<crate::typecheck::EffectInst> = fty
                .effects
                .iter()
                .map(|r| crate::typecheck::EffectInst {
                    name: r.name.clone(),
                    args: r.args.iter().map(type_expr_to_ty).collect(),
                })
                .collect();
            Ty::Fn(Box::new(crate::typecheck::FnSig {
                params,
                ret,
                effects,
                effect_row_var: None,
            }))
        }
        TypeExpr::Tuple { elems, .. } => Ty::Tuple(elems.iter().map(type_expr_to_ty).collect()),
    }
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;

    #[test]
    fn canon_ty_renders_primitives() {
        assert_eq!(canon_ty(&Ty::Int), "Int");
        assert_eq!(canon_ty(&Ty::String), "String");
        assert_eq!(canon_ty(&Ty::Bool), "Bool");
        assert_eq!(canon_ty(&Ty::Char), "Char");
        assert_eq!(canon_ty(&Ty::Byte), "Byte");
        assert_eq!(canon_ty(&Ty::Unit), "Unit");
    }

    #[test]
    fn canon_ty_renders_non_generic_user_type() {
        let t = Ty::User("Option".to_string(), Vec::new());
        assert_eq!(canon_ty(&t), "Option");
    }

    #[test]
    fn canon_ty_renders_one_arg_user_type() {
        let t = Ty::User("Option".to_string(), vec![Ty::Int]);
        assert_eq!(canon_ty(&t), "Option$Int");
    }

    #[test]
    fn canon_ty_renders_two_arg_user_type() {
        let t = Ty::User("Map".to_string(), vec![Ty::String, Ty::Int]);
        assert_eq!(canon_ty(&t), "Map$String$Int");
    }

    #[test]
    fn canon_ty_renders_nested_user_type() {
        let inner = Ty::User("Option".to_string(), vec![Ty::Int]);
        let outer = Ty::User("List".to_string(), vec![inner]);
        assert_eq!(canon_ty(&outer), "List$Option$Int");
    }

    #[test]
    fn canon_ty_disambiguates_underscore_named_user_types() {
        // `type List_Option[A]` (legal user identifier with single
        // underscore) instantiated at Int versus `List[Option[Int]]`
        // (nested generic application). The `$` separator
        // structurally distinguishes them — at the AST level, where
        // uniqueness is enforced. Reviewer Comment 2 #1 closure.
        let underscored = Ty::User("List_Option".to_string(), vec![Ty::Int]);
        let nested = Ty::User(
            "List".to_string(),
            vec![Ty::User("Option".to_string(), vec![Ty::Int])],
        );
        assert_eq!(canon_ty(&underscored), "List_Option$Int");
        assert_eq!(canon_ty(&nested), "List$Option$Int");
        assert_ne!(canon_ty(&underscored), canon_ty(&nested));
    }

    // ----------------------------------------------------------------
    // Plan B' Stage 6.8 Task 103 R1 fixup 1 — canon_ty for Ty::Fn.
    // Pin the mangling format so a generic helper instantiated with a
    // fn-typed type-arg gets a stable, distinct symbol rather than
    // tripping the prior `unreachable!`.
    // ----------------------------------------------------------------

    #[test]
    fn canon_ty_renders_zero_param_fn_type() {
        let t = Ty::Fn(Box::new(crate::typecheck::FnSig {
            params: vec![],
            ret: Ty::Int,
            effects: vec![],
            effect_row_var: None,
        }));
        assert_eq!(canon_ty(&t), "Fn$Ret$Int");
    }

    #[test]
    fn canon_ty_renders_one_param_fn_type_no_effects() {
        let t = Ty::Fn(Box::new(crate::typecheck::FnSig {
            params: vec![Ty::Int],
            ret: Ty::String,
            effects: vec![],
            effect_row_var: None,
        }));
        assert_eq!(canon_ty(&t), "Fn$Int$Ret$String");
    }

    #[test]
    fn canon_ty_renders_two_param_fn_type_with_effects() {
        let t = Ty::Fn(Box::new(crate::typecheck::FnSig {
            params: vec![Ty::Int, Ty::Bool],
            ret: Ty::Unit,
            effects: vec![
                crate::typecheck::EffectInst::bare("IO"),
                crate::typecheck::EffectInst::bare("Choose"),
            ],
            effect_row_var: None,
        }));
        // Effects sort: Choose < IO.
        assert_eq!(canon_ty(&t), "Fn$Int$Bool$Ret$Unit$Eff$Choose$IO");
    }

    #[test]
    fn canon_ty_fn_effect_order_is_canonical() {
        // Two structurally identical fn-types with different effect
        // declaration order canonicalise to the same mangle. Effect
        // rows are semantically sets; mangling sorts.
        let a = Ty::Fn(Box::new(crate::typecheck::FnSig {
            params: vec![Ty::Int],
            ret: Ty::Int,
            effects: vec![
                crate::typecheck::EffectInst::bare("IO"),
                crate::typecheck::EffectInst::bare("Choose"),
            ],
            effect_row_var: None,
        }));
        let b = Ty::Fn(Box::new(crate::typecheck::FnSig {
            params: vec![Ty::Int],
            ret: Ty::Int,
            effects: vec![
                crate::typecheck::EffectInst::bare("Choose"),
                crate::typecheck::EffectInst::bare("IO"),
            ],
            effect_row_var: None,
        }));
        assert_eq!(canon_ty(&a), canon_ty(&b));
    }

    #[test]
    fn canon_ty_fn_nested_fn_param_renders_recursively() {
        // `((Int) -> Int ![]) -> Int ![]` — a fn-returning-fn.
        let inner = Ty::Fn(Box::new(crate::typecheck::FnSig {
            params: vec![Ty::Int],
            ret: Ty::Int,
            effects: vec![],
            effect_row_var: None,
        }));
        let outer = Ty::Fn(Box::new(crate::typecheck::FnSig {
            params: vec![inner],
            ret: Ty::Int,
            effects: vec![],
            effect_row_var: None,
        }));
        assert_eq!(canon_ty(&outer), "Fn$Fn$Int$Ret$Int$Ret$Int");
    }

    #[test]
    fn mangle_fn_keeps_non_generic_names() {
        assert_eq!(mangle_fn("main", &[]), "main");
        assert_eq!(mangle_fn("fib", &[]), "fib");
    }

    #[test]
    fn mangle_fn_appends_double_dollar_per_arg() {
        let args = vec![Ty::Int, Ty::String];
        assert_eq!(mangle_fn("identity", &args), "identity$$Int$$String");
    }

    #[test]
    fn mangle_fn_handles_nested_generics() {
        let inner = Ty::User("Option".to_string(), vec![Ty::Int]);
        let outer = Ty::User("List".to_string(), vec![inner]);
        let args = vec![outer.clone(), outer];
        assert_eq!(
            mangle_fn("list_map", &args),
            "list_map$$List$Option$Int$$List$Option$Int"
        );
    }

    #[test]
    fn mangle_type_keeps_non_generic_names() {
        assert_eq!(mangle_type("Option", &[]), "Option");
    }

    #[test]
    fn mangle_type_appends_args() {
        let args = vec![Ty::Int];
        assert_eq!(mangle_type("Option", &args), "Option$$Int");
    }

    #[test]
    fn mangle_ctor_unchanged_for_non_generic_type() {
        assert_eq!(mangle_ctor("Some", &[]), "Some");
    }

    #[test]
    fn mangle_ctor_appends_type_args() {
        let args = vec![Ty::Int];
        assert_eq!(mangle_ctor("Some", &args), "Some$$Int");
    }

    #[test]
    fn ty_to_type_expr_round_trips_simple() {
        let span = Span::synthetic("t");
        let t = Ty::Int;
        let te = ty_to_type_expr(&t, &span);
        let back = type_expr_to_ty(&te);
        assert_eq!(back, t);
    }

    #[test]
    fn ty_to_type_expr_renders_generic_user_as_mangled_named() {
        let span = Span::synthetic("t");
        let t = Ty::User("Option".to_string(), vec![Ty::Int]);
        let te = ty_to_type_expr(&t, &span);
        match te {
            TypeExpr::Named(name, _) => assert_eq!(name, "Option$$Int"),
            _ => panic!("expected Named, got {te:?}"),
        }
    }

    #[test]
    fn substitution_resolves_var_through_by_var() {
        let mut s = Substitution::default();
        s.by_var.insert(7, Ty::Int);
        let resolved = s.apply_to_ty(&Ty::Var(7));
        assert_eq!(resolved, Ty::Int);
    }

    #[test]
    fn substitution_passes_through_unbound_var() {
        let s = Substitution::default();
        let resolved = s.apply_to_ty(&Ty::Var(3));
        assert_eq!(resolved, Ty::Var(3));
    }

    #[test]
    fn substitution_resolves_user_args_recursively() {
        let mut s = Substitution::default();
        s.by_var.insert(0, Ty::Int);
        let t = Ty::User("Option".to_string(), vec![Ty::Var(0)]);
        let resolved = s.apply_to_ty(&t);
        assert_eq!(resolved, Ty::User("Option".to_string(), vec![Ty::Int]));
    }

    #[test]
    fn type_expr_to_ty_handles_nested_apply() {
        let span = Span::synthetic("t");
        let te = TypeExpr::Apply {
            name: "Option".to_string(),
            args: vec![TypeExpr::Apply {
                name: "List".to_string(),
                args: vec![TypeExpr::Named("Int".to_string(), span.clone())],
                span: span.clone(),
            }],
            span: span.clone(),
        };
        let ty = type_expr_to_ty(&te);
        assert_eq!(
            ty,
            Ty::User(
                "Option".to_string(),
                vec![Ty::User("List".to_string(), vec![Ty::Int])],
            )
        );
    }

    // ===== End-to-end pipeline tests =====
    //
    // These tests run a sigil source string through lex / parse /
    // resolve / typecheck / elaborate / monomorphize and inspect the
    // post-pass IR. They pin the externally-observable contract:
    // generic decls disappear, concrete clones with mangled names
    // appear, and `Apply` / generic-param refs are absent.

    /// Run the front-end through monomorph. Returns the post-mono
    /// items list. Panics on typecheck error — tests should only
    /// supply valid programs.
    fn run_pipeline_to_mono(src: &str) -> Vec<Item> {
        use crate::elaborate;
        use crate::lexer;
        use crate::parser;
        use crate::resolve;
        use crate::typecheck;

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
        mono.anf.checked.program.items
    }

    /// Find the FnDecl with the given name in an items list.
    fn find_fn<'a>(items: &'a [Item], name: &str) -> Option<&'a FnDecl> {
        items.iter().find_map(|i| match i {
            Item::Fn(f) if f.name == name => Some(f.as_ref()),
            _ => None,
        })
    }

    /// Find the TypeDecl with the given name in an items list.
    fn find_type<'a>(items: &'a [Item], name: &str) -> Option<&'a TypeDecl> {
        items.iter().find_map(|i| match i {
            Item::Type(t) if t.name == name => Some(t.as_ref()),
            _ => None,
        })
    }

    #[test]
    fn non_generic_program_passes_through_unchanged() {
        let src = r#"
            fn main() -> Int ![] {
                42
            }
        "#;
        let items = run_pipeline_to_mono(src);
        // Non-generic programs hit the early-return: items list is
        // identical to the post-elaborate items (one fn, no clones).
        let main = find_fn(&items, "main").expect("main present");
        assert_eq!(main.generic_params.len(), 0);
    }

    #[test]
    fn generic_fn_called_at_int_produces_int_clone() {
        let src = r#"
            fn id[A](x: A) -> A ![] {
                x
            }
            fn main() -> Int ![] {
                id(42)
            }
        "#;
        let items = run_pipeline_to_mono(src);
        // Original generic decl is dropped.
        assert!(
            find_fn(&items, "id").is_none(),
            "generic `id` should be dropped from post-mono IR"
        );
        // Mangled clone exists.
        let id_int = find_fn(&items, "id$$Int").expect("id$$Int clone present");
        assert_eq!(
            id_int.generic_params.len(),
            0,
            "clone has no generic_params"
        );
        // Param type is the substituted concrete `Int`.
        match &id_int.params[0].ty {
            TypeExpr::Named(n, _) => assert_eq!(n, "Int"),
            other => panic!("expected Named(Int), got {other:?}"),
        }
        // Return type is concrete `Int`.
        match &id_int.return_type {
            TypeExpr::Named(n, _) => assert_eq!(n, "Int"),
            other => panic!("expected Named(Int), got {other:?}"),
        }
        // Main is preserved (BFS order from main).
        assert!(find_fn(&items, "main").is_some());
    }

    #[test]
    fn generic_fn_two_instantiations_clones_twice() {
        let src = r#"
            fn id[A](x: A) -> A ![] {
                x
            }
            fn main() -> Int ![] {
                let a: Int = id(42);
                let b: String = id("hello");
                a
            }
        "#;
        let items = run_pipeline_to_mono(src);
        assert!(find_fn(&items, "id").is_none());
        assert!(find_fn(&items, "id$$Int").is_some());
        assert!(find_fn(&items, "id$$String").is_some());
    }

    #[test]
    fn generic_fn_unreachable_from_main_is_dropped() {
        let src = r#"
            fn id[A](x: A) -> A ![] {
                x
            }
            fn main() -> Int ![] {
                42
            }
        "#;
        let items = run_pipeline_to_mono(src);
        // `id` is generic and never called from main → dropped
        // entirely. No clones produced because no use sites.
        assert!(find_fn(&items, "id").is_none());
        assert!(find_fn(&items, "id$$Int").is_none());
        let fn_count = items.iter().filter(|i| matches!(i, Item::Fn(_))).count();
        assert_eq!(fn_count, 1, "only main remains");
    }

    #[test]
    fn generic_type_with_unit_ctor_clones_per_instantiation() {
        let src = r#"
            type Holder[A] = | Empty | Hold(A)
            fn main() -> Int ![] {
                let h: Holder[Int] = Empty;
                42
            }
        "#;
        let items = run_pipeline_to_mono(src);
        // Generic type is dropped; concrete clone is present.
        assert!(find_type(&items, "Holder").is_none());
        let holder_int = find_type(&items, "Holder$$Int").expect("Holder$$Int present");
        assert_eq!(holder_int.generic_params.len(), 0);
        // Ctor names in the clone are mangled.
        let ctor_names: Vec<&str> = holder_int
            .variants
            .iter()
            .map(|v| v.name.as_str())
            .collect();
        assert!(ctor_names.contains(&"Empty$$Int"), "Empty$$Int present");
        assert!(ctor_names.contains(&"Hold$$Int"), "Hold$$Int present");
    }

    #[test]
    fn generic_type_at_two_instantiations_clones_twice() {
        let src = r#"
            type Box[A] = | Wrap(A)
            fn main() -> Int ![] {
                let bi: Box[Int] = Wrap(42);
                let bs: Box[String] = Wrap("x");
                42
            }
        "#;
        let items = run_pipeline_to_mono(src);
        assert!(find_type(&items, "Box").is_none());
        assert!(find_type(&items, "Box$$Int").is_some());
        assert!(find_type(&items, "Box$$String").is_some());
    }

    #[test]
    fn post_mono_program_passes_codegen_walker() {
        // Once monomorph completes, the codegen-entry walker must
        // accept the result. This is the key invariant the codegen-
        // entry assert depends on.
        let src = r#"
            fn id[A](x: A) -> A ![] {
                x
            }
            fn main() -> Int ![] {
                id(42)
            }
        "#;
        let items = run_pipeline_to_mono(src);
        let prog = Program {
            file: "test.sigil".to_string(),
            items,
        };
        assert!(
            !crate::codegen::contains_apply_or_generic_ref(&prog),
            "post-monomorphization program must pass the codegen-entry walker"
        );
    }

    #[test]
    fn imports_preserved_through_mono() {
        let src = r#"
            import std.io
            fn id[A](x: A) -> A ![] {
                x
            }
            fn main() -> Int ![] {
                id(42)
            }
        "#;
        let items = run_pipeline_to_mono(src);
        let import_count = items
            .iter()
            .filter(|i| matches!(i, Item::Import(_)))
            .count();
        assert_eq!(import_count, 1, "import preserved");
    }

    #[test]
    fn ctor_use_at_two_instantiations_produces_distinct_ctor_names() {
        // Both `Wrap(42)` and `Wrap("x")` reference the same ctor
        // name in source; post-mono they must resolve to distinct
        // mangled names so the global ctor registry stays unique.
        let src = r#"
            type Box[A] = | Wrap(A)
            fn main() -> Int ![] {
                let bi: Box[Int] = Wrap(42);
                let bs: Box[String] = Wrap("x");
                42
            }
        "#;
        let items = run_pipeline_to_mono(src);
        let box_int = find_type(&items, "Box$$Int").expect("Box$$Int");
        let box_string = find_type(&items, "Box$$String").expect("Box$$String");
        assert_eq!(box_int.variants[0].name, "Wrap$$Int");
        assert_eq!(box_string.variants[0].name, "Wrap$$String");
    }

    #[test]
    fn match_against_generic_scrutinee_rewrites_pattern_ctors() {
        let src = r#"
            type Maybe[A] = | Nada | Just(A)
            fn main() -> Int ![] {
                let m: Maybe[Int] = Just(42);
                match m {
                    Nada => 0,
                    Just(n) => n,
                }
            }
        "#;
        let items = run_pipeline_to_mono(src);
        let main = find_fn(&items, "main").expect("main");
        // Reviewer Comment 2 #3 closure: assert *both* mangled ctors
        // are present, not just `>= 1`. Previous form would pass with
        // only `Nada$$Int` even if `Just$$Int` was missing.
        let mut saw_nada = false;
        let mut saw_just = false;
        walk_block_for_ctor_patterns(&main.body, &mut |name| {
            if name == "Nada$$Int" {
                saw_nada = true;
            }
            if name == "Just$$Int" {
                saw_just = true;
            }
        });
        assert!(saw_nada, "Nada$$Int must appear in match patterns");
        assert!(saw_just, "Just$$Int must appear in match patterns");
    }

    #[test]
    fn recursive_generic_type_termination_one_clone_per_arg_tuple() {
        // Reviewer Comment 2 #3 closure: a recursive generic type
        // (`type List[A] = | Nil | Cons(A, List[A])`) used with a
        // single instantiation must produce *exactly one* `List$$Int`
        // clone, not loop or duplicate. The dedup-by-mangled-name
        // worklist (`type_seen`) prevents the loop; this test pins
        // the contract.
        let src = r#"
            type List[A] = | Nil | Cons(A, List[A])
            fn main() -> Int ![] {
                let xs: List[Int] = Cons(42, Cons(43, Nil));
                42
            }
        "#;
        let items = run_pipeline_to_mono(src);
        let list_int_count = items
            .iter()
            .filter(|i| matches!(i, Item::Type(t) if t.name == "List$$Int"))
            .count();
        assert_eq!(
            list_int_count, 1,
            "exactly one List$$Int clone — recursive type must dedup not loop"
        );
        assert!(
            find_type(&items, "List").is_none(),
            "original generic List must be dropped"
        );
    }

    #[test]
    fn self_recursive_generic_fn_terminates() {
        // Reviewer Comment 2 #3 closure: a generic fn that calls
        // itself inside its own body must clone exactly once per
        // (name, type-args) tuple — recursive call sites resolve to
        // the *same* instantiation as the enclosing clone (because
        // the inner call's fresh-var resolves to the outer fn's
        // generic-param var, which `Substitution::by_var` then
        // resolves to the concrete arg).
        let src = r#"
            fn loops[A](x: A) -> Int ![] {
                let y: Int = loops_helper(x);
                y
            }
            fn loops_helper[A](x: A) -> Int ![] {
                42
            }
            fn main() -> Int ![] {
                loops(7)
            }
        "#;
        let items = run_pipeline_to_mono(src);
        let loops_clones = items
            .iter()
            .filter_map(|i| match i {
                Item::Fn(f) if f.name.starts_with("loops") => Some(f.name.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        // Exactly one `loops$$Int` clone (the outer) and one
        // `loops_helper$$Int` clone (called from inside).
        assert!(
            loops_clones.contains(&"loops$$Int"),
            "loops$$Int present, got {loops_clones:?}"
        );
        assert!(
            loops_clones.contains(&"loops_helper$$Int"),
            "loops_helper$$Int present, got {loops_clones:?}"
        );
        // Original generics dropped.
        assert!(find_fn(&items, "loops").is_none());
        assert!(find_fn(&items, "loops_helper").is_none());
    }

    #[test]
    fn generic_fn_calling_generic_fn_resolves_var_chain() {
        // Reviewer Comment 1 #5 closure: a generic fn calling
        // another generic fn exercises the
        // `Ty::Var(callee_fresh) → Ty::Var(outer_A) → concrete`
        // chain through `Substitution::by_var`. The inner fn's
        // captured instantiation references the outer fn's var; at
        // mono descent, the outer fn's var resolves to the concrete
        // arg from main's call site, then the inner's var-ref
        // resolves through the same substitution.
        let src = r#"
            fn use_id[B](y: B) -> B ![] {
                inner(y)
            }
            fn inner[A](x: A) -> A ![] {
                x
            }
            fn main() -> Int ![] {
                use_id(42)
            }
        "#;
        let items = run_pipeline_to_mono(src);
        // `use_id$$Int` cloned from main's call.
        assert!(
            find_fn(&items, "use_id$$Int").is_some(),
            "use_id$$Int present"
        );
        // `inner$$Int` cloned from inside `use_id$$Int`'s body — its
        // captured instantiation resolved through the var chain.
        assert!(
            find_fn(&items, "inner$$Int").is_some(),
            "inner$$Int present (resolved via var chain)"
        );
        assert!(find_fn(&items, "inner$$VarUnresolved").is_none());
    }

    #[test]
    fn end_to_end_nested_generic_clone_name_matches_pinned_format() {
        // Reviewer Comment 2 #3 closure: end-to-end variant of
        // `mangle_fn_handles_nested_generics` that runs a real
        // sigil program through the full pipeline and asserts the
        // resulting clone name matches the pinned format. Catches
        // any drift between the unit-level format string and what
        // the pipeline actually produces.
        let src = r#"
            type Option[A] = | None | Some(A)
            fn unwrap[A](o: Option[A], d: A) -> A ![] {
                d
            }
            fn main() -> Int ![] {
                let inner: Option[Int] = Some(7);
                let outer: Option[Option[Int]] = Some(inner);
                let x: Option[Int] = unwrap(outer, None);
                42
            }
        "#;
        let items = run_pipeline_to_mono(src);
        let fn_names: Vec<&str> = items
            .iter()
            .filter_map(|i| match i {
                Item::Fn(f) => Some(f.name.as_str()),
                _ => None,
            })
            .collect();
        // unwrap is called at A=Option[Int], so the clone is named
        // `unwrap$$Option$Int`. The nested `Option` arg renders with
        // the within-arg `$` separator.
        assert!(
            fn_names.contains(&"unwrap$$Option$Int"),
            "unwrap$$Option$Int present, got {fn_names:?}"
        );
        let type_names: Vec<&str> = items
            .iter()
            .filter_map(|i| match i {
                Item::Type(t) => Some(t.name.as_str()),
                _ => None,
            })
            .collect();
        // `Option[Int]` and `Option[Option[Int]]` are distinct
        // instantiations, both cloned.
        assert!(
            type_names.contains(&"Option$$Int"),
            "Option$$Int present, got {type_names:?}"
        );
        assert!(
            type_names.contains(&"Option$$Option$Int"),
            "Option$$Option$Int present, got {type_names:?}"
        );
    }

    #[test]
    fn ctor_call_site_callee_ident_is_rewritten() {
        // Reviewer Comment 1 #1 closure: assert the *call-site*
        // rewrite (not just the TypeDecl mangling) resolves the
        // callee Ident to the mangled ctor name. Walks main's body
        // to find the Call with callee `Wrap`, asserts its callee
        // ident text is `Wrap$$Int` post-mono.
        let src = r#"
            type Box[A] = | Wrap(A)
            fn main() -> Int ![] {
                let bi: Box[Int] = Wrap(42);
                42
            }
        "#;
        let items = run_pipeline_to_mono(src);
        let main = find_fn(&items, "main").expect("main");
        let mut saw_mangled_callee = false;
        walk_block_for_call_callees(&main.body, &mut |callee_name| {
            if callee_name == "Wrap$$Int" {
                saw_mangled_callee = true;
            }
            assert_ne!(
                callee_name, "Wrap",
                "post-mono call site must NOT use unmangled `Wrap`"
            );
        });
        assert!(
            saw_mangled_callee,
            "Wrap$$Int must appear as a call's callee Ident post-mono"
        );
    }

    #[test]
    fn nested_generic_ctor_pattern_threads_inner_scrut_ty() {
        // Reviewer round-3 regression closure (PR #16 comment
        // 4318208... — the "Request changes" verdict). This is the
        // exact reproducer the reviewer ran against the previous
        // fix-up commit, where `unreachable!` fired on a legitimate
        // v1 program. The proper fix threads per-sub-pattern field
        // types so an inner `Cons(h, t)` pattern of `Option[List[Int]]`
        // sees `Ty::User("List", [Ty::Int])` and mangles to
        // `Cons$$Int`, while the outer `Some(...)` mangles to
        // `Some$$List$Int`.
        let src = r#"
            type List[A] = | Nil | Cons(A, List[A])
            type Option[A] = | None | Some(A)
            fn main() -> Int ![] {
                let opt: Option[List[Int]] = Some(Cons(1, Nil));
                match opt {
                    None => 0,
                    Some(Cons(h, t)) => h,
                    Some(Nil) => 99,
                }
            }
        "#;
        let items = run_pipeline_to_mono(src);
        let main = find_fn(&items, "main").expect("main");
        // Walk all ctor patterns in main and assert mangling. The
        // outer Option ctors are mangled with the full type-arg
        // tuple `List$Int`; the inner List ctors with `Int`.
        let mut saw_some_outer = false;
        let mut saw_none_outer = false;
        let mut saw_cons_inner = false;
        let mut saw_nil_inner = false;
        walk_block_for_ctor_patterns(&main.body, &mut |name| {
            // Reject any unmangled ctor remaining in patterns —
            // every reachable ctor here belongs to a generic type.
            assert_ne!(name, "Some", "outer Some must be mangled");
            assert_ne!(name, "None", "outer None must be mangled");
            assert_ne!(name, "Cons", "inner Cons must be mangled");
            assert_ne!(name, "Nil", "inner Nil must be mangled");
            if name == "Some$$List$Int" {
                saw_some_outer = true;
            }
            if name == "None$$List$Int" {
                saw_none_outer = true;
            }
            if name == "Cons$$Int" {
                saw_cons_inner = true;
            }
            if name == "Nil$$Int" {
                saw_nil_inner = true;
            }
        });
        assert!(saw_some_outer, "Some$$List$Int must appear");
        assert!(saw_none_outer, "None$$List$Int must appear");
        assert!(saw_cons_inner, "Cons$$Int must appear (inner pattern)");
        assert!(saw_nil_inner, "Nil$$Int must appear (inner pattern)");
        // All four reachable type instantiations should be cloned.
        assert!(find_type(&items, "Option$$List$Int").is_some());
        assert!(find_type(&items, "List$$Int").is_some());
    }

    #[test]
    fn effect_row_var_preserved_on_clone_passes_codegen_walker() {
        // Reviewer round-3 closure: pin the effect_row_var
        // preservation invariant. Plan B v1 doesn't monomorphize
        // rows — clones inherit `effect_row_var` from the original.
        // Codegen-entry walker (relaxed in this PR) must accept
        // `Some(_)`. An accidental future change that drops
        // effect_row_var on the clone wouldn't be caught without
        // this test.
        let src = r#"
            fn id[A](x: A) -> A ![ | e] {
                x
            }
            fn main() -> Int ![] {
                id(42)
            }
        "#;
        let items = run_pipeline_to_mono(src);
        let id_int = find_fn(&items, "id$$Int").expect("id$$Int clone");
        // Original `id[A]` had a row variable; the clone preserves
        // it (Plan B v1 row erasure happens at codegen, not mono).
        assert!(
            id_int.effect_row_var.is_some(),
            "row variable should be preserved on the clone (rows aren't monomorphized in v1)"
        );
        // Walker now accepts the clone — pre-PR-#16 it would have
        // rejected at the `effect_row_var.is_some()` arm.
        let prog = Program {
            file: "test.sigil".to_string(),
            items,
        };
        assert!(
            !crate::codegen::contains_apply_or_generic_ref(&prog),
            "post-mono program with row-polymorphic clone must pass codegen-entry walker"
        );
    }

    #[test]
    fn two_fn_clones_with_nested_generic_type_args() {
        // Reviewer Comment 1 #5 closure: two clones of the same
        // generic fn, each at a *different* nested-generic
        // instantiation, must produce two distinct clones.
        let src = r#"
            type Wrapper[A] = | Wrap(A)
            fn idw[A](x: Wrapper[A]) -> Wrapper[A] ![] {
                x
            }
            fn main() -> Int ![] {
                let a: Wrapper[Int] = Wrap(1);
                let b: Wrapper[String] = Wrap("x");
                let _ai: Wrapper[Int] = idw(a);
                let _as: Wrapper[String] = idw(b);
                42
            }
        "#;
        let items = run_pipeline_to_mono(src);
        let idw_clones: Vec<&str> = items
            .iter()
            .filter_map(|i| match i {
                Item::Fn(f) if f.name.starts_with("idw") => Some(f.name.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            idw_clones.contains(&"idw$$Int"),
            "idw$$Int present, got {idw_clones:?}"
        );
        assert!(
            idw_clones.contains(&"idw$$String"),
            "idw$$String present, got {idw_clones:?}"
        );
    }

    fn walk_block_for_call_callees(b: &Block, cb: &mut impl FnMut(&str)) {
        for s in &b.stmts {
            match s {
                Stmt::Let(l) => walk_expr_for_call_callees(&l.value, cb),
                Stmt::Expr(e) => walk_expr_for_call_callees(e, cb),
                Stmt::Perform(p) => {
                    for a in &p.args {
                        walk_expr_for_call_callees(a, cb);
                    }
                }
            }
        }
        if let Some(t) = &b.tail {
            walk_expr_for_call_callees(t, cb);
        }
    }

    fn walk_expr_for_call_callees(e: &Expr, cb: &mut impl FnMut(&str)) {
        match e {
            Expr::Call { callee, args, .. } => {
                if let Expr::Ident(name, _) = callee.as_ref() {
                    cb(name);
                }
                walk_expr_for_call_callees(callee, cb);
                for a in args {
                    walk_expr_for_call_callees(a, cb);
                }
            }
            Expr::Match { arms, .. } => {
                for a in arms {
                    walk_expr_for_call_callees(&a.body, cb);
                }
            }
            Expr::Block(b) => walk_block_for_call_callees(b, cb),
            Expr::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                walk_expr_for_call_callees(cond, cb);
                walk_block_for_call_callees(then_block, cb);
                walk_block_for_call_callees(else_block, cb);
            }
            _ => {}
        }
    }

    fn walk_block_for_ctor_patterns(b: &Block, cb: &mut impl FnMut(&str)) {
        for s in &b.stmts {
            match s {
                Stmt::Let(l) => walk_expr_for_ctor_patterns(&l.value, cb),
                Stmt::Expr(e) => walk_expr_for_ctor_patterns(e, cb),
                Stmt::Perform(p) => {
                    for a in &p.args {
                        walk_expr_for_ctor_patterns(a, cb);
                    }
                }
            }
        }
        if let Some(t) = &b.tail {
            walk_expr_for_ctor_patterns(t, cb);
        }
    }

    fn walk_expr_for_ctor_patterns(e: &Expr, cb: &mut impl FnMut(&str)) {
        match e {
            Expr::Match { arms, .. } => {
                for a in arms {
                    walk_pattern_for_ctor(&a.pattern, cb);
                    walk_expr_for_ctor_patterns(&a.body, cb);
                }
            }
            Expr::Block(b) => walk_block_for_ctor_patterns(b, cb),
            Expr::Call { callee, args, .. } => {
                walk_expr_for_ctor_patterns(callee, cb);
                for a in args {
                    walk_expr_for_ctor_patterns(a, cb);
                }
            }
            Expr::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                walk_expr_for_ctor_patterns(cond, cb);
                walk_block_for_ctor_patterns(then_block, cb);
                walk_block_for_ctor_patterns(else_block, cb);
            }
            _ => {}
        }
    }

    fn walk_pattern_for_ctor(p: &Pattern, cb: &mut impl FnMut(&str)) {
        if let Pattern::Ctor { name, fields, .. } = p {
            cb(name);
            match fields {
                CtorPatternFields::Unit => {}
                CtorPatternFields::Positional(ps) => {
                    for sp in ps {
                        walk_pattern_for_ctor(sp, cb);
                    }
                }
                CtorPatternFields::Record(fs) => {
                    for f in fs {
                        walk_pattern_for_ctor(&f.pattern, cb);
                    }
                }
            }
        }
    }
}
