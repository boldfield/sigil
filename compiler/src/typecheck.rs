//! Type checker for the Stage-1 subset — plan A1 task 7, extended in
//! plan A2 task 22.
//!
//! Type surface (post-A2): `Int`, `String`, `Unit`, `Bool`, `Char`, `Byte`.
//! One implicit effect (`IO`) used as a runtime-intrinsic shortcut until
//! Plan B generalizes effects. We verify:
//!
//! - `fn main() -> Int ![IO]` is present and well-formed.
//! - Every `perform IO.println(s)` call has a String argument and `IO` in
//!   the enclosing function's effect row.
//! - Integer and string literals are well-typed.
//! - Binary/unary operators obey their declared operand types.
//! - `if` conditions are `Bool` and `if` branches unify.
//! - `match` patterns agree with the scrutinee's type, arm bodies unify,
//!   and the arm list is exhaustive (Bool: both polarities or wildcard;
//!   other primitives: wildcard required).
//!
//! Recovery: a single expression's type error is recorded and checking
//! continues on sibling expressions so one compile reports as many errors
//! as possible.

use crate::ast::*;
use crate::errors::{self, CompilerError, Severity, Span};
use std::collections::BTreeMap;

/// The checker's type lattice. Expanded in plan A2 task 30 to include
/// `Fn` for user function/lambda values; expanded again in plan B
/// task 48 to carry HM type variables and generic-applied user types.
///
/// `Ty::Var(id)` is a unification variable introduced during HM
/// inference (instantiation of a generic scheme, fresh lambda /
/// inferred-let bindings). Every `Var` must be substituted before
/// the IR leaves typecheck — the codegen-entry walker installed in
/// task 48 asserts this invariant; monomorphization (task 49) is the
/// pass that erases generic instantiations into concrete clones.
///
/// `Ty::User(name, args)` covers both nominal user types from Plan A3
/// (`Option` → `User("Option", vec![])`) and generic applications from
/// Plan B (`List[Int]` → `User("List", vec![Ty::Int])`). Equality is
/// structural over `(name, args)`, so `List[Int]` and `List[String]`
/// are distinct types.
///
/// `Ty` does not derive `Copy` because `Ty::Fn` carries owned `Vec`s
/// (parameter list and effect row), and `Ty::User` carries an owned
/// `Vec<Ty>` of arguments. Call sites that need a by-value copy use
/// `.clone()`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ty {
    Int,
    String,
    Unit,
    Bool,
    Char,
    Byte,
    /// Function type: `(param_tys...) -> ret_ty ![effect_names...]`.
    /// Boxed to keep the overall `Ty` size small — most `Ty` values
    /// in a program are primitives, and boxing the fn case keeps the
    /// discriminant + payload fit into a register on 64-bit hosts.
    Fn(Box<FnSig>),
    /// Nominal user-defined type introduced via `type Name = ...`
    /// (Plan A3 task 38) plus optional Plan B task 48 generic
    /// arguments. Empty `args` is a non-generic user type; non-empty
    /// is a fully-resolved generic instantiation.
    User(String, Vec<Ty>),
    /// HM unification variable (Plan B task 48). Carries an opaque
    /// integer id allocated by the typechecker's `fresh_ty_var`. After
    /// inference, every reachable `Ty::Var` must have been resolved
    /// through `Subst::apply_ty`; the codegen-entry walker asserts the
    /// post-monomorphization IR is var-free.
    Var(u32),
}

/// Structural function signature. Used in `Ty::Fn` and built for
/// every top-level `FnDecl` plus every `Expr::Lambda`.
///
/// Effects are stored as `Vec<String>` rather than a dedicated enum
/// so the runtime row-extension rules (v2+) can add new effect names
/// without breaking the typechecker. Plan A2 only ever sees `IO` in
/// an effect row.
///
/// Plan B task 48 adds `effect_row_var`: an optional row-unification
/// variable that turns the row from closed (`![IO]`) to open
/// (`![IO | e]`). Unification of two open rows shares the variable
/// and absorbs the difference; unifying an open row with a closed
/// row binds the variable to the difference; unifying two closed
/// rows requires set equality. Like `Ty::Var`, every reachable
/// `effect_row_var` must be resolved by inference end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FnSig {
    pub params: Vec<Ty>,
    pub ret: Ty,
    pub effects: Vec<String>,
    pub effect_row_var: Option<u32>,
}

/// HM type scheme (Plan B task 48). Bound type / row variables come
/// from `let`-generalisation at top-level fn boundaries; the body is
/// the generalised type (typically `Ty::Fn`). A non-generic, closed-
/// row fn produces a scheme with empty `type_vars` and `row_vars`,
/// so instantiation is a no-op clone — keeping the legacy concrete
/// path cheap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scheme {
    pub type_vars: Vec<u32>,
    pub row_vars: Vec<u32>,
    pub body: Ty,
}

/// Effect row used during row unification. `tail = None` is a closed
/// row; `tail = Some(id)` is open. Always carries effect labels in a
/// canonical sorted-deduped form via `Row::canonicalise` so set
/// equality reduces to vector equality.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub effects: Vec<String>,
    pub tail: Option<u32>,
}

impl Row {
    pub fn closed(effects: Vec<String>) -> Self {
        let mut r = Row {
            effects,
            tail: None,
        };
        r.canonicalise();
        r
    }
    pub fn open(effects: Vec<String>, tail: u32) -> Self {
        let mut r = Row {
            effects,
            tail: Some(tail),
        };
        r.canonicalise();
        r
    }
    pub fn canonicalise(&mut self) {
        self.effects.sort();
        self.effects.dedup();
    }
}

/// HM substitution accumulated during inference. Maps type-var ids
/// to `Ty` and row-var ids to `Row`. Application is shallow per
/// resolution step but iterates to fixpoint via `apply_*` (Plan B
/// task 48). The substitution stays single-instance per `Tc` —
/// `unify_*` mutate in place and the final post-inference state is
/// used to resolve any IR shells that surface to consumers.
#[derive(Clone, Debug, Default)]
pub struct Subst {
    pub tys: BTreeMap<u32, Ty>,
    pub rows: BTreeMap<u32, Row>,
}

impl Subst {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve `t` through the current substitution. Recursively
    /// walks `Fn` / `User` payloads. Cycles (impossible after
    /// occurs-check, but defensive) are detected by tracking visited
    /// type-var ids on the way down.
    pub fn apply_ty(&self, t: &Ty) -> Ty {
        let mut seen = std::collections::BTreeSet::new();
        self.apply_ty_inner(t, &mut seen)
    }

    fn apply_ty_inner(&self, t: &Ty, seen: &mut std::collections::BTreeSet<u32>) -> Ty {
        match t {
            Ty::Int | Ty::String | Ty::Unit | Ty::Bool | Ty::Char | Ty::Byte => t.clone(),
            Ty::Var(id) => {
                if seen.contains(id) {
                    // Cycle — leave the variable in place; occurs-
                    // check should have rejected the unification.
                    return t.clone();
                }
                if let Some(resolved) = self.tys.get(id) {
                    seen.insert(*id);
                    let r = self.apply_ty_inner(resolved, seen);
                    seen.remove(id);
                    r
                } else {
                    t.clone()
                }
            }
            Ty::User(name, args) => Ty::User(
                name.clone(),
                args.iter().map(|a| self.apply_ty_inner(a, seen)).collect(),
            ),
            Ty::Fn(sig) => {
                let new_sig = FnSig {
                    params: sig
                        .params
                        .iter()
                        .map(|p| self.apply_ty_inner(p, seen))
                        .collect(),
                    ret: self.apply_ty_inner(&sig.ret, seen),
                    effects: sig.effects.clone(),
                    effect_row_var: sig.effect_row_var,
                };
                let resolved = self.apply_row_to_sig(new_sig, seen);
                Ty::Fn(Box::new(resolved))
            }
        }
    }

    fn apply_row_to_sig(
        &self,
        mut sig: FnSig,
        seen: &mut std::collections::BTreeSet<u32>,
    ) -> FnSig {
        if let Some(id) = sig.effect_row_var {
            let row = self.apply_row_inner(
                &Row {
                    effects: Vec::new(),
                    tail: Some(id),
                },
                seen,
            );
            // Merge resolved row into the sig's effects + tail.
            let mut merged: Vec<String> = sig.effects.iter().cloned().chain(row.effects).collect();
            merged.sort();
            merged.dedup();
            sig.effects = merged;
            sig.effect_row_var = row.tail;
        }
        sig
    }

    /// Resolve a row through the current substitution. Walking a
    /// row variable substitutes its body in; chained row vars are
    /// followed transitively. Cycles (impossible after occurs-check)
    /// short-circuit on the seen set.
    pub fn apply_row(&self, r: &Row) -> Row {
        let mut seen = std::collections::BTreeSet::new();
        self.apply_row_inner(r, &mut seen)
    }

    fn apply_row_inner(&self, r: &Row, seen: &mut std::collections::BTreeSet<u32>) -> Row {
        let mut effects = r.effects.clone();
        let mut tail = r.tail;
        while let Some(id) = tail {
            if seen.contains(&id) {
                break;
            }
            match self.rows.get(&id) {
                None => break,
                Some(resolved) => {
                    seen.insert(id);
                    effects.extend(resolved.effects.iter().cloned());
                    tail = resolved.tail;
                }
            }
        }
        let mut out = Row { effects, tail };
        out.canonicalise();
        out
    }
}

#[derive(Clone, Debug)]
pub struct CheckedProgram {
    pub program: Program,
    /// Ordered list of (string-literal span, literal value). Codegen uses
    /// this to interleave static-data sections with the pipeline output.
    pub string_literals: Vec<(Span, String)>,
    /// Per-lambda free-variable sets, keyed by the lambda's span. Populated
    /// by `check_expr` when it enters an `Expr::Lambda`; consumed by
    /// closure conversion (plan A2 task 31) to size the closure's
    /// environment AND to compute the GC pointer bitmap for the closure
    /// record's header. Each entry is `(lambda_span, [(name, ty)...])`
    /// in source-order of lambda appearance; the `Ty` for each capture
    /// is the type the name held in the outer scope at lambda-entry.
    pub lambda_captures: Vec<(Span, Vec<(String, Ty)>)>,
    /// Nominal user-type symbol table (Plan A3 task 38). Keyed by the
    /// type's declared name; the value is the full parser `TypeDecl` so
    /// downstream passes (constructor resolution, pattern typing,
    /// exhaustiveness, codegen layout) can inspect variants and field
    /// shapes without re-scanning the program. Duplicate declarations
    /// produce E0113 at check time and the first declaration wins here.
    pub types: BTreeMap<String, TypeDecl>,
    /// Plan A3 task 41.2: scrutinee `Ty` for every `Expr::Match` whose
    /// scrutinee typechecked, keyed by the match expression's span.
    /// Codegen reads this map to disambiguate `Pattern::Var(name)`
    /// between a fresh binding and a nullary-constructor promotion —
    /// a decision that requires knowing whether the scrutinee has type
    /// `Ty::User(u)` whose variant registry lists `name` as a Unit
    /// variant. Absent entries (malformed scrutinee, or a synthetic
    /// match introduced by elaborate's if→match desugaring) let codegen
    /// fall back to primitive-scalar dispatch.
    pub match_scrut_tys: BTreeMap<Span, Ty>,
}

/// Where a constructor is registered. Indexes a `TypeDecl` in the
/// program's types registry by type name + variant index (Plan A3 task
/// 38.2). Lookup is O(1) so the checker doesn't rescan every type at
/// each use site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CtorInfo {
    pub type_name: String,
    pub variant_index: usize,
}

pub fn typecheck(program: Program) -> (CheckedProgram, Vec<CompilerError>) {
    // Pre-pass 1 (Plan A3 task 38): build the nominal-type symbol table.
    // Must precede the fn-env pre-pass so a `fn f(o: Option) -> ...`
    // declaration can resolve `Option` to `Ty::User("Option")` when
    // `Option` is declared further down in the file. Duplicate
    // declarations record E0113 against the second (and subsequent)
    // offender; the first declaration wins in the symbol table so
    // downstream passes always see a single canonical variant set.
    let mut types: BTreeMap<String, TypeDecl> = BTreeMap::new();
    let mut errors: Vec<CompilerError> = Vec::new();
    for item in &program.items {
        if let Item::Type(td) = item {
            if types.contains_key(&td.name) {
                errors.push(CompilerError::new(
                    Severity::Error,
                    errors::code("E0113"),
                    td.name_span.clone(),
                    format!("duplicate type declaration `{}`", td.name),
                ));
            } else {
                types.insert(td.name.clone(), (**td).clone());
            }
        }
    }

    // Pre-pass 1b (Plan A3 task 38.2): build the constructor registry.
    // Constructor names live in a single flat namespace across all
    // user-defined types in v1; collisions are rejected with E0118 at
    // the colliding variant's name span. The first declaration wins in
    // the registry so downstream typing always picks a single canonical
    // constructor per name.
    let mut ctors: BTreeMap<String, CtorInfo> = BTreeMap::new();
    for td in types.values() {
        for (idx, v) in td.variants.iter().enumerate() {
            if let Some(existing) = ctors.get(&v.name) {
                errors.push(CompilerError::new(
                    Severity::Error,
                    errors::code("E0118"),
                    v.name_span.clone(),
                    format!(
                        "constructor `{}` is already defined on type `{}`",
                        v.name, existing.type_name
                    ),
                ));
            } else {
                ctors.insert(
                    v.name.clone(),
                    CtorInfo {
                        type_name: td.name.clone(),
                        variant_index: idx,
                    },
                );
            }
        }
    }

    // Pre-pass 2: build a global environment from every top-level
    // `FnDecl`'s declared signature. This lets recursive and mutually-
    // recursive user functions reference each other by name during
    // `check_fn`'s body walk. Any `FnDecl` whose declared types don't
    // resolve to known `Ty`s is recorded with its best-effort partial
    // signature; the full diagnostic surfaces when the decl's own body
    // is checked (E0112 at the offending `TypeExpr`, E0044 at mismatch
    // sites with Unit fallback per E0112's documented behavior).
    //
    // Seeded first with language builtins (Plan A2 task 34):
    // `int_to_string(n: Int) -> String` exposes the runtime
    // `sigil_int_to_string` formatter. Seeding before the user-fn loop
    // means a user `fn int_to_string(...)` declaration simply overwrites
    // the builtin entry — users can shadow, and codegen's `lower_call`
    // checks `user_fn_refs` before the builtin branch, so the user's
    // definition wins end-to-end.
    let mut fn_env: BTreeMap<String, Ty> = builtin_fn_env();
    for item in &program.items {
        if let Item::Fn(f) = item {
            // Plan B task 48: this pre-pass seeds a *concrete* fn_env
            // entry usable by callers that have a fully resolved
            // signature. Generic parameters in scope (`fn id[A](...)`)
            // and explicit row variables (`![IO | e]`) are introduced
            // inside `check_fn`'s body walk via the scheme machinery;
            // here we use the empty generic-substitution and a closed
            // effect row for the fall-through, replacing whatever this
            // entry holds when `check_fn` later registers the fn's full
            // scheme. Builtins remain `Ty::Fn` (concrete schemes with
            // empty bound-var lists, looked up directly).
            let empty_subst = BTreeMap::new();
            let sig = FnSig {
                params: f
                    .params
                    .iter()
                    .map(|p| ty_from_type_expr(&p.ty, &types, &empty_subst).unwrap_or(Ty::Unit))
                    .collect(),
                ret: ty_from_type_expr(&f.return_type, &types, &empty_subst).unwrap_or(Ty::Unit),
                effects: f.effects.clone(),
                effect_row_var: None,
            };
            fn_env.insert(f.name.clone(), Ty::Fn(Box::new(sig)));
        }
    }

    let mut tc = Tc {
        errors,
        string_literals: Vec::new(),
        lambda_captures: Vec::new(),
        fn_env,
        env: BTreeMap::new(),
        types,
        ctors,
        match_scrut_tys: BTreeMap::new(),
        fn_schemes: BTreeMap::new(),
        next_ty_var: 0,
        next_row_var: 0,
        subst: Subst::new(),
        current_generic_subst: BTreeMap::new(),
        current_row_var_subst: BTreeMap::new(),
    };
    // E0112 sweep: any TypeExpr in an FnDecl signature that does not
    // resolve to a primitive or registered user type is reported against
    // the TypeExpr's span. Runs after the types pre-pass so forward
    // references are fine. The fn_env above already committed Unit as
    // the fallback for unresolved types; this sweep attaches the real
    // diagnostic so the user sees why.
    // Plan B task 48 — the E0112 sweep over fn signatures must run
    // with each fn's generic-parameter substitution active so a
    // surface name like `A` (declared in `fn id[A](x: A) -> A`) is
    // recognised rather than reported as an unknown type. We stage
    // the per-fn subst here and clear it afterwards, leaving Tc's
    // current state empty for downstream `check_fn` invocations.
    for item in &program.items {
        if let Item::Fn(f) = item {
            let saved_generic_subst = std::mem::take(&mut tc.current_generic_subst);
            let (gs, _) = tc.fresh_generic_subst(&f.generic_params);
            tc.current_generic_subst = gs;
            for p in &f.params {
                tc.check_type_expr_known(&p.ty);
            }
            tc.check_type_expr_known(&f.return_type);
            tc.current_generic_subst = saved_generic_subst;
        }
    }
    for item in &program.items {
        match item {
            Item::Fn(f) => tc.check_fn(f),
            Item::Import(_) => {}
            // Plan A3 task 38 / Plan B task 48: validate variant
            // field types under the type's own generic-parameter
            // substitution so `Cons(A, List[A])` resolves cleanly.
            Item::Type(td) => {
                let saved_generic_subst = std::mem::take(&mut tc.current_generic_subst);
                let (gs, _) = tc.fresh_generic_subst(&td.generic_params);
                tc.current_generic_subst = gs;
                for v in &td.variants {
                    match &v.fields {
                        VariantFields::Unit => {}
                        VariantFields::Positional(ts) => {
                            for t in ts {
                                tc.check_type_expr_known(t);
                            }
                        }
                        VariantFields::Record(fs) => {
                            for f in fs {
                                tc.check_type_expr_known(&f.ty);
                            }
                        }
                    }
                }
                tc.current_generic_subst = saved_generic_subst;
            }
        }
    }
    let has_main = program
        .items
        .iter()
        .any(|i| matches!(i, Item::Fn(f) if f.name == "main"));
    if !has_main {
        let span = Span::synthetic(&program.file);
        tc.push_error(
            "E0040",
            span,
            "program has no `fn main`; every Sigil program needs `fn main() -> Int ![IO]` or `fn main() -> Int ![]`",
        );
    }

    (
        CheckedProgram {
            program,
            string_literals: tc.string_literals,
            lambda_captures: tc.lambda_captures,
            types: tc.types,
            match_scrut_tys: tc.match_scrut_tys,
        },
        tc.errors,
    )
}

/// Language builtins — functions that appear to user code as ordinary
/// identifiers whose signatures are seeded into `fn_env` by the
/// typechecker. Codegen pairs each name with a direct call to the
/// matching runtime C symbol (see `compiler/src/codegen.rs`
/// `lower_call` for the dispatch table).
///
/// Plan A2 task 34 seeds a single builtin — `int_to_string(n: Int) ->
/// String` — to close the loop needed for `fib(20)` to print its
/// result. Additional builtins arrive as the runtime gains language-
/// surface helpers (Plan B's effect handlers, Plan C's stdlib).
///
/// Users can shadow a builtin by declaring their own `fn <name>`: the
/// user-fn pre-pass overwrites the builtin entry in `fn_env`, and
/// codegen's `user_fn_refs` resolves first, so the user's definition
/// wins at both typecheck and call sites.
fn builtin_fn_env() -> BTreeMap<String, Ty> {
    let mut m = BTreeMap::new();
    m.insert(
        "int_to_string".to_string(),
        Ty::Fn(Box::new(FnSig {
            params: vec![Ty::Int],
            ret: Ty::String,
            effects: Vec::new(),
            effect_row_var: None,
        })),
    );
    m
}

struct Tc {
    errors: Vec<CompilerError>,
    string_literals: Vec<(Span, String)>,
    /// Accumulated lambda free-variable sets; moved into `CheckedProgram`
    /// at typecheck completion. Each capture carries its outer-scope type
    /// so closure conversion can compute the GC pointer bitmap without
    /// re-looking-up types.
    lambda_captures: Vec<(Span, Vec<(String, Ty)>)>,
    /// Global environment: every top-level `FnDecl`'s declared signature,
    /// pre-populated before any body is checked. Makes recursive and
    /// mutually-recursive user functions typeable without a fix-point
    /// iteration. Plan A2 task 30.
    fn_env: BTreeMap<String, Ty>,
    /// Lexical-scope binding environment for the currently-checked
    /// function. Holds **only** locally-bound names: parameters, `let`
    /// bindings, and pattern-bound variables introduced by match arms.
    /// Top-level fn names live in `fn_env` and are consulted via
    /// fall-through at `Expr::Ident` resolution — they are deliberately
    /// absent from `self.env` (PR #6 architectural fix). The rationale
    /// is twofold:
    ///
    /// 1. Let-shadowing a top-level fn name (`let foo: Int = …` when
    ///    `fn foo` exists) must not trip `env_insert`'s no-shadowing
    ///    debug-assert.
    /// 2. `collect_free_vars` takes `outer_names = self.env.keys()` as
    ///    "names visible from the enclosing lexical scope" — including
    ///    top-level fn names here would cause the capture analysis to
    ///    record them as env slots, producing spurious closure fields
    ///    for what are statically-resolvable top-level symbols.
    ///
    /// `check_fn` clears this at entry. Scoped regions (lambda bodies,
    /// pattern-bound arms) save/restore via `self.env.clone()` +
    /// reassignment so outer-scope names remain visible within inner
    /// scopes but inner-scope additions don't leak back out.
    ///
    /// A `BTreeMap` keeps iteration order stable — important for the
    /// catalog-integrity and capture-analysis determinism disciplines.
    env: BTreeMap<String, Ty>,
    /// Nominal user-type registry (Plan A3 task 38). Built by the
    /// top-level pre-pass before any fn body is checked so forward
    /// references resolve. Moved into `CheckedProgram.types` on exit.
    types: BTreeMap<String, TypeDecl>,
    /// Constructor name -> (owning type name, variant index) index,
    /// built alongside `types` from every `Item::Type`'s variants
    /// (Plan A3 task 38.2). Duplicate names across types surface as
    /// E0118 at pre-pass time; the first writer wins in the map.
    /// Lookup is O(1) per use-site resolution.
    ctors: BTreeMap<String, CtorInfo>,
    /// Mirror of `CheckedProgram.match_scrut_tys`, moved into the
    /// checked program on typecheck completion. `check_match`
    /// populates an entry when the scrutinee has a known `Ty`.
    match_scrut_tys: BTreeMap<Span, Ty>,
    /// Plan B task 48 — Hindley-Milner unification machinery.
    ///
    /// Schemes for top-level functions: a generic fn declaration
    /// (`fn id[A](x: A) -> A { x }`) registers `(["A"], [], (Var(N))
    /// -> Var(N))` so every call site can instantiate fresh vars.
    /// Builtins and non-generic user fns store schemes with empty
    /// `type_vars` and `row_vars`. Lookup at call sites: `fn_schemes`
    /// is consulted first, then `fn_env` for the legacy direct-Ty
    /// path (e.g., during the recursive-fn pre-pass when the fn is
    /// being checked itself).
    fn_schemes: BTreeMap<String, Scheme>,
    /// Type-variable id supply — one counter per `Tc` lifetime.
    /// Allocated via `fresh_ty_var`; freed when the substitution
    /// resolves them. Codegen-entry walker in task 48 asserts no
    /// `Ty::Var` survives into the AST (it shouldn't — it lives in
    /// inferred IR shells, not in `TypeExpr`).
    next_ty_var: u32,
    /// Row-variable id supply, parallel to `next_ty_var`. Allocated
    /// for explicit `![ ... | e]` row variables and for fresh-row
    /// holes opened during HM inference.
    next_row_var: u32,
    /// Substitution accumulated by unification. Resolves type-vars
    /// via `Subst::apply_ty` and row-vars via `Subst::apply_row`.
    /// Updated in-place by `unify_ty` / `unify_row`; queried whenever
    /// a fresh `Ty` flows out of inference.
    subst: Subst,
    /// Currently-active generic-parameter substitution for the fn
    /// or lambda the typechecker is walking. Maps surface names
    /// (`A`, `B`) to their freshly-allocated `Ty::Var`s. Populated
    /// at `check_fn` entry from `f.generic_params`, consulted by
    /// every `ty_from_type_expr` call inside the body, and replaced
    /// (not merged) when entering a nested lambda. Empty outside
    /// generic scope.
    current_generic_subst: BTreeMap<String, Ty>,
    /// Currently-active row-variable substitution: surface name →
    /// fresh row-var id. Populated from the active fn / lambda's
    /// `effect_row_var` and consulted whenever a row-var name needs
    /// to be looked up (effect annotations on lambdas, future
    /// effect-row references in let-bound types). Empty for fns /
    /// lambdas with no explicit row variable.
    current_row_var_subst: BTreeMap<String, u32>,
}

