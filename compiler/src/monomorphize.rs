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
//! - Mangled name: `f__<canon(TA)>__<canon(TB)>__...`.
//!
//! For a generic type `Foo[T1, T2, ...]` instantiated at the same
//! tuple: `Foo__<canon(TA)>__<canon(TB)>__...`. Each ctor `C` of
//! `Foo[T1, T2]` is renamed to `C__<canon(TA)>__<canon(TB)>` so
//! the global ctor namespace stays unique across instantiations.
//!
//! `canon(Ty)` is recursive:
//! - Primitives render as themselves: `Int`, `String`, `Bool`,
//!   `Char`, `Byte`, `Unit`.
//! - `User(name, [])` renders as `name`.
//! - `User(name, [a1, a2, ...])` renders as
//!   `name_<canon(a1)>_<canon(a2)>_...` — single underscore between
//!   parts within a single type-arg, double underscore between
//!   top-level type-args. The asymmetric separator keeps the format
//!   unambiguous as long as user fn / type names contain no
//!   double-underscore (the same constraint plan A2 relied on for
//!   `$lambda_N` synthetic names).
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

#[derive(Clone, Debug)]
pub struct MonoProgram {
    pub anf: AnfProgram,
}

/// Run monomorphization on the post-elaborate IR. Returns the input
/// unchanged when no generic decls exist (the common case for plan
/// A1/A2/A3 programs); otherwise produces a fully-specialised
/// `Program` and updates the types registry to match.
pub fn monomorphize(mut anf: AnfProgram) -> MonoProgram {
    let needs_mono = program_has_generics(&anf.checked.program);
    if !needs_mono {
        return MonoProgram { anf };
    }

    let new_items = {
        let mono = Monomorphizer::new(&anf.checked);
        mono.run()
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
    anf.checked.types = new_types;
    MonoProgram { anf }
}

/// Quick check: does the program contain any generic decls? Skipping
/// the entire pass on non-generic programs keeps plan A1/A2/A3 codegen
/// budget unchanged and avoids touching the AST at all.
fn program_has_generics(program: &Program) -> bool {
    for item in &program.items {
        match item {
            Item::Fn(f) => {
                if !f.generic_params.is_empty() {
                    return true;
                }
                // Effect-row vars without type generics still need to
                // be handled by this pass to satisfy the codegen-entry
                // invariant — but Plan B v1 erases row vars at codegen,
                // not here. So just type generics gate the work.
            }
            Item::Type(td) => {
                if !td.generic_params.is_empty() {
                    return true;
                }
            }
            Item::Import(_) => {}
        }
    }
    false
}

// ---------------------------------------------------------------- name mangling

/// Canonical render of a `Ty` for use inside a mangled symbol name.
/// See module-level docs for the exact format. Public so unit tests
/// in this module can pin the contract.
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
                    s.push('_');
                    s.push_str(&canon_ty(a));
                }
                s
            }
        }
        Ty::Var(_) => {
            // Should never appear in a fully-resolved type-arg tuple
            // reaching mangling. If it does, the upstream resolver
            // failed to substitute through the outer fn's generic
            // params — we render a deterministic placeholder so the
            // failure is visible in object dumps rather than producing
            // collisions.
            "VarUnresolved".to_string()
        }
        Ty::Fn(_) => {
            // No `TypeExpr::Fn` surface syntax in v1 — a `Ty::Fn`
            // reaching mangling would mean a future-plan feature
            // landed without updating mangling. Render as a fixed
            // placeholder so the name stays stable across builds.
            "Fn".to_string()
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
        s.push_str("__");
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
        s.push_str("__");
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
        s.push_str("__");
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
        }
    }

    fn run(mut self) -> Vec<Item> {
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
        // Imports first.
        for item in self.original_items {
            if let Item::Import(_) = item {
                out.push(item.clone());
            }
        }
        for td in self.output_types {
            out.push(Item::Type(Box::new(td)));
        }
        for f in self.output_fns {
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
                TypeExpr::Named(mangled, span.clone())
            }
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
                let new_callee = self.rewrite_expr(callee, subst);
                // If the call's callee is an Ident whose span has a
                // captured ctor instantiation, the rewrite above
                // already mangled the ctor name. Same for fn calls.
                // Just pass through the rewritten components.
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
            } => Expr::Lambda {
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
            },
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
        }
    }

    fn rewrite_pattern(&mut self, p: &Pattern, scrut_ty: &Option<Ty>) -> Pattern {
        match p {
            Pattern::IntLit(_, _) | Pattern::BoolLit(_, _) | Pattern::CharLit(_, _) => p.clone(),
            Pattern::Wildcard(_) | Pattern::Var(_, _) => p.clone(),
            Pattern::Tuple(ps, span) => Pattern::Tuple(
                ps.iter()
                    .map(|sp| self.rewrite_pattern(sp, scrut_ty))
                    .collect(),
                span.clone(),
            ),
            Pattern::Ctor { name, fields, span } => {
                // Determine the type-args from the scrutinee's
                // concrete type. Falls back to no-op (no mangling)
                // when the scrutinee type is unavailable or non-
                // generic.
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
                let new_fields = match fields {
                    CtorPatternFields::Unit => CtorPatternFields::Unit,
                    CtorPatternFields::Positional(ps) => {
                        // Sub-patterns are over the field types of
                        // the variant — the field types may be
                        // generic in the type's params, but Pattern
                        // nodes don't carry types so the existing
                        // sub-pattern walk just recurses with the
                        // same scrut_ty for nested user-type
                        // patterns. v1 doesn't yet thread per-
                        // sub-pattern type-args; nested generic
                        // ctor patterns are handled by re-resolving
                        // through ctor_to_type at each level.
                        CtorPatternFields::Positional(
                            ps.iter()
                                .map(|sp| self.rewrite_pattern(sp, scrut_ty))
                                .collect(),
                        )
                    }
                    CtorPatternFields::Record(fs) => CtorPatternFields::Record(
                        fs.iter()
                            .map(|f| CtorPatternField {
                                name: f.name.clone(),
                                pattern: self.rewrite_pattern(&f.pattern, scrut_ty),
                                span: f.span.clone(),
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
            Ty::Fn(sig) => Ty::Fn(Box::new(crate::typecheck::FnSig {
                params: sig.params.iter().map(|p| self.apply_to_ty(p)).collect(),
                ret: self.apply_to_ty(&sig.ret),
                effects: sig.effects.clone(),
                effect_row_var: sig.effect_row_var,
            })),
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
fn ty_to_type_expr(ty: &Ty, span: &Span) -> TypeExpr {
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
        Ty::Var(_) => {
            // Should never escape monomorphization. If it does,
            // produce a deterministic placeholder so downstream
            // failure is observable.
            TypeExpr::Named("__VarUnresolved".to_string(), span.clone())
        }
        Ty::Fn(_) => {
            // No `TypeExpr::Fn` surface yet (Plan A3 carryover).
            TypeExpr::Named("__Fn".to_string(), span.clone())
        }
    }
}

/// Convert a fully-rewritten `TypeExpr` (no `Apply`, no generic-param
/// references, no `Var`) into a `Ty`. Used when resolving the args
/// of a `TypeExpr::Apply` recursively.
fn type_expr_to_ty(te: &TypeExpr) -> Ty {
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
        assert_eq!(canon_ty(&t), "Option_Int");
    }

    #[test]
    fn canon_ty_renders_two_arg_user_type() {
        let t = Ty::User("Map".to_string(), vec![Ty::String, Ty::Int]);
        assert_eq!(canon_ty(&t), "Map_String_Int");
    }

    #[test]
    fn canon_ty_renders_nested_user_type() {
        let inner = Ty::User("Option".to_string(), vec![Ty::Int]);
        let outer = Ty::User("List".to_string(), vec![inner]);
        assert_eq!(canon_ty(&outer), "List_Option_Int");
    }

    #[test]
    fn mangle_fn_keeps_non_generic_names() {
        assert_eq!(mangle_fn("main", &[]), "main");
        assert_eq!(mangle_fn("fib", &[]), "fib");
    }

    #[test]
    fn mangle_fn_appends_double_underscore_per_arg() {
        let args = vec![Ty::Int, Ty::String];
        assert_eq!(mangle_fn("identity", &args), "identity__Int__String");
    }

    #[test]
    fn mangle_fn_handles_nested_generics() {
        let inner = Ty::User("Option".to_string(), vec![Ty::Int]);
        let outer = Ty::User("List".to_string(), vec![inner]);
        let args = vec![outer.clone(), outer];
        assert_eq!(
            mangle_fn("list_map", &args),
            "list_map__List_Option_Int__List_Option_Int"
        );
    }

    #[test]
    fn mangle_type_keeps_non_generic_names() {
        assert_eq!(mangle_type("Option", &[]), "Option");
    }

    #[test]
    fn mangle_type_appends_args() {
        let args = vec![Ty::Int];
        assert_eq!(mangle_type("Option", &args), "Option__Int");
    }

    #[test]
    fn mangle_ctor_unchanged_for_non_generic_type() {
        assert_eq!(mangle_ctor("Some", &[]), "Some");
    }

    #[test]
    fn mangle_ctor_appends_type_args() {
        let args = vec![Ty::Int];
        assert_eq!(mangle_ctor("Some", &args), "Some__Int");
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
            TypeExpr::Named(name, _) => assert_eq!(name, "Option__Int"),
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
        let id_int = find_fn(&items, "id__Int").expect("id__Int clone present");
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
        assert!(find_fn(&items, "id__Int").is_some());
        assert!(find_fn(&items, "id__String").is_some());
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
        assert!(find_fn(&items, "id__Int").is_none());
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
        let holder_int = find_type(&items, "Holder__Int").expect("Holder__Int present");
        assert_eq!(holder_int.generic_params.len(), 0);
        // Ctor names in the clone are mangled.
        let ctor_names: Vec<&str> = holder_int
            .variants
            .iter()
            .map(|v| v.name.as_str())
            .collect();
        assert!(ctor_names.contains(&"Empty__Int"), "Empty__Int present");
        assert!(ctor_names.contains(&"Hold__Int"), "Hold__Int present");
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
        assert!(find_type(&items, "Box__Int").is_some());
        assert!(find_type(&items, "Box__String").is_some());
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
        let box_int = find_type(&items, "Box__Int").expect("Box__Int");
        let box_string = find_type(&items, "Box__String").expect("Box__String");
        assert_eq!(box_int.variants[0].name, "Wrap__Int");
        assert_eq!(box_string.variants[0].name, "Wrap__String");
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
        // Walk main's body to find the Match expression. The match
        // arms' patterns must reference the mangled ctor names.
        let mut found_mangled = 0;
        walk_block_for_ctor_patterns(&main.body, &mut |name| {
            if name == "Nada__Int" || name == "Just__Int" {
                found_mangled += 1;
            }
        });
        assert!(
            found_mangled >= 1,
            "expected at least one mangled ctor pattern in match arms"
        );
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