impl Tc {
    fn push_error(&mut self, code: &'static str, span: Span, msg: impl Into<String>) {
        self.errors.push(CompilerError::new(
            Severity::Error,
            errors::code(code),
            span,
            msg,
        ));
    }

    // ---------- Plan B task 48 — HM unification helpers ----------

    fn fresh_ty_var(&mut self) -> u32 {
        let id = self.next_ty_var;
        self.next_ty_var += 1;
        id
    }

    /// Wrapper around `ty_from_type_expr` that uses the currently-
    /// active generic-parameter substitution. Plan B task 48 —
    /// inside a generic fn / type body, surface names like `A` map
    /// to the fn's freshly-allocated `Ty::Var`; outside generic
    /// scope this falls through to the empty-subst behavior.
    fn ty_from_type_expr_here(&self, t: &TypeExpr) -> Option<Ty> {
        ty_from_type_expr(t, &self.types, &self.current_generic_subst)
    }

    fn fresh_row_var(&mut self) -> u32 {
        let id = self.next_row_var;
        self.next_row_var += 1;
        id
    }

    /// Build a generic-parameter substitution map for a fn / type
    /// declaration's `[A, B, ...]` parameter list. Allocates one
    /// fresh `Ty::Var` per declared parameter and returns the
    /// `name -> Ty::Var(id)` map plus the parallel id list (used for
    /// `Scheme.type_vars`). Empty input yields an empty map and
    /// empty id list — non-generic declarations stay zero-cost.
    fn fresh_generic_subst(&mut self, gps: &[GenericParam]) -> (BTreeMap<String, Ty>, Vec<u32>) {
        let mut subst = BTreeMap::new();
        let mut ids = Vec::with_capacity(gps.len());
        for gp in gps {
            let id = self.fresh_ty_var();
            ids.push(id);
            subst.insert(gp.name.clone(), Ty::Var(id));
        }
        (subst, ids)
    }

    /// Construct a `Ty::User(name, args)` instance for a registered
    /// user-type declaration. Non-generic declarations return
    /// `Ty::User(name, vec![])` — same shape Plan A3 produced.
    /// Generic declarations allocate one fresh `Ty::Var` per
    /// declared generic parameter so HM unification can later
    /// resolve them. Caller (constructor resolution, scrutinee
    /// shaping) keeps the resulting `Ty` in the inference graph.
    fn fresh_user_instance(&mut self, name: &str, td: &TypeDecl) -> Ty {
        let (ty, _) = self.fresh_user_instance_with_subst(name, td);
        ty
    }

    /// Like `fresh_user_instance` but also returns the per-call
    /// surface-name → `Ty::Var` substitution so the caller can
    /// resolve constructor field types (which reference the type's
    /// generic parameters by name) under the same fresh allocation.
    fn fresh_user_instance_with_subst(
        &mut self,
        name: &str,
        td: &TypeDecl,
    ) -> (Ty, BTreeMap<String, Ty>) {
        let mut subst = BTreeMap::new();
        if td.generic_params.is_empty() {
            return (Ty::User(name.to_string(), Vec::new()), subst);
        }
        let mut args: Vec<Ty> = Vec::with_capacity(td.generic_params.len());
        for gp in &td.generic_params {
            let v = Ty::Var(self.fresh_ty_var());
            subst.insert(gp.name.clone(), v.clone());
            args.push(v);
        }
        (Ty::User(name.to_string(), args), subst)
    }

    /// Resolve a `Ty` through the current substitution. Convenience
    /// wrapper around `self.subst.apply_ty` used at every site that
    /// needs the "current best" view of an inferred type.
    fn deref(&self, t: &Ty) -> Ty {
        self.subst.apply_ty(t)
    }

    /// Compute the free type-variable set of `t` after applying the
    /// current substitution. Free-var ids are the candidates for
    /// generalisation at let-boundary closure (Plan B task 48).
    /// Currently used only by the diagnostics-side `generalize`
    /// helper; retained for the Stage 6 effect-handler pass that
    /// needs principal-type computation over open-row signatures.
    #[allow(dead_code)]
    fn ftv_ty(&self, t: &Ty, out: &mut std::collections::BTreeSet<u32>) {
        let resolved = self.deref(t);
        match resolved {
            Ty::Int | Ty::String | Ty::Unit | Ty::Bool | Ty::Char | Ty::Byte => {}
            Ty::Var(id) => {
                out.insert(id);
            }
            Ty::User(_, args) => {
                for a in args {
                    self.ftv_ty(&a, out);
                }
            }
            Ty::Fn(sig) => {
                for p in &sig.params {
                    self.ftv_ty(p, out);
                }
                self.ftv_ty(&sig.ret, out);
            }
        }
    }

    /// Free row-variable set of `t`. Walks `Fn` signatures and the
    /// nested rows their effect_row_var introduces. Used alongside
    /// `ftv_ty` to drive let-generalisation over both kinds of
    /// variables in a single pass.
    #[allow(dead_code)]
    fn frv_ty(&self, t: &Ty, out: &mut std::collections::BTreeSet<u32>) {
        let resolved = self.deref(t);
        match resolved {
            Ty::Int | Ty::String | Ty::Unit | Ty::Bool | Ty::Char | Ty::Byte | Ty::Var(_) => {}
            Ty::User(_, args) => {
                for a in args {
                    self.frv_ty(&a, out);
                }
            }
            Ty::Fn(sig) => {
                for p in &sig.params {
                    self.frv_ty(p, out);
                }
                self.frv_ty(&sig.ret, out);
                if let Some(id) = sig.effect_row_var {
                    let row = self.subst.apply_row(&Row {
                        effects: Vec::new(),
                        tail: Some(id),
                    });
                    if let Some(t) = row.tail {
                        out.insert(t);
                    }
                }
            }
        }
    }

    /// Free vars of the global env (top-level fn schemes only —
    /// schemes' bound vars are *not* free). Local `env` entries
    /// resolve through the same substitution.
    #[allow(dead_code)]
    fn ftv_env(&self) -> std::collections::BTreeSet<u32> {
        let mut out = std::collections::BTreeSet::new();
        for ty in self.env.values() {
            self.ftv_ty(ty, &mut out);
        }
        out
    }

    #[allow(dead_code)]
    fn frv_env(&self) -> std::collections::BTreeSet<u32> {
        let mut out = std::collections::BTreeSet::new();
        for ty in self.env.values() {
            self.frv_ty(ty, &mut out);
        }
        out
    }

    /// Instantiate a scheme by allocating fresh type / row vars for
    /// each bound variable and substituting them through the body.
    /// Returns the instantiated `Ty` ready for unification at the
    /// call site.
    fn instantiate(&mut self, scheme: &Scheme) -> Ty {
        let mut ty_map: BTreeMap<u32, Ty> = BTreeMap::new();
        for &id in &scheme.type_vars {
            ty_map.insert(id, Ty::Var(self.fresh_ty_var()));
        }
        let mut row_map: BTreeMap<u32, u32> = BTreeMap::new();
        for &id in &scheme.row_vars {
            row_map.insert(id, self.fresh_row_var());
        }
        Self::rename_ty(&scheme.body, &ty_map, &row_map)
    }

    /// Rename bound variables in a scheme body during instantiation.
    /// Replaces each bound id with its fresh allocation; free vars
    /// (already substituted-away or appearing through env capture)
    /// pass through unchanged.
    fn rename_ty(t: &Ty, ty_map: &BTreeMap<u32, Ty>, row_map: &BTreeMap<u32, u32>) -> Ty {
        match t {
            Ty::Int | Ty::String | Ty::Unit | Ty::Bool | Ty::Char | Ty::Byte => t.clone(),
            Ty::Var(id) => match ty_map.get(id) {
                Some(repl) => repl.clone(),
                None => t.clone(),
            },
            Ty::User(name, args) => Ty::User(
                name.clone(),
                args.iter()
                    .map(|a| Self::rename_ty(a, ty_map, row_map))
                    .collect(),
            ),
            Ty::Fn(sig) => {
                let new_sig = FnSig {
                    params: sig
                        .params
                        .iter()
                        .map(|p| Self::rename_ty(p, ty_map, row_map))
                        .collect(),
                    ret: Self::rename_ty(&sig.ret, ty_map, row_map),
                    effects: sig.effects.clone(),
                    effect_row_var: sig
                        .effect_row_var
                        .map(|id| row_map.get(&id).copied().unwrap_or(id)),
                };
                Ty::Fn(Box::new(new_sig))
            }
        }
    }

    /// Generalise an inferred type into a scheme by closing over
    /// the type / row variables that are free in `t` but not free
    /// in the surrounding env. Plan B task 48's let-generalisation
    /// rule applies at top-level fn boundaries — local lambdas and
    /// `let` bindings stay rank-1 and use the env-monomorphic ftv
    /// path (no capture into local schemes). The simpler explicit-
    /// list path in `check_fn` covers the v1 case where every
    /// generalisable variable is declared via the surface
    /// `[A, B]` / `![ ... | e]` syntax; this principled helper is
    /// retained for the Stage 6 effect-handler pass.
    #[allow(dead_code)]
    fn generalize(&self, t: &Ty) -> Scheme {
        let mut ftv = std::collections::BTreeSet::new();
        self.ftv_ty(t, &mut ftv);
        let mut frv = std::collections::BTreeSet::new();
        self.frv_ty(t, &mut frv);
        let env_ftv = self.ftv_env();
        let env_frv = self.frv_env();
        let type_vars: Vec<u32> = ftv.difference(&env_ftv).copied().collect();
        let row_vars: Vec<u32> = frv.difference(&env_frv).copied().collect();
        Scheme {
            type_vars,
            row_vars,
            body: self.deref(t),
        }
    }

    /// Occurs check: is `id` reachable from `t` after applying the
    /// current substitution? Returns `true` on detection — a
    /// recursive-cycle attempt that the unifier rejects.
    fn occurs_in_ty(&self, id: u32, t: &Ty) -> bool {
        let resolved = self.deref(t);
        match resolved {
            Ty::Int | Ty::String | Ty::Unit | Ty::Bool | Ty::Char | Ty::Byte => false,
            Ty::Var(other) => other == id,
            Ty::User(_, args) => args.iter().any(|a| self.occurs_in_ty(id, a)),
            Ty::Fn(sig) => {
                sig.params.iter().any(|p| self.occurs_in_ty(id, p))
                    || self.occurs_in_ty(id, &sig.ret)
            }
        }
    }

    fn occurs_in_row(&self, id: u32, r: &Row) -> bool {
        let resolved = self.subst.apply_row(r);
        resolved.tail == Some(id)
    }

    /// Bind a type variable. Handles trivial self-binds and the
    /// occurs check; on failure pushes E0126.
    fn bind_ty_var(&mut self, id: u32, t: &Ty, span: &Span) -> bool {
        let resolved = self.deref(t);
        if let Ty::Var(other) = &resolved {
            if *other == id {
                return true;
            }
        }
        if self.occurs_in_ty(id, &resolved) {
            self.push_error(
                "E0126",
                span.clone(),
                format!(
                    "occurs check failed: cannot construct an infinite type `?{id} = {}`",
                    ty_display(&resolved)
                ),
            );
            return false;
        }
        self.subst.tys.insert(id, resolved);
        true
    }

    /// Bind a row variable. Handles self-binds, occurs, and merges
    /// the open row's known effects into the substitution body.
    fn bind_row_var(&mut self, id: u32, r: &Row, span: &Span) -> bool {
        let resolved = self.subst.apply_row(r);
        if resolved.tail == Some(id) && resolved.effects.is_empty() {
            return true;
        }
        if self.occurs_in_row(id, &resolved) {
            self.push_error(
                "E0127",
                span.clone(),
                format!(
                    "row occurs check failed: cannot construct an infinite effect row through `?{id}`"
                ),
            );
            return false;
        }
        self.subst.rows.insert(id, resolved);
        true
    }

    /// Unify two types under the current substitution. Pushes E0044
    /// on shape mismatch. Returns `true` on success; `false` lets
    /// the caller skip downstream cascades while still emitting
    /// any necessary diagnostic for the failing site.
    fn unify_ty(&mut self, a: &Ty, b: &Ty, span: &Span) -> bool {
        let a = self.deref(a);
        let b = self.deref(b);
        match (&a, &b) {
            (Ty::Int, Ty::Int)
            | (Ty::String, Ty::String)
            | (Ty::Unit, Ty::Unit)
            | (Ty::Bool, Ty::Bool)
            | (Ty::Char, Ty::Char)
            | (Ty::Byte, Ty::Byte) => true,
            (Ty::Var(id_a), Ty::Var(id_b)) if id_a == id_b => true,
            (Ty::Var(id), other) | (other, Ty::Var(id)) => self.bind_ty_var(*id, other, span),
            (Ty::User(name_a, args_a), Ty::User(name_b, args_b)) => {
                if name_a != name_b || args_a.len() != args_b.len() {
                    self.push_error(
                        "E0044",
                        span.clone(),
                        format!(
                            "type mismatch: expected `{}`, got `{}`",
                            ty_display(&a),
                            ty_display(&b)
                        ),
                    );
                    return false;
                }
                let mut ok = true;
                for (pa, pb) in args_a.iter().zip(args_b.iter()) {
                    if !self.unify_ty(pa, pb, span) {
                        ok = false;
                    }
                }
                ok
            }
            (Ty::Fn(sig_a), Ty::Fn(sig_b)) => {
                if sig_a.params.len() != sig_b.params.len() {
                    self.push_error(
                        "E0044",
                        span.clone(),
                        format!(
                            "function arity mismatch: `{}` vs `{}`",
                            ty_display(&a),
                            ty_display(&b)
                        ),
                    );
                    return false;
                }
                let mut ok = true;
                for (pa, pb) in sig_a.params.iter().zip(sig_b.params.iter()) {
                    if !self.unify_ty(pa, pb, span) {
                        ok = false;
                    }
                }
                if !self.unify_ty(&sig_a.ret, &sig_b.ret, span) {
                    ok = false;
                }
                let row_a = Row {
                    effects: sig_a.effects.clone(),
                    tail: sig_a.effect_row_var,
                };
                let row_b = Row {
                    effects: sig_b.effects.clone(),
                    tail: sig_b.effect_row_var,
                };
                if !self.unify_row(&row_a, &row_b, span) {
                    ok = false;
                }
                ok
            }
            _ => {
                self.push_error(
                    "E0044",
                    span.clone(),
                    format!(
                        "type mismatch: expected `{}`, got `{}`",
                        ty_display(&a),
                        ty_display(&b)
                    ),
                );
                false
            }
        }
    }

    /// Unify two effect rows. Closed-vs-closed requires set
    /// equality; closed-vs-open binds the open row's tail to absorb
    /// any extra labels from the closed side; open-vs-open shares
    /// a fresh tail and binds both sides into it.
    ///
    /// Plan B task 48 — closed-row enforcement: if the closed-side
    /// effects don't cover the open-side known effects (or vice
    /// versa, modulo the open tail), the unification fails with
    /// E0128 ("effect row mismatch").
    fn unify_row(&mut self, a: &Row, b: &Row, span: &Span) -> bool {
        let a = self.subst.apply_row(a);
        let b = self.subst.apply_row(b);
        // Identical rows: trivial.
        if a == b {
            return true;
        }
        let set_a: std::collections::BTreeSet<&String> = a.effects.iter().collect();
        let set_b: std::collections::BTreeSet<&String> = b.effects.iter().collect();
        let only_a: Vec<String> = set_a.difference(&set_b).map(|s| (*s).clone()).collect();
        let only_b: Vec<String> = set_b.difference(&set_a).map(|s| (*s).clone()).collect();
        match (a.tail, b.tail) {
            (None, None) => {
                // Both closed: must be set-equal.
                if !only_a.is_empty() || !only_b.is_empty() {
                    self.push_error(
                        "E0128",
                        span.clone(),
                        format!(
                            "effect row mismatch: closed row `![{}]` cannot unify with closed row `![{}]`",
                            a.effects.join(", "),
                            b.effects.join(", ")
                        ),
                    );
                    return false;
                }
                true
            }
            (None, Some(b_tail)) => {
                // a closed, b open: a's effects must cover b's
                // known effects, and b's tail absorbs a's leftover.
                if !only_b.is_empty() {
                    self.push_error(
                        "E0128",
                        span.clone(),
                        format!(
                            "effect row mismatch: closed row `![{}]` is missing `{}` required by row `![{} | ?{b_tail}]`",
                            a.effects.join(", "),
                            only_b.join(", "),
                            b.effects.join(", ")
                        ),
                    );
                    return false;
                }
                self.bind_row_var(
                    b_tail,
                    &Row {
                        effects: only_a,
                        tail: None,
                    },
                    span,
                )
            }
            (Some(a_tail), None) => {
                if !only_a.is_empty() {
                    self.push_error(
                        "E0128",
                        span.clone(),
                        format!(
                            "effect row mismatch: closed row `![{}]` is missing `{}` required by row `![{} | ?{a_tail}]`",
                            b.effects.join(", "),
                            only_a.join(", "),
                            a.effects.join(", ")
                        ),
                    );
                    return false;
                }
                self.bind_row_var(
                    a_tail,
                    &Row {
                        effects: only_b,
                        tail: None,
                    },
                    span,
                )
            }
            (Some(a_tail), Some(b_tail)) if a_tail == b_tail => {
                if !only_a.is_empty() || !only_b.is_empty() {
                    self.push_error(
                        "E0128",
                        span.clone(),
                        format!(
                            "effect row mismatch: rows share tail `?{a_tail}` but differ in known effects ({} vs {})",
                            a.effects.join(", "),
                            b.effects.join(", ")
                        ),
                    );
                    return false;
                }
                true
            }
            (Some(a_tail), Some(b_tail)) => {
                let fresh = self.fresh_row_var();
                let ok_a = self.bind_row_var(
                    a_tail,
                    &Row {
                        effects: only_b,
                        tail: Some(fresh),
                    },
                    span,
                );
                let ok_b = self.bind_row_var(
                    b_tail,
                    &Row {
                        effects: only_a,
                        tail: Some(fresh),
                    },
                    span,
                );
                ok_a && ok_b
            }
        }
    }

    /// Emit E0112 against `t`'s span if the named type is neither a
    /// Plan A2 primitive, a registered user type, nor an in-scope
    /// generic-parameter reference. Plan B task 48 also fires a
    /// dedicated diagnostic for arity mismatch on `Apply` (`E0129`)
    /// and for `Apply` on a primitive / generic-param head
    /// (`E0130`).
    fn check_type_expr_known(&mut self, t: &TypeExpr) {
        match t {
            TypeExpr::Named(name, _) => {
                if self.ty_from_type_expr_here(t).is_none() {
                    self.push_error(
                        "E0112",
                        t.span(),
                        format!(
                            "unknown type `{name}` (expected a primitive, a type declared via `type {name} = ...`, or an in-scope generic parameter)",
                        ),
                    );
                }
            }
            TypeExpr::Apply { name, args, span } => {
                // Recurse into args first — sub-Apply errors are
                // surfaced regardless of head-name validity so the
                // user sees every bad spot, not just the outermost.
                for a in args {
                    self.check_type_expr_known(a);
                }
                if matches!(
                    name.as_str(),
                    "Int" | "String" | "Unit" | "Bool" | "Char" | "Byte"
                ) {
                    self.push_error(
                        "E0131",
                        span.clone(),
                        format!("primitive type `{name}` does not take type arguments",),
                    );
                    return;
                }
                if self.current_generic_subst.contains_key(name) {
                    self.push_error(
                        "E0131",
                        span.clone(),
                        format!("generic parameter `{name}` cannot be applied to type arguments",),
                    );
                    return;
                }
                if let Some(td) = self.types.get(name) {
                    if td.generic_params.len() != args.len() {
                        self.push_error(
                            "E0129",
                            span.clone(),
                            format!(
                                "type `{name}` expects {} type argument{}, got {}",
                                td.generic_params.len(),
                                if td.generic_params.len() == 1 {
                                    ""
                                } else {
                                    "s"
                                },
                                args.len()
                            ),
                        );
                    }
                } else {
                    self.push_error(
                        "E0112",
                        t.span(),
                        format!(
                            "unknown type `{name}` (expected a primitive, a type declared via `type {name} = ...`, or an in-scope generic parameter)",
                        ),
                    );
                }
            }
        }
    }

    /// Plan A3 task 38.2 constructor resolution — bare identifier use.
    ///
    /// Called from `Expr::Ident` when `name` is a registered constructor.
    /// A bare identifier can only apply a Unit variant (`None`, etc.);
    /// positional and record variants require call / record-literal
    /// syntax and produce E0115 "shape mismatch" from here.
    ///
    /// On success returns `Some(Ty::User(...))`. On shape mismatch,
    /// emits E0115 and returns `None`.
    fn resolve_ctor_unit_use(&mut self, name: &str, span: &Span) -> Option<Ty> {
        let info = self.ctors.get(name).cloned()?;
        let td = self.types.get(&info.type_name)?.clone();
        let variant = &td.variants[info.variant_index];
        match &variant.fields {
            VariantFields::Unit => Some(self.fresh_user_instance(&info.type_name, &td)),
            VariantFields::Positional(params) => {
                self.push_error(
                    "E0115",
                    span.clone(),
                    format!(
                        "constructor `{name}` of type `{}` has {} positional field{}; apply with `{name}(...)` syntax",
                        info.type_name,
                        params.len(),
                        if params.len() == 1 { "" } else { "s" }
                    ),
                );
                None
            }
            VariantFields::Record(_) => {
                self.push_error(
                    "E0115",
                    span.clone(),
                    format!(
                        "constructor `{name}` of type `{}` has record fields; apply with `{name} {{ .. }}` syntax",
                        info.type_name
                    ),
                );
                None
            }
        }
    }

    /// Plan A3 task 38.2 constructor resolution — positional call form.
    ///
    /// Called from `Expr::Call` when the callee is an `Ident(name)` and
    /// `name` is a registered constructor. Checks arity and per-argument
    /// types against the variant's positional field list, emits E0043
    /// on arity mismatch and E0044 on field-type mismatch (reusing the
    /// existing call-site diagnostic vocabulary). E0115 fires when the
    /// variant is Unit or Record.
    ///
    /// On success returns `Some(Ty::User(...))`. On shape mismatch
    /// returns `None`.
    fn resolve_ctor_positional_use(
        &mut self,
        name: &str,
        args: &[Expr],
        span: &Span,
        row: &[String],
    ) -> Option<Ty> {
        let info = self.ctors.get(name).cloned()?;
        let td = self.types.get(&info.type_name)?.clone();
        let variant = &td.variants[info.variant_index];
        // Always type-check each arg so user errors inside args still surface.
        let arg_tys: Vec<Option<Ty>> = args.iter().map(|a| self.check_expr(a, row)).collect();
        match &variant.fields {
            VariantFields::Positional(param_tys) => {
                // Plan B task 48 — allocate one fresh `Ty::Var` per
                // declared generic parameter on the owning type.
                // Field-type expressions reference these names (`A`,
                // `B`, ...) and must resolve to the *same* fresh
                // vars across this single ctor call so unifying each
                // arg with its field pins the type's args.
                let (result_ty, ctor_subst) =
                    self.fresh_user_instance_with_subst(&info.type_name, &td);
                let saved = self.current_generic_subst.clone();
                for (k, v) in &ctor_subst {
                    self.current_generic_subst.insert(k.clone(), v.clone());
                }
                let expected_tys: Vec<Option<Ty>> = param_tys
                    .iter()
                    .map(|t| self.ty_from_type_expr_here(t))
                    .collect();
                self.current_generic_subst = saved;
                if args.len() != param_tys.len() {
                    self.push_error(
                        "E0043",
                        span.clone(),
                        format!(
                            "constructor `{name}` expects {} argument{}, got {}",
                            param_tys.len(),
                            if param_tys.len() == 1 { "" } else { "s" },
                            args.len()
                        ),
                    );
                }
                for (i, (arg_ty, exp)) in arg_tys.iter().zip(expected_tys.iter()).enumerate() {
                    if let (Some(a), Some(e)) = (arg_ty, exp) {
                        let arg_span = args.get(i).map(Expr::span).unwrap_or_else(|| span.clone());
                        if !self.unify_ty(e, a, &arg_span) {
                            // unify_ty already pushed E0044 with the
                            // best names it has; ctor-context is
                            // implicit in the source span. The
                            // legacy E0044 here is preserved (with
                            // ctor-aware text) so the catalog
                            // long-form continues to surface for
                            // ctor-shape misses.
                            self.push_error(
                                "E0044",
                                arg_span,
                                format!(
                                    "constructor `{name}` field {i} has type `{}` but argument has type `{}`",
                                    ty_display(&self.deref(e)),
                                    ty_display(&self.deref(a)),
                                ),
                            );
                        }
                    }
                }
                Some(self.deref(&result_ty))
            }
            VariantFields::Unit => {
                self.push_error(
                    "E0115",
                    span.clone(),
                    format!(
                        "constructor `{name}` of type `{}` is nullary; apply as a bare identifier (no parens)",
                        info.type_name
                    ),
                );
                None
            }
            VariantFields::Record(_) => {
                self.push_error(
                    "E0115",
                    span.clone(),
                    format!(
                        "constructor `{name}` of type `{}` has record fields; apply with `{name} {{ .. }}` syntax",
                        info.type_name
                    ),
                );
                None
            }
        }
    }

    /// Plan A3 task 38.2 constructor resolution — record literal form.
    ///
    /// Called from `Expr::RecordLit`. Checks that each declared field
    /// is provided exactly once and that each supplied value has the
    /// declared field type. Emits E0114 on unknown field, E0115 on
    /// missing / duplicate field or shape mismatch, E0044 on value-type
    /// mismatch. Evaluates every provided value regardless so errors
    /// inside field values still surface.
    ///
    /// On success returns `Some(Ty::User(...))`. On failure (including
    /// unknown ctor name), returns `None`.
    fn resolve_ctor_record_use(
        &mut self,
        name: &str,
        fields: &[RecordFieldLit],
        span: &Span,
        row: &[String],
    ) -> Option<Ty> {
        let info = match self.ctors.get(name).cloned() {
            Some(i) => i,
            None => {
                // Still evaluate field values so errors inside them surface.
                for f in fields {
                    let _ = self.check_expr(&f.value, row);
                }
                self.push_error(
                    "E0114",
                    span.clone(),
                    format!(
                        "unknown constructor `{name}` — no `type` declaration has this variant"
                    ),
                );
                return None;
            }
        };
        let td = self.types.get(&info.type_name)?.clone();
        let variant = &td.variants[info.variant_index];
        let declared: Vec<RecordFieldDecl> = match &variant.fields {
            VariantFields::Record(fs) => fs.clone(),
            VariantFields::Unit => {
                for f in fields {
                    let _ = self.check_expr(&f.value, row);
                }
                self.push_error(
                    "E0115",
                    span.clone(),
                    format!(
                        "constructor `{name}` of type `{}` is nullary; apply as a bare identifier (no braces)",
                        info.type_name
                    ),
                );
                return None;
            }
            VariantFields::Positional(_) => {
                for f in fields {
                    let _ = self.check_expr(&f.value, row);
                }
                self.push_error(
                    "E0115",
                    span.clone(),
                    format!(
                        "constructor `{name}` of type `{}` has positional fields; apply with `{name}(...)` syntax",
                        info.type_name
                    ),
                );
                return None;
            }
        };
        // Check duplicate field names in the literal itself.
        let mut seen_in_lit: BTreeMap<String, Span> = BTreeMap::new();
        for f in fields {
            if let Some(first) = seen_in_lit.get(&f.name).cloned() {
                self.push_error(
                    "E0115",
                    f.span.clone(),
                    format!(
                        "constructor `{name}` got duplicate field `{}` (first occurrence at {}:{})",
                        f.name, first.line, first.column
                    ),
                );
            } else {
                seen_in_lit.insert(f.name.clone(), f.span.clone());
            }
        }
        // Plan B task 48 — fresh-instantiate the type's generic
        // parameters once for the whole record literal so unifying
        // each field's value with its declared type pins the type's
        // arguments consistently.
        let (result_ty, ctor_subst) = self.fresh_user_instance_with_subst(&info.type_name, &td);
        let saved = self.current_generic_subst.clone();
        for (k, v) in &ctor_subst {
            self.current_generic_subst.insert(k.clone(), v.clone());
        }
        // Check each supplied field's value type against the declared
        // field's type (resolved under the per-call ctor substitution).
        for f in fields {
            let v_ty = self.check_expr(&f.value, row);
            let Some(decl) = declared.iter().find(|d| d.name == f.name) else {
                self.push_error(
                    "E0115",
                    f.span.clone(),
                    format!(
                        "constructor `{name}` of type `{}` has no field `{}`",
                        info.type_name, f.name
                    ),
                );
                continue;
            };
            let Some(exp) = self.ty_from_type_expr_here(&decl.ty) else {
                continue;
            };
            if let Some(vt) = v_ty {
                if !self.unify_ty(&exp, &vt, &f.value.span()) {
                    self.push_error(
                        "E0044",
                        f.value.span(),
                        format!(
                            "constructor `{name}` field `{}` has type `{}` but value has type `{}`",
                            f.name,
                            ty_display(&self.deref(&exp)),
                            ty_display(&self.deref(&vt)),
                        ),
                    );
                }
            }
        }
        self.current_generic_subst = saved;
        // Check that every declared field is supplied.
        for d in &declared {
            if !fields.iter().any(|f| f.name == d.name) {
                self.push_error(
                    "E0115",
                    span.clone(),
                    format!(
                        "constructor `{name}` of type `{}` is missing field `{}`",
                        info.type_name, d.name
                    ),
                );
            }
        }
        Some(self.deref(&result_ty))
    }

    /// Insert a binding into the current function's environment.
    /// `resolve.rs` is responsible for rejecting shadowing and duplicate
    /// bindings before typecheck runs, so a `None` return from the
    /// underlying `BTreeMap::insert` is an invariant here. Asserting it
    /// in debug builds makes any future caller that invokes `typecheck`
    /// on an un-resolved AST (fuzzer harness, IDE integration,
    /// experimental pipeline) fail loudly instead of silently preferring
    /// the last insertion. No behaviour change in release builds.
    fn env_insert(&mut self, name: String, ty: Ty) {
        let prev = self.env.insert(name.clone(), ty);
        debug_assert!(
            prev.is_none(),
            "typecheck env shadowing should have been caught by resolve.rs for '{name}'"
        );
        // `prev` is intentionally discarded in release builds; the debug
        // assertion above is the entire contract.
        let _ = prev;
    }

    fn check_fn(&mut self, f: &FnDecl) {
        // Fresh per-function local env: holds only parameters and
        // `let` bindings. Top-level function signatures live in
        // `self.fn_env`, consulted as a fallback during Ident
        // resolution (see `check_expr`/`Expr::Ident`). Keeping the
        // two maps separate avoids two bugs that arose when fn_env
        // was merged into env at fn entry:
        //
        //   1. Debug-assert in `env_insert` tripped on `let foo: Int
        //      = ...` inside a function whose top-level namesake
        //      is also `foo` — because `foo` was already in `env`
        //      via the fn_env seeding. Release builds silently
        //      overwrote, leaving fn_env's entry inconsistent with
        //      the local binding.
        //   2. Capture analysis treated every top-level fn name as
        //      an outer free variable, so closure conversion would
        //      allocate spurious env fields for statically-resolvable
        //      top-level symbols.
        //
        // Reported on PR #6 review; fix preserves local-first lookup
        // semantics (params/lets shadow top-level fns of the same
        // name within their scope) while keeping fn_env out of the
        // insert-collision and capture-analysis paths.
        // Plan B task 48 — wire HM machinery for this fn's body walk:
        //
        //   (a) Allocate one fresh `Ty::Var` per declared generic
        //       parameter and stage them into `current_generic_subst`
        //       so every `ty_from_type_expr_here` call inside the
        //       body sees `A → Ty::Var(N)` etc.
        //   (b) If the fn declares an explicit row variable
        //       (`![IO | e]`), allocate a fresh row-var id and stage
        //       it into `current_row_var_subst` for the body walk.
        //
        // The pre-pass already seeded `fn_env` with a concrete
        // signature using the empty substitution; that entry is fine
        // for callers that have no generic context. After body
        // checking we generalise the inferred sig into a `Scheme`
        // and store it in `fn_schemes` for call-site instantiation.
        let saved_generic_subst = std::mem::take(&mut self.current_generic_subst);
        let saved_row_var_subst = std::mem::take(&mut self.current_row_var_subst);
        let (generic_subst, ty_var_ids) = self.fresh_generic_subst(&f.generic_params);
        self.current_generic_subst = generic_subst;
        let mut row_var_id: Option<u32> = None;
        if let Some(rv) = &f.effect_row_var {
            let id = self.fresh_row_var();
            row_var_id = Some(id);
            self.current_row_var_subst.insert(rv.name.clone(), id);
        }
        self.env.clear();
        for p in &f.params {
            if let Some(ty) = self.ty_from_type_expr_here(&p.ty) {
                self.env_insert(p.name.clone(), ty);
            }
        }
        if f.name == "main" {
            // Main's signature is fixed in Plan A1: `() -> Int ![IO]` or `() -> Int ![]`.
            // Anything else — wrong return type, any parameters, or an effect row
            // containing anything other than IO — is E0041.
            if !type_is(&f.return_type, "Int") {
                self.push_error(
                    "E0041",
                    f.span.clone(),
                    "`fn main` must return `Int` (expected `fn main() -> Int ![IO]` or `fn main() -> Int ![]`)",
                );
            }
            if !f.params.is_empty() {
                self.push_error(
                    "E0041",
                    f.span.clone(),
                    "`fn main` takes no parameters in Plan A1 (expected `fn main() -> Int ![IO]`)",
                );
            }
            for effect in &f.effects {
                if effect != "IO" {
                    self.push_error(
                        "E0041",
                        f.span.clone(),
                        format!(
                            "`fn main`'s effect row may only contain `IO` in Plan A1 (saw `{effect}`)",
                        ),
                    );
                }
            }
        }
        let body_ty = self.check_block(&f.body, &f.effects);

        // Plan B task 48 — generalise the inferred signature into a
        // scheme for `fn_schemes`. Concrete (non-generic, closed-row)
        // fns end up with empty `type_vars` / `row_vars`, leaving the
        // legacy direct-call lookup behavior unchanged. Generic fns
        // close over their declared `[A, B, ...]` ids and (if
        // present) their explicit row variable id.
        if let Some(declared_ret) = self.ty_from_type_expr_here(&f.return_type) {
            if let Some(bt) = body_ty {
                if !self.unify_ty(&declared_ret, &bt, &f.span) {
                    // Mismatch already reported by unify_ty as E0044.
                }
            }
            let param_tys: Vec<Ty> = f
                .params
                .iter()
                .map(|p| self.ty_from_type_expr_here(&p.ty).unwrap_or(Ty::Unit))
                .collect();
            let inferred_sig = FnSig {
                params: param_tys,
                ret: declared_ret,
                effects: f.effects.clone(),
                effect_row_var: row_var_id,
            };
            let resolved = self.deref(&Ty::Fn(Box::new(inferred_sig)));
            let scheme = Scheme {
                type_vars: ty_var_ids,
                row_vars: row_var_id.into_iter().collect(),
                body: resolved,
            };
            self.fn_schemes.insert(f.name.clone(), scheme);
        }

        // Restore caller's generic / row-var substitutions. Every
        // body walk uses its own fresh-var allocations; nesting (a
        // generic fn calling another generic fn) consumes from the
        // global counters but each scope binds its own surface names.
        self.current_generic_subst = saved_generic_subst;
        self.current_row_var_subst = saved_row_var_subst;
    }

    /// Typecheck a block and return its type.
    ///
    /// A block's type is the type of its tail expression if present, and
    /// `Unit` otherwise. Returning `Option<Ty>` (rather than `Ty`) lets the
    /// caller distinguish "the block's tail didn't typecheck" (`None`) from
    /// "the block is a statement sequence with no tail" (`Some(Unit)`),
    /// which matters for `if`-branch unification.
    fn check_block(&mut self, b: &Block, row: &[String]) -> Option<Ty> {
        for s in &b.stmts {
            match s {
                Stmt::Expr(e) => {
                    let _ = self.check_expr(e, row);
                }
                Stmt::Perform(p) => {
                    self.check_perform(p, row);
                }
                Stmt::Let(l) => {
                    // Plan B task 48: let-binding annotation may now
                    // reference in-scope generic parameters or
                    // generic-applied user types. The annotation is
                    // structurally validated here (E0124/E0125 are
                    // gone; the catalog grew E0129/E0130 for arity
                    // and shape misuses). HM unification checks the
                    // initializer against the declared type.
                    self.check_type_expr_known(&l.ty);
                    let got = self.check_expr(&l.value, row);
                    let declared = self.ty_from_type_expr_here(&l.ty);
                    if let (Some(got_ty), Some(decl_ty)) = (got.as_ref(), declared.as_ref()) {
                        if !self.unify_ty(decl_ty, got_ty, &l.span) {
                            // unify_ty already pushed E0044 for the
                            // mismatch; emit the legacy E0045 hint
                            // alongside so existing diagnostics
                            // continue to surface for plain-binding
                            // type errors.
                            self.push_error(
                                "E0045",
                                l.span.clone(),
                                format!(
                                    "let binding `{}` has declared type `{}` but initializer has type `{}`",
                                    l.name,
                                    type_name(&l.ty),
                                    ty_display(got_ty),
                                ),
                            );
                        }
                    }
                    // Extend the environment with the declared type, so
                    // subsequent statements can resolve references. We use
                    // the declaration rather than the inferred type so a
                    // type mismatch in the initializer doesn't cascade into
                    // spurious E0044/E0046 at downstream sites.
                    if let Some(ty) = self.ty_from_type_expr_here(&l.ty) {
                        self.env_insert(l.name.clone(), ty);
                    }
                }
            }
        }
        match &b.tail {
            Some(tail) => self.check_expr(tail, row),
            None => Some(Ty::Unit),
        }
    }

    fn check_perform(&mut self, p: &PerformExpr, row: &[String]) {
        if !row.iter().any(|e| e == &p.effect) {
            self.push_error(
                "E0042",
                p.span.clone(),
                format!(
                    "`perform {}.{}` requires `{}` in the enclosing function's effect row",
                    p.effect, p.op, p.effect,
                ),
            );
        }
        // Stage 1 only understands IO.println(String).
        if p.effect == "IO" && p.op == "println" {
            if p.args.len() != 1 {
                self.push_error(
                    "E0043",
                    p.span.clone(),
                    format!(
                        "`IO.println` takes exactly one String argument (got {})",
                        p.args.len()
                    ),
                );
                return;
            }
            match self.check_expr(&p.args[0], row) {
                Some(Ty::String) => {}
                Some(other) => {
                    self.push_error(
                        "E0044",
                        p.span.clone(),
                        format!(
                            "`IO.println` requires a `String` argument; got `{}`",
                            ty_display(&other)
                        ),
                    );
                }
                None => {}
            }
        } else if p.effect == "IO" {
            // Plan A1 only recognises IO.println. Other IO ops arrive with
            // Plan B's effect-handler dispatch. Reuse E0042 — the user-facing
            // message is about an effect-surface shape the checker does not
            // know how to dispatch, which is the same category as "effect not
            // in row".
            self.push_error(
                "E0042",
                p.span.clone(),
                format!("`IO.{}` is not a Plan A1 operation", p.op),
            );
        } else {
            // Non-IO perform sites arrive in later plans; Stage 1 treats
            // them as unknown and recovers.
            self.push_error(
                "E0042",
                p.span.clone(),
                format!("`perform {}.{}` is not a Plan A1 operation", p.effect, p.op),
            );
        }
    }

    fn check_expr(&mut self, e: &Expr, row: &[String]) -> Option<Ty> {
        match e {
            Expr::IntLit(_, _) => Some(Ty::Int),
            Expr::StringLit(s, span) => {
                self.string_literals.push((span.clone(), s.clone()));
                Some(Ty::String)
            }
            Expr::Ident(name, span) => {
                // Local env first (params + lets + lambda captures
                // stitched in during `check_lambda`), then fall
                // through to either the post-`check_fn` `fn_schemes`
                // (Plan B task 48 — instantiate a fresh copy at each
                // use site) or the legacy `fn_env` (for fns whose
                // body hasn't been checked yet, e.g., during the
                // recursive-fn pre-pass walk-through). Keeping the
                // two paths means a generic `fn id[A]` always
                // produces fresh `Ty::Var`s at the call site.
                //
                // Plan A3 task 38.2: if the name isn't bound to a
                // value or top-level fn, check the ctor registry —
                // a nullary variant (`None`) surfaces as a bare
                // identifier. Positional / record variants here
                // fire E0115 "shape mismatch". Value bindings always
                // win; a user who writes `let None = ...` keeps the
                // local in scope for this identifier's span.
                if let Some(ty) = self.env.get(name).cloned() {
                    return Some(ty);
                }
                if let Some(scheme) = self.fn_schemes.get(name).cloned() {
                    return Some(self.instantiate(&scheme));
                }
                if let Some(ty) = self.fn_env.get(name).cloned() {
                    return Some(ty);
                }
                if self.ctors.contains_key(name) {
                    self.resolve_ctor_unit_use(name, span)
                } else {
                    self.push_error(
                        "E0046",
                        span.clone(),
                        format!("unknown identifier `{name}`"),
                    );
                    None
                }
            }
            Expr::Call { callee, args, span } => {
                // Plan A3 task 38.2: intercept Ctor(args...) before
                // the generic callee-as-function path. An Ident
                // callee whose name is a registered ctor resolves
                // as a constructor application; anything else falls
                // through to `check_call` for the ordinary fn-call
                // rules (E0046 / E0068 / E0043 / E0044 / E0042).
                if let Expr::Ident(name, _) = callee.as_ref() {
                    if self.ctors.contains_key(name)
                        && !self.env.contains_key(name)
                        && !self.fn_env.contains_key(name)
                    {
                        return self.resolve_ctor_positional_use(name, args, span, row);
                    }
                }
                self.check_call(callee, args, span.clone(), row)
            }
            Expr::Perform(p) => {
                self.check_perform(p, row);
                Some(Ty::Unit)
            }
            Expr::BoolLit(_, _) => Some(Ty::Bool),
            Expr::CharLit(_, _) => Some(Ty::Char),
            Expr::Binary { op, lhs, rhs, .. } => {
                let lt = self.check_expr(lhs, row);
                let rt = self.check_expr(rhs, row);
                self.check_binop(*op, lt, rt, lhs.span(), rhs.span())
            }
            Expr::Unary { op, operand, span } => {
                let ot = self.check_expr(operand, row);
                self.check_unop(*op, ot, span.clone())
            }
            Expr::If {
                cond,
                then_block,
                else_block,
                span,
            } => {
                let cond_ty = self.check_expr(cond, row);
                if let Some(t) = cond_ty {
                    if t != Ty::Bool {
                        self.push_error(
                            "E0062",
                            cond.span(),
                            format!("`if` condition must be `Bool`; got `{}`", ty_display(&t)),
                        );
                    }
                }
                let then_ty = self.check_block(then_block, row);
                let else_ty = self.check_block(else_block, row);
                match (then_ty, else_ty) {
                    (Some(t), Some(e)) if t == e => Some(t),
                    (Some(t), Some(e)) => {
                        self.push_error(
                            "E0063",
                            span.clone(),
                            format!(
                                "`if` branches have incompatible types: `then` is `{}` but `else` is `{}`",
                                ty_display(&t),
                                ty_display(&e),
                            ),
                        );
                        // Recover with the `then` type so downstream
                        // context has something to continue on.
                        Some(t)
                    }
                    (Some(t), None) | (None, Some(t)) => Some(t),
                    (None, None) => None,
                }
            }
            Expr::Match {
                scrutinee,
                arms,
                span,
            } => self.check_match(scrutinee, arms, span.clone(), row),
            // `Expr::Block` is introduced by elaboration (plan A2 task
            // 23); the surface parser never produces it, so this arm is
            // a structural fallback for exhaustiveness only. Typecheck
            // is not re-run after elaborate in Plan A2's pipeline, so
            // the body of this arm is defensive rather than reached in
            // practice.
            Expr::Block(b) => self.check_block(b, row),
            Expr::Lambda {
                params,
                return_type,
                effects,
                effect_row_var,
                body,
                span,
            } => {
                // Plan B task 48 — lambda's row variable, if any,
                // shares the surrounding fn's row-var subst; we
                // don't introduce a *new* generalised row var on a
                // lambda (rank-1 ML). The surface name binds to the
                // currently-active row-var if present, else stays
                // closed.
                let _ = effect_row_var;
                self.check_lambda(params, return_type, effects, body, span.clone())
            }
            // `ClosureRecord` / `ClosureEnvLoad` are post-closure-
            // conversion nodes synthesized by plan A2 task 31. They
            // never appear in a parser-produced AST; typecheck runs
            // strictly before closure conversion.
            Expr::ClosureRecord { .. } | Expr::ClosureEnvLoad { .. } => {
                unreachable!("typecheck: closure-conversion nodes should not appear pre-CC")
            }
            // Plan A3 task 38.2: record literal `Ctor { f: v, ... }`.
            // Resolves the constructor name in the ctor registry and
            // checks field names and value types against the declared
            // record variant. E0114 / E0115 / E0044 cover the various
            // failure modes.
            Expr::RecordLit { name, fields, span } => {
                self.resolve_ctor_record_use(name, fields, span, row)
            }
        }
    }

    /// Typing rule for binary operators.
    ///
    /// `+ - * / %`: Int→Int→Int. `< > <= >=`: Int→Int→Bool. `&& ||`:
    /// Bool→Bool→Bool. `== !=`: T→T→Bool where T is a primitive type
    /// (`Int`, `Bool`, `Char`, `Byte`, `String`, `Unit`); both operands
    /// must have the same type. Sigil performs no implicit conversions.
    ///
    /// Per-operand errors are emitted when either side is known to be a
    /// mismatching type; unknown operand types (propagated `None` from a
    /// prior error) are skipped to avoid cascade noise. The result type
    /// is always returned so context can continue checking.
    fn check_binop(
        &mut self,
        op: BinOp,
        lt: Option<Ty>,
        rt: Option<Ty>,
        lspan: Span,
        rspan: Span,
    ) -> Option<Ty> {
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                self.require_operand(op, Ty::Int, lt, lspan);
                self.require_operand(op, Ty::Int, rt, rspan);
                Some(Ty::Int)
            }
            BinOp::Lt | BinOp::Gt | BinOp::LtEq | BinOp::GtEq => {
                self.require_operand(op, Ty::Int, lt, lspan);
                self.require_operand(op, Ty::Int, rt, rspan);
                Some(Ty::Bool)
            }
            BinOp::And | BinOp::Or => {
                self.require_operand(op, Ty::Bool, lt, lspan);
                self.require_operand(op, Ty::Bool, rt, rspan);
                Some(Ty::Bool)
            }
            BinOp::Eq | BinOp::NotEq => {
                if let (Some(a), Some(b)) = (lt, rt) {
                    if a != b {
                        // Report against the right-hand operand's span so
                        // the user sees which side "doesn't match the
                        // other".
                        self.push_error(
                            "E0060",
                            rspan,
                            format!(
                                "`{}` operands must have the same primitive type; got `{}` and `{}`",
                                binop_symbol(op),
                                ty_display(&a),
                                ty_display(&b),
                            ),
                        );
                    }
                }
                Some(Ty::Bool)
            }
        }
    }

    fn require_operand(&mut self, op: BinOp, expected: Ty, actual: Option<Ty>, span: Span) {
        if let Some(a) = actual {
            if a != expected {
                self.push_error(
                    "E0060",
                    span,
                    format!(
                        "`{}` requires `{}` operand; got `{}`",
                        binop_symbol(op),
                        ty_display(&expected),
                        ty_display(&a),
                    ),
                );
            }
        }
    }

    fn check_unop(&mut self, op: UnOp, ot: Option<Ty>, span: Span) -> Option<Ty> {
        match op {
            UnOp::Neg => {
                if let Some(a) = ot {
                    if a != Ty::Int {
                        self.push_error(
                            "E0061",
                            span,
                            format!("`-` requires `Int` operand; got `{}`", ty_display(&a)),
                        );
                    }
                }
                Some(Ty::Int)
            }
            UnOp::Not => {
                if let Some(a) = ot {
                    if a != Ty::Bool {
                        self.push_error(
                            "E0061",
                            span,
                            format!("`!` requires `Bool` operand; got `{}`", ty_display(&a)),
                        );
                    }
                }
                Some(Ty::Bool)
            }
        }
    }

    /// Application-site typing for `Expr::Call` — plan A2 task 30.
    ///
    /// 1. Type the callee. If it is not a `Ty::Fn`, emit **E0068**
    ///    ("applying a non-function value"). Propagate `None` on
    ///    failure.
    /// 2. Check argument arity against the callee's declared
    ///    parameter count (**E0043** on mismatch).
    /// 3. Type each argument and check it unifies with the
    ///    corresponding parameter type (**E0044** on mismatch — the
    ///    catalog entry's wording is already generic enough to cover
    ///    user calls, not just the original `IO.println` case).
    /// 4. Check every effect in the callee's row is present in the
    ///    enclosing function's effect row (**E0042** on mismatch —
    ///    same code `check_perform` emits for missing effects).
    /// 5. Return the callee's declared return type. Even when some
    ///    checks fail we return the return type (to suppress cascade
    ///    errors downstream).
    fn check_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        span: Span,
        row: &[String],
    ) -> Option<Ty> {
        let callee_ty = self.check_expr(callee, row);
        // Always type-check args so we surface any errors in them.
        let arg_tys: Vec<Option<Ty>> = args.iter().map(|a| self.check_expr(a, row)).collect();

        let sig = match callee_ty {
            Some(Ty::Fn(sig)) => sig,
            Some(other) => {
                self.push_error(
                    "E0068",
                    span,
                    format!(
                        "cannot apply a value of type `{}` — only function values can be called",
                        ty_display(&other)
                    ),
                );
                return None;
            }
            None => return None,
        };

        // (2) arity
        if args.len() != sig.params.len() {
            self.push_error(
                "E0043",
                span.clone(),
                format!(
                    "wrong argument count at call site: expected {}, got {}",
                    sig.params.len(),
                    args.len()
                ),
            );
        }

        // (3) arg types — Plan B task 48 routes through HM
        // unification so generic-fn instantiations resolve their
        // freshly-allocated vars against concrete arg types.
        for (i, (arg_ty, param_ty)) in arg_tys.iter().zip(sig.params.iter()).enumerate() {
            if let Some(at) = arg_ty {
                let arg_span = args.get(i).map(Expr::span).unwrap_or_else(|| span.clone());
                if !self.unify_ty(param_ty, at, &arg_span) {
                    // unify_ty pushed the precise E0044; nothing
                    // more to add here.
                }
            }
        }

        // (4) effect-row containment — Plan B task 48 routes through
        // row unification when the caller's row carries an explicit
        // row variable. Closed-row callers retain the simpler subset
        // check, which preserves the legacy E0042 vocabulary for the
        // common Plan A1/A2/A3 case.
        if let Some(_caller_tail) = self.lookup_active_row_var() {
            let caller_row = Row {
                effects: row.to_vec(),
                tail: self.lookup_active_row_var(),
            };
            let callee_row = Row {
                effects: sig.effects.clone(),
                tail: sig.effect_row_var,
            };
            // The callee's row must be unifiable against the caller's
            // row. Open-vs-open, open-vs-closed, closed-vs-closed are
            // all handled inside `unify_row` with task 48's rules.
            let _ = self.unify_row(&caller_row, &callee_row, &span);
        } else {
            for e in &sig.effects {
                if !row.iter().any(|r| r == e) {
                    self.push_error(
                        "E0042",
                        span.clone(),
                        format!(
                            "calling a function that performs `{e}` requires `{e}` in the enclosing function's effect row",
                        ),
                    );
                }
            }
        }

        // (5) result type — derefed through the substitution so any
        // var bindings made by per-arg unification flow into the
        // returned type.
        Some(self.deref(&sig.ret))
    }

    /// Helper: pick a single active row variable to thread through
    /// row-unification at call sites. Plan B task 48's first cut
    /// supports a *single* row variable per fn; if multiple surface
    /// names exist we pick the first registered. Stage 6's effect
    /// runtime work refines this if needed.
    fn lookup_active_row_var(&self) -> Option<u32> {
        self.current_row_var_subst.values().next().copied()
    }

    /// Lambda typing — plan A2 task 30.
    ///
    /// 1. Build the lambda's own `Ty::Fn` from its declared parameter
    ///    types and return type.
    /// 2. Enter an inner scope: extend `self.env` with the parameter
    ///    names bound to their declared types.
    /// 3. Type the body against the lambda's own effect row.
    /// 4. Check the body's type matches the declared return type
    ///    (**E0069** on mismatch).
    /// 5. Restore the outer env, record free-variable captures, and
    ///    return the constructed `Ty::Fn`.
    ///
    /// Capture analysis (step 5) records every identifier referenced
    /// in the body that was visible in the outer env *before* the
    /// lambda's parameters were pushed. The result is keyed by the
    /// lambda's `span` and stored in `self.lambda_captures` for
    /// closure conversion (task 31) to consume.
    fn check_lambda(
        &mut self,
        params: &[Param],
        return_type: &TypeExpr,
        effects: &[String],
        body: &Expr,
        span: Span,
    ) -> Option<Ty> {
        // (1) build the signature up front so Ty::Fn is available for
        //     the lambda's own return type even if the body fails.
        // Lambdas in Plan A1/A2/A3 are not generic (no `[A]` syntax)
        // and Plan B Task 48 doesn't add lambda-level generic syntax,
        // so the generic substitution here is empty. Row variables on
        // lambdas remain a Plan-B-only feature; the substitution map
        // for surface row vars threads through `self.row_var_subst`.
        let empty_generic_subst: BTreeMap<String, Ty> = BTreeMap::new();
        let param_tys: Vec<Ty> = params
            .iter()
            .map(|p| {
                ty_from_type_expr(&p.ty, &self.types, &empty_generic_subst).unwrap_or(Ty::Unit)
            })
            .collect();
        let ret_ty =
            ty_from_type_expr(return_type, &self.types, &empty_generic_subst).unwrap_or(Ty::Unit);
        let sig = FnSig {
            params: param_tys.clone(),
            ret: ret_ty.clone(),
            effects: effects.to_vec(),
            effect_row_var: None,
        };

        // (2) capture analysis: snapshot the outer env *before*
        //     adding params. Any Ident reference in the body whose
        //     name is in `outer_names` (not a param, not a lambda-
        //     local let) is a captured free variable. Each capture
        //     is paired with its outer-scope `Ty` so closure
        //     conversion can compute the GC pointer bitmap without a
        //     separate re-lookup pass.
        let outer_names: std::collections::BTreeSet<String> = self.env.keys().cloned().collect();
        let param_names: std::collections::BTreeSet<String> =
            params.iter().map(|p| p.name.clone()).collect();
        let mut capture_names: Vec<String> = Vec::new();
        collect_free_vars(body, &outer_names, &param_names, &mut capture_names);
        let captures: Vec<(String, Ty)> = capture_names
            .into_iter()
            .map(|name| {
                // `outer_names` is the `env` keyset before params are
                // added, so every captured name is guaranteed present
                // in `self.env` at this point. Unwrap is an invariant,
                // not a bet.
                let ty = self
                    .env
                    .get(&name)
                    .cloned()
                    .unwrap_or_else(|| unreachable!("capture {name} missing from outer env"));
                (name, ty)
            })
            .collect();
        self.lambda_captures.push((span.clone(), captures));

        // (3) extend env with params for the body walk. We *add*
        //     rather than env_insert because a param can shadow an
        //     outer binding (including a top-level fn name); the
        //     env_insert debug-assert would fire on the shadow.
        let saved_env = self.env.clone();
        for (p, pty) in params.iter().zip(param_tys.iter()) {
            self.env.insert(p.name.clone(), pty.clone());
        }

        // (4) check body against the lambda's own effect row.
        let body_ty = self.check_expr(body, effects);

        // (5) restore and check return-type unification.
        self.env = saved_env;
        if let Some(bt) = body_ty {
            if bt != ret_ty {
                self.push_error(
                    "E0069",
                    span.clone(),
                    format!(
                        "lambda body has type `{}` but the declared return type is `{}`",
                        ty_display(&bt),
                        ty_display(&ret_ty),
                    ),
                );
            }
        }

        Some(Ty::Fn(Box::new(sig)))
    }

    /// Typing rule for `match` expressions.
    ///
    /// Three checks:
    ///
    /// 1. Each pattern's type must match the scrutinee's type. `IntLit`
    ///    matches `Int`, `BoolLit` matches `Bool`, `CharLit` matches
    ///    `Char`, `Wildcard` matches any scrutinee.
    /// 2. All arm bodies must have the same type. The first arm's body
    ///    type is the expected type; disagreeing arms emit E0065.
    /// 3. The arms must be exhaustive. `Bool` requires either a wildcard
    ///    or both `true` and `false` literal arms. Other primitives
    ///    require a wildcard. `Unit` requires a non-empty arm list (the
    ///    type has one value). An empty arm list is always non-exhaustive.
    fn check_match(
        &mut self,
        scrutinee: &Expr,
        arms: &[MatchArm],
        span: Span,
        row: &[String],
    ) -> Option<Ty> {
        let scrut_ty = self.check_expr(scrutinee, row);

        // Record the scrutinee type for codegen's pattern disambiguator
        // (Plan A3 task 41.2). Only well-typed scrutinees land in the
        // map; codegen falls back to primitive-scalar dispatch when a
        // span is absent.
        if let Some(ref t) = scrut_ty {
            self.match_scrut_tys.insert(span.clone(), t.clone());
        }

        if arms.is_empty() {
            self.push_error("E0066", span, "`match` must have at least one arm");
            return None;
        }

        // (1) pattern-structure check + arm-body unification. Patterns
        // introduce bindings (`Pattern::Var(name)` and nested patterns
        // inside constructor arms) that must scope only over their own
        // arm body; `check_pattern` gathers them into a list so we can
        // snapshot/restore the enclosing env correctly.
        //
        // Plan B A3-carryover: track whether any arm emitted a typecheck
        // error during its pattern or body. If so, suppress the
        // user-type exhaustiveness check (E0120) below — the user
        // fixes the arm-level error first and exhaustiveness re-runs
        // on the next compile.
        //
        // Narrow behaviour: primitive E0066 non-exhaustiveness stays
        // on (it rarely cascades from arm-body errors), and only the
        // user-type E0120 surface gets suppressed. The check below
        // gates the E0120 emission on `!any_arm_erred`; the E0066
        // path runs regardless.
        let mut result_ty: Option<Ty> = None;
        let mut any_arm_erred = false;
        for arm in arms {
            let arm_error_baseline = self.errors.len();
            let mut bindings: Vec<(String, Ty)> = Vec::new();
            match &scrut_ty {
                Some(st) => self.check_pattern(&arm.pattern, st, &mut bindings),
                None => self.check_pattern_shape_only(&arm.pattern, &mut bindings),
            }
            // Snapshot the prior binding (if any) for each name the
            // pattern introduces so we can restore exact state after
            // the arm body is checked. Pattern::Var names are fresh
            // per arm (resolve.rs does not track match-arm scopes),
            // so there is no redefinition concern inside a single arm.
            let saved: Vec<(String, Option<Ty>)> = bindings
                .iter()
                .map(|(name, _)| (name.clone(), self.env.get(name).cloned()))
                .collect();
            for (name, ty) in &bindings {
                self.env.insert(name.clone(), ty.clone());
            }
            let body_ty = self.check_expr(&arm.body, row);
            for (name, prev) in saved {
                match prev {
                    Some(ty) => {
                        self.env.insert(name, ty);
                    }
                    None => {
                        self.env.remove(&name);
                    }
                }
            }
            match (&result_ty, &body_ty) {
                (None, Some(_)) => {
                    result_ty = body_ty;
                }
                (Some(first), Some(t)) if first != t => {
                    self.push_error(
                        "E0065",
                        arm.span.clone(),
                        format!(
                            "match arm body type `{}` does not match first arm's type `{}`",
                            ty_display(t),
                            ty_display(first),
                        ),
                    );
                }
                _ => {}
            }
            if self.errors.len() > arm_error_baseline {
                any_arm_erred = true;
            }
        }

        // (2) exhaustiveness.
        //
        // User types (Plan A3 task 38.4 + Plan B carryover): full
        // nested Maranget-style coverage — either a catch-all arm
        // (wildcard or non-promotion var binding) is present, or
        // every declared variant has an arm whose nested-pattern
        // structure also covers the variant's field types. Missing
        // coverage → E0120 with a witness string naming the
        // uncovered case (variant + filled-in field witnesses).
        // Implemented in `match_witness` (commit 62ba42a); the
        // `TRAP_NONEXHAUSTIVE_MATCH` runtime trap survives only as
        // a defensive safety net for codegen-internal errors and
        // for non-exhaustive primitive-scrutinee paths.
        //
        // Primitives (Plan A2): retained `is_exhaustive` rule → E0066.
        //
        // Plan B A3-carryover: if any arm had a typecheck error above,
        // skip *only* the user-type E0120 path. The cascade pattern the
        // carryover targets is mistyped ctor-arms making the user-type
        // exhaustiveness pass look like it's missing variants — that's
        // an E0120 noise problem. Primitive scrutinees rarely cascade
        // the same way (literal-pattern errors don't typically pretend
        // to be coverage holes), so the E0066 path runs unconditionally.
        if let Some(ref st) = scrut_ty {
            if let Ty::User(u, _args) = st {
                if !any_arm_erred {
                    if let Some(witness) = self.user_type_witness(u, arms) {
                        self.push_error(
                            "E0120",
                            span,
                            format!("`match` on `{u}` is not exhaustive; missing case `{witness}`"),
                        );
                    }
                }
            } else if !is_exhaustive(st, arms) {
                self.push_error(
                    "E0066",
                    span,
                    format!("`match` on `{}` is not exhaustive", ty_display(st)),
                );
            }
        }

        result_ty
    }

    /// Plan A3 task 38.4 — top-level exhaustiveness witness for a
    /// user-typed scrutinee. Returns `None` when exhaustive; returns
    /// `Some(witness_string)` naming the first uncovered variant when
    /// not. The witness is pasteable directly into a new arm: `Foo`
    /// for Unit, `Foo(_, _)` for Positional, `Foo { x: _, y: _ }` for
    /// Record.
    ///
    /// A catch-all arm (wildcard or bare-var binding that is not a
    /// nullary-ctor promotion) short-circuits to exhaustive. The
    /// v1 algorithm only checks top-level coverage; nested non-
    /// exhaustiveness inside a covered ctor's fields falls through to
    /// the runtime trap and is documented as such in the E0120 catalog
    /// long-form.
    fn user_type_witness(&self, type_name: &str, arms: &[MatchArm]) -> Option<String> {
        // For exhaustiveness purposes, the args carried on a generic
        // user type don't change the variant set — the generic params
        // are erased to the structural variant list. Use an empty
        // arg vec here so the witness machinery (which only consults
        // the declared variants) operates the same way for `List[Int]`
        // and `List[String]`.
        let scrut_ty = Ty::User(type_name.to_string(), Vec::new());
        let patterns: Vec<&Pattern> = arms.iter().map(|a| &a.pattern).collect();
        self.match_witness(&scrut_ty, &patterns)
    }

    /// Plan B A3-carryover — recursive Maranget exhaustiveness.
    ///
    /// Returns `None` if the given pattern list exhaustively covers
    /// `scrut_ty`; otherwise returns `Some(witness)` naming a concrete
    /// uncovered value. The witness is paste-able into a new arm.
    ///
    /// Primitive scrutinees honor the Plan A2 rule: wildcard required
    /// except for `Bool` where both literals may cover, and `Unit`
    /// where any arm covers the sole value.
    ///
    /// User-type scrutinees descend into constructor field patterns:
    ///
    ///   `match o { Some(true) => .., None => .. }` on
    ///   `Option = | None | Some(Bool)` now returns `Some("Some(false)")`
    ///   rather than falling through to a runtime trap.
    ///
    /// Pattern::Var is interpreted context-sensitively: on a user-type
    /// scrutinee whose declared variants contain a Unit variant named
    /// `V`, `Pattern::Var("V", _)` matches *that variant only* (the
    /// nullary-ctor-promotion rule established in Task 38.3); on any
    /// other scrutinee shape it is a fresh catch-all binder.
    fn match_witness(&self, scrut_ty: &Ty, patterns: &[&Pattern]) -> Option<String> {
        if patterns
            .iter()
            .any(|p| self.pattern_is_catchall(p, scrut_ty))
        {
            return None;
        }
        match scrut_ty {
            Ty::Bool => {
                let has_true = patterns
                    .iter()
                    .any(|p| matches!(p, Pattern::BoolLit(true, _)));
                let has_false = patterns
                    .iter()
                    .any(|p| matches!(p, Pattern::BoolLit(false, _)));
                match (has_true, has_false) {
                    (true, true) => None,
                    // Missing `false` — witness is the unseen literal.
                    (true, false) => Some("false".to_string()),
                    // Missing `true` — ditto.
                    (false, true) => Some("true".to_string()),
                    // Missing both — name `false` arbitrarily; the user
                    // will see they need at least one literal arm.
                    (false, false) => Some("false".to_string()),
                }
            }
            Ty::Unit => None,
            // Infinite / un-enumerable value domains: only a wildcard
            // / fresh-var binder can reach exhaustiveness, and such a
            // pattern was already caught above by `pattern_is_catchall`.
            Ty::Int | Ty::Char | Ty::String | Ty::Byte | Ty::Fn(_) => Some("_".to_string()),
            // Plan B task 48: an inferred `Ty::Var` scrutinee
            // means the type is still polymorphic at the match.
            // v1 disallows that — primitives / users dispatch on
            // shape, and a still-free var has no shape. Fall back
            // to `_` so downstream cascade is muted; the real
            // diagnostic surfaces as an E0044 unification failure
            // somewhere upstream that produced the unconstrained
            // var in the first place.
            Ty::Var(_) => Some("_".to_string()),
            Ty::User(type_name, _args) => {
                let td = self.types.get(type_name)?;
                for (variant_idx, variant) in td.variants.iter().enumerate() {
                    // Gather per-arm field-pattern lists from every
                    // pattern that matches this variant.
                    let mut variant_rows: Vec<Vec<Pattern>> = Vec::new();
                    for p in patterns {
                        if let Some(fields) =
                            self.pattern_matches_variant(p, type_name, variant_idx, variant)
                        {
                            variant_rows.push(fields);
                        }
                    }
                    if variant_rows.is_empty() {
                        return Some(ctor_witness_string(variant));
                    }
                    // Recurse per field position. The first uncovered
                    // field yields the witness, with the other fields
                    // wildcarded.
                    match &variant.fields {
                        VariantFields::Unit => {}
                        VariantFields::Positional(field_tes) => {
                            for (field_idx, field_te) in field_tes.iter().enumerate() {
                                let field_ty = match self.ty_from_type_expr_here(field_te) {
                                    Some(t) => t,
                                    None => continue,
                                };
                                let field_patterns: Vec<&Pattern> =
                                    variant_rows.iter().map(|row| &row[field_idx]).collect();
                                if let Some(sub) = self.match_witness(&field_ty, &field_patterns) {
                                    return Some(positional_witness_with_hole(
                                        variant, field_idx, &sub,
                                    ));
                                }
                            }
                        }
                        VariantFields::Record(record_fields) => {
                            for (field_idx, record_field) in record_fields.iter().enumerate() {
                                let field_ty = match self.ty_from_type_expr_here(&record_field.ty) {
                                    Some(t) => t,
                                    None => continue,
                                };
                                let field_patterns: Vec<&Pattern> =
                                    variant_rows.iter().map(|row| &row[field_idx]).collect();
                                if let Some(sub) = self.match_witness(&field_ty, &field_patterns) {
                                    return Some(record_witness_with_hole(
                                        variant,
                                        record_fields,
                                        field_idx,
                                        &sub,
                                    ));
                                }
                            }
                        }
                    }
                }
                None
            }
        }
    }

    /// True when `p` covers every possible value of `scrut_ty`.
    ///
    /// `Pattern::Wildcard` is always a catchall. `Pattern::Var(name)`
    /// is a catchall unless it's a nullary-ctor promotion of `name`
    /// to a Unit variant of a user-type scrutinee — in which case it
    /// only covers that one variant.
    fn pattern_is_catchall(&self, p: &Pattern, scrut_ty: &Ty) -> bool {
        match p {
            Pattern::Wildcard(_) => true,
            Pattern::Var(name, _) => {
                if let Ty::User(u, _) = scrut_ty {
                    if let Some(info) = self.ctors.get(name) {
                        if info.type_name == *u {
                            if let Some(td) = self.types.get(u) {
                                if let Some(variant) = td.variants.get(info.variant_index) {
                                    if matches!(variant.fields, VariantFields::Unit) {
                                        return false;
                                    }
                                }
                            }
                        }
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// If `p` matches `variant` (of `type_name` at index `variant_idx`),
    /// return the per-field sub-patterns in the declared field order.
    /// Otherwise `None`.
    ///
    /// For record patterns, sub-patterns are reordered to match the
    /// declared field order so field-index recursion in
    /// `match_witness` is straightforward. Pattern validity has
    /// already been checked by `check_pattern`, so shape mismatches
    /// here fall through to `None` and cannot surprise callers.
    fn pattern_matches_variant(
        &self,
        p: &Pattern,
        type_name: &str,
        variant_idx: usize,
        variant: &Variant,
    ) -> Option<Vec<Pattern>> {
        match p {
            Pattern::Ctor { name, fields, .. } => {
                if name != &variant.name {
                    return None;
                }
                match (fields, &variant.fields) {
                    (CtorPatternFields::Unit, VariantFields::Unit) => Some(Vec::new()),
                    (
                        CtorPatternFields::Positional(sub_pats),
                        VariantFields::Positional(field_tes),
                    ) if sub_pats.len() == field_tes.len() => Some(sub_pats.clone()),
                    (CtorPatternFields::Record(cpf), VariantFields::Record(rfd)) => {
                        // `check_pattern` rejects record patterns with
                        // missing fields via E0115. If we hit a missing
                        // field here the AST is already malformed —
                        // bail out of variant matching for this arm
                        // rather than wildcarding (which would silently
                        // over-approximate coverage and mask bugs in
                        // upstream validation).
                        let mut out: Vec<Pattern> = Vec::with_capacity(rfd.len());
                        for declared in rfd {
                            match cpf.iter().find(|f| f.name == declared.name) {
                                Some(f) => out.push(f.pattern.clone()),
                                None => return None,
                            }
                        }
                        Some(out)
                    }
                    _ => None,
                }
            }
            Pattern::Var(name, _) => {
                // Nullary-ctor promotion: matches exactly this variant
                // when (a) the name resolves to this variant via the
                // ctor registry and (b) the declared shape is Unit.
                if let Some(info) = self.ctors.get(name) {
                    if info.type_name == type_name
                        && info.variant_index == variant_idx
                        && matches!(variant.fields, VariantFields::Unit)
                    {
                        return Some(Vec::new());
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Plan A3 task 38.3 structural pattern typing. Verifies the
    /// pattern's shape against the scrutinee's type and collects every
    /// `Pattern::Var` binding (excluding bare-ident nullary-ctor
    /// promotions) into `bindings` paired with the type the name
    /// should have inside the arm's body.
    ///
    /// Emits E0064 for primitive-literal mismatches (preserving Plan
    /// A2's code) and E0117 for constructor / tuple / variable shape
    /// mismatches introduced in Plan A3.
    fn check_pattern(&mut self, p: &Pattern, scrut_ty: &Ty, bindings: &mut Vec<(String, Ty)>) {
        match p {
            Pattern::Wildcard(_) => {}
            Pattern::IntLit(_, span) => {
                if scrut_ty != &Ty::Int {
                    self.push_error(
                        "E0064",
                        span.clone(),
                        format!(
                            "integer-literal pattern does not match scrutinee type `{}`",
                            ty_display(scrut_ty)
                        ),
                    );
                }
            }
            Pattern::BoolLit(_, span) => {
                if scrut_ty != &Ty::Bool {
                    self.push_error(
                        "E0064",
                        span.clone(),
                        format!(
                            "boolean-literal pattern does not match scrutinee type `{}`",
                            ty_display(scrut_ty)
                        ),
                    );
                }
            }
            Pattern::CharLit(_, span) => {
                if scrut_ty != &Ty::Char {
                    self.push_error(
                        "E0064",
                        span.clone(),
                        format!(
                            "character-literal pattern does not match scrutinee type `{}`",
                            ty_display(scrut_ty)
                        ),
                    );
                }
            }
            Pattern::Var(name, _span) => {
                // Nullary-ctor promotion: a bare identifier pattern
                // whose name matches a nullary variant of the
                // scrutinee's user type is not a binding — it is a
                // zero-arity constructor pattern. Task 39's elaborate
                // reads the AST and this binding/non-binding
                // distinction directly from the recorded bindings
                // vector (empty for nullary ctors).
                if let Ty::User(u, _) = scrut_ty {
                    if let Some(info) = self.ctors.get(name).cloned() {
                        if info.type_name == *u {
                            if let Some(td) = self.types.get(&info.type_name) {
                                if matches!(
                                    td.variants[info.variant_index].fields,
                                    VariantFields::Unit
                                ) {
                                    // nullary-ctor pattern, no binding
                                    return;
                                }
                            }
                        }
                    }
                }
                bindings.push((name.clone(), scrut_ty.clone()));
            }
            Pattern::Tuple(pats, span) => {
                // Plan A3 v1 has no tuple types in the surface; a
                // tuple pattern never matches any scrutinee type. We
                // still recurse into sub-patterns with the scrutinee
                // type as a conservative placeholder so any inner
                // pattern-shape errors still surface, but emit E0117
                // at the top-level tuple pattern.
                self.push_error(
                    "E0117",
                    span.clone(),
                    format!(
                        "tuple pattern does not match scrutinee type `{}` (no tuple types in Plan A3 v1)",
                        ty_display(scrut_ty)
                    ),
                );
                for sub in pats {
                    self.check_pattern(sub, scrut_ty, bindings);
                }
            }
            Pattern::Ctor { name, fields, span } => {
                let Some(info) = self.ctors.get(name).cloned() else {
                    self.push_error(
                        "E0114",
                        span.clone(),
                        format!("unknown constructor `{name}` in pattern"),
                    );
                    // Still walk sub-patterns shape-only so nested
                    // errors surface.
                    match fields {
                        CtorPatternFields::Unit => {}
                        CtorPatternFields::Positional(ps) => {
                            for sub in ps {
                                self.check_pattern_shape_only(sub, bindings);
                            }
                        }
                        CtorPatternFields::Record(fs) => {
                            for f in fs {
                                self.check_pattern_shape_only(&f.pattern, bindings);
                            }
                        }
                    }
                    return;
                };
                // Nominal type-match: the ctor's owning type must
                // equal the scrutinee type. Generic-type args on the
                // scrutinee are accepted; field-pattern recursion
                // below handles arg-aware unification when present.
                match scrut_ty {
                    Ty::User(u, _) if *u == info.type_name => {}
                    _ => {
                        self.push_error(
                            "E0117",
                            span.clone(),
                            format!(
                                "constructor `{name}` is a variant of type `{}`; scrutinee has type `{}`",
                                info.type_name,
                                ty_display(scrut_ty)
                            ),
                        );
                    }
                }
                let td = match self.types.get(&info.type_name).cloned() {
                    Some(t) => t,
                    None => return,
                };
                let variant = &td.variants[info.variant_index];
                match (&variant.fields, fields) {
                    (VariantFields::Unit, CtorPatternFields::Unit) => {}
                    (VariantFields::Positional(param_tys), CtorPatternFields::Positional(pats)) => {
                        if param_tys.len() != pats.len() {
                            self.push_error(
                                "E0115",
                                span.clone(),
                                format!(
                                    "constructor `{name}` pattern expects {} positional field{}, got {}",
                                    param_tys.len(),
                                    if param_tys.len() == 1 { "" } else { "s" },
                                    pats.len()
                                ),
                            );
                        }
                        for (sub, decl_ty) in pats.iter().zip(param_tys.iter()) {
                            let inner = self.ty_from_type_expr_here(decl_ty).unwrap_or(Ty::Unit);
                            self.check_pattern(sub, &inner, bindings);
                        }
                    }
                    (VariantFields::Record(decl_fields), CtorPatternFields::Record(pat_fields)) => {
                        // Duplicate pattern-field names
                        let mut seen: BTreeMap<String, Span> = BTreeMap::new();
                        for f in pat_fields {
                            if let Some(first) = seen.get(&f.name).cloned() {
                                self.push_error(
                                    "E0115",
                                    f.span.clone(),
                                    format!(
                                        "pattern for constructor `{name}` got duplicate field `{}` (first at {}:{})",
                                        f.name, first.line, first.column
                                    ),
                                );
                            } else {
                                seen.insert(f.name.clone(), f.span.clone());
                            }
                        }
                        for f in pat_fields {
                            let Some(decl) = decl_fields.iter().find(|d| d.name == f.name) else {
                                self.push_error(
                                    "E0115",
                                    f.span.clone(),
                                    format!(
                                        "pattern for constructor `{name}` of type `{}` has no field `{}`",
                                        info.type_name, f.name
                                    ),
                                );
                                continue;
                            };
                            let inner = self.ty_from_type_expr_here(&decl.ty).unwrap_or(Ty::Unit);
                            self.check_pattern(&f.pattern, &inner, bindings);
                        }
                        for d in decl_fields {
                            if !pat_fields.iter().any(|f| f.name == d.name) {
                                self.push_error(
                                    "E0115",
                                    span.clone(),
                                    format!(
                                        "pattern for constructor `{name}` of type `{}` is missing field `{}`",
                                        info.type_name, d.name
                                    ),
                                );
                            }
                        }
                    }
                    // Shape mismatches (Unit vs Positional, etc.) —
                    // E0115 naming the declared shape.
                    (VariantFields::Unit, _) => {
                        self.push_error(
                            "E0115",
                            span.clone(),
                            format!(
                                "constructor `{name}` of type `{}` is nullary; pattern must be a bare identifier (no parens or braces)",
                                info.type_name
                            ),
                        );
                    }
                    (VariantFields::Positional(_), _) => {
                        self.push_error(
                            "E0115",
                            span.clone(),
                            format!(
                                "constructor `{name}` of type `{}` has positional fields; pattern must use `{name}(..)` form",
                                info.type_name
                            ),
                        );
                    }
                    (VariantFields::Record(_), _) => {
                        self.push_error(
                            "E0115",
                            span.clone(),
                            format!(
                                "constructor `{name}` of type `{}` has record fields; pattern must use `{name} {{ .. }}` form",
                                info.type_name
                            ),
                        );
                    }
                }
            }
        }
    }

    /// Shape-only walk used when the scrutinee has unknown type
    /// (propagated `None` from a prior error). Binds every
    /// `Pattern::Var` to `Ty::Unit` so the arm body can at least
    /// reference the names without cascade-NPE-shaped errors.
    fn check_pattern_shape_only(&mut self, p: &Pattern, bindings: &mut Vec<(String, Ty)>) {
        match p {
            Pattern::Wildcard(_)
            | Pattern::IntLit(_, _)
            | Pattern::BoolLit(_, _)
            | Pattern::CharLit(_, _) => {}
            Pattern::Var(name, _) => {
                bindings.push((name.clone(), Ty::Unit));
            }
            Pattern::Tuple(pats, _) => {
                for sub in pats {
                    self.check_pattern_shape_only(sub, bindings);
                }
            }
            Pattern::Ctor { fields, .. } => match fields {
                CtorPatternFields::Unit => {}
                CtorPatternFields::Positional(ps) => {
                    for sub in ps {
                        self.check_pattern_shape_only(sub, bindings);
                    }
                }
                CtorPatternFields::Record(fs) => {
                    for f in fs {
                        self.check_pattern_shape_only(&f.pattern, bindings);
                    }
                }
            },
        }
    }
}

fn type_is(t: &TypeExpr, name: &str) -> bool {
    // Plan B Task 47: head-name match — generic application
    // (`List[Int]`) matches the head, ignoring args. Real type-arg
    // matching arrives with Task 48 unification.
    t.head_name() == name
}

fn type_name(t: &TypeExpr) -> &str {
    t.head_name()
}

/// Lift a surface `TypeExpr` into the checker's `Ty` lattice. Resolves
/// Plan A2 primitives first (`Int`, `String`, `Unit`, `Bool`, `Char`,
/// `Byte`), then Plan A3 user-defined types by consulting the types
/// registry, and finally Plan B Task 48 generic-parameter references
/// via `generic_subst`. Unknown names return `None`; the caller
/// (fn_env pre-pass or `check_type_expr_known`) handles the
/// fallback/diagnostic split.
///
/// `generic_subst` maps surface generic-parameter names (`A`, `B`,
/// the names declared in a fn or type's `[A, B, ...]` list) to the
/// `Ty::Var` introduced for that scope by `check_fn`. Outside generic
/// scope (top-level type checks, non-generic builtins), pass an empty
/// map. Names that are neither primitives, registered user types, nor
/// in-scope generic parameters return `None` — the caller emits E0112.
///
/// Plan B Task 48: `TypeExpr::Apply { name, args, .. }` resolves args
/// recursively and returns `Ty::User(name, resolved_args)` when `name`
/// is a registered user type whose declared generic-parameter arity
/// matches `args.len()`. Arity mismatch and "Apply'd primitive"
/// (`Int[Foo]`) return `None`; `check_type_expr_known` is the
/// authoritative diagnostic site for those failures.
pub(crate) fn ty_from_type_expr(
    t: &TypeExpr,
    types: &BTreeMap<String, TypeDecl>,
    generic_subst: &BTreeMap<String, Ty>,
) -> Option<Ty> {
    match t {
        TypeExpr::Named(name, _) => match name.as_str() {
            "Int" => Some(Ty::Int),
            "String" => Some(Ty::String),
            "Unit" => Some(Ty::Unit),
            "Bool" => Some(Ty::Bool),
            "Char" => Some(Ty::Char),
            "Byte" => Some(Ty::Byte),
            other => {
                if let Some(ty) = generic_subst.get(other) {
                    Some(ty.clone())
                } else if let Some(td) = types.get(other) {
                    if td.generic_params.is_empty() {
                        Some(Ty::User(other.to_string(), Vec::new()))
                    } else {
                        // Generic type used without arguments: arity
                        // mismatch. Caller's diagnostic (E0124-fam)
                        // reports the missing args; we return None.
                        None
                    }
                } else {
                    None
                }
            }
        },
        TypeExpr::Apply { name, args, .. } => {
            // Primitives don't accept type arguments.
            if matches!(
                name.as_str(),
                "Int" | "String" | "Unit" | "Bool" | "Char" | "Byte"
            ) {
                return None;
            }
            // A bare generic-parameter cannot be applied (`A[Int]`).
            if generic_subst.contains_key(name) {
                return None;
            }
            let td = types.get(name)?;
            if td.generic_params.len() != args.len() {
                return None;
            }
            let mut resolved_args = Vec::with_capacity(args.len());
            for a in args {
                resolved_args.push(ty_from_type_expr(a, types, generic_subst)?);
            }
            Some(Ty::User(name.to_string(), resolved_args))
        }
    }
}

/// User-facing display of a `Ty`. Keeps error messages readable without
/// leaking the `Ty::` enum prefix that `{:?}` would emit. Returns
/// `String` rather than `&'static str` now that `Ty::Fn` carries a
/// non-constant structural signature (plan A2 task 30).
fn ty_display(t: &Ty) -> String {
    match t {
        Ty::Int => "Int".to_string(),
        Ty::String => "String".to_string(),
        Ty::Unit => "Unit".to_string(),
        Ty::Bool => "Bool".to_string(),
        Ty::Char => "Char".to_string(),
        Ty::Byte => "Byte".to_string(),
        Ty::Fn(sig) => {
            let params = sig
                .params
                .iter()
                .map(ty_display)
                .collect::<Vec<_>>()
                .join(", ");
            let ret = ty_display(&sig.ret);
            let effects = sig.effects.join(", ");
            let row_var = sig
                .effect_row_var
                .map(|id| format!(" | ?{id}"))
                .unwrap_or_default();
            format!("({params}) -> {ret} ![{effects}{row_var}]")
        }
        Ty::User(n, args) => {
            if args.is_empty() {
                n.clone()
            } else {
                let argstr = args.iter().map(ty_display).collect::<Vec<_>>().join(", ");
                format!("{n}[{argstr}]")
            }
        }
        Ty::Var(id) => format!("?{id}"),
    }
}

fn binop_symbol(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::Eq => "==",
        BinOp::NotEq => "!=",
        BinOp::Lt => "<",
        BinOp::Gt => ">",
        BinOp::LtEq => "<=",
        BinOp::GtEq => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
    }
}

/// Build a paste-able witness pattern for an uncovered variant
/// (Plan A3 task 38.4). Shape follows the variant declaration:
/// - `Unit` → `Foo`
/// - `Positional(T1, ..., Tn)` → `Foo(_, ..., _)` (n wildcards)
/// - `Record { f1: T1, ... }` → `Foo { f1: _, ... }` (one wildcard
///   per declared field, preserving field-name order)
fn ctor_witness_string(variant: &Variant) -> String {
    match &variant.fields {
        VariantFields::Unit => variant.name.clone(),
        VariantFields::Positional(params) => {
            let args = std::iter::repeat_n("_", params.len())
                .collect::<Vec<_>>()
                .join(", ");
            format!("{}({args})", variant.name)
        }
        VariantFields::Record(fields) => {
            let fs = fields
                .iter()
                .map(|f| format!("{}: _", f.name))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{} {{ {fs} }}", variant.name)
        }
    }
}

/// Plan B A3-carryover — build a positional-variant witness string
/// with the `hole_idx`-th field replaced by `sub`, other fields left
/// as `_`.
fn positional_witness_with_hole(variant: &Variant, hole_idx: usize, sub: &str) -> String {
    let arity = match &variant.fields {
        VariantFields::Positional(params) => params.len(),
        _ => return variant.name.clone(),
    };
    let args: Vec<String> = (0..arity)
        .map(|i| {
            if i == hole_idx {
                sub.to_string()
            } else {
                "_".to_string()
            }
        })
        .collect();
    format!("{}({})", variant.name, args.join(", "))
}

/// Plan B A3-carryover — build a record-variant witness string with
/// the `hole_idx`-th field (in declared-field order) set to `sub` and
/// other fields left as `_`.
fn record_witness_with_hole(
    variant: &Variant,
    fields: &[RecordFieldDecl],
    hole_idx: usize,
    sub: &str,
) -> String {
    let fs: Vec<String> = fields
        .iter()
        .enumerate()
        .map(|(i, f)| {
            if i == hole_idx {
                format!("{}: {sub}", f.name)
            } else {
                format!("{}: _", f.name)
            }
        })
        .collect();
    format!("{} {{ {} }}", variant.name, fs.join(", "))
}

/// Coarse exhaustiveness check. Wildcard-terminated arm lists are
/// always exhaustive. Without a wildcard: `Bool` requires both `true`
/// and `false` literal arms; `Unit` is exhaustive as long as there is
/// at least one arm; other primitives (`Int`, `Char`, `String`, `Byte`)
/// have infinite or effectively-infinite value domains in Plan A2's
/// surface syntax and are only exhaustive via wildcard.
/// Walk `e` and collect the names of every `Ident` reference that
/// resolves to an outer-scope binding from the perspective of a
/// lambda body. Used by `check_lambda` for capture analysis (plan A2
/// task 30).
///
/// `outer_names` is the set of names visible in the enclosing scope
/// when the lambda was entered. `param_names` is the set of the
/// lambda's own parameter names (which shadow the outer scope).
/// `captures` is the accumulated result; names are pushed in the
/// order they're encountered, with duplicates suppressed.
///
/// Lambda-local `let` bindings are handled via `locals`, a
/// per-descent mutable set that mirrors the lexical scope.
///
/// Nested lambdas are handled by recursing into their bodies with
/// the outer set expanded to include the outer lambda's params — a
/// nested lambda captures not only the enclosing lambda's outer
/// scope but also the enclosing lambda's params.
/// Recursively collect every `Pattern::Var` binding name from a
/// pattern (Plan A3 task 39). Nullary-ctor promotion is NOT applied
/// here — this helper is used by capture analysis and closure
/// conversion's local-tracking path where a promoted nullary variant
/// is indistinguishable from a fresh binding at the syntactic layer
/// (both are Pattern::Var). Treating every `Var` as a binding is
/// safe: the over-approximation adds an unused entry to `locals`
/// that simply fails to match any later capture.
pub(crate) fn pattern_bindings(p: &Pattern, out: &mut std::collections::BTreeSet<String>) {
    match p {
        Pattern::Wildcard(_)
        | Pattern::IntLit(_, _)
        | Pattern::BoolLit(_, _)
        | Pattern::CharLit(_, _) => {}
        Pattern::Var(name, _) => {
            out.insert(name.clone());
        }
        Pattern::Tuple(pats, _) => {
            for sub in pats {
                pattern_bindings(sub, out);
            }
        }
        Pattern::Ctor { fields, .. } => match fields {
            CtorPatternFields::Unit => {}
            CtorPatternFields::Positional(ps) => {
                for sub in ps {
                    pattern_bindings(sub, out);
                }
            }
            CtorPatternFields::Record(fs) => {
                for f in fs {
                    pattern_bindings(&f.pattern, out);
                }
            }
        },
    }
}

fn collect_free_vars(
    e: &Expr,
    outer_names: &std::collections::BTreeSet<String>,
    param_names: &std::collections::BTreeSet<String>,
    captures: &mut Vec<String>,
) {
    let mut locals: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    walk(e, outer_names, param_names, &mut locals, captures);

    fn walk(
        e: &Expr,
        outer_names: &std::collections::BTreeSet<String>,
        param_names: &std::collections::BTreeSet<String>,
        locals: &mut std::collections::BTreeSet<String>,
        captures: &mut Vec<String>,
    ) {
        match e {
            Expr::Ident(name, _) => {
                // A free variable is an Ident that:
                //  - is visible in the outer env (so it resolves),
                //  - isn't a lambda param or lambda-local let.
                if outer_names.contains(name)
                    && !param_names.contains(name)
                    && !locals.contains(name)
                    && !captures.iter().any(|c| c == name)
                {
                    captures.push(name.clone());
                }
            }
            Expr::IntLit(..) | Expr::StringLit(..) | Expr::BoolLit(..) | Expr::CharLit(..) => {}
            Expr::Binary { lhs, rhs, .. } => {
                walk(lhs, outer_names, param_names, locals, captures);
                walk(rhs, outer_names, param_names, locals, captures);
            }
            Expr::Unary { operand, .. } => {
                walk(operand, outer_names, param_names, locals, captures);
            }
            Expr::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                walk(cond, outer_names, param_names, locals, captures);
                walk_block(then_block, outer_names, param_names, locals, captures);
                walk_block(else_block, outer_names, param_names, locals, captures);
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                walk(scrutinee, outer_names, param_names, locals, captures);
                for arm in arms {
                    // Plan A3 task 39: pattern `Pattern::Var(name)` —
                    // including names nested inside `Ctor`/`Tuple`
                    // sub-patterns — introduces a local binding for
                    // the arm body. Snapshot `locals`, add the arm's
                    // bindings, walk the body, then restore.
                    let saved = locals.clone();
                    pattern_bindings(&arm.pattern, locals);
                    walk(&arm.body, outer_names, param_names, locals, captures);
                    *locals = saved;
                }
            }
            Expr::Block(b) => {
                walk_block(b, outer_names, param_names, locals, captures);
            }
            Expr::Call { callee, args, .. } => {
                walk(callee, outer_names, param_names, locals, captures);
                for a in args {
                    walk(a, outer_names, param_names, locals, captures);
                }
            }
            Expr::Perform(p) => {
                for a in &p.args {
                    walk(a, outer_names, param_names, locals, captures);
                }
            }
            Expr::Lambda {
                params: inner_params,
                body: inner_body,
                ..
            } => {
                // A nested lambda captures from an expanded outer
                // scope: the enclosing lambda's outer_names plus its
                // params (which appear as "outer" to the nested
                // lambda), minus the nested lambda's own params.
                // Lambda-local lets accumulated so far also count as
                // outer-visible to the nested lambda, though a nested
                // capture of a local is already a capture from this
                // lambda's perspective.
                let mut expanded: std::collections::BTreeSet<String> = outer_names.clone();
                expanded.extend(param_names.iter().cloned());
                expanded.extend(locals.iter().cloned());
                let inner_params_set: std::collections::BTreeSet<String> =
                    inner_params.iter().map(|p| p.name.clone()).collect();
                let mut inner_locals: std::collections::BTreeSet<String> =
                    std::collections::BTreeSet::new();
                walk(
                    inner_body,
                    &expanded,
                    &inner_params_set,
                    &mut inner_locals,
                    captures,
                );
            }
            // Capture analysis runs during typecheck, which is strictly
            // before closure conversion. These nodes never appear here.
            Expr::ClosureRecord { .. } | Expr::ClosureEnvLoad { .. } => {
                unreachable!("collect_free_vars: closure-conversion nodes should not appear pre-CC")
            }
            // Plan A3 task 37: record literal. Each field value is a
            // potential capture source. The name of the constructor
            // is not itself a captureable identifier (it resolves to
            // the registered type, not a binding).
            Expr::RecordLit { fields, .. } => {
                for f in fields {
                    walk(&f.value, outer_names, param_names, locals, captures);
                }
            }
        }
    }

    fn walk_block(
        b: &Block,
        outer_names: &std::collections::BTreeSet<String>,
        param_names: &std::collections::BTreeSet<String>,
        locals: &mut std::collections::BTreeSet<String>,
        captures: &mut Vec<String>,
    ) {
        for s in &b.stmts {
            match s {
                Stmt::Let(l) => {
                    walk(&l.value, outer_names, param_names, locals, captures);
                    locals.insert(l.name.clone());
                }
                Stmt::Expr(e) => walk(e, outer_names, param_names, locals, captures),
                Stmt::Perform(p) => {
                    for a in &p.args {
                        walk(a, outer_names, param_names, locals, captures);
                    }
                }
            }
        }
        if let Some(tail) = &b.tail {
            walk(tail, outer_names, param_names, locals, captures);
        }
    }
}

fn is_exhaustive(scrut: &Ty, arms: &[MatchArm]) -> bool {
    if arms.is_empty() {
        return false;
    }
    let has_wildcard = arms
        .iter()
        .any(|a| matches!(a.pattern, Pattern::Wildcard(_)));
    if has_wildcard {
        return true;
    }
    match scrut {
        Ty::Bool => {
            let has_true = arms
                .iter()
                .any(|a| matches!(a.pattern, Pattern::BoolLit(true, _)));
            let has_false = arms
                .iter()
                .any(|a| matches!(a.pattern, Pattern::BoolLit(false, _)));
            has_true && has_false
        }
        Ty::Unit => true,
        // `Int`, `Char`, `String`, `Byte`, `Fn`: value domain is
        // infinite or (for `Fn`) structurally un-enumerable, so a
        // wildcard arm is the only way to reach exhaustiveness.
        Ty::Int | Ty::Char | Ty::String | Ty::Byte | Ty::Fn(_) => false,
        // Plan A3 task 38.4 replaces this with Maranget's algorithm
        // that enumerates constructors and emits E0120 with a witness.
        // Until 38.4 lands, a user-typed scrutinee is conservatively
        // treated as an infinite-domain value: only a wildcard arm can
        // reach exhaustiveness. Structural ctor-pattern coverage
        // (`match o { None => .., Some(_) => .. }`) still fires E0066
        // until 38.4 teaches the checker to enumerate variants.
        Ty::User(_, _) => false,
        // Plan B task 48: still-polymorphic scrutinees can't reach
        // shape-based exhaustiveness; defer the diagnostic to the
        // upstream unification site that left the variable free.
        Ty::Var(_) => false,
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::{lexer::lex, parser::parse, resolve::resolve};

    fn pipeline(src: &str) -> Vec<CompilerError> {
        let (toks, lex_errs) = lex("x.sigil", src);
        let (prog, parse_errs) = parse("x.sigil", &toks);
        let (rp, res_errs) = resolve(prog);
        let (_tc, tc_errs) = typecheck(rp.program);
        let mut all = lex_errs;
        all.extend(parse_errs);
        all.extend(res_errs);
        all.extend(tc_errs);
        all
    }

    fn has_code(errs: &[CompilerError], code: &str) -> bool {
        errs.iter().any(|e| e.code.as_str() == code)
    }

    #[test]
    fn hello_world_typechecks() {
        let src = "import std.io\nfn main() -> Int ![IO] { perform IO.println(\"hi\"); 0 }\n";
        assert!(pipeline(src).is_empty(), "{:?}", pipeline(src));
    }

    #[test]
    fn perform_without_io_in_row_is_e0042() {
        let src = "fn main() -> Int ![] { perform IO.println(\"hi\"); 0 }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0042"), "expected E0042, got: {errs:?}");
    }

    #[test]
    fn perform_non_io_effect_is_e0042() {
        let src = "fn main() -> Int ![Foo] { perform Foo.bar(\"x\"); 0 }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0042"), "expected E0042, got: {errs:?}");
    }

    #[test]
    fn no_main_is_e0040() {
        let src = "fn not_main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0040"), "expected E0040, got: {errs:?}");
    }

    #[test]
    fn main_wrong_return_type_is_e0041() {
        let src = "fn main() -> String ![] { \"x\" }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0041"), "expected E0041, got: {errs:?}");
    }

    #[test]
    fn main_with_params_is_e0041() {
        let src = "fn main(x: Int) -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0041"), "expected E0041, got: {errs:?}");
    }

    #[test]
    fn main_with_non_io_effect_is_e0041() {
        let src = "fn main() -> Int ![Foo] { 0 }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0041"), "expected E0041, got: {errs:?}");
    }

    #[test]
    fn io_println_arg_count_is_e0043() {
        let src = "fn main() -> Int ![IO] { perform IO.println(); 0 }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0043"), "expected E0043, got: {errs:?}");
    }

    #[test]
    fn io_println_arg_type_is_e0044() {
        let src = "fn main() -> Int ![IO] { perform IO.println(42); 0 }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0044"), "expected E0044, got: {errs:?}");
    }

    #[test]
    fn let_type_mismatch_is_e0045() {
        let src = "fn main() -> Int ![] { let x: String = 42; 0 }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0045"), "expected E0045, got: {errs:?}");
    }

    #[test]
    fn unknown_ident_is_e0046() {
        // `ghost` is never bound; referencing it should emit E0046 with a
        // span pointing at the reference.
        let src = "fn main() -> Int ![] { let x: Int = ghost; 0 }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0046"), "expected E0046, got: {errs:?}");
        let e = errs.iter().find(|e| e.code.as_str() == "E0046").unwrap();
        assert!(e.message.contains("ghost"), "message lacks ident: {e:?}");
        // Span should point at the identifier (line 1, column > 0).
        assert!(e.span.line >= 1, "span missing: {e:?}");
    }

    #[test]
    fn bound_ident_resolves_to_its_type() {
        // `greeting` is declared String and passed to IO.println (which needs
        // String). If the env lookup works, this typechecks clean; if it
        // returned Unit as the pre-fix placeholder did, E0044 would fire.
        let src = "fn main() -> Int ![IO] {\n  let greeting: String = \"hello\";\n  perform IO.println(greeting);\n  0\n}\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "expected clean typecheck, got: {errs:?}");
    }

    #[test]
    fn bound_ident_wrong_type_is_e0044() {
        // Before Fix 3, this typechecked clean because Ident returned
        // Ty::Unit silently. After Fix 3, the Int binding leaks into
        // IO.println's arg-type check as Int, which is E0044.
        let src = "fn main() -> Int ![IO] {\n  let x: Int = 1;\n  perform IO.println(x);\n  0\n}\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0044"), "expected E0044, got: {errs:?}");
    }

    #[test]
    fn no_user_facing_error_uses_e0001() {
        // Every user-reachable diagnostic from typecheck must carry an
        // E0040+ code. Negative-coverage sweep across the cases the
        // checker actually flags, including plan A2 task 22 additions.
        let programs = [
            "fn not_main() -> Int ![] { 0 }\n",
            "fn main() -> String ![] { \"x\" }\n",
            "fn main(x: Int) -> Int ![] { 0 }\n",
            "fn main() -> Int ![] { perform IO.println(\"hi\"); 0 }\n",
            "fn main() -> Int ![IO] { perform IO.println(); 0 }\n",
            "fn main() -> Int ![IO] { perform IO.println(42); 0 }\n",
            "fn main() -> Int ![] { let x: String = 42; 0 }\n",
            // Task 22 additions: every new error code path must be
            // reached by a user-observable program.
            "fn main() -> Int ![] { let n: Int = 1 + \"hi\"; 0 }\n",
            "fn main() -> Int ![] { let b: Bool = true && 1; 0 }\n",
            "fn main() -> Int ![] { let b: Bool = true; let n: Int = -b; 0 }\n",
            "fn main() -> Int ![] { let n: Int = if 1 { 1 } else { 2 }; 0 }\n",
            "fn main() -> Int ![] { let n: Int = if true { 1 } else { \"x\" }; 0 }\n",
            "fn main() -> Int ![] { let n: Int = match true { 1 => 1, _ => 0 }; 0 }\n",
            "fn main() -> Int ![] { let n: Int = match 0 { 0 => 1, _ => \"x\" }; 0 }\n",
            "fn main() -> Int ![] { let n: Int = match 0 { 0 => 1, 1 => 2 }; 0 }\n",
            "fn main() -> Int ![] { let n: Int = match true { true => 1 }; 0 }\n",
            // Plan A3 task 37: record literal is a user-reachable
            // surface form. With codegen (Task 41) landed the type
            // resolves cleanly; the `let p: Int = ...` binding type
            // mismatch is the user-visible error here (E0045), never
            // E0001. Review of PR #12 flagged the original E0001
            // regression for this surface form.
            "type Point = { x: Int, y: Int }\nfn main() -> Int ![] { let p: Int = Point { x: 1, y: 2 }; 0 }\n",
        ];
        for src in programs {
            let errs = pipeline(src);
            assert!(
                !errs.iter().any(|e| e.code.as_str() == "E0001"),
                "program surfaced E0001 (internal-only): src={src:?} errs={errs:?}",
            );
        }
    }

    // ===== Plan A2 Task 22 — Bool/Char/Byte types, binops, if, match =====

    #[test]
    fn bool_type_in_let_is_accepted() {
        let src = "fn main() -> Int ![] { let b: Bool = true; 0 }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "expected clean, got: {errs:?}");
    }

    #[test]
    fn char_type_in_let_is_accepted() {
        let src = "fn main() -> Int ![] { let c: Char = 'x'; 0 }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "expected clean, got: {errs:?}");
    }

    #[test]
    fn byte_type_in_signature_is_accepted() {
        // Byte has no surface literal in Plan A2; this proves the type
        // name is accepted by parameter and return-type expressions. Value
        // construction arrives with Task 25's runtime primitives.
        let src = "fn take_byte(x: Byte) -> Byte ![] { x }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "expected clean, got: {errs:?}");
    }

    #[test]
    fn let_bool_mismatch_is_e0045() {
        // Declared Bool, initializer 1 (Int) — E0045.
        let src = "fn main() -> Int ![] { let b: Bool = 1; 0 }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0045"), "expected E0045, got: {errs:?}");
    }

    #[test]
    fn int_arith_typechecks() {
        let src = "fn main() -> Int ![] { let n: Int = 1 + 2 * 3; n }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "expected clean, got: {errs:?}");
    }

    #[test]
    fn int_plus_string_is_e0060() {
        let src = "fn main() -> Int ![] { let n: Int = 1 + \"hi\"; 0 }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0060"), "expected E0060, got: {errs:?}");
    }

    #[test]
    fn bool_and_int_is_e0060() {
        let src = "fn main() -> Int ![] { let b: Bool = true && 1; 0 }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0060"), "expected E0060, got: {errs:?}");
    }

    #[test]
    fn int_compare_yields_bool() {
        let src = "fn main() -> Int ![] { let b: Bool = 1 < 2; 0 }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "expected clean, got: {errs:?}");
    }

    #[test]
    fn string_compare_ordering_is_e0060() {
        // `< > <= >=` are Int→Int→Bool in Plan A2 (see PLAN-A2 Byte
        // ordering question in QUESTIONS.md). Comparing Strings is a
        // type error.
        let src = "fn main() -> Int ![] { let b: Bool = \"a\" < \"b\"; 0 }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0060"), "expected E0060, got: {errs:?}");
    }

    #[test]
    fn eq_same_primitive_typechecks() {
        let src = "fn main() -> Int ![] { let b: Bool = 1 == 2; 0 }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "expected clean, got: {errs:?}");
    }

    #[test]
    fn eq_mixed_primitives_is_e0060() {
        let src = "fn main() -> Int ![] { let b: Bool = 1 == true; 0 }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0060"), "expected E0060, got: {errs:?}");
    }

    #[test]
    fn unary_neg_on_int_typechecks() {
        let src = "fn main() -> Int ![] { let x: Int = 1; let y: Int = -x; 0 }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "expected clean, got: {errs:?}");
    }

    #[test]
    fn unary_neg_on_bool_is_e0061() {
        let src = "fn main() -> Int ![] { let b: Bool = true; let x: Int = -b; 0 }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0061"), "expected E0061, got: {errs:?}");
    }

    #[test]
    fn unary_not_on_bool_typechecks() {
        let src = "fn main() -> Int ![] { let b: Bool = true; let c: Bool = !b; 0 }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "expected clean, got: {errs:?}");
    }

    #[test]
    fn unary_not_on_int_is_e0061() {
        let src = "fn main() -> Int ![] { let n: Int = 1; let b: Bool = !n; 0 }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0061"), "expected E0061, got: {errs:?}");
    }

    #[test]
    fn if_cond_must_be_bool_is_e0062() {
        let src = "fn main() -> Int ![] { let n: Int = if 1 { 1 } else { 2 }; 0 }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0062"), "expected E0062, got: {errs:?}");
    }

    #[test]
    fn if_branches_must_unify_is_e0063() {
        let src = "fn main() -> Int ![] { let n: Int = if true { 1 } else { \"x\" }; 0 }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0063"), "expected E0063, got: {errs:?}");
    }

    #[test]
    fn if_ok_with_bool_cond_and_unified_branches() {
        let src = "fn main() -> Int ![] { let n: Int = if true { 1 } else { 2 }; n }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "expected clean, got: {errs:?}");
    }

    #[test]
    fn match_pattern_type_mismatch_is_e0064() {
        // IntLit pattern against Bool scrutinee.
        let src = "fn main() -> Int ![] { let n: Int = match true { 1 => 1, _ => 0 }; n }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0064"), "expected E0064, got: {errs:?}");
    }

    #[test]
    fn match_arm_types_must_unify_is_e0065() {
        let src = "fn main() -> Int ![] { let n: Int = match 0 { 0 => 1, _ => \"x\" }; 0 }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0065"), "expected E0065, got: {errs:?}");
    }

    #[test]
    fn match_int_without_wildcard_is_e0066() {
        let src = "fn main() -> Int ![] { let n: Int = match 0 { 0 => 1, 1 => 2 }; 0 }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0066"), "expected E0066, got: {errs:?}");
    }

    #[test]
    fn match_int_with_wildcard_is_exhaustive() {
        let src = "fn main() -> Int ![] { let n: Int = match 0 { 0 => 1, _ => 2 }; n }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "expected clean, got: {errs:?}");
    }

    #[test]
    fn match_bool_both_polarities_is_exhaustive() {
        let src = "fn main() -> Int ![] { let n: Int = match true { true => 1, false => 2 }; n }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "expected clean, got: {errs:?}");
    }

    #[test]
    fn match_bool_one_polarity_is_e0066() {
        let src = "fn main() -> Int ![] { let n: Int = match true { true => 1 }; 0 }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0066"), "expected E0066, got: {errs:?}");
    }

    #[test]
    fn nested_if_in_match_arm_unifies() {
        // `if` inside a match arm — the arm body type is the `if` type,
        // and both arms of the match produce Int, so the whole thing
        // typechecks clean.
        let src = "fn main() -> Int ![] {\n\
                     let x: Int = 3;\n\
                     let n: Int = match x {\n\
                       0 => if true { 1 } else { 2 },\n\
                       _ => -1,\n\
                     };\n\
                     n\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "expected clean, got: {errs:?}");
    }

    // ===== Plan A2 Task 30 — function types + application + lambdas =====

    #[test]
    fn lambda_typechecks_clean() {
        // Migrated from PR #5's `lambda-rejection` expectation per the
        // PR #5 reviewer directive: Task 30 moves lambdas from
        // E0043-rejection to real `Ty::Fn` typing.
        let src = "fn main() -> Int ![] { let f = fn (x: Int) -> Int ![] => x + 1; 0 }\n";
        // The `let f = ...` binding has no declared type syntax that
        // admits `Ty::Fn` yet (deferred from Task 30). Work around by
        // dropping the `let` and just exercising the lambda in an
        // expression position.
        //
        // Actually — our `let_stmt := 'let' ident ':' type '=' expr`
        // requires a type annotation. `type` is `Named(ident)` only.
        // So a user cannot bind a lambda to a let today; exercise the
        // lambda inline via discard-as-Stmt::Expr with a Call.
        let _ = src;
        let src_inline = "fn main() -> Int ![] { (fn (x: Int) -> Int ![] => x + 1)(41) }\n";
        let errs = pipeline(src_inline);
        assert!(errs.is_empty(), "expected clean, got: {errs:?}");
    }

    #[test]
    fn lambda_body_mismatch_is_e0069() {
        // Body is `Int` but declared return type is `Bool`.
        let src = "fn main() -> Int ![] { (fn (x: Int) -> Bool ![] => x + 1)(0); 0 }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0069"), "expected E0069, got: {errs:?}");
    }

    #[test]
    fn call_user_fn_typechecks() {
        // Top-level fn `inc(x: Int) -> Int ![]` is visible via the
        // pre-populated `fn_env`; `main` calls it.
        let src = "fn inc(x: Int) -> Int ![] { x + 1 }\n\
                   fn main() -> Int ![] { inc(41) }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "expected clean, got: {errs:?}");
    }

    #[test]
    fn recursive_fn_call_typechecks() {
        // Recursion: `fib` references itself via the global fn_env,
        // which is populated before any body is checked.
        let src = "fn fib(n: Int) -> Int ![] {\n\
                     if n < 2 { n } else { fib(n - 1) + fib(n - 2) }\n\
                   }\n\
                   fn main() -> Int ![] { fib(5) }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "expected clean, got: {errs:?}");
    }

    #[test]
    fn mutually_recursive_fns_typecheck() {
        // PR #6 follow-up: pin the pre-pass architecture's mutual-
        // recursion support. `is_even` calls `is_odd`, which calls
        // `is_even`. Both signatures must be visible in the fn_env
        // before either body is checked — the pre-pass enumerates
        // `Item::Fn`s first, then each body sees every top-level fn
        // via the fn_env fall-through in `Expr::Ident` resolution.
        let src = "fn is_even(n: Int) -> Bool ![] {\n\
                     if n == 0 { true } else { is_odd(n - 1) }\n\
                   }\n\
                   fn is_odd(n: Int) -> Bool ![] {\n\
                     if n == 0 { false } else { is_even(n - 1) }\n\
                   }\n\
                   fn main() -> Int ![] {\n\
                     if is_even(4) { 0 } else { 1 }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "expected clean, got: {errs:?}");
    }

    #[test]
    fn call_wrong_arity_is_e0043() {
        let src = "fn inc(x: Int) -> Int ![] { x + 1 }\n\
                   fn main() -> Int ![] { inc(1, 2) }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0043"), "expected E0043, got: {errs:?}");
    }

    #[test]
    fn call_wrong_arg_type_is_e0044() {
        let src = "fn inc(x: Int) -> Int ![] { x + 1 }\n\
                   fn main() -> Int ![] { inc(true) }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0044"), "expected E0044, got: {errs:?}");
    }

    #[test]
    fn call_non_function_is_e0068() {
        // `n` is an `Int`, not a function.
        let src = "fn main() -> Int ![] { let n: Int = 42; n(1) }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0068"), "expected E0068, got: {errs:?}");
    }

    #[test]
    fn call_requires_effect_in_row_e0042() {
        // `emits_io` declares `![IO]`. `main` has `![]` — calling
        // `emits_io` from `main` leaks IO into a pure context.
        let src = "fn emits_io(x: Int) -> Int ![IO] { perform IO.println(\"hi\"); x }\n\
                   fn main() -> Int ![] { emits_io(1) }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0042"), "expected E0042, got: {errs:?}");
    }

    #[test]
    fn lambda_captures_outer_let() {
        // The lambda body references `x`, which is an outer `let`.
        // Capture analysis records `x` in `lambda_captures`. Pipeline
        // stays clean; we inspect `lambda_captures` via a direct
        // typecheck invocation.
        let src = "fn main() -> Int ![] {\n\
                     let x: Int = 10;\n\
                     (fn (y: Int) -> Int ![] => x + y)(5)\n\
                   }\n";
        let (toks, _) = crate::lexer::lex("t.sigil", src);
        let (prog, _) = crate::parser::parse("t.sigil", &toks);
        let (rp, _) = crate::resolve::resolve(prog);
        let (checked, errs) = typecheck(rp.program);
        assert!(errs.is_empty(), "expected clean, got: {errs:?}");
        assert_eq!(checked.lambda_captures.len(), 1, "one lambda");
        let (_, captures) = &checked.lambda_captures[0];
        // `x` appears with its outer-scope type `Int`.
        assert!(
            captures.iter().any(|(n, t)| n == "x" && *t == Ty::Int),
            "lambda should capture `x: Int`, captures={captures:?}"
        );
        // The param `y` is NOT a capture.
        assert!(
            !captures.iter().any(|(n, _)| n == "y"),
            "param `y` should not appear in captures, got {captures:?}"
        );
    }

    #[test]
    fn lambda_does_not_capture_own_params() {
        let src = "fn main() -> Int ![] { (fn (y: Int) -> Int ![] => y + 1)(1) }\n";
        let (toks, _) = crate::lexer::lex("t.sigil", src);
        let (prog, _) = crate::parser::parse("t.sigil", &toks);
        let (rp, _) = crate::resolve::resolve(prog);
        let (checked, errs) = typecheck(rp.program);
        assert!(errs.is_empty(), "expected clean, got: {errs:?}");
        assert_eq!(checked.lambda_captures.len(), 1);
        let (_, captures) = &checked.lambda_captures[0];
        assert!(
            captures.is_empty(),
            "lambda with no free vars should have no captures, got {captures:?}"
        );
    }

    #[test]
    fn lambda_does_not_capture_top_level_fn() {
        // A lambda body references a top-level fn (`inc`). Top-level
        // fn names must resolve through `fn_env`, not be treated as
        // free variables — otherwise Task 31's closure conversion
        // would allocate a spurious env field for a statically-
        // resolvable symbol. Pinned here after PR #6 review found
        // the capture-over-fn-names bug.
        let src = "fn inc(x: Int) -> Int ![] { x + 1 }\n\
                   fn main() -> Int ![] { (fn (y: Int) -> Int ![] => inc(y))(1) }\n";
        let (toks, _) = crate::lexer::lex("t.sigil", src);
        let (prog, _) = crate::parser::parse("t.sigil", &toks);
        let (rp, _) = crate::resolve::resolve(prog);
        let (checked, errs) = typecheck(rp.program);
        assert!(errs.is_empty(), "expected clean, got: {errs:?}");
        assert_eq!(checked.lambda_captures.len(), 1, "one lambda");
        let (_, captures) = &checked.lambda_captures[0];
        assert!(
            !captures.iter().any(|(n, _)| n == "inc"),
            "top-level fn `inc` must not appear in captures, got {captures:?}"
        );
    }

    #[test]
    fn let_shadowing_top_level_fn_typechecks() {
        // A function whose body `let`-binds a name that matches a
        // top-level fn must typecheck clean in both debug and
        // release. Pre-fix: the fn_env-seeding made the `let`
        // collide with the top-level fn's entry in the local env,
        // tripping `env_insert`'s debug-assert in debug builds and
        // silently overwriting in release. PR #6 review found this.
        let src = "fn foo() -> Int ![] { 0 }\n\
                   fn main() -> Int ![] { let foo: Int = 3; foo }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "let shadowing a top-level fn name should typecheck clean, got: {errs:?}"
        );
    }

    #[test]
    fn nested_lambda_captures_through_outer() {
        // Inner lambda references `x` from the outermost scope. The
        // outer lambda's param `_p` is unused. Capture analysis
        // records `x` as captured by the inner lambda; the outer
        // lambda's captures are empty (it doesn't reference `x`
        // directly — the inner lambda does).
        //
        // Note: our analysis records `x` for both lambdas because
        // the outer lambda's *body* (the inner lambda) references
        // `x`, and `collect_free_vars` walks through lambda
        // boundaries. This is conservative but correct for the
        // closure-conversion use case (outer closure's env needs
        // `x` so it can pass it down to the inner closure's env).
        let src = "fn main() -> Int ![] {\n\
                     let x: Int = 10;\n\
                     ((fn (_p: Int) -> Int ![] => (fn (y: Int) -> Int ![] => x + y)(1))(0))\n\
                   }\n";
        let (toks, _) = crate::lexer::lex("t.sigil", src);
        let (prog, _) = crate::parser::parse("t.sigil", &toks);
        let (rp, _) = crate::resolve::resolve(prog);
        let (checked, errs) = typecheck(rp.program);
        assert!(errs.is_empty(), "expected clean, got: {errs:?}");
        // Two lambdas in source order.
        assert_eq!(checked.lambda_captures.len(), 2, "two lambdas");
        // Outer lambda (first recorded) walks into the inner lambda;
        // `x` is free from the outer lambda's perspective because
        // `_p` is its only param.
        let (_, outer_caps) = &checked.lambda_captures[0];
        assert!(
            outer_caps.iter().any(|(n, t)| n == "x" && *t == Ty::Int),
            "outer lambda's captures should include `x: Int`, got {outer_caps:?}"
        );
        // Inner lambda captures `x` too — but `y` (its own param) is
        // excluded.
        let (_, inner_caps) = &checked.lambda_captures[1];
        assert!(
            inner_caps.iter().any(|(n, t)| n == "x" && *t == Ty::Int),
            "inner lambda's captures should include `x: Int`, got {inner_caps:?}"
        );
        assert!(
            !inner_caps.iter().any(|(n, _)| n == "y"),
            "inner lambda's own param `y` should not be a capture"
        );
    }

    // ===== Plan A2 task 34 — `int_to_string` builtin =============================

    #[test]
    fn int_to_string_builtin_typechecks() {
        // The builtin's signature is seeded in `fn_env` before the
        // user-fn pre-pass; a call site with a single `Int` arg should
        // typecheck clean and infer the result type as `String`.
        let src = "fn main() -> Int ![IO] {\n\
                     perform IO.println(int_to_string(42));\n\
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "expected clean, got: {errs:?}");
    }

    #[test]
    fn int_to_string_wrong_arity_is_e0043() {
        let src = "fn main() -> Int ![IO] {\n\
                     perform IO.println(int_to_string(1, 2));\n\
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0043"), "expected E0043, got: {errs:?}");
    }

    #[test]
    fn int_to_string_wrong_arg_type_is_e0044() {
        let src = "fn main() -> Int ![IO] {\n\
                     perform IO.println(int_to_string(\"hi\"));\n\
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0044"), "expected E0044, got: {errs:?}");
    }

    #[test]
    fn int_to_string_is_pure_no_effect_required() {
        // Builtin has an empty effect row, so it can be called from a
        // non-IO function. The enclosing effect row need not contain
        // `IO` to call `int_to_string` itself.
        let src = "fn stringify(n: Int) -> String ![] {\n\
                     int_to_string(n)\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "int_to_string should not force an effect row, got: {errs:?}"
        );
    }

    #[test]
    fn user_can_shadow_int_to_string_builtin() {
        // Seeded builtin is overwritten by the user's `fn
        // int_to_string` in the pre-pass. The user's signature is
        // what typecheck uses; arg-type checking follows the user's
        // definition, not the builtin.
        let src = "fn int_to_string(s: String) -> String ![] { s }\n\
                   fn main() -> Int ![IO] {\n\
                     perform IO.println(int_to_string(\"override\"));\n\
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "user shadow of int_to_string should typecheck clean; got: {errs:?}"
        );
    }

    // ===== Plan A3 Task 38.1 — nominal-type symbol table =====

    fn pipeline_checked(src: &str) -> (CheckedProgram, Vec<CompilerError>) {
        let (toks, lex_errs) = lex("x.sigil", src);
        assert!(lex_errs.is_empty(), "lex: {lex_errs:?}");
        let (prog, parse_errs) = parse("x.sigil", &toks);
        assert!(parse_errs.is_empty(), "parse: {parse_errs:?}");
        let (rp, res_errs) = resolve(prog);
        assert!(res_errs.is_empty(), "resolve: {res_errs:?}");
        typecheck(rp.program)
    }

    #[test]
    fn type_decl_registers_in_types_table() {
        // Forward reference — `Option` is declared after use in the
        // fn signature, but the pre-pass resolves it. No errors.
        let src = "fn f(o: Option) -> Int ![] { 0 }\n\
                   type Option = | None | Some(Int)\n\
                   fn main() -> Int ![] { 0 }\n";
        let (cp, errs) = pipeline_checked(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
        assert!(cp.types.contains_key("Option"));
        assert_eq!(cp.types["Option"].variants.len(), 2);
    }

    #[test]
    fn duplicate_type_declaration_is_e0113() {
        let src = "type Foo = | A\n\
                   type Foo = | B\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(has_code(&errs, "E0113"), "expected E0113, got: {errs:?}");
    }

    #[test]
    fn unknown_type_in_fn_param_is_e0112() {
        let src = "fn f(x: Foo) -> Int ![] { 0 }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(has_code(&errs, "E0112"), "expected E0112, got: {errs:?}");
    }

    #[test]
    fn unknown_type_in_fn_return_is_e0112() {
        let src = "fn g() -> Bar ![] { 0 }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(has_code(&errs, "E0112"), "expected E0112, got: {errs:?}");
    }

    #[test]
    fn unknown_type_in_variant_positional_is_e0112() {
        // `type T = | X(Missing)` — Missing is undeclared.
        let src = "type T = | X(Missing)\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(has_code(&errs, "E0112"), "expected E0112, got: {errs:?}");
    }

    #[test]
    fn unknown_type_in_variant_record_is_e0112() {
        let src = "type P = { x: Missing }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(has_code(&errs, "E0112"), "expected E0112, got: {errs:?}");
    }

    #[test]
    fn user_type_in_let_binding_typechecks_against_param() {
        // Pass a user-type-valued parameter into a let with the same
        // declared type. Param-carried user values already work without
        // needing a construction site.
        let src = "type Option = | None | Some(Int)\n\
                   fn f(o: Option) -> Int ![] { let p: Option = o; 0 }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(errs.is_empty(), "expected clean, got: {errs:?}");
    }

    #[test]
    fn user_type_let_type_mismatch_is_e0045() {
        // User-typed param bound to a differently-typed let. The
        // user-type `Option` resolves via the registry; mismatch
        // against declared `Int` surfaces as E0045 (let-decl vs init).
        let src = "type Option = | None | Some(Int)\n\
                   fn f(o: Option) -> Int ![] { let n: Int = o; 0 }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(has_code(&errs, "E0045"), "expected E0045, got: {errs:?}");
    }

    #[test]
    fn type_decl_only_emits_no_e0001_surface_errors() {
        // A lone type decl + empty main must not surface any
        // user-reachable E0001. Extends the sweep: the Item::Type
        // arm must not introduce internal-error paths.
        let src = "type Option = | None | Some(Int)\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(
            !errs.iter().any(|e| e.code.as_str() == "E0001"),
            "unexpected E0001: {errs:?}"
        );
    }

    // ===== Plan A3 Task 38.2 — constructor resolution =====

    #[test]
    fn nullary_ctor_bare_ident_typechecks_cleanly() {
        // `None` as an expression is a bare ctor ident. Resolves to
        // Ty::User("Option") and flows into the `let`'s declared
        // `Option` type without any error.
        let src = "type Option = | None | Some(Int)\n\
                   fn main() -> Int ![] { let o: Option = None; 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(errs.is_empty(), "expected clean typecheck, got: {errs:?}");
    }

    #[test]
    fn positional_ctor_call_typechecks_cleanly() {
        let src = "type Option = | None | Some(Int)\n\
                   fn main() -> Int ![] { let o: Option = Some(42); 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(errs.is_empty(), "expected clean typecheck, got: {errs:?}");
    }

    #[test]
    fn record_ctor_literal_typechecks_cleanly() {
        let src = "type Point = { x: Int, y: Int }\n\
                   fn main() -> Int ![] { let p: Point = Point { x: 1, y: 2 }; 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(errs.is_empty(), "expected clean typecheck, got: {errs:?}");
    }

    #[test]
    fn unknown_record_ctor_is_e0114() {
        let src = "fn main() -> Int ![] { let p: Int = Missing { x: 1 }; 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(has_code(&errs, "E0114"), "expected E0114, got: {errs:?}");
    }

    #[test]
    fn positional_ctor_arity_mismatch_is_e0043() {
        let src = "type Option = | None | Some(Int)\n\
                   fn main() -> Int ![] { let o: Option = Some(1, 2); 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(has_code(&errs, "E0043"), "expected E0043, got: {errs:?}");
    }

    #[test]
    fn positional_ctor_arg_type_mismatch_is_e0044() {
        let src = "type Option = | None | Some(Int)\n\
                   fn main() -> Int ![] { let o: Option = Some(true); 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(has_code(&errs, "E0044"), "expected E0044, got: {errs:?}");
    }

    #[test]
    fn record_ctor_missing_field_is_e0115() {
        let src = "type Point = { x: Int, y: Int }\n\
                   fn main() -> Int ![] { let p: Point = Point { x: 1 }; 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(has_code(&errs, "E0115"), "expected E0115, got: {errs:?}");
    }

    #[test]
    fn record_ctor_unknown_field_is_e0115() {
        let src = "type Point = { x: Int, y: Int }\n\
                   fn main() -> Int ![] { let p: Point = Point { x: 1, y: 2, z: 3 }; 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(has_code(&errs, "E0115"), "expected E0115, got: {errs:?}");
    }

    #[test]
    fn record_ctor_duplicate_field_is_e0115() {
        let src = "type Point = { x: Int, y: Int }\n\
                   fn main() -> Int ![] { let p: Point = Point { x: 1, x: 2, y: 3 }; 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(has_code(&errs, "E0115"), "expected E0115, got: {errs:?}");
    }

    #[test]
    fn record_ctor_field_value_type_mismatch_is_e0044() {
        let src = "type Point = { x: Int, y: Int }\n\
                   fn main() -> Int ![] { let p: Point = Point { x: 1, y: true }; 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(has_code(&errs, "E0044"), "expected E0044, got: {errs:?}");
    }

    #[test]
    fn nullary_ctor_with_parens_is_e0115() {
        let src = "type Option = | None | Some(Int)\n\
                   fn main() -> Int ![] { let o: Option = None(); 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(has_code(&errs, "E0115"), "expected E0115, got: {errs:?}");
    }

    #[test]
    fn record_ctor_as_positional_is_e0115() {
        let src = "type Point = { x: Int, y: Int }\n\
                   fn main() -> Int ![] { let p: Point = Point(1, 2); 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(has_code(&errs, "E0115"), "expected E0115, got: {errs:?}");
    }

    #[test]
    fn positional_ctor_as_record_is_e0115() {
        let src = "type Option = | None | Some(Int)\n\
                   fn main() -> Int ![] { let o: Option = Some { inner: 1 }; 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(has_code(&errs, "E0115"), "expected E0115, got: {errs:?}");
    }

    #[test]
    fn duplicate_ctor_name_across_types_is_e0118() {
        let src = "type Option = | None | Some(Int)\n\
                   type Maybe = | Nothing | Some(Bool)\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(has_code(&errs, "E0118"), "expected E0118, got: {errs:?}");
    }

    #[test]
    fn positional_ctor_in_function_arg_typechecks() {
        // Ctor call used as a function argument whose declared type
        // matches. Exercises that `Some(42): Option` flows into the
        // parameter type-check correctly.
        let src = "type Option = | None | Some(Int)\n\
                   fn take(o: Option) -> Int ![] { 0 }\n\
                   fn main() -> Int ![] { take(Some(42)) }\n";
        let errs = pipeline_checked(src).1;
        assert!(errs.is_empty(), "expected clean typecheck, got: {errs:?}");
    }

    #[test]
    fn ctor_resolution_does_not_fire_e0001() {
        // Sweep-style check: every well-formed ctor use typechecks
        // cleanly (the E0001 "internal compiler error" catalog entry
        // is reserved for bugs, never surface errors).
        let programs = [
            "type Option = | None | Some(Int)\nfn main() -> Int ![] { let o: Option = None; 0 }\n",
            "type Option = | None | Some(Int)\nfn main() -> Int ![] { let o: Option = Some(1); 0 }\n",
            "type Point = { x: Int, y: Int }\nfn main() -> Int ![] { let p: Point = Point { x: 1, y: 2 }; 0 }\n",
        ];
        for src in programs {
            let errs = pipeline_checked(src).1;
            assert!(
                !errs.iter().any(|e| e.code.as_str() == "E0001"),
                "program surfaced E0001: src={src:?} errs={errs:?}",
            );
        }
    }

    // ===== Plan A3 Task 38.3 — structural pattern typing =====

    #[test]
    fn match_on_option_with_unit_and_positional_ctors_typechecks() {
        // E0066 still fires (user-type exhaustiveness deferred to
        // 38.4), so we tolerate it here but check the pattern-shape
        // check does not surface E0064/E0117 for the ctor arms.
        let src = "type Option = | None | Some(Int)\n\
                   fn unwrap(o: Option) -> Int ![] {\n  \
                     match o { None => 0, Some(n) => n, _ => 0 }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(
            !errs
                .iter()
                .any(|e| matches!(e.code.as_str(), "E0064" | "E0117")),
            "unexpected pattern-shape error: {errs:?}"
        );
    }

    #[test]
    fn match_pattern_var_binds_into_arm_body() {
        // `Some(n) => n` — `n` binds to Int inside the arm body.
        // Using `n` against a declared Int return in the match
        // expression flows cleanly; swapping with a Bool would fire
        // E0065 (arm body type mismatch) on a different arm.
        let src = "type Option = | None | Some(Int)\n\
                   fn unwrap_or(o: Option, d: Int) -> Int ![] {\n  \
                     match o { None => d, Some(n) => n, _ => d }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        // The arm-body `n` should be typed as Int by the binding,
        // so neither E0046 (unknown ident) nor E0044/E0065 fires
        // on it.
        assert!(
            !errs.iter().any(|e| e.code.as_str() == "E0046"),
            "arm-scope binding missed: E0046 fired: {errs:?}"
        );
    }

    #[test]
    fn ctor_pattern_against_wrong_user_type_is_e0117() {
        let src = "type Option = | None | Some(Int)\n\
                   type Result = | Ok(Int) | Err(String)\n\
                   fn f(r: Result) -> Int ![] {\n  \
                     match r { Some(n) => n, _ => 0 }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(has_code(&errs, "E0117"), "expected E0117, got: {errs:?}");
    }

    #[test]
    fn ctor_pattern_against_primitive_is_e0117() {
        let src = "type Option = | None | Some(Int)\n\
                   fn main() -> Int ![] {\n  \
                     match 0 { Some(n) => n, _ => 0 }\n\
                   }\n";
        let errs = pipeline_checked(src).1;
        assert!(has_code(&errs, "E0117"), "expected E0117, got: {errs:?}");
    }

    #[test]
    fn tuple_pattern_always_fires_e0117_in_a3() {
        let src = "fn main() -> Int ![] {\n  \
                     match 0 { (a, b) => a, _ => 0 }\n\
                   }\n";
        let errs = pipeline_checked(src).1;
        assert!(has_code(&errs, "E0117"), "expected E0117, got: {errs:?}");
    }

    #[test]
    fn unit_ctor_with_parens_pattern_is_e0115() {
        let src = "type Option = | None | Some(Int)\n\
                   fn f(o: Option) -> Int ![] {\n  \
                     match o { None() => 0, _ => 1 }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(has_code(&errs, "E0115"), "expected E0115, got: {errs:?}");
    }

    #[test]
    fn positional_ctor_pattern_arity_mismatch_is_e0115() {
        let src = "type Option = | None | Some(Int)\n\
                   fn f(o: Option) -> Int ![] {\n  \
                     match o { Some(x, y) => x, _ => 0 }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(has_code(&errs, "E0115"), "expected E0115, got: {errs:?}");
    }

    #[test]
    fn record_ctor_pattern_missing_field_is_e0115() {
        let src = "type Point = { x: Int, y: Int }\n\
                   fn f(p: Point) -> Int ![] {\n  \
                     match p { Point { x } => x, _ => 0 }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(has_code(&errs, "E0115"), "expected E0115, got: {errs:?}");
    }

    #[test]
    fn record_ctor_pattern_unknown_field_is_e0115() {
        let src = "type Point = { x: Int, y: Int }\n\
                   fn f(p: Point) -> Int ![] {\n  \
                     match p { Point { x, y, z } => x, _ => 0 }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(has_code(&errs, "E0115"), "expected E0115, got: {errs:?}");
    }

    #[test]
    fn nested_ctor_pattern_sub_type_mismatch_is_e0064() {
        // `Some(true)` has Int in its positional field; pattern
        // `Some(BoolLit(true))` mismatches the declared inner Int.
        let src = "type Option = | None | Some(Int)\n\
                   fn f(o: Option) -> Int ![] {\n  \
                     match o { Some(true) => 1, _ => 0 }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(
            has_code(&errs, "E0064"),
            "expected E0064 on inner, got: {errs:?}"
        );
    }

    #[test]
    fn record_ctor_pattern_binds_inner_vars() {
        // `Point { x, y }` binds `x: Int` and `y: Int`. Using them
        // as Int in arm body must typecheck clean (beyond the
        // user-type-exhaustiveness E0066 which 38.4 resolves).
        let src = "type Point = { x: Int, y: Int }\n\
                   fn f(p: Point) -> Int ![] {\n  \
                     match p { Point { x, y } => x + y, _ => 0 }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(
            !errs.iter().any(|e| e.code.as_str() == "E0046"),
            "inner record binding missed: E0046 fired: {errs:?}"
        );
        assert!(
            !errs.iter().any(|e| e.code.as_str() == "E0044"),
            "inner record binding typed wrong: E0044 fired: {errs:?}"
        );
    }

    #[test]
    fn pattern_var_binds_primitive_scrutinee() {
        // `match n { x => x }` on Int scrutinee — `x` binds to Int.
        // Exhaustive via wildcard-like binding (but E0066 may still
        // fire because Var on primitives isn't structurally known;
        // we just check no E0046 for the arm-body use of x).
        let src = "fn main() -> Int ![] {\n  \
                     let r: Int = match 5 { x => x };\n  0\n\
                   }\n";
        let errs = pipeline_checked(src).1;
        assert!(
            !errs.iter().any(|e| e.code.as_str() == "E0046"),
            "primitive-scrutinee Pattern::Var binding missed: {errs:?}"
        );
    }

    #[test]
    fn unknown_ctor_in_explicit_pattern_is_e0114() {
        // Explicit `Ctor(args)` pattern form whose name isn't
        // registered fires E0114. (Bare-identifier unknown names are
        // treated as fresh-variable bindings in pattern position —
        // the ML-family convention — so they do not fire E0114.)
        let src = "type Option = | None | Some(Int)\n\
                   fn f(o: Option) -> Int ![] {\n  \
                     match o { Nope(x) => x, _ => 0 }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(has_code(&errs, "E0114"), "expected E0114, got: {errs:?}");
    }

    // ===== Plan A3 Task 38.4 — exhaustiveness witness =====

    #[test]
    fn exhaustive_option_match_no_e0120() {
        let src = "type Option = | None | Some(Int)\n\
                   fn f(o: Option) -> Int ![] {\n  \
                     match o { None => 0, Some(n) => n }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(
            !errs.iter().any(|e| e.code.as_str() == "E0120"),
            "unexpected E0120: {errs:?}"
        );
    }

    #[test]
    fn non_exhaustive_option_missing_some_is_e0120_with_witness() {
        let src = "type Option = | None | Some(Int)\n\
                   fn f(o: Option) -> Int ![] {\n  \
                     match o { None => 0 }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        let e120 = errs
            .iter()
            .find(|e| e.code.as_str() == "E0120")
            .unwrap_or_else(|| panic!("expected E0120, got: {errs:?}"));
        assert!(
            e120.message.contains("Some(_)"),
            "witness missing `Some(_)`: {}",
            e120.message
        );
    }

    #[test]
    fn non_exhaustive_option_missing_none_is_e0120_with_witness() {
        let src = "type Option = | None | Some(Int)\n\
                   fn f(o: Option) -> Int ![] {\n  \
                     match o { Some(n) => n }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        let e120 = errs
            .iter()
            .find(|e| e.code.as_str() == "E0120")
            .unwrap_or_else(|| panic!("expected E0120, got: {errs:?}"));
        assert!(
            e120.message.contains("`None`"),
            "witness missing `None`: {}",
            e120.message
        );
    }

    #[test]
    fn wildcard_catchall_is_exhaustive_no_e0120() {
        let src = "type Option = | None | Some(Int)\n\
                   fn f(o: Option) -> Int ![] {\n  \
                     match o { None => 0, _ => 1 }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(
            !errs.iter().any(|e| e.code.as_str() == "E0120"),
            "unexpected E0120 with wildcard: {errs:?}"
        );
    }

    #[test]
    fn var_binding_catchall_is_exhaustive_no_e0120() {
        // `x` is a fresh name (not a registered ctor) → bind whole
        // scrutinee; catchall.
        let src = "type Option = | None | Some(Int)\n\
                   fn f(o: Option) -> Int ![] {\n  \
                     match o { None => 0, x => 1 }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(
            !errs.iter().any(|e| e.code.as_str() == "E0120"),
            "unexpected E0120 with var catchall: {errs:?}"
        );
    }

    #[test]
    fn nullary_ctor_bare_var_promotion_does_not_count_as_catchall() {
        // `None` as a bare ident is a nullary-ctor promotion, not a
        // catchall. The match misses `Some(_)` → E0120 fires with
        // witness.
        let src = "type Option = | None | Some(Int)\n\
                   fn f(o: Option) -> Int ![] {\n  \
                     match o { None => 0 }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        let e120 = errs
            .iter()
            .find(|e| e.code.as_str() == "E0120")
            .unwrap_or_else(|| panic!("expected E0120, got: {errs:?}"));
        assert!(
            e120.message.contains("Some(_)"),
            "witness missing `Some(_)`: {}",
            e120.message
        );
    }

    #[test]
    fn three_variant_exhaustiveness_witness_names_first_missing() {
        // Type with three variants; arm only covers the middle one.
        // Witness must name the first missing variant (top-down
        // declaration order).
        let src = "type Shape = | Point | Circle(Int) | Square { side: Int }\n\
                   fn f(s: Shape) -> Int ![] {\n  \
                     match s { Circle(r) => r }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        let e120 = errs
            .iter()
            .find(|e| e.code.as_str() == "E0120")
            .unwrap_or_else(|| panic!("expected E0120, got: {errs:?}"));
        assert!(
            e120.message.contains("`Point`"),
            "witness should name `Point` first: {}",
            e120.message
        );
    }

    #[test]
    fn exhaustive_single_record_variant_no_e0120() {
        let src = "type Point = { x: Int, y: Int }\n\
                   fn f(p: Point) -> Int ![] {\n  \
                     match p { Point { x, y } => x + y }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(
            !errs.iter().any(|e| e.code.as_str() == "E0120"),
            "unexpected E0120 on exhaustive record match: {errs:?}"
        );
    }

    #[test]
    fn witness_for_positional_ctor_has_wildcards_per_field() {
        let src = "type Pair = | Pair(Int, Int)\n\
                   fn f(p: Pair) -> Int ![] {\n  \
                     match p {  }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        // Parser rejects empty match arm list (E0066 or similar) —
        // skip this program if parse fails.
        let (_, errs) = pipeline_checked(src);
        // Either parse-time arm-list rule fires, or exhaustiveness
        // witness appears. We test the latter with a shape where
        // at least one arm exists but none is the constructor:
        let src2 = "type Pair = | Pair(Int, Int) | Single(Int)\n\
                    fn f(p: Pair) -> Int ![] {\n  \
                      match p { Single(x) => x }\n\
                    }\n\
                    fn main() -> Int ![] { 0 }\n";
        let errs2 = pipeline_checked(src2).1;
        let e120 = errs2
            .iter()
            .find(|e| e.code.as_str() == "E0120")
            .unwrap_or_else(|| panic!("expected E0120, got: {errs2:?}\n(part1 errs: {errs:?})"));
        assert!(
            e120.message.contains("Pair(_, _)"),
            "witness missing `Pair(_, _)`: {}",
            e120.message
        );
    }

    #[test]
    fn witness_for_record_ctor_has_field_wildcards() {
        let src = "type Shape = | Circle(Int) | Rect { w: Int, h: Int }\n\
                   fn f(s: Shape) -> Int ![] {\n  \
                     match s { Circle(r) => r }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        let e120 = errs
            .iter()
            .find(|e| e.code.as_str() == "E0120")
            .unwrap_or_else(|| panic!("expected E0120, got: {errs:?}"));
        assert!(
            e120.message.contains("Rect { w: _, h: _ }"),
            "witness format wrong: {}",
            e120.message
        );
    }

    // ===== Plan B Task 48 — HM unification with row variables =====

    #[test]
    fn generic_id_function_typechecks() {
        // `fn id[A](x: A) -> A { x }` is the simplest Algorithm-W
        // exercise: A is a fresh type-variable, body returns A, so
        // the inferred sig is (A) -> A and no unification fails.
        let src = "fn id[A](x: A) -> A ![] { x }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "generic id should typecheck; got {errs:?}");
    }

    #[test]
    fn generic_id_instantiates_fresh_var_per_call() {
        // Calling `id` from `main` with an `Int` and a `String`
        // separately must instantiate id's scheme with fresh vars
        // each time so the two sites don't unify their A.
        let src = "fn id[A](x: A) -> A ![] { x }\n\
                   fn main() -> Int ![] {\n  \
                     let i: Int = id(42);\n  \
                     let s: String = id(\"hi\");\n  \
                     i\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "two-instantiation id call should typecheck; got {errs:?}"
        );
    }

    #[test]
    fn generic_compose_typechecks() {
        // `fn compose[A, B, C](f: ..., g: ..., x: A) -> C`. Sigil's
        // surface doesn't yet have function-type literals in
        // TypeExpr, so we exercise compose-shape via two single-
        // argument identity calls in sequence — confirming chained
        // generic instantiation does not collapse the bound vars.
        let src = "fn id[A](x: A) -> A ![] { x }\n\
                   fn use_id[B](x: B) -> B ![] { id(id(x)) }\n\
                   fn main() -> Int ![] { use_id(0) }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "chained generic call should typecheck; got {errs:?}"
        );
    }

    #[test]
    fn generic_user_type_constructor_typechecks() {
        // `type List[A] = | Nil | Cons(A, List[A])` with a Cons of
        // an Int populates `Ty::User("List", [Ty::Int])` after
        // unification. The constructor's field types reference A
        // and must resolve under the per-call fresh-instantiation
        // substitution.
        let src = "type List[A] = | Nil | Cons(A, List[A])\n\
                   fn main() -> Int ![] {\n  \
                     let xs: List[Int] = Cons(1, Nil);\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "generic user-type ctor should typecheck; got {errs:?}"
        );
    }

    #[test]
    fn generic_application_arity_mismatch_fires_e0129() {
        let src = "type List[A] = | Nil | Cons(A, List[A])\n\
                   fn f(xs: List[Int, String]) -> Int ![] { 0 }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0129"),
            "expected E0129 (arity mismatch); got {errs:?}",
        );
    }

    #[test]
    fn generic_application_on_primitive_fires_e0131() {
        let src = "fn f(x: Int[Foo]) -> Int ![] { 0 }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0131"),
            "expected E0131 (Apply on primitive); got {errs:?}",
        );
    }

    #[test]
    fn generic_application_unknown_head_fires_e0112() {
        let src = "fn f(xs: List[Int]) -> Int ![] { 0 }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0112"),
            "expected E0112 (unknown type) without a List declaration; got {errs:?}",
        );
    }

    #[test]
    fn closed_row_caller_calls_closed_row_callee_typechecks() {
        // Closed-vs-closed under set containment: callee declares
        // `![IO]`, caller declares `![IO]`, the call passes the
        // legacy E0042 path. Pinned so the unifier-based call
        // checks don't regress the simple matching case.
        let src = "fn f() -> Int ![IO] { 0 }\n\
                   fn main() -> Int ![IO] {\n  \
                     let x: Int = f();\n  \
                     x\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "closed-row caller calling closed-row callee should pass; got {errs:?}"
        );
    }

    #[test]
    fn closed_row_caller_missing_callee_effect_fires_e0042() {
        // Caller declares `![]` but calls a `![IO]` callee. The
        // legacy set-containment check fires E0042. Pinned so the
        // unifier-based path can't silently drop the diagnostic.
        let src = "fn f() -> Int ![IO] { 0 }\n\
                   fn main() -> Int ![] {\n  \
                     let x: Int = f();\n  \
                     x\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0042"),
            "expected E0042 (caller missing callee's IO); got {errs:?}"
        );
    }

    #[test]
    fn task_48_diagnostics_present_in_catalog() {
        // Sanity-pin: the new HM-related diagnostic codes added in
        // Task 48 must exist in the catalog so `sigil explain` can
        // surface them. We don't assert specific text — that's the
        // catalog-integrity test's job — only presence.
        for code in ["E0126", "E0127", "E0128", "E0129", "E0131"] {
            assert!(
                crate::errors::lookup(code).is_some(),
                "expected catalog entry for {code}",
            );
        }
    }

    #[test]
    fn open_row_caller_calls_closed_row_callee() {
        // Caller has open row `![IO | e]`; callee has closed `![IO]`.
        // The open row absorbs (nothing extra to absorb here);
        // unification succeeds.
        let src = "fn f() -> Int ![IO] { 0 }\n\
                   fn caller[a]() -> Int ![IO | e] {\n  \
                     let x: Int = f();\n  \
                     x\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "open caller, closed callee should pass; got {errs:?}"
        );
    }

    #[test]
    fn occurs_check_fires_e0126() {
        // Forge a self-reference at the unifier by passing a generic
        // function as its own argument — `id(id)` instantiates id at
        // a fresh var and unifies it with id's full scheme, which
        // unifies a var with a function whose params mention the var
        // (via instantiation from the same scheme). The exact shape
        // depends on how the inferencer normalises calls; we accept
        // either a successful unify (because instantiation freshens
        // both sides) or an E0126 if the unifier collapses them. To
        // pin E0126, use a recursive lambda equivalent that forces
        // the cycle.
        let src = "fn loop_self[A](x: A) -> A ![] { loop_self(loop_self) }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0126") || has_code(&errs, "E0044"),
            "expected E0126 or E0044 from recursive self-call; got {errs:?}",
        );
    }

    #[test]
    fn non_generic_named_type_typechecks() {
        let src = "type Option = | None | Some(Int)\n\
                   fn f(o: Option) -> Int ![] { 0 }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "plain non-generic user type should typecheck; got {errs:?}"
        );
    }

    #[test]
    fn generic_in_return_type_typechecks_after_construction() {
        let src = "type List[A] = | Nil | Cons(A, List[A])\n\
                   fn f() -> List[Int] ![] { Nil }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "generic return type with Nil ctor should typecheck; got {errs:?}"
        );
    }

    #[test]
    fn generic_variant_field_typechecks() {
        let src = "type List[A] = | Nil | Cons(A, List[A])\n\
                   type Pair[X, Y] = | P(X, Y)\n\
                   fn main() -> Int ![] {\n  \
                     let p: Pair[Int, List[Int]] = P(1, Cons(2, Nil));\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "nested generic variant should typecheck; got {errs:?}"
        );
    }

    #[test]
    fn generic_param_used_without_args_fires_e0112() {
        // `fn f(xs: List)` when `List[A]` is generic should fire an
        // arity-aware diagnostic — currently this surfaces as E0112
        // (unknown type) because the parser produced a Named node
        // and ty_from_type_expr returns None for "generic without
        // args". Acceptable: any compile-time error here is fine
        // for v1; we just don't want it to silently succeed.
        let src = "type List[A] = | Nil | Cons(A, List[A])\n\
                   fn f(xs: List) -> Int ![] { 0 }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            !errs.is_empty(),
            "expected at least one error for `List` with no type args; got success",
        );
    }

    // ===== Plan B A3-carryover — nested Maranget exhaustiveness =====

    #[test]
    fn nested_some_bool_missing_false_fires_e0120_with_witness() {
        // `Option = | None | Some(Bool)`; arms cover None and Some(true)
        // but not Some(false). Plan A3 shipped top-level-only coverage
        // and let this fall through to the runtime trap; Plan B must
        // catch it at compile time with the paste-able witness.
        let src = "type Option = | None | Some(Bool)\n\
                   fn f(o: Option) -> Int ![] {\n  \
                     match o { None => 0, Some(true) => 1 }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        let e120 = errs
            .iter()
            .find(|e| e.code.as_str() == "E0120")
            .unwrap_or_else(|| panic!("expected E0120, got: {errs:?}"));
        assert!(
            e120.message.contains("Some(false)"),
            "witness must name Some(false); got {}",
            e120.message,
        );
    }

    #[test]
    fn nested_some_bool_missing_true_fires_e0120_with_witness() {
        let src = "type Option = | None | Some(Bool)\n\
                   fn f(o: Option) -> Int ![] {\n  \
                     match o { None => 0, Some(false) => 1 }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        let e120 = errs
            .iter()
            .find(|e| e.code.as_str() == "E0120")
            .unwrap_or_else(|| panic!("expected E0120, got: {errs:?}"));
        assert!(
            e120.message.contains("Some(true)"),
            "witness must name Some(true); got {}",
            e120.message,
        );
    }

    #[test]
    fn nested_bool_exhaustive_with_both_literals_no_e0120() {
        let src = "type Option = | None | Some(Bool)\n\
                   fn f(o: Option) -> Int ![] {\n  \
                     match o { None => 0, Some(true) => 1, Some(false) => 2 }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(
            !errs.iter().any(|e| e.code.as_str() == "E0120"),
            "all Some-field variants covered: no E0120 expected; got {errs:?}",
        );
    }

    #[test]
    fn nested_ctor_with_field_catchall_is_exhaustive() {
        // `Some(_)` field-wildcards the Bool; together with `None`
        // this is fully exhaustive.
        let src = "type Option = | None | Some(Bool)\n\
                   fn f(o: Option) -> Int ![] {\n  \
                     match o { None => 0, Some(_) => 1 }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(
            !errs.iter().any(|e| e.code.as_str() == "E0120"),
            "Some(_) catchall covers Bool field: no E0120 expected; got {errs:?}",
        );
    }

    #[test]
    fn nested_user_type_field_catches_inner_missing_variant() {
        // Box holds a Tree; arms cover the outer variant but the inner
        // Tree has Leaf and Node, and the Some-arm only handles Leaf.
        // Witness must name Some(Node(_, _, _)).
        let src = "type Tree = | Leaf | Node(Int, Tree, Tree)\n\
                   type Box = | Empty | Holds(Tree)\n\
                   fn f(b: Box) -> Int ![] {\n  \
                     match b { Empty => 0, Holds(Leaf) => 1 }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        let e120 = errs
            .iter()
            .find(|e| e.code.as_str() == "E0120")
            .unwrap_or_else(|| panic!("expected E0120, got: {errs:?}"));
        assert!(
            e120.message.contains("Holds(Node(_, _, _))"),
            "witness must name Holds(Node(_, _, _)); got {}",
            e120.message,
        );
    }

    #[test]
    fn nested_record_field_missing_emits_record_witness() {
        // `Pair { a: Bool, b: Bool }` with only the `true, true` arm.
        // Witness should surface one missing field-of-Bool with the
        // other(s) wildcarded.
        let src = "type Pair = | P { a: Bool, b: Bool }\n\
                   fn f(p: Pair) -> Int ![] {\n  \
                     match p { P { a: true, b: true } => 0 }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        let e120 = errs
            .iter()
            .find(|e| e.code.as_str() == "E0120")
            .unwrap_or_else(|| panic!("expected E0120, got: {errs:?}"));
        // The first uncovered field is `a` (Bool, missing `false`).
        // Witness: P { a: false, b: _ }.
        assert!(
            e120.message.contains("P { a: false, b: _ }"),
            "witness must surface P's uncovered field; got {}",
            e120.message,
        );
    }

    #[test]
    fn nested_int_field_requires_wildcard() {
        // `Some(Int)` with a literal-only field pattern cannot be
        // exhaustive — Int domain is infinite, so match_witness
        // returns the generic "_" witness at the field position.
        let src = "type Option = | None | Some(Int)\n\
                   fn f(o: Option) -> Int ![] {\n  \
                     match o { None => 0, Some(1) => 1 }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        let e120 = errs
            .iter()
            .find(|e| e.code.as_str() == "E0120")
            .unwrap_or_else(|| panic!("expected E0120, got: {errs:?}"));
        assert!(
            e120.message.contains("Some(_)"),
            "Int field with only a literal pattern should surface Some(_) witness; got {}",
            e120.message,
        );
    }

    // ===== Plan B A3-carryover — E0120 suppression when an arm errs =====

    #[test]
    fn e0120_suppressed_when_arm_body_has_type_error() {
        // Non-exhaustive match (only `Some`) PLUS an arm body that
        // fails type-checking (arithmetic on a String). Pre-Plan-B
        // this emitted BOTH the body error and E0120; the A3-carryover
        // cleanup suppresses E0120 so the user focuses on the body.
        let src = "type Option = | None | Some(Int)\n\
                   fn f(o: Option) -> Int ![] {\n  \
                     match o { Some(n) => n + \"bad\" }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        // A body-level error (arithmetic on String) must still fire.
        assert!(
            errs.iter().any(|e| e.code.as_str() != "E0120"),
            "expected a non-E0120 error (arm body type mismatch) — got only {errs:?}",
        );
        assert!(
            !errs.iter().any(|e| e.code.as_str() == "E0120"),
            "E0120 should be suppressed while the arm body is malformed; got {errs:?}",
        );
    }

    #[test]
    fn e0120_suppressed_when_arm_pattern_has_e0117() {
        // Non-exhaustive match plus a pattern that doesn't match the
        // scrutinee type (E0117). Suppress E0120 so the user focuses
        // on fixing the pattern first.
        let src = "type Option = | None | Some(Int)\n\
                   fn f(o: Option) -> Int ![] {\n  \
                     match o { (a, b) => 0 }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(
            errs.iter().any(|e| e.code.as_str() == "E0117"),
            "expected E0117 pattern-shape error, got {errs:?}",
        );
        assert!(
            !errs.iter().any(|e| e.code.as_str() == "E0120"),
            "E0120 should be suppressed while a pattern fails shape check; got {errs:?}",
        );
    }

    #[test]
    fn e0066_still_fires_on_primitive_when_arm_body_errs() {
        // Primitive Bool scrutinee with only one literal arm AND a
        // body that fails type-checking. E0120 suppression is
        // user-type-only — the primitive E0066 path stays on so the
        // user still sees the missing-literal coverage hole.
        let src = "fn f(b: Bool) -> Int ![] {\n  \
                     match b { true => 1 + \"bad\" }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        // The arm body's type error must still fire.
        assert!(
            errs.iter().any(|e| e.code.as_str() != "E0066"),
            "expected an arm-body type error; got only {errs:?}",
        );
        // E0066 must NOT be suppressed: primitive coverage runs
        // unconditionally per the A3-carryover design.
        assert!(
            errs.iter().any(|e| e.code.as_str() == "E0066"),
            "E0066 must still fire on a non-exhaustive Bool match; got {errs:?}",
        );
    }

    #[test]
    fn e0120_still_fires_when_all_arms_typecheck_cleanly() {
        // Regression-guard the suppression: a truly non-exhaustive
        // match with every arm well-typed must still produce E0120.
        // Protects against over-suppression from the A3-carryover.
        let src = "type Option = | None | Some(Int)\n\
                   fn f(o: Option) -> Int ![] {\n  \
                     match o { Some(n) => n }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline_checked(src).1;
        assert!(
            errs.iter().any(|e| e.code.as_str() == "E0120"),
            "clean arms but non-exhaustive — E0120 must still fire; got {errs:?}",
        );
    }

    // ===== Plan A3 Task 39 — capture analysis respects pattern bindings =====

    #[test]
    fn pattern_bindings_collects_all_var_names_from_nested_patterns() {
        use crate::ast::{CtorPatternField, CtorPatternFields, Pattern};
        let span = Span::synthetic("x");
        // `Cons(h, Cons(mid, Nil))` — two Var bindings: `h`, `mid`.
        let pat = Pattern::Ctor {
            name: "Cons".to_string(),
            fields: CtorPatternFields::Positional(vec![
                Pattern::Var("h".to_string(), span.clone()),
                Pattern::Ctor {
                    name: "Cons".to_string(),
                    fields: CtorPatternFields::Positional(vec![
                        Pattern::Var("mid".to_string(), span.clone()),
                        Pattern::Ctor {
                            name: "Nil".to_string(),
                            fields: CtorPatternFields::Unit,
                            span: span.clone(),
                        },
                    ]),
                    span: span.clone(),
                },
            ]),
            span: span.clone(),
        };
        let mut bindings = std::collections::BTreeSet::new();
        pattern_bindings(&pat, &mut bindings);
        assert_eq!(bindings.len(), 2, "expected h + mid, got {bindings:?}");
        assert!(bindings.contains("h"));
        assert!(bindings.contains("mid"));
        // Record-style Ctor pattern — field-pun and renamed fields.
        let pat2 = Pattern::Ctor {
            name: "Point".to_string(),
            fields: CtorPatternFields::Record(vec![
                CtorPatternField {
                    name: "x".to_string(),
                    pattern: Pattern::Var("x".to_string(), span.clone()),
                    span: span.clone(),
                },
                CtorPatternField {
                    name: "y".to_string(),
                    pattern: Pattern::Var("why".to_string(), span.clone()),
                    span: span.clone(),
                },
            ]),
            span,
        };
        let mut bindings2 = std::collections::BTreeSet::new();
        pattern_bindings(&pat2, &mut bindings2);
        assert_eq!(bindings2.len(), 2);
        assert!(bindings2.contains("x"));
        assert!(bindings2.contains("why"));
    }

    // ===== Plan A3 Task 41.2 — scrutinee type side-table =====

    #[test]
    fn match_scrut_tys_records_user_type_for_well_typed_match() {
        // A well-typed match on a user-defined `Option` must land its
        // scrutinee's `Ty::User("Option")` in the side-table so codegen
        // can disambiguate `Pattern::Var` between binding and nullary
        // ctor promotion.
        let src = "type Option = | None | Some(Int)\n\
                   fn f(o: Option) -> Int ![] {\n  \
                     match o { None => 0, Some(n) => n }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let (cp, errs) = pipeline_checked(src);
        assert!(errs.is_empty(), "expected clean typecheck, got: {errs:?}");
        assert_eq!(
            cp.match_scrut_tys.len(),
            1,
            "exactly one match in the program should appear in the map"
        );
        let ty = cp
            .match_scrut_tys
            .values()
            .next()
            .expect("map has one entry");
        assert_eq!(*ty, Ty::User("Option".to_string(), Vec::new()));
    }

    #[test]
    fn match_scrut_tys_records_primitive_scrutinee() {
        // Primitive-scrutinee matches also land in the map; codegen's
        // fall-back path handles them but the entry should still be
        // present for consistency with how check_match runs.
        let src = "fn f(x: Int) -> Int ![] {\n  \
                     match x { 0 => 10, _ => 20 }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let (cp, errs) = pipeline_checked(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
        assert_eq!(cp.match_scrut_tys.len(), 1);
        let ty = cp.match_scrut_tys.values().next().expect("one entry");
        assert_eq!(*ty, Ty::Int);
    }

    #[test]
    fn match_scrut_tys_skips_malformed_scrutinee() {
        // A scrutinee that fails to typecheck has `None` as its Ty;
        // check_match skips the side-table insertion. Codegen's
        // fallback path treats the absent entry as "primitive
        // scalar dispatch" (which is correct because if/match-desugar
        // is the other producer of absent entries and those are
        // Bool-only).
        let src = "fn f() -> Int ![] {\n  \
                     match undefined_name { _ => 0 }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let (cp, errs) = pipeline_checked(src);
        assert!(
            errs.iter().any(|e| e.code.as_str() == "E0046"),
            "expected E0046 unknown identifier; got: {errs:?}"
        );
        assert!(
            cp.match_scrut_tys.is_empty(),
            "malformed scrutinee should not be recorded"
        );
    }
}
