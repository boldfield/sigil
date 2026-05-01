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
    /// Plan D Task 113 — tuple type `(T1, T2, ...)`. Arity ≥ 2.
    /// Element-wise unification (each `elems[i]` must unify
    /// position-by-position with the other tuple's `elems[i]`). The
    /// codegen-side runtime layout is a heap record `{header, elem
    /// [0], ..., elem[N-1]}` with elements at offsets `8 + 8*i`
    /// (tuples have no discriminant word; sum-type ctors lay fields
    /// at `16 + 8*i` after the discriminant). The GC pointer bitmap
    /// reflects per-slot pointer-ness.
    Tuple(Vec<Ty>),
    /// Plan D Task 117 — first-class continuation type. Allocated
    /// per-handle at typecheck (one fresh `ScopeId::Concrete(N)` per
    /// `handle ... with` expression's pre-pass entry). The arm's
    /// continuation binding `k` gets type
    /// `Continuation { op_ret, ret, scope_id }` where `op_ret` is
    /// the op's return type (the value `k` accepts) and `ret` is
    /// the handler's overall result type (the value `k(arg)`
    /// evaluates to).
    ///
    /// **Distinct from `Ty::Fn`** even though `k(arg)` reduces to a
    /// fn-call shape at the surface — the typechecker enforces a
    /// dynamic-extent escape barrier (E0145) on `Continuation`-typed
    /// values that doesn't apply to `Fn`-typed values, and the
    /// codegen routes `Continuation` calls through the
    /// `lower_k_pair_call` trampoline emission instead of standard
    /// closure-convention dispatch.
    ///
    /// **Why no value-position constructor**: there's no surface
    /// syntax that *produces* a `Ty::Continuation` value at the
    /// expression level. The only way for a value to have this
    /// type is via the typechecker's `check_handle` arm-processing,
    /// which binds `k` with a fresh `Continuation`. The follow-up
    /// PR #62 added a *type-position* surface form
    /// (`Continuation[op_ret, ret]` — see `ty_from_type_expr_with_-
    /// rows`'s Apply arm) so users can name k's binding type:
    /// `let f: Continuation[Int, Int] = k`. The value-position
    /// non-user-constructible invariant is preserved — there's no
    /// `Continuation { ... }` constructor or analogous expression
    /// surface; `check_handle` remains the sole producer at the
    /// value level. The `Continuation` type name is reserved
    /// (rejected as a user type-decl name in the type-decl pre-pass
    /// to avoid ambiguity with the surface form).
    ///
    /// **Why not Box-the-fields**: `Continuation` is rare (only at
    /// arm-binding sites and let-bindings of those). Boxing the
    /// payload to keep `Ty` register-sized is the standard idiom
    /// here (mirrors `Ty::Fn`).
    Continuation(Box<ContinuationTy>),
}

/// Plan D Task 117 — payload of `Ty::Continuation`. Boxed inside
/// `Ty::Continuation` to keep the parent `Ty` discriminant + payload
/// register-sized (mirrors `Ty::Fn(Box<FnSig>)` precedent).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContinuationTy {
    /// The op's return type. `k(arg)` requires `arg`'s type to unify
    /// with this.
    pub op_ret: Ty,
    /// The handler's overall result type. `k(arg)` evaluates to this.
    pub ret: Ty,
    /// Identifier of the originating handle. Allocated per-handle at
    /// typecheck. Concrete IDs are assigned by the handle pre-pass;
    /// region-polymorphic fns introduce `Var` IDs that unify against
    /// surrounding handle IDs at call sites (mirroring row-var
    /// substitution from Plan B Stage 5).
    pub scope_id: ScopeId,
}

/// Plan D Task 117 — handle scope identifier. Acts as a region tag
/// distinguishing different `handle` expressions' continuation types
/// at the type level. Two `Ty::Continuation` values unify only if
/// their `scope_id`s unify (concrete IDs must be equal; vars bind
/// to concretes or each other).
///
/// Reuses Plan B Stage 5 row-var infrastructure idiom: `Concrete`
/// is the resolved-pin form (one per handle expression), `Var` is
/// the unification-variable form (used in region-polymorphic fn
/// schemes; bound to a concrete at instantiation time).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScopeId {
    /// Concrete handle scope. The `u32` is allocated per-handle by
    /// `Tc::fresh_scope_id`; each `handle` expression's pre-pass
    /// gets a fresh ID. Equal-vs-equal unifies; not-equal-vs-not-
    /// equal fires E0145 ("continuations from different handles
    /// cannot unify").
    Concrete(u32),
    /// Region-polymorphic scope variable. Allocated per-fn at
    /// scheme-instantiation time. Unifies via
    /// `Subst::bind_scope_var` (parallel to `bind_row_var` from
    /// Plan B Stage 5). Every reachable `Var` must be resolved by
    /// inference end (codegen-entry guard rejects survivors).
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
    pub effects: Vec<EffectInst>,
    pub effect_row_var: Option<u32>,
}

/// Plan D Task 114 — Ty-level analogue of `ast::EffectRef`. Rows
/// store effect references with their type-arg lists so generic
/// effect declarations (`effect Raise[E]`) can be referenced
/// distinctly under different instantiations (`![Raise[Int]]` vs
/// `![Raise[String]]`). Bare-name references (`IO`, `Mem`) carry
/// `args: vec![]`; non-empty `args` are the substituted type-args
/// at the row site.
///
/// Equality is structural over `(name, args)`. Two `EffectInst`s
/// with the same name but different args (`Raise[Int]` and
/// `Raise[String]`) compare unequal — row unification must
/// propagate this distinction. `Ord` is **not** derived because
/// `Ty` itself has no total order; `Row::canonicalise` sorts by
/// `name` and dedups by full structural equality, preserving
/// distinct instantiations of the same effect-decl name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectInst {
    pub name: String,
    pub args: Vec<Ty>,
}

impl EffectInst {
    /// Convenience constructor for the bare-name case (the dominant
    /// shape pre-Task-114-surface and for builtins).
    pub fn bare(name: impl Into<String>) -> Self {
        EffectInst {
            name: name.into(),
            args: Vec::new(),
        }
    }

    /// Render to surface form: `Raise` for bare-name refs,
    /// `Raise[Int, String]` for type-parameterized. Used by
    /// `ty_display` and diagnostic messages.
    pub fn display_str(&self) -> String {
        if self.args.is_empty() {
            self.name.clone()
        } else {
            let parts: Vec<String> = self.args.iter().map(ty_display).collect();
            format!("{}[{}]", self.name, parts.join(", "))
        }
    }
}

/// Plan D Task 114 — convert a slice of AST `EffectRef`s to
/// `Vec<EffectInst>` for installation in `FnSig.effects`. Args are
/// converted via `ty_from_type_expr`; the surrounding fn's generic
/// substitution lets `Raise[E]` resolve `E` to its outer-fn binding.
/// `effects_registry` argument carries the program's effect-decl
/// registry so arity-check (E0143) can fire when the row-site arg
/// count diverges from the decl's `generic_params` len.
pub(crate) fn effect_refs_to_insts(
    rs: &[crate::ast::EffectRef],
    types: &std::collections::BTreeMap<String, crate::ast::TypeDecl>,
    generic_subst: &std::collections::BTreeMap<String, Ty>,
) -> Vec<EffectInst> {
    rs.iter()
        .map(|r| {
            let args: Vec<Ty> = r
                .args
                .iter()
                // Fallback to Ty::Unit on resolution failure; the
                // caller's `check_type_expr_known` walk will have
                // already pushed the precise diagnostic. This
                // mirrors `ty_from_type_expr_here`'s defensive
                // unwrap pattern at line ~5440.
                .map(|t| ty_from_type_expr(t, types, generic_subst).unwrap_or(Ty::Unit))
                .collect();
            EffectInst {
                name: r.name.clone(),
                args,
            }
        })
        .collect()
}

/// Plan D Task 114 — reverse direction (Ty -> AST). Given a slice
/// of `EffectInst`s and a span, build `Vec<EffectRef>` for AST-
/// reconstruction sites (`monomorphize::ty_to_type_expr`). Args
/// flow through `ty_to_type_expr` element-wise.
pub(crate) fn insts_to_effect_refs(
    insts: &[EffectInst],
    span: &crate::errors::Span,
) -> Vec<crate::ast::EffectRef> {
    insts
        .iter()
        .map(|e| crate::ast::EffectRef {
            name: e.name.clone(),
            args: e
                .args
                .iter()
                .map(|t| crate::monomorphize::ty_to_type_expr(t, span))
                .collect(),
            span: span.clone(),
        })
        .collect()
}

/// Render a slice of `EffectInst`s as a comma-separated surface
/// string. Used by E0042 / E0128 / E0136 diagnostics.
pub(crate) fn effects_display(es: &[EffectInst]) -> String {
    es.iter()
        .map(|e| e.display_str())
        .collect::<Vec<_>>()
        .join(", ")
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

/// Plan B task 49 — concrete-instantiation record for a single use
/// site of a generic top-level fn or generic user type.
///
/// `name` is the surface name of the callee fn or type. `type_args`
/// is the inferred per-call type-argument list, in the callee's
/// `generic_params` declared order, fully resolved through the
/// typecheck substitution. Empty `type_args` means the callee was
/// non-generic at this site — kept for symmetry but monomorphization
/// treats those as identity.
///
/// Type-args may still contain `Ty::Var` nodes when the surrounding
/// fn is itself generic and the inferred args reference the outer
/// fn's generic parameters; the monomorphizer resolves those when
/// it descends into the cloned outer fn's body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenericInstantiation {
    pub name: String,
    pub type_args: Vec<Ty>,
}

/// Effect row used during row unification. `tail = None` is a closed
/// row; `tail = Some(id)` is open. Always carries effect labels in a
/// canonical sorted-deduped form via `Row::canonicalise` so set
/// equality reduces to vector equality.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub effects: Vec<EffectInst>,
    pub tail: Option<u32>,
}

impl Row {
    pub fn closed(effects: Vec<EffectInst>) -> Self {
        let mut r = Row {
            effects,
            tail: None,
        };
        r.canonicalise();
        r
    }
    pub fn open(effects: Vec<EffectInst>, tail: u32) -> Self {
        let mut r = Row {
            effects,
            tail: Some(tail),
        };
        r.canonicalise();
        r
    }
    pub fn canonicalise(&mut self) {
        self.effects.sort_by(|a, b| a.name.cmp(&b.name));
        self.effects.dedup_by(|a, b| a == b);
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
            Ty::Tuple(elems) => {
                Ty::Tuple(elems.iter().map(|e| self.apply_ty_inner(e, seen)).collect())
            }
            Ty::Fn(sig) => {
                let new_sig = FnSig {
                    params: sig
                        .params
                        .iter()
                        .map(|p| self.apply_ty_inner(p, seen))
                        .collect(),
                    ret: self.apply_ty_inner(&sig.ret, seen),
                    // Plan D Stage 12 R3 — apply substitution to
                    // each EffectInst's args. Without this, a row
                    // carrying `Raise[Var(N)]` survives subst
                    // application unchanged even after Var(N) was
                    // bound — symmetric with rename_ty's Stage 12
                    // fix for scheme instantiation.
                    effects: sig
                        .effects
                        .iter()
                        .map(|ei| EffectInst {
                            name: ei.name.clone(),
                            args: ei
                                .args
                                .iter()
                                .map(|a| self.apply_ty_inner(a, seen))
                                .collect(),
                        })
                        .collect(),
                    effect_row_var: sig.effect_row_var,
                };
                let resolved = self.apply_row_to_sig(new_sig, seen);
                Ty::Fn(Box::new(resolved))
            }
            Ty::Continuation(c) => {
                // Plan D Task 117 (a) — only Concrete scope ids
                // exist today (`check_handle` is the sole producer
                // and always allocates Concrete). Pass through
                // unchanged. ScopeId substitution will land
                // alongside scope-var unification (Task 117
                // follow-up); assert here so the integration point
                // surfaces loudly when Var producers are wired.
                let scope_id = match &c.scope_id {
                    ScopeId::Concrete(n) => ScopeId::Concrete(*n),
                    ScopeId::Var(_) => unreachable!(
                        "apply_ty_inner: ScopeId::Var reached substitution — Task 117 (a) \
                         produces only Concrete scope ids; region-polymorphic schemes are \
                         deferred to Task 117 follow-up which must wire Subst-of-ScopeId \
                         before allocating Var"
                    ),
                };
                Ty::Continuation(Box::new(ContinuationTy {
                    op_ret: self.apply_ty_inner(&c.op_ret, seen),
                    ret: self.apply_ty_inner(&c.ret, seen),
                    scope_id,
                }))
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
            let mut merged: Vec<EffectInst> =
                sig.effects.iter().cloned().chain(row.effects).collect();
            merged.sort_by(|a, b| a.name.cmp(&b.name));
            merged.dedup_by(|a, b| a == b);
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
        // Plan D Stage 12 R3 — apply substitution to each
        // EffectInst's args. Without this, a row carrying
        // `Raise[Var(N)]` survives subst application unchanged
        // even after Var(N) was bound elsewhere. Symmetric with
        // `apply_ty_inner`'s Ty::Fn fix.
        let mut effects: Vec<EffectInst> = r
            .effects
            .iter()
            .map(|ei| EffectInst {
                name: ei.name.clone(),
                args: ei
                    .args
                    .iter()
                    .map(|a| self.apply_ty_inner(a, seen))
                    .collect(),
            })
            .collect();
        let mut tail = r.tail;
        while let Some(id) = tail {
            if seen.contains(&id) {
                break;
            }
            match self.rows.get(&id) {
                None => break,
                Some(resolved) => {
                    seen.insert(id);
                    // Apply subst to args of resolved-row's effects
                    // too — same reason.
                    for ei in &resolved.effects {
                        effects.push(EffectInst {
                            name: ei.name.clone(),
                            args: ei
                                .args
                                .iter()
                                .map(|a| self.apply_ty_inner(a, seen))
                                .collect(),
                        });
                    }
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
    /// Plan B' Stage 6.8 Task 104 — per-call-site callee Ty for
    /// indirect-call sites whose callee resolved to `Ty::Fn(sig)`.
    /// Keyed by the call expression's span. Direct calls (callee
    /// is `Ident(top_level_fn_name)` or a builtin name) skip
    /// insertion since codegen resolves them via the `user_fn_refs`
    /// registry — this keeps the table tight (one entry per
    /// indirect call) instead of one per Call AST node.
    ///
    /// **Span-survival invariant.** Spans are preserved across
    /// monomorphize cloning (the AST clone is structural — it copies
    /// `Span` values verbatim, never re-spans), so a single
    /// side-table entry serves all instantiations of a generic fn.
    /// `lower_call` keys the side-table by the in-scope Call's
    /// span and gets back the typecheck-resolved `Ty::Fn` regardless
    /// of which monomorphized clone is currently being lowered.
    ///
    /// Phase C+ Part 1 activates this side-table for `Call(...)`
    /// callees — when the callee is a call returning a `Ty::Fn`
    /// value (e.g. `make_adder(5)(7)`), codegen reads the side-table
    /// at the call's span to derive the indirect signature without
    /// re-walking the callee. The `Ident(local)` callee path uses
    /// `Lowerer.local_fn_types` (the surface `FnTypeExpr` from the
    /// param / let-binding annotation); the two paths converge at
    /// signature construction.
    pub call_callee_tys: BTreeMap<Span, Ty>,
    /// Plan B task 49 — per-fn polymorphic schemes recorded at the
    /// end of the typecheck pass. Monomorphization reads these to
    /// build the surface-name → fresh-var mapping when cloning a
    /// generic fn at a concrete type-arg tuple. Non-generic fns have
    /// schemes with empty `type_vars`, kept for uniformity.
    pub fn_schemes: BTreeMap<String, Scheme>,
    /// Plan B task 49 — per-call-site instantiation index for
    /// references to top-level fns. Keyed by the use-site span (the
    /// `Expr::Ident` callee's span, or the bare-Ident value-context
    /// span when the fn is referenced as a value). Monomorphization
    /// looks up the type-args at each call/value reference and clones
    /// the callee at the resolved type-arg tuple.
    pub call_site_instantiations: BTreeMap<Span, GenericInstantiation>,
    /// Plan B task 49 — per-construction-site instantiation index for
    /// constructor uses of generic user types. Populated by all three
    /// ctor resolution paths (unit ident, positional call, record
    /// literal). Keyed by the construction span. The
    /// `GenericInstantiation::name` is the *type* name (`Option`,
    /// `List`), not the ctor name, since the type's generic parameters
    /// are what got instantiated.
    pub ctor_site_instantiations: BTreeMap<Span, GenericInstantiation>,
    /// Plan B task 54 — effect declaration registry. Built in the
    /// top-level pre-pass; keyed by effect name. Stored on the
    /// `CheckedProgram` so Task 55's CPS transform can look up
    /// operation signatures for each `perform` and `handle` site
    /// without re-scanning the program.
    ///
    /// Duplicate effect names emit E0136 at the second declaration's
    /// name span; the first declaration wins in this map. Within an
    /// effect, duplicate operation names emit E0137 at the second
    /// op's name span; the first op wins inside the registered
    /// `EffectDecl::ops`.
    ///
    /// E0133 was lifted in the Task 55 foundation phase (`b3af204`);
    /// effect declarations now compile end-to-end through codegen.
    /// The registry is populated by the typecheck pre-pass and
    /// consulted by `check_perform`'s non-IO dispatch and by the
    /// effect/op ID assignment in `typecheck()` that codegen reads
    /// for the runtime handler-stack ABI.
    pub effects: BTreeMap<String, EffectDecl>,
    /// Plan B Task 55 — stable per-effect ID (u32) for the runtime
    /// handler-stack ABI. Assigned alphabetically over `effects` keys
    /// so the same source program produces the same IDs across builds
    /// (deterministic; no order dependence on declaration order).
    /// Codegen uses these as the `effect_id` arg to
    /// `sigil_handler_frame_new` and `sigil_perform`.
    pub effect_ids: BTreeMap<String, u32>,
    /// Plan B Task 55 — stable per-op ID (u32) within an effect for
    /// the runtime handler-stack ABI. Keyed by `(effect_name,
    /// op_name)`. Op IDs are assigned alphabetically within each
    /// effect, starting at 0 per effect; `(effect_name, op_name)` is
    /// the tuple used as the lookup key. Codegen uses these as the
    /// `op_id` arg to `sigil_handler_frame_set_arm` and
    /// `sigil_perform`.
    pub op_ids: BTreeMap<(String, String), u32>,
    /// Plan B Task 55 (Phase 4d) — per-handle, per-arm capture
    /// signatures, keyed by the handle expression's span. The outer
    /// `Vec` parallels `Expr::Handle::op_arms` in declaration order;
    /// each inner `Vec<(String, Ty)>` is the arm body's free-variable
    /// set with each name's outer-scope `Ty`, deduped and in
    /// first-encounter order over the arm body's syntactic walk.
    /// "Free" here means: not bound by the arm's user params, not the
    /// arm's `k_name`, not a top-level fn / ctor / builtin. A name
    /// captured by the arm body resolves to a value in the enclosing
    /// fn's lexical scope — typically a let-binding or fn-param of the
    /// surrounding fn, or (when the surrounding fn is a synthetic
    /// lambda fn after closure conversion) a slot in the enclosing
    /// closure's environment.
    ///
    /// Codegen's Phase 4d closure-record allocation reads this map at
    /// each `Expr::Handle` site to size the per-arm closure record,
    /// derive the GC pointer bitmap (via `slot_kind_for_ty`), and
    /// produce env_exprs evaluated in the enclosing scope. The
    /// synth-pass arm fn lowering uses the same captures list to
    /// rewrite `Expr::Ident(name, ..)` references in the arm body
    /// into `Expr::ClosureEnvLoad { index, kind, name, .. }` reading
    /// from the arm's `closure_ptr` at the arm-local slot index.
    ///
    /// Empty inner vecs (arm body with no captures) are recorded
    /// explicitly so codegen can pass `null` as the arm's closure_ptr
    /// (no allocation needed) without re-deriving emptiness.
    pub handle_arm_captures: BTreeMap<Span, Vec<Vec<(String, Ty)>>>,

    /// Plan B Task 55 (Phase 4g) — per-`Expr::Handle` return-arm
    /// captures, parallel to `handle_arm_captures` but a single Vec
    /// (not Vec-of-Vec) since each handle has at most one return arm.
    /// `None` (key absent) means the handle has no return arm; an
    /// empty Vec means a return arm with no outer-scope captures
    /// (codegen passes null `closure_ptr` to
    /// `sigil_handler_frame_set_return`); a non-empty Vec records the
    /// names + types in arm-local slot order matching the rewritten
    /// arm body's `Expr::ClosureEnvLoad { index }` references.
    ///
    /// Mirrors the Phase 4d capture-collection convention (see
    /// `handle_arm_captures` doc): collected against the saved env
    /// (the surrounding fn's lexical scope at the handle expression,
    /// before the return-arm `v` binding installs); top-level fn /
    /// ctor / builtin names that resolve outside the env are NOT
    /// captures (codegen resolves them through the user-fn / ctor /
    /// builtin tables).
    pub handle_return_arm_captures: BTreeMap<Span, Vec<(String, Ty)>>,
    /// Plan B Stage 6 cleanup — per-`Expr::Handle` body type.
    ///
    /// The handle expression's body has a typecheck-determined `Ty`
    /// that the codegen pre-pass for return-arm synth fns needs to
    /// size the `v` binding correctly (Phase 4g shipped with
    /// `binding_ty = I64` hardcoded; that's correct for I64 bodies
    /// but produces verifier errors for narrow-type bodies — Bool,
    /// Char — when the return arm body uses `v` at narrow type).
    /// This side-table threads the body's `Ty` from typecheck to
    /// the codegen pre-pass via the handle's `Span`.
    ///
    /// Resolves the `#[ignore]`'d e2e
    /// `handle_with_bool_body_and_return_arm_uses_v_pending_proper_-
    /// binding_ty` per the option-2 closure point in
    /// `[Stage 6 cleanup]`.
    pub handle_body_ty: BTreeMap<Span, Ty>,
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

/// Plan B Task 57 — names of effects synthesized into `tc.effects`
/// at typecheck pre-pass start. Order is load-bearing: `effect_id`
/// assignment phase 1 walks this slice in order, giving `ArithError
/// = 0` and `IO = 1`. User effects start at id 2 in alphabetical
/// order (phase 2). The `main` shim hardcodes the resulting effect_
/// ids when emitting the top-level handler frames; the reserved-low-
/// id convention is what keeps those constants stable per program.
pub const BUILTIN_EFFECT_NAMES: &[&str] = &["ArithError", "IO", "Mem"];

/// Plan B Task 57 — construct synthetic `EffectDecl`s for the
/// builtin effects (`IO`, `ArithError`). Returned in the order
/// fixed by `BUILTIN_EFFECT_NAMES` so callers iterating the result
/// see ArithError before IO. ArithError carries two ops:
/// `div_by_zero` (op_id 0) and `mod_by_zero` (op_id 1) — preserves
/// Plan A2's distinct stderr messages for `/` vs. `%` div-by-zero;
/// per `[DEVIATION Task 57] ArithError op return type` both ops
/// have return type `Int` (v1 simplification; v2 path to `Never` is
/// documented).
fn builtin_effects() -> Vec<EffectDecl> {
    let span = Span::synthetic("<builtin>");
    let mut out = Vec::with_capacity(BUILTIN_EFFECT_NAMES.len());
    out.push(EffectDecl {
        name: "ArithError".to_string(),
        name_span: span.clone(),
        generic_params: Vec::new(),
        resumes_many: false,
        ops: vec![
            EffectOp {
                name: "div_by_zero".to_string(),
                name_span: span.clone(),
                generic_params: Vec::new(),
                params: Vec::new(),
                return_type: TypeExpr::Named("Int".to_string(), span.clone()),
                span: span.clone(),
            },
            EffectOp {
                name: "mod_by_zero".to_string(),
                name_span: span.clone(),
                generic_params: Vec::new(),
                params: Vec::new(),
                return_type: TypeExpr::Named("Int".to_string(), span.clone()),
                span: span.clone(),
            },
        ],
        span: span.clone(),
    });
    out.push(EffectDecl {
        name: "IO".to_string(),
        name_span: span.clone(),
        generic_params: Vec::new(),
        resumes_many: false,
        ops: vec![
            // Plan C Task 70 — `print(s)`: write `s` without newline.
            EffectOp {
                name: "print".to_string(),
                name_span: span.clone(),
                generic_params: Vec::new(),
                params: vec![TypeExpr::Named("String".to_string(), span.clone())],
                return_type: TypeExpr::Named("Unit".to_string(), span.clone()),
                span: span.clone(),
            },
            // Plan A1 / Plan B Task 57 — `println(s)`.
            EffectOp {
                name: "println".to_string(),
                name_span: span.clone(),
                generic_params: Vec::new(),
                params: vec![TypeExpr::Named("String".to_string(), span.clone())],
                return_type: TypeExpr::Named("Unit".to_string(), span.clone()),
                span: span.clone(),
            },
            // Plan C Task 70 — `read_file(path) -> String`. Aborts
            // on IO error / invalid UTF-8.
            EffectOp {
                name: "read_file".to_string(),
                name_span: span.clone(),
                generic_params: Vec::new(),
                params: vec![TypeExpr::Named("String".to_string(), span.clone())],
                return_type: TypeExpr::Named("String".to_string(), span.clone()),
                span: span.clone(),
            },
            // Plan C Task 70 — `read_line() -> String`. Trailing CR/LF
            // stripped. EOF without bytes returns the empty string.
            EffectOp {
                name: "read_line".to_string(),
                name_span: span.clone(),
                generic_params: Vec::new(),
                params: Vec::new(),
                return_type: TypeExpr::Named("String".to_string(), span.clone()),
                span: span.clone(),
            },
            // Plan C Task 70 — `write_file(path, data)`. Replaces
            // existing contents.
            EffectOp {
                name: "write_file".to_string(),
                name_span: span.clone(),
                generic_params: Vec::new(),
                params: vec![
                    TypeExpr::Named("String".to_string(), span.clone()),
                    TypeExpr::Named("String".to_string(), span.clone()),
                ],
                return_type: TypeExpr::Named("Unit".to_string(), span.clone()),
                span: span.clone(),
            },
        ],
        span: span.clone(),
    });
    // Plan C Task 66 — Mem is a marker effect (zero ops). Functions
    // that mutate `MutArray[A]` declare `![Mem]` in their effect row;
    // the compiler rejects mutation calls from rows without Mem
    // (E0042). No runtime handler frame is installed because there
    // are no ops to dispatch — the "top-level Mem handler" is the
    // type-level absence of a deeper override. See
    // `[DEVIATION Task 66]` in `PLAN_C_DEVIATIONS.md`.
    out.push(EffectDecl {
        name: "Mem".to_string(),
        name_span: span.clone(),
        generic_params: Vec::new(),
        resumes_many: false,
        ops: Vec::new(),
        span,
    });
    debug_assert_eq!(
        out.len(),
        BUILTIN_EFFECT_NAMES.len(),
        "builtin_effects() count must match BUILTIN_EFFECT_NAMES",
    );
    debug_assert!(
        out.iter()
            .zip(BUILTIN_EFFECT_NAMES.iter())
            .all(|(d, n)| d.name == *n),
        "builtin_effects() order must match BUILTIN_EFFECT_NAMES",
    );
    out
}

pub fn typecheck(mut program: Program) -> (CheckedProgram, Vec<CompilerError>) {
    // Pre-pass 1 (Plan A3 task 38): build the nominal-type symbol table.
    // Must precede the fn-env pre-pass so a `fn f(o: Option) -> ...`
    // declaration can resolve `Option` to `Ty::User("Option")` when
    // `Option` is declared further down in the file. Duplicate
    // declarations record E0113 against the second (and subsequent)
    // offender; the first declaration wins in the symbol table so
    // downstream passes always see a single canonical variant set.
    let mut types: BTreeMap<String, TypeDecl> = BTreeMap::new();
    let mut errors: Vec<CompilerError> = Vec::new();
    // Plan C Task 65 — builtin generic types injected before user
    // types. `Array[A]` has no user-constructible variants (its only
    // constructors are the runtime FFI primitives `sigil_array_*`,
    // exposed via builtin generic schemes in `fn_schemes` below). A
    // user `type Array = ...` declaration trips E0113 (duplicate)
    // through the existing user-type loop — Array is not shadowable.
    for builtin in builtin_types(&program.file) {
        types.insert(builtin.name.clone(), builtin);
    }
    for item in &program.items {
        if let Item::Type(td) = item {
            // Plan D Task 117 (continuation-surface, PR #62 followup):
            // `Continuation` is reserved for the type-position
            // continuation surface form. A user `type Continuation[A,
            // B] { ... }` would silently lose under
            // `ty_from_type_expr`'s special-case shunt at the Apply
            // arm, and outside a handler arm body the user would get
            // a misleading E0145 ("Continuation annotations are only
            // valid inside a handler arm body") for what they think
            // is *their* type. Reject at the type-decl pre-pass
            // with a precise diagnostic.
            if td.name == "Continuation" {
                errors.push(CompilerError::new(
                    Severity::Error,
                    errors::code("E0113"),
                    td.name_span.clone(),
                    "`Continuation` is reserved for the type-position \
                     continuation surface form (`Continuation[op_ret, ret]` \
                     names a handler arm's `k` binding type); user type \
                     declarations cannot use this name"
                        .to_string(),
                ));
                continue;
            }
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
    // Plan B task 48 — `fn_env` carries only builtins (concrete
    // signatures looked up directly). Every user fn registers a
    // polymorphic `Scheme` in `fn_schemes` instead, so call sites
    // see fresh `Ty::Var`s per use. Pre-registration happens below
    // *before* any body is checked, which closes the source-order
    // hole reviewers flagged: a forward reference to `id` from
    // `use_id` (when `id` is declared further down the file) now
    // hits the polymorphic scheme rather than a stale Unit-fallback
    // `fn_env` entry.
    // Pre-pass 1c (Plan B task 54): build the effect registry.
    // Effect names share a single flat namespace; duplicates surface
    // as E0136 against the second offender's name span (first wins
    // in the registry so downstream typing always picks a single
    // canonical operation set per effect name). Within an effect
    // body, operation names share a single namespace; duplicates
    // surface as E0137 against the second op's name span (first
    // wins inside the registered `EffectDecl::ops`).
    //
    // Building the registry here (in the same Vec<CompilerError>
    // slot the types pre-pass uses) lets later effect-decl walks
    // (`Item::Effect`'s typecheck arm, `Expr::Handle`'s op-arm env
    // extension, `check_perform`'s non-IO dispatch) consult the
    // canonical EffectDecl rather than re-scanning the program.
    // E0133 was lifted in the Task 55 foundation phase (`b3af204`);
    // the registry-population walk continues unchanged. Codegen
    // reads `effect_ids` / `op_ids` (assigned at the end of
    // `typecheck()`) for the runtime handler-stack ABI.
    let mut effects: BTreeMap<String, EffectDecl> = BTreeMap::new();
    // Plan B Task 57 — synthetic builtin effects (`IO`, `ArithError`)
    // are injected into `tc.effects` before walking user-declared
    // effects. User redeclaration of a builtin name triggers the
    // existing E0136 duplicate path because the builtin is already
    // present in the BTreeMap. See `[DEVIATION Task 57] Builtin-effect
    // injection (vs. full stdlib loading)` in `PLAN_B_DEVIATIONS.md`
    // for why these are constructed in code rather than parsed from
    // `std/*.sigil` (full stdlib loading is deferred to a later task
    // per Plan B's "Do not implement Stage 7+ features" hard rule).
    for builtin in builtin_effects() {
        effects.insert(builtin.name.clone(), builtin);
    }
    for item in &program.items {
        if let Item::Effect(ed) = item {
            if effects.contains_key(&ed.name) {
                errors.push(CompilerError::new(
                    Severity::Error,
                    errors::code("E0136"),
                    ed.name_span.clone(),
                    format!("duplicate effect declaration `{}`", ed.name),
                ));
            } else {
                let mut canonical = (**ed).clone();
                let mut seen_ops: BTreeMap<String, Span> = BTreeMap::new();
                let mut deduped_ops: Vec<EffectOp> = Vec::with_capacity(canonical.ops.len());
                for op in canonical.ops.drain(..) {
                    if let Some(_first) = seen_ops.get(&op.name) {
                        errors.push(CompilerError::new(
                            Severity::Error,
                            errors::code("E0137"),
                            op.name_span.clone(),
                            format!("duplicate operation `{}` on effect `{}`", op.name, ed.name),
                        ));
                    } else {
                        seen_ops.insert(op.name.clone(), op.name_span.clone());
                        deduped_ops.push(op);
                    }
                }
                canonical.ops = deduped_ops;
                effects.insert(ed.name.clone(), canonical);
            }
        }
    }

    let fn_env: BTreeMap<String, Ty> = builtin_fn_env();
    let mut tc = Tc {
        errors,
        string_literals: Vec::new(),
        lambda_captures: Vec::new(),
        fn_env,
        env: BTreeMap::new(),
        types,
        ctors,
        match_scrut_tys: BTreeMap::new(),
        call_callee_tys: BTreeMap::new(),
        fn_schemes: BTreeMap::new(),
        next_ty_var: 0,
        next_row_var: 0,
        next_scope_id: 0,
        current_arm_scope_id: None,
        subst: Subst::new(),
        current_generic_subst: BTreeMap::new(),
        current_row_var_subst: BTreeMap::new(),
        pending_call_instantiations: Vec::new(),
        pending_ctor_instantiations: Vec::new(),
        effects,
        handler_scopes: Vec::new(),
        handle_arm_captures: BTreeMap::new(),
        handle_return_arm_captures: BTreeMap::new(),
        handle_body_ty: BTreeMap::new(),
    };
    // Plan C Task 65 — builtin generic schemes for `array_alloc` /
    // `array_empty` / `array_length` / `array_get` / `array_set`.
    // Registered before the user-fn scheme pre-pass so a user
    // `fn array_alloc(...)` declaration overwrites the builtin
    // (same shadowing model as `int_to_string`).
    register_builtin_array_schemes(&mut tc);
    // Plan C Task 66 — builtin generic schemes for `mut_array_*`
    // ops, gated by the Mem marker effect.
    register_builtin_mut_array_schemes(&mut tc);
    // Plan C Task 66.5 — non-generic builtin schemes for the byte
    // runtime: `byte_array_*`, `string_to_bytes`,
    // `string_from_bytes_*`, `byte_in_range`, `byte_truncate`.
    register_builtin_byte_array_schemes(&mut tc);
    // Plan C Task 66.6 — non-generic builtin schemes for the
    // `mut_byte_array_*` ops, gated by the Mem marker effect.
    register_builtin_mut_byte_array_schemes(&mut tc);
    // Plan C Task 69 — boxed Int64 arithmetic / comparison /
    // conversion / stringify primitives.
    register_builtin_int64_schemes(&mut tc);
    // Plan C Task 67 — StringBuilder rope primitives, gated by
    // the Mem marker effect.
    register_builtin_string_builder_schemes(&mut tc);
    // Plan C Task 68 — extended String primitives (byte-indexed
    // accessor / comparison / search / trim / parse).
    register_builtin_string_schemes(&mut tc);
    // Plan C Task 75 — Random pseudo-int builtin.
    register_builtin_random_schemes(&mut tc);
    // Plan C Task 76 — OS clock builtin.
    register_builtin_clock_schemes(&mut tc);
    // Pre-pass: register a polymorphic `Scheme` per user fn under
    // its declared generic-parameter / row-variable allocations, so
    // mutual and forward references resolve through `fn_schemes`'s
    // `instantiate` path during body checks. After `check_fn` runs
    // each body, the entry is overwritten with the *inferred*-
    // resolved scheme (typically identical for non-generic fns,
    // possibly more constrained for fns whose body pinned a
    // declared generic to a specific type).
    for item in &program.items {
        if let Item::Fn(f) = item {
            let (gs, ty_var_ids) = tc.fresh_generic_subst(&f.generic_params);
            let row_var_id = f.effect_row_var.as_ref().map(|_| tc.fresh_row_var());
            let saved = std::mem::take(&mut tc.current_generic_subst);
            let saved_row_subst = std::mem::take(&mut tc.current_row_var_subst);
            tc.current_generic_subst = gs;
            // Plan D Task 116 R1 — seed the row-var subst with the
            // SAME row-var id that's about to be installed in the
            // Scheme. Without this seeding, inner fn-type rows
            // (`(...) -> R ![ ... | r ]`) inside this fn's params /
            // return resolve their `r` to None, and forward-ref
            // callers instantiating from this Scheme see
            // outer.effect_row_var = Some(id) but inner.effect_row_var
            // = None — Scheme.row_vars and inner row-var ids must
            // agree, otherwise rename_ty's row_map miss leaves the
            // inner None and subsumption silently coerces a
            // row-poly callee to closed.
            if let (Some(rv), Some(id)) = (f.effect_row_var.as_ref(), row_var_id) {
                tc.current_row_var_subst.insert(rv.name.clone(), id);
            }
            let params: Vec<Ty> = f
                .params
                .iter()
                .map(|p| tc.ty_from_type_expr_here(&p.ty).unwrap_or(Ty::Unit))
                .collect();
            let ret = tc
                .ty_from_type_expr_here(&f.return_type)
                .unwrap_or(Ty::Unit);
            // Plan D Task 116 R1 — `effect_refs_to_insts` for f's
            // own row needs the active generic_subst (so a fn-decl
            // row like `![Raise[A]]` resolves `A` correctly).
            // Defer the restore until after the FnSig is built.
            // Pre-existing Task 114 bug noted by R1 reviewer.
            let effects = effect_refs_to_insts(&f.effects, &tc.types, &tc.current_generic_subst);
            tc.current_generic_subst = saved;
            tc.current_row_var_subst = saved_row_subst;
            let sig = FnSig {
                params,
                ret,
                effects,
                effect_row_var: row_var_id,
            };
            let scheme = Scheme {
                type_vars: ty_var_ids,
                row_vars: row_var_id.map(|id| vec![id]).unwrap_or_default(),
                body: Ty::Fn(Box::new(sig)),
            };
            tc.fn_schemes.insert(f.name.clone(), scheme);
        }
    }
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
            let saved_row_var_subst = std::mem::take(&mut tc.current_row_var_subst);
            let (gs, _) = tc.fresh_generic_subst(&f.generic_params);
            tc.current_generic_subst = gs;
            // Plan D Task 116 — seed the row-var subst with the
            // fn's own row variable (if any). This lets the
            // pre-pass walk resolve inner fn-type row variables
            // (`(...) -> R ![ ... | r ]`) against the enclosing
            // fn's row var when the names match. Allocates a
            // fresh id mirroring `check_fn`'s behavior at line
            // 3239 area; the id is not retained between pre-pass
            // and check_fn (each phase does its own allocation
            // for its own subst), but the surface-name → id map
            // shape is consistent.
            if let Some(rv) = &f.effect_row_var {
                let id = tc.fresh_row_var();
                tc.current_row_var_subst.insert(rv.name.clone(), id);
            }
            for p in &f.params {
                tc.check_type_expr_known(&p.ty);
            }
            tc.check_type_expr_known(&f.return_type);
            // Plan D Task 114 — walk the fn-decl row's effect-refs
            // for arity / unknown-effect diagnostics.
            for eref in &f.effects {
                for a in &eref.args {
                    tc.check_type_expr_known(a);
                }
                tc.check_effect_ref_arity(eref);
            }
            tc.current_generic_subst = saved_generic_subst;
            tc.current_row_var_subst = saved_row_var_subst;
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
            // Plan B Task 55 — `effect Name[T] { op: ... }` has been
            // pre-passed into `tc.effects`; the staged `E0133` gate
            // is lifted now that codegen lowers `perform` through
            // `sigil_perform` and the runtime handler-stack ABI from
            // Task 56. We still walk the op-decl types so any
            // unrelated type error in an op signature surfaces in
            // this pass.
            Item::Effect(ed) => {
                let saved_generic_subst = std::mem::take(&mut tc.current_generic_subst);
                let (gs, _) = tc.fresh_generic_subst(&ed.generic_params);
                let eff_decl_subst = gs.clone();
                tc.current_generic_subst = gs;
                for op in &ed.ops {
                    // Plan D Task 115 — per-op generic params layer
                    // on top of the effect-decl's substitution while
                    // checking this op's param / return types. E0144
                    // fires when an op's per-op param shadows an
                    // effect-decl param of the same name.
                    let mut op_layer = eff_decl_subst.clone();
                    let (op_gs, _) = tc.fresh_generic_subst(&op.generic_params);
                    for gp in &op.generic_params {
                        if eff_decl_subst.contains_key(&gp.name) {
                            tc.push_error(
                                "E0144",
                                gp.span.clone(),
                                format!(
                                    "per-op generic parameter `{}` shadows the effect-decl \
                                     generic of the same name (`effect {}[{}, ...]`); use a \
                                     distinct name for the per-op generic",
                                    gp.name, ed.name, gp.name,
                                ),
                            );
                        }
                    }
                    for (k, v) in op_gs {
                        op_layer.insert(k, v);
                    }
                    tc.current_generic_subst = op_layer;
                    for p in &op.params {
                        tc.check_type_expr_known(p);
                    }
                    tc.check_type_expr_known(&op.return_type);
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

    // Plan B task 49 — resolve pending per-call-site and per-ctor-
    // site instantiations through the final substitution to produce
    // concrete `Ty` arg tuples. Vars that remain unbound after
    // inference (e.g. inside a generic fn body, where the inner call
    // resolved to the *outer* fn's still-free type-var) are kept as
    // `Ty::Var(_)`; the monomorphizer applies the outer fn's
    // declared-name → concrete-Ty substitution when it descends into
    // the cloned body and re-resolves these.
    //
    // **E0132 ambiguous polymorphism check.** If a resolved type-arg
    // is still `Ty::Var(id)` AND `id` is not an outer fn's generic
    // parameter id (which would resolve at clone time), the call site
    // is genuinely unconstrained: monomorphization would silently
    // mangle the use site to a placeholder name. Emit a hard error so
    // the user can either pin the parameter via context or drop it
    // from the signature.
    let outer_fn_var_ids: std::collections::BTreeSet<u32> = tc
        .fn_schemes
        .values()
        .flat_map(|s| s.type_vars.iter().copied())
        .collect();
    // Build a quick name → declared-generic-param-name list lookup
    // from the program for the diagnostic's parameter name.
    let mut fn_param_names: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut type_param_names: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for item in &program.items {
        match item {
            Item::Fn(f) => {
                fn_param_names.insert(
                    f.name.clone(),
                    f.generic_params.iter().map(|gp| gp.name.clone()).collect(),
                );
            }
            Item::Type(td) => {
                type_param_names.insert(
                    td.name.clone(),
                    td.generic_params.iter().map(|gp| gp.name.clone()).collect(),
                );
            }
            Item::Import(_) => {}
            // Plan B task 53 — effect declarations carry their own
            // generic params for op signatures, but they don't
            // participate in the call-site / ctor-site instantiation
            // resolution this builder is wired to. Skip.
            Item::Effect(_) => {}
        }
    }
    let pending_calls = std::mem::take(&mut tc.pending_call_instantiations);
    let mut resolved_calls: BTreeMap<Span, GenericInstantiation> = BTreeMap::new();
    for (span, name, var_ids) in pending_calls {
        let mut type_args: Vec<Ty> = Vec::with_capacity(var_ids.len());
        for (i, id) in var_ids.iter().enumerate() {
            let resolved = tc.subst.apply_ty(&Ty::Var(*id));
            if let Ty::Var(remaining_id) = &resolved {
                if !outer_fn_var_ids.contains(remaining_id) {
                    let pname = fn_param_names
                        .get(&name)
                        .and_then(|v| v.get(i))
                        .cloned()
                        .unwrap_or_else(|| format!("_{i}"));
                    tc.push_error(
                        "E0132",
                        span.clone(),
                        format!(
                            "ambiguous polymorphism: type parameter `{pname}` of `{name}` is unconstrained at this call site"
                        ),
                    );
                }
            }
            type_args.push(resolved);
        }
        resolved_calls.insert(span, GenericInstantiation { name, type_args });
    }
    let pending_ctors = std::mem::take(&mut tc.pending_ctor_instantiations);
    let mut resolved_ctors: BTreeMap<Span, GenericInstantiation> = BTreeMap::new();
    for (span, name, var_ids) in pending_ctors {
        let mut type_args: Vec<Ty> = Vec::with_capacity(var_ids.len());
        for (i, id) in var_ids.iter().enumerate() {
            let resolved = tc.subst.apply_ty(&Ty::Var(*id));
            if let Ty::Var(remaining_id) = &resolved {
                if !outer_fn_var_ids.contains(remaining_id) {
                    let pname = type_param_names
                        .get(&name)
                        .and_then(|v| v.get(i))
                        .cloned()
                        .unwrap_or_else(|| format!("_{i}"));
                    tc.push_error(
                        "E0132",
                        span.clone(),
                        format!(
                            "ambiguous polymorphism: type parameter `{pname}` of `{name}` is unconstrained at this construction site"
                        ),
                    );
                }
            }
            type_args.push(resolved);
        }
        resolved_ctors.insert(span, GenericInstantiation { name, type_args });
    }

    // Plan B Task 55 + Task 57 — assign stable effect_id / op_id
    // integers for the runtime handler-stack ABI. **Effect IDs use
    // a reserved-low-id convention** per `[DEVIATION Task 57]
    // Builtin-effect injection`: phase 1 walks `BUILTIN_EFFECT_NAMES`
    // in fixed order, assigning ids 0 (`ArithError`) and 1 (`IO`);
    // phase 2 walks user effects in alphabetical order (BTreeMap
    // iteration over `tc.effects.keys()`, skipping builtin names),
    // starting at id `BUILTIN_EFFECT_NAMES.len()`. The `main` shim
    // hardcodes effect_ids 0 and 1 when emitting top-level handler
    // frames; this convention is what keeps those constants stable
    // across user programs regardless of how many user effects are
    // declared. Op IDs are assigned alphabetically within each
    // effect over its declared `ops` Vec (sorted into a temporary
    // because EffectDecl::ops preserves declaration order, not lex
    // order). Same source program → same IDs across builds.
    let mut effect_ids: BTreeMap<String, u32> = BTreeMap::new();
    let mut op_ids: BTreeMap<(String, String), u32> = BTreeMap::new();
    // Phase 1: builtins.
    for (i, name) in BUILTIN_EFFECT_NAMES.iter().enumerate() {
        effect_ids.insert((*name).to_string(), i as u32);
    }
    // Phase 2: user effects.
    let mut next_user_id: u32 = BUILTIN_EFFECT_NAMES.len() as u32;
    for eff_name in tc.effects.keys() {
        if BUILTIN_EFFECT_NAMES.contains(&eff_name.as_str()) {
            continue;
        }
        effect_ids.insert(eff_name.clone(), next_user_id);
        next_user_id += 1;
    }
    // Op IDs: alphabetical per effect (unchanged across Task 55/57).
    for (eff_name, eff_decl) in tc.effects.iter() {
        let mut op_names: Vec<&str> = eff_decl.ops.iter().map(|o| o.name.as_str()).collect();
        op_names.sort();
        // E0137 fires upstream for duplicate op names within an
        // effect; reaching this point with dups would indicate the
        // typecheck pre-pass dropped the diagnostic. Assert rather
        // than dedup so any future regression is loud.
        debug_assert!(
            op_names.windows(2).all(|w| w[0] != w[1]),
            "op_names must be deduplicated by E0137 in the effects pre-pass"
        );
        for (op_idx, op_name) in op_names.iter().enumerate() {
            op_ids.insert((eff_name.clone(), (*op_name).to_string()), op_idx as u32);
        }
    }

    // Plan B' Stage 6.8 Phase B + C++ — apply final substitution
    // to every Ty recorded in `lambda_captures`. Captures recorded
    // mid-typecheck (e.g., k inside an arm body whose
    // handler_overall_ty was a fresh row var at the arm body's
    // check time) often hold unresolved `Ty::Var` references that
    // get bound later by unification at handle-end. Codegen's
    // `cranelift_ty_of_ty` rejects `Ty::Var(_)`, so this end-of-
    // typecheck deref pass ensures every recorded Ty is concrete.
    let raw_lambda_captures = std::mem::take(&mut tc.lambda_captures);
    let mut resolved_lambda_captures: Vec<(Span, Vec<(String, Ty)>)> =
        Vec::with_capacity(raw_lambda_captures.len());
    for (span, caps) in raw_lambda_captures {
        let resolved_caps: Vec<(String, Ty)> = caps
            .into_iter()
            .map(|(name, ty)| {
                let resolved = tc.deref(&ty);
                (name, resolved)
            })
            .collect();
        resolved_lambda_captures.push((span, resolved_caps));
    }

    // Plan B' Stage 6.8 R5 Finding 2 (preemptive) — same end-of-
    // typecheck deref pass for `call_callee_tys`. Phase C+ Part 1's
    // codegen consumer (`lower_call`'s Call-callee path) reads
    // these Tys via `cranelift_ty_of_ty` which rejects `Ty::Var(_)`.
    // For inner-fn calls inside a generic surrounding fn, generic
    // params remain free `Ty::Var`s at check_call time; this deref
    // pass resolves anything that gets bound by later inference.
    // Generic-context calls whose Vars REMAIN free after
    // end-of-typecheck still need monomorphize-rebuilds-per-clone
    // (parallel to `lambda_captures_resolved`); this deref handles
    // the var-bound-later subset that's the typical case.
    let raw_call_callee_tys = std::mem::take(&mut tc.call_callee_tys);
    let mut resolved_call_callee_tys: BTreeMap<Span, Ty> = BTreeMap::new();
    for (span, ty) in raw_call_callee_tys {
        resolved_call_callee_tys.insert(span, tc.deref(&ty));
    }

    // Plan D Task 117 (continuation-surface) — desugar the
    // let-bound k pattern.
    //
    // After typecheck verifies that `let f: Continuation[op_ret,
    // ret] = k` is well-typed inside an arm body (the arm-context-
    // tracking E0145 + cross-handle E0145 + bare-Ident-RHS check
    // pin the surface contract), rewrite the AST so downstream
    // codegen sees the pre-existing supported shapes:
    //
    //   `let f: Continuation[A, B] = k; ... f(arg) ...`
    //   →
    //   `... k(arg) ...`
    //
    // The let-stmt is elided and every subsequent reference to
    // `f` is renamed to the originating arm's `k_name`. After
    // rewrite, the arm body matches the existing Slice C
    // recognizer paths (k as direct callee in let-RHS or tail);
    // no codegen-side machinery for "let-bound k as 2-slot stack
    // local" is needed.
    //
    // Gated on no typecheck errors (PR #62 followup): a partial-
    // compilation consumer (IDE-style) that observes both the
    // errors and the rewritten AST should not see desugar-
    // produced rewrites for shapes typecheck didn't validate.
    // The driver aborts on errors anyway; this guard makes the
    // contract explicit.
    let has_errors = tc
        .errors
        .iter()
        .any(|e| matches!(e.severity, Severity::Error));
    if !has_errors {
        desugar_let_bound_continuations(&mut program);
    }

    (
        CheckedProgram {
            program,
            string_literals: tc.string_literals,
            lambda_captures: resolved_lambda_captures,
            types: tc.types,
            match_scrut_tys: tc.match_scrut_tys,
            call_callee_tys: resolved_call_callee_tys,
            fn_schemes: tc.fn_schemes,
            call_site_instantiations: resolved_calls,
            ctor_site_instantiations: resolved_ctors,
            effects: tc.effects,
            effect_ids,
            op_ids,
            handle_arm_captures: tc.handle_arm_captures,
            handle_return_arm_captures: tc.handle_return_arm_captures,
            handle_body_ty: tc.handle_body_ty,
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

/// Plan C Task 65 — builtin generic types registered in `tc.types`
/// before user types. `Array[A]` is opaque (no user-constructible
/// variants); its values are produced exclusively via the builtin
/// generic functions registered alongside in `fn_schemes`.
///
/// `file` is the user program's file path, used to construct
/// synthetic spans that point at the user file rather than a
/// fictional builtin location. Diagnostics that reference a
/// builtin TypeDecl's span will surface against the user's own
/// program — fine for v1's narrow surface (Array doesn't surface
/// in user-visible diagnostics today).
fn builtin_types(file: &str) -> Vec<TypeDecl> {
    use crate::ast::{GenericParam, TypeDecl};
    let span = Span::synthetic(file);
    vec![
        TypeDecl {
            name: "Array".to_string(),
            name_span: span.clone(),
            generic_params: vec![GenericParam {
                name: "A".to_string(),
                span: span.clone(),
            }],
            variants: Vec::new(),
            span: span.clone(),
        },
        // Plan C Task 66 — MutArray[A] is opaque; constructed via
        // the Mem-effected builtin `mut_array_new`. Same layout
        // shape as Array but tag TAG_MUT_ARRAY at runtime.
        TypeDecl {
            name: "MutArray".to_string(),
            name_span: span.clone(),
            generic_params: vec![GenericParam {
                name: "A".to_string(),
                span: span.clone(),
            }],
            variants: Vec::new(),
            span: span.clone(),
        },
        // Plan C Task 66.5 — ByteArray is opaque, non-generic.
        // Constructed exclusively via the runtime primitives
        // registered alongside (`byte_array_alloc`, `byte_array_empty`,
        // `byte_array_concat`, `byte_array_slice`, `string_to_bytes`,
        // `string_from_bytes_alloc`).
        TypeDecl {
            name: "ByteArray".to_string(),
            name_span: span.clone(),
            generic_params: Vec::new(),
            variants: Vec::new(),
            span: span.clone(),
        },
        // Plan C Task 66.6 — MutByteArray is opaque, non-generic.
        // Constructed via `mut_byte_array_new(len, fill)` (Mem-
        // gated). `mut_byte_array_set` mutates in place.
        TypeDecl {
            name: "MutByteArray".to_string(),
            name_span: span.clone(),
            generic_params: Vec::new(),
            variants: Vec::new(),
            span: span.clone(),
        },
        // Plan C Task 69 — Int64 is opaque, non-generic. Boxed
        // 64-bit signed integer; constructed via `int64_from_int`
        // and the arithmetic / comparison / conversion / stringify
        // builtins.
        TypeDecl {
            name: "Int64".to_string(),
            name_span: span.clone(),
            generic_params: Vec::new(),
            variants: Vec::new(),
            span: span.clone(),
        },
        // Plan C Task 67 — StringBuilder is opaque, non-generic.
        // Runtime-backed segmented rope; constructed via
        // `sb_new()` and consumed by `sb_finalize` (Mem-gated).
        TypeDecl {
            name: "StringBuilder".to_string(),
            name_span: span.clone(),
            generic_params: Vec::new(),
            variants: Vec::new(),
            span,
        },
    ]
}

/// Plan C Task 65 — builtin generic function schemes for the array
/// runtime primitives. Inserted into `tc.fn_schemes` after the
/// generic-fn-allocation pre-pass so user fns of the same names
/// shadow these (mirrors `int_to_string`'s shadowability via
/// `fn_env`). Allocates fresh type-var ids per builtin so each
/// scheme is fully resolved before instantiation at call sites.
///
/// Operations registered:
/// - `array_alloc[A](Int, A) -> Array[A] ![]`
/// - `array_empty[A]() -> Array[A] ![]`
/// - `array_length[A](Array[A]) -> Int ![]`
/// - `array_get[A](Array[A], Int) -> A ![]`
/// - `array_set[A](Array[A], Int, A) -> Array[A] ![]`
fn register_builtin_array_schemes(tc: &mut Tc) {
    // Allocate one fresh type-var per scheme. Each scheme's `A`
    // is independently fresh so cross-scheme unification stays
    // sound (e.g. `array_alloc[A]` and `array_get[A]` use
    // different `A_id` slots).
    let make_scheme = |params: Vec<Ty>, ret: Ty, type_vars: Vec<u32>| Scheme {
        type_vars,
        row_vars: Vec::new(),
        body: Ty::Fn(Box::new(FnSig {
            params,
            ret,
            effects: Vec::new(),
            effect_row_var: None,
        })),
    };
    {
        let a = tc.fresh_ty_var();
        let array_a = Ty::User("Array".to_string(), vec![Ty::Var(a)]);
        tc.fn_schemes.insert(
            "array_alloc".to_string(),
            make_scheme(vec![Ty::Int, Ty::Var(a)], array_a, vec![a]),
        );
    }
    {
        let a = tc.fresh_ty_var();
        let array_a = Ty::User("Array".to_string(), vec![Ty::Var(a)]);
        tc.fn_schemes.insert(
            "array_empty".to_string(),
            make_scheme(vec![], array_a, vec![a]),
        );
    }
    {
        let a = tc.fresh_ty_var();
        let array_a = Ty::User("Array".to_string(), vec![Ty::Var(a)]);
        tc.fn_schemes.insert(
            "array_length".to_string(),
            make_scheme(vec![array_a], Ty::Int, vec![a]),
        );
    }
    {
        let a = tc.fresh_ty_var();
        let array_a = Ty::User("Array".to_string(), vec![Ty::Var(a)]);
        tc.fn_schemes.insert(
            "array_get".to_string(),
            make_scheme(vec![array_a, Ty::Int], Ty::Var(a), vec![a]),
        );
    }
    {
        let a = tc.fresh_ty_var();
        let array_a = Ty::User("Array".to_string(), vec![Ty::Var(a)]);
        let array_a_clone = Ty::User("Array".to_string(), vec![Ty::Var(a)]);
        tc.fn_schemes.insert(
            "array_set".to_string(),
            make_scheme(vec![array_a, Ty::Int, Ty::Var(a)], array_a_clone, vec![a]),
        );
    }
}

/// Plan C Task 66 — builtin generic schemes for the `MutArray[A]`
/// runtime primitives. Each declares `effects: vec!["Mem"]` so a
/// caller without `Mem` in its effect row trips E0042 at the call
/// site. Operations:
///
/// - `mut_array_new[A](Int, A) -> MutArray[A] ![Mem]`
/// - `mut_array_length[A](MutArray[A]) -> Int ![Mem]`
/// - `mut_array_get[A](MutArray[A], Int) -> A ![Mem]`
/// - `mut_array_set[A](MutArray[A], Int, A) -> Unit ![Mem]`
fn register_builtin_mut_array_schemes(tc: &mut Tc) {
    let make_scheme = |params: Vec<Ty>, ret: Ty, type_vars: Vec<u32>| Scheme {
        type_vars,
        row_vars: Vec::new(),
        body: Ty::Fn(Box::new(FnSig {
            params,
            ret,
            effects: vec![EffectInst::bare("Mem")],
            effect_row_var: None,
        })),
    };
    {
        let a = tc.fresh_ty_var();
        let mut_array_a = Ty::User("MutArray".to_string(), vec![Ty::Var(a)]);
        tc.fn_schemes.insert(
            "mut_array_new".to_string(),
            make_scheme(vec![Ty::Int, Ty::Var(a)], mut_array_a, vec![a]),
        );
    }
    {
        let a = tc.fresh_ty_var();
        let mut_array_a = Ty::User("MutArray".to_string(), vec![Ty::Var(a)]);
        tc.fn_schemes.insert(
            "mut_array_length".to_string(),
            make_scheme(vec![mut_array_a], Ty::Int, vec![a]),
        );
    }
    {
        let a = tc.fresh_ty_var();
        let mut_array_a = Ty::User("MutArray".to_string(), vec![Ty::Var(a)]);
        tc.fn_schemes.insert(
            "mut_array_get".to_string(),
            make_scheme(vec![mut_array_a, Ty::Int], Ty::Var(a), vec![a]),
        );
    }
    {
        let a = tc.fresh_ty_var();
        let mut_array_a = Ty::User("MutArray".to_string(), vec![Ty::Var(a)]);
        tc.fn_schemes.insert(
            "mut_array_set".to_string(),
            make_scheme(vec![mut_array_a, Ty::Int, Ty::Var(a)], Ty::Unit, vec![a]),
        );
    }
}

/// Plan C Task 66.5 — non-generic builtin schemes for the byte
/// runtime primitives.
///
/// `ByteArray` is an opaque non-generic builtin type with a
/// flat-byte payload. Operations registered here are simple
/// concrete-typed builtins (no `forall A` quantifier needed).
///
/// Operations:
/// - `byte_array_alloc(Int, Byte) -> ByteArray ![]`
/// - `byte_array_empty() -> ByteArray ![]`
/// - `byte_array_length(ByteArray) -> Int ![]`
/// - `byte_array_get(ByteArray, Int) -> Byte ![]`
/// - `byte_array_concat(ByteArray, ByteArray) -> ByteArray ![]`
/// - `byte_array_slice(ByteArray, Int, Int) -> ByteArray ![]`
/// - `string_to_bytes(String) -> ByteArray ![]`
/// - `string_from_bytes_validate(ByteArray) -> Int ![]`
/// - `string_from_bytes_alloc(ByteArray) -> String ![]`
/// - `byte_in_range(Int) -> Bool ![]`
/// - `byte_truncate(Int) -> Byte ![]`
///
/// `byte_from_int(n) -> Option[Byte]` ships in pure sigil under
/// `std/byte_array.sigil` using `byte_in_range` + `byte_truncate`
/// + `Option[Byte]` constructors.
fn register_builtin_byte_array_schemes(tc: &mut Tc) {
    let make_scheme = |params: Vec<Ty>, ret: Ty| Scheme {
        type_vars: Vec::new(),
        row_vars: Vec::new(),
        body: Ty::Fn(Box::new(FnSig {
            params,
            ret,
            effects: Vec::new(),
            effect_row_var: None,
        })),
    };
    let byte_array_ty = || Ty::User("ByteArray".to_string(), Vec::new());
    tc.fn_schemes.insert(
        "byte_array_alloc".to_string(),
        make_scheme(vec![Ty::Int, Ty::Byte], byte_array_ty()),
    );
    tc.fn_schemes.insert(
        "byte_array_empty".to_string(),
        make_scheme(vec![], byte_array_ty()),
    );
    tc.fn_schemes.insert(
        "byte_array_length".to_string(),
        make_scheme(vec![byte_array_ty()], Ty::Int),
    );
    tc.fn_schemes.insert(
        "byte_array_get".to_string(),
        make_scheme(vec![byte_array_ty(), Ty::Int], Ty::Byte),
    );
    tc.fn_schemes.insert(
        "byte_array_concat".to_string(),
        make_scheme(vec![byte_array_ty(), byte_array_ty()], byte_array_ty()),
    );
    tc.fn_schemes.insert(
        "byte_array_slice".to_string(),
        make_scheme(vec![byte_array_ty(), Ty::Int, Ty::Int], byte_array_ty()),
    );
    tc.fn_schemes.insert(
        "string_to_bytes".to_string(),
        make_scheme(vec![Ty::String], byte_array_ty()),
    );
    tc.fn_schemes.insert(
        "string_from_bytes_validate".to_string(),
        make_scheme(vec![byte_array_ty()], Ty::Int),
    );
    tc.fn_schemes.insert(
        "string_from_bytes_alloc".to_string(),
        make_scheme(vec![byte_array_ty()], Ty::String),
    );
    tc.fn_schemes.insert(
        "byte_in_range".to_string(),
        make_scheme(vec![Ty::Int], Ty::Bool),
    );
    tc.fn_schemes.insert(
        "byte_truncate".to_string(),
        make_scheme(vec![Ty::Int], Ty::Byte),
    );
    tc.fn_schemes.insert(
        "byte_to_int".to_string(),
        make_scheme(vec![Ty::Byte], Ty::Int),
    );
}

/// Plan C Task 66.6 — `MutByteArray` operations gated on the `Mem`
/// effect. Mirrors `register_builtin_mut_array_schemes` shape but
/// non-generic (byte payload is fixed at I8). Operations:
///
/// - `mut_byte_array_new(Int, Byte) -> MutByteArray ![Mem]`
/// - `mut_byte_array_length(MutByteArray) -> Int ![Mem]`
/// - `mut_byte_array_get(MutByteArray, Int) -> Byte ![Mem]`
/// - `mut_byte_array_set(MutByteArray, Int, Byte) -> Unit ![Mem]`
fn register_builtin_mut_byte_array_schemes(tc: &mut Tc) {
    let make_scheme = |params: Vec<Ty>, ret: Ty| Scheme {
        type_vars: Vec::new(),
        row_vars: Vec::new(),
        body: Ty::Fn(Box::new(FnSig {
            params,
            ret,
            effects: vec![EffectInst::bare("Mem")],
            effect_row_var: None,
        })),
    };
    let mba_ty = || Ty::User("MutByteArray".to_string(), Vec::new());
    tc.fn_schemes.insert(
        "mut_byte_array_new".to_string(),
        make_scheme(vec![Ty::Int, Ty::Byte], mba_ty()),
    );
    tc.fn_schemes.insert(
        "mut_byte_array_length".to_string(),
        make_scheme(vec![mba_ty()], Ty::Int),
    );
    tc.fn_schemes.insert(
        "mut_byte_array_get".to_string(),
        make_scheme(vec![mba_ty(), Ty::Int], Ty::Byte),
    );
    tc.fn_schemes.insert(
        "mut_byte_array_set".to_string(),
        make_scheme(vec![mba_ty(), Ty::Int, Ty::Byte], Ty::Unit),
    );
}

/// Plan C Task 69 — boxed `Int64` builtin schemes.
///
/// Operations:
/// - `int64_from_int(Int) -> Int64 ![]`
/// - `int64_add(Int64, Int64) -> Int64 ![]` (wraps)
/// - `int64_sub(Int64, Int64) -> Int64 ![]` (wraps)
/// - `int64_mul(Int64, Int64) -> Int64 ![]` (wraps)
/// - `int64_div(Int64, Int64) -> Int64 ![]` (aborts on `0` or `i64::MIN/-1`)
/// - `int64_mod(Int64, Int64) -> Int64 ![]` (aborts on `0`)
/// - `int64_neg(Int64) -> Int64 ![]` (wraps on `i64::MIN`)
/// - `int64_eq` / `_lt` / `_le` / `_gt` / `_ge` `(Int64, Int64) -> Bool ![]`
/// - `int64_to_int(Int64) -> Int ![]` (saturating)
/// - `int64_to_string(Int64) -> String ![]`
fn register_builtin_int64_schemes(tc: &mut Tc) {
    let make_scheme = |params: Vec<Ty>, ret: Ty| Scheme {
        type_vars: Vec::new(),
        row_vars: Vec::new(),
        body: Ty::Fn(Box::new(FnSig {
            params,
            ret,
            effects: Vec::new(),
            effect_row_var: None,
        })),
    };
    let i64_ty = || Ty::User("Int64".to_string(), Vec::new());
    tc.fn_schemes.insert(
        "int64_from_int".to_string(),
        make_scheme(vec![Ty::Int], i64_ty()),
    );
    for op in [
        "int64_add",
        "int64_sub",
        "int64_mul",
        "int64_div",
        "int64_mod",
    ] {
        tc.fn_schemes.insert(
            op.to_string(),
            make_scheme(vec![i64_ty(), i64_ty()], i64_ty()),
        );
    }
    tc.fn_schemes.insert(
        "int64_neg".to_string(),
        make_scheme(vec![i64_ty()], i64_ty()),
    );
    for cmp in ["int64_eq", "int64_lt", "int64_le", "int64_gt", "int64_ge"] {
        tc.fn_schemes.insert(
            cmp.to_string(),
            make_scheme(vec![i64_ty(), i64_ty()], Ty::Bool),
        );
    }
    tc.fn_schemes.insert(
        "int64_to_int".to_string(),
        make_scheme(vec![i64_ty()], Ty::Int),
    );
    tc.fn_schemes.insert(
        "int64_to_string".to_string(),
        make_scheme(vec![i64_ty()], Ty::String),
    );
}

/// Plan C Task 67 — `StringBuilder` builtin schemes (Mem-gated).
///
/// Operations:
/// - `sb_new() -> StringBuilder ![Mem]`
/// - `sb_append(StringBuilder, String) -> Unit ![Mem]`
/// - `sb_finalize(StringBuilder) -> String ![Mem]`
fn register_builtin_string_builder_schemes(tc: &mut Tc) {
    let make_scheme = |params: Vec<Ty>, ret: Ty| Scheme {
        type_vars: Vec::new(),
        row_vars: Vec::new(),
        body: Ty::Fn(Box::new(FnSig {
            params,
            ret,
            effects: vec![EffectInst::bare("Mem")],
            effect_row_var: None,
        })),
    };
    let sb_ty = || Ty::User("StringBuilder".to_string(), Vec::new());
    tc.fn_schemes
        .insert("sb_new".to_string(), make_scheme(vec![], sb_ty()));
    tc.fn_schemes.insert(
        "sb_append".to_string(),
        make_scheme(vec![sb_ty(), Ty::String], Ty::Unit),
    );
    tc.fn_schemes.insert(
        "sb_finalize".to_string(),
        make_scheme(vec![sb_ty()], Ty::String),
    );
}

/// Plan C Task 68 — extended String primitives.
///
/// All operate on `TAG_STRING` headers and use byte offsets. Code-
/// point-aware variants (`string_char_at`, `string_chars`) and the
/// List-returning helpers (`string_split`, `string_join`) are
/// deferred to Task 68 part 2 (alongside the namespace fix that
/// lets a stdlib module use `Char` + `List` + `Result` together).
///
/// Operations:
/// - `string_concat(String, String) -> String ![]`
/// - `string_substring(String, Int, Int) -> String ![]`
/// - `string_byte_at(String, Int) -> Byte ![]`
/// - `string_compare(String, String) -> Int ![]`
/// - `string_starts_with(String, String) -> Bool ![]`
/// - `string_ends_with(String, String) -> Bool ![]`
/// - `string_contains(String, String) -> Bool ![]`
/// - `string_index_of(String, String) -> Int ![]`
/// - `string_trim(String) -> String ![]`
/// - `string_to_int_validate(String) -> Int ![]`
/// - `string_to_int_parse(String) -> Int ![]`
/// - `string_length(String) -> Int ![]`
fn register_builtin_string_schemes(tc: &mut Tc) {
    let make_scheme = |params: Vec<Ty>, ret: Ty| Scheme {
        type_vars: Vec::new(),
        row_vars: Vec::new(),
        body: Ty::Fn(Box::new(FnSig {
            params,
            ret,
            effects: Vec::new(),
            effect_row_var: None,
        })),
    };
    tc.fn_schemes.insert(
        "string_concat".to_string(),
        make_scheme(vec![Ty::String, Ty::String], Ty::String),
    );
    tc.fn_schemes.insert(
        "string_substring".to_string(),
        make_scheme(vec![Ty::String, Ty::Int, Ty::Int], Ty::String),
    );
    tc.fn_schemes.insert(
        "string_byte_at".to_string(),
        make_scheme(vec![Ty::String, Ty::Int], Ty::Byte),
    );
    tc.fn_schemes.insert(
        "string_compare".to_string(),
        make_scheme(vec![Ty::String, Ty::String], Ty::Int),
    );
    tc.fn_schemes.insert(
        "string_starts_with".to_string(),
        make_scheme(vec![Ty::String, Ty::String], Ty::Bool),
    );
    tc.fn_schemes.insert(
        "string_ends_with".to_string(),
        make_scheme(vec![Ty::String, Ty::String], Ty::Bool),
    );
    tc.fn_schemes.insert(
        "string_contains".to_string(),
        make_scheme(vec![Ty::String, Ty::String], Ty::Bool),
    );
    tc.fn_schemes.insert(
        "string_index_of".to_string(),
        make_scheme(vec![Ty::String, Ty::String], Ty::Int),
    );
    tc.fn_schemes.insert(
        "string_trim".to_string(),
        make_scheme(vec![Ty::String], Ty::String),
    );
    tc.fn_schemes.insert(
        "string_to_int_validate".to_string(),
        make_scheme(vec![Ty::String], Ty::Int),
    );
    tc.fn_schemes.insert(
        "string_to_int_parse".to_string(),
        make_scheme(vec![Ty::String], Ty::Int),
    );
    tc.fn_schemes.insert(
        "string_length".to_string(),
        make_scheme(vec![Ty::String], Ty::Int),
    );
}

/// Plan C Task 75 — `Random` builtin schemes.
///
/// `random_pseudo_int(): Int ![]` — process-global xorshift64 PRNG
/// (NOT cryptographically secure; see `runtime/src/random.rs`).
/// Used by the `run_pseudo_random` handler in `std/random.sigil`.
fn register_builtin_random_schemes(tc: &mut Tc) {
    let make_scheme = |params: Vec<Ty>, ret: Ty| Scheme {
        type_vars: Vec::new(),
        row_vars: Vec::new(),
        body: Ty::Fn(Box::new(FnSig {
            params,
            ret,
            effects: Vec::new(),
            effect_row_var: None,
        })),
    };
    tc.fn_schemes.insert(
        "random_pseudo_int".to_string(),
        make_scheme(vec![], Ty::Int),
    );
}

/// Plan C Task 76 — `Clock` builtin schemes.
///
/// `clock_os_now(): Int ![]` — nanos since the Unix epoch, drawn
/// from `SystemTime::now()`. Used by the `run_os_clock` handler
/// in `std/clock.sigil`.
fn register_builtin_clock_schemes(tc: &mut Tc) {
    let make_scheme = |params: Vec<Ty>, ret: Ty| Scheme {
        type_vars: Vec::new(),
        row_vars: Vec::new(),
        body: Ty::Fn(Box::new(FnSig {
            params,
            ret,
            effects: Vec::new(),
            effect_row_var: None,
        })),
    };
    tc.fn_schemes
        .insert("clock_os_now".to_string(), make_scheme(vec![], Ty::Int));
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
    /// Mirror of `CheckedProgram.call_callee_tys`. Plan B' Stage 6.8
    /// Task 104 — populated by `check_call` with the callee's `Ty`
    /// (typically `Ty::Fn(sig)`); codegen reads via `lower_call` to
    /// resolve the indirect-call signature when the callee is a
    /// let-bound / param-bound fn-typed value.
    call_callee_tys: BTreeMap<Span, Ty>,
    /// Plan B task 48 — Hindley-Milner unification machinery.
    ///
    /// Schemes for top-level functions: a generic fn declaration
    /// (`fn id[A](x: A) -> A ![] { x }`) registers `(["A"], [], (Var(N))
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
    /// Plan D Task 117 — handle-scope id supply. One fresh
    /// `ScopeId::Concrete(N)` per `Expr::Handle` at typecheck. Used
    /// to tag the `Ty::Continuation` bound to each arm's `k` so
    /// continuations from different handles can't unify (E0145).
    next_scope_id: u32,
    /// Plan D Task 117 (continuation-surface) — current handler arm
    /// body's scope id, set during arm-body typecheck walks. When
    /// `Some(N)`, user-written `Continuation[op_ret, ret]` type
    /// annotations resolve to `Ty::Continuation { ..., scope_id:
    /// Concrete(N) }` matching the enclosing handle. When `None`
    /// (annotation appears outside any arm body), the Continuation
    /// surface fires E0145 ("Continuation annotations are only
    /// valid inside a handler arm body"). Stack-discipline: saved/
    /// restored across nested arm bodies so each annotation gets
    /// the innermost enclosing arm's scope id.
    current_arm_scope_id: Option<u32>,
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
    /// Plan B task 49 — pending fn-call instantiation captures. Each
    /// entry is `(use_site_span, fn_name, fresh_var_ids)` recorded
    /// at the moment `instantiate` allocated fresh `Ty::Var`s for a
    /// generic fn's bound type vars. The outer typecheck driver
    /// resolves these through `subst` after all body checks complete
    /// to produce `CheckedProgram::call_site_instantiations`.
    pending_call_instantiations: Vec<(Span, String, Vec<u32>)>,
    /// Plan B task 49 — pending ctor-site instantiation captures.
    /// Each entry is `(use_site_span, type_name, fresh_var_ids)`
    /// recorded by the three `resolve_ctor_*_use` paths when the
    /// owning type has non-empty `generic_params`. Resolved through
    /// `subst` at end of typecheck to produce
    /// `CheckedProgram::ctor_site_instantiations`.
    pending_ctor_instantiations: Vec<(Span, String, Vec<u32>)>,
    /// Plan B task 54 — effect declaration registry. Mirror of
    /// `CheckedProgram.effects`, moved into the checked program at
    /// typecheck completion. Populated by the top-level pre-pass; the
    /// `Item::Effect` walk consults this rather than rebuilding it.
    /// `check_perform` and `check_handle` look operations up here.
    effects: BTreeMap<String, EffectDecl>,
    /// Plan B task 54 — stack of in-flight handler scopes. Each scope
    /// records, per discharged effect name, the generic-parameter
    /// substitution allocated *once* at handle-expr entry. Pushed
    /// when the handler body is checked, popped when the body walk
    /// completes; arm bodies and the optional return-arm body run
    /// after the pop, so they do not see the discharged effects in
    /// scope.
    ///
    /// Within a body walk, `check_perform` consults this stack (top-
    /// down) to find an existing substitution for the performed
    /// effect — keeping cross-perform / cross-arm generic-parameter
    /// instantiation consistent within a single handle. A `perform`
    /// of an effect not on the stack falls through to a fresh
    /// allocation, matching pre-Task-54 behaviour.
    ///
    /// Empty during normal fn body walks (no enclosing handle); only
    /// non-empty inside `check_handle`'s body recursion.
    handler_scopes: Vec<HandlerScope>,
    /// Plan B Task 55 (Phase 4d) — accumulator for the per-handle
    /// per-arm capture signatures, moved into
    /// `CheckedProgram::handle_arm_captures` at typecheck completion.
    /// `check_handle` populates one entry per `Expr::Handle`'s span at
    /// the end of arm-walking (after `saved_env` is restored from the
    /// per-arm bindings); the recorded `Vec<(String, Ty)>` per arm is
    /// the free-variable set computed against the saved enclosing-fn
    /// env, so each free name's `Ty` is exactly the type the
    /// surrounding fn would observe at the handle expression's
    /// position.
    handle_arm_captures: BTreeMap<Span, Vec<Vec<(String, Ty)>>>,
    /// Plan B Task 55 (Phase 4g) — per-handle return-arm captures
    /// (parallel to `handle_arm_captures`; each handle has at most
    /// one return arm so this is a flat `Vec<(String, Ty)>`, not a
    /// `Vec<Vec<...>>`). Populated during `check_handle`'s return-
    /// arm walk against the saved env (the surrounding fn's lexical
    /// scope at the handle expression, before the return-arm `v`
    /// binding installs). Codegen reads this via
    /// `CheckedProgram::handle_return_arm_captures` to allocate the
    /// closure record passed as `closure_ptr` to
    /// `sigil_handler_frame_set_return`.
    handle_return_arm_captures: BTreeMap<Span, Vec<(String, Ty)>>,
    /// Plan B Stage 6 cleanup — per-`Expr::Handle` body type.
    /// Populated during `check_handle`'s body walk; consumed by
    /// codegen's return-arm pre-pass to size the `v` binding's
    /// Cranelift type correctly. See
    /// `CheckedProgram::handle_body_ty`.
    handle_body_ty: BTreeMap<Span, Ty>,
}

/// Plan B task 54 — one handler's effect-instantiation cache.
/// Allocated at `check_handle` entry; consumed by `check_perform`
/// inside the handler body via the `Tc.handler_scopes` stack.
#[derive(Clone, Debug, Default)]
struct HandlerScope {
    /// `effect_name -> (effect's generic-param name -> fresh Ty::Var)`.
    /// The inner map carries the same shape as `Tc.current_generic_subst`,
    /// so it plugs directly into `ty_from_type_expr` when resolving
    /// op param / return types.
    effect_substs: BTreeMap<String, BTreeMap<String, Ty>>,
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
        ty_from_type_expr_with_rows(
            t,
            &self.types,
            &self.current_generic_subst,
            &self.current_row_var_subst,
            self.current_arm_scope_id,
        )
    }

    fn fresh_row_var(&mut self) -> u32 {
        let id = self.next_row_var;
        self.next_row_var += 1;
        id
    }

    /// Plan D Task 117 — allocate a fresh handle-scope id. Each
    /// `Expr::Handle` calls this once at the top of `check_handle`;
    /// the resulting `ScopeId::Concrete(N)` tags every
    /// `Ty::Continuation` produced for that handle's arm `k`
    /// bindings, so unification of continuations from different
    /// handles fires E0145.
    fn fresh_scope_id(&mut self) -> u32 {
        // Mirrors the implicit overflow-discipline of
        // `fresh_ty_var` / `fresh_row_var`: u32 wraparound at
        // 4 billion handles is not a realistic v1 limit, but trip
        // the assert if it ever happens in test rather than
        // silently re-using ids.
        debug_assert!(
            self.next_scope_id != u32::MAX,
            "Tc::fresh_scope_id: u32 overflow — 4 billion handles allocated in one \
             typecheck pass is implausible; check for a leaked allocator loop"
        );
        let id = self.next_scope_id;
        self.next_scope_id += 1;
        id
    }

    /// Build a generic-parameter substitution map for a fn / type
    /// declaration's `[A, B, ...]` parameter list. Allocates one
    /// fresh `Ty::Var` per declared parameter and returns the
    /// `name -> Ty::Var(id)` map plus the parallel id list (used for
    /// `Scheme.type_vars`). Empty input yields an empty map and
    /// empty id list — non-generic declarations stay zero-cost.
    ///
    /// **Allocation-order invariant** (load-bearing for `bind_ty_var`'s
    /// lower-id-is-outer-canonical direction fix from Task 63). The
    /// returned IDs are consecutive starting at the pre-call value of
    /// `next_ty_var`. Within a single `check_fn` call this method is
    /// invoked BEFORE any body-walk fresh-var allocation, so every
    /// outer-fn var has a lower ID than every body-walk fresh-var.
    /// `bind_ty_var` relies on `min(id, other)` selecting the outer-
    /// canonical representative when both vars are unbound.
    /// Structural pin: `fresh_ty_var_is_monotonic_counter` and
    /// `outer_fn_vars_have_lower_ids_than_body_fresh_vars_after_typecheck`
    /// in `mod tests`.
    fn fresh_generic_subst(&mut self, gps: &[GenericParam]) -> (BTreeMap<String, Ty>, Vec<u32>) {
        let pre_floor = self.next_ty_var;
        let mut subst = BTreeMap::new();
        let mut ids = Vec::with_capacity(gps.len());
        for gp in gps {
            let id = self.fresh_ty_var();
            ids.push(id);
            subst.insert(gp.name.clone(), Ty::Var(id));
        }
        debug_assert!(
            ids.iter()
                .enumerate()
                .all(|(i, id)| *id == pre_floor + i as u32),
            "fresh_generic_subst postcondition: allocated IDs must be consecutive \
             starting from pre-call next_ty_var (={pre_floor}); got {ids:?}"
        );
        debug_assert_eq!(
            self.next_ty_var,
            pre_floor + gps.len() as u32,
            "fresh_generic_subst postcondition: next_ty_var advanced by exactly {} (got {})",
            gps.len(),
            self.next_ty_var
        );
        (subst, ids)
    }

    /// Construct a `Ty::User(name, args)` instance for a registered
    /// user-type declaration plus the per-call surface-name → `Ty::Var`
    /// substitution and the freshly-allocated type-var ids in the
    /// type's declared generic-parameter order.
    ///
    /// Non-generic declarations return `Ty::User(name, vec![])` —
    /// same shape Plan A3 produced — with empty subst and empty ids.
    /// Generic declarations allocate one fresh `Ty::Var` per declared
    /// generic parameter so HM unification can later resolve them
    /// (Plan B task 48); the parallel ids are recorded by the ctor
    /// resolvers so monomorphization (Plan B task 49) can recover
    /// the concrete per-construction-site type-arg tuple after
    /// inference completes.
    fn fresh_user_instance_with_subst_and_ids(
        &mut self,
        name: &str,
        td: &TypeDecl,
    ) -> (Ty, BTreeMap<String, Ty>, Vec<u32>) {
        let mut subst = BTreeMap::new();
        if td.generic_params.is_empty() {
            return (Ty::User(name.to_string(), Vec::new()), subst, Vec::new());
        }
        let mut args: Vec<Ty> = Vec::with_capacity(td.generic_params.len());
        let mut fresh_ids: Vec<u32> = Vec::with_capacity(td.generic_params.len());
        for gp in &td.generic_params {
            let id = self.fresh_ty_var();
            fresh_ids.push(id);
            let v = Ty::Var(id);
            subst.insert(gp.name.clone(), v.clone());
            args.push(v);
        }
        (Ty::User(name.to_string(), args), subst, fresh_ids)
    }

    /// Resolve a `Ty` through the current substitution. Convenience
    /// wrapper around `self.subst.apply_ty` used at every site that
    /// needs the "current best" view of an inferred type.
    fn deref(&self, t: &Ty) -> Ty {
        self.subst.apply_ty(t)
    }

    /// Instantiate a scheme by allocating fresh type / row vars for
    /// each bound variable and substituting them through the body.
    /// Returns the instantiated `Ty` plus the parallel list of
    /// freshly-allocated type-var ids in the scheme's declared
    /// order (`scheme.type_vars`).
    ///
    /// Plan B task 49 — the monomorphization pass needs the parallel
    /// id list to recover the per-call-site type-arg tuple after
    /// inference completes (apply `subst` to each fresh var to get
    /// the concrete `Ty`). Non-generic schemes return an empty
    /// `Vec`, kept for uniformity.
    fn instantiate_with_vars(&mut self, scheme: &Scheme) -> (Ty, Vec<u32>) {
        let mut ty_map: BTreeMap<u32, Ty> = BTreeMap::new();
        let mut fresh_ids: Vec<u32> = Vec::with_capacity(scheme.type_vars.len());
        for &id in &scheme.type_vars {
            let fresh = self.fresh_ty_var();
            fresh_ids.push(fresh);
            ty_map.insert(id, Ty::Var(fresh));
        }
        let mut row_map: BTreeMap<u32, u32> = BTreeMap::new();
        for &id in &scheme.row_vars {
            row_map.insert(id, self.fresh_row_var());
        }
        (Self::rename_ty(&scheme.body, &ty_map, &row_map), fresh_ids)
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
            Ty::Tuple(elems) => Ty::Tuple(
                elems
                    .iter()
                    .map(|e| Self::rename_ty(e, ty_map, row_map))
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
                    // Plan D Stage 12 — rename type-var ids inside
                    // each EffectInst's args. Without this, a
                    // generic fn whose row carries `Raise[E]`
                    // doesn't get its E renamed at scheme
                    // instantiation, leaving the row's E
                    // disconnected from the fresh `Var(E_fresh)`
                    // bound by arg unification.
                    effects: sig
                        .effects
                        .iter()
                        .map(|ei| EffectInst {
                            name: ei.name.clone(),
                            args: ei
                                .args
                                .iter()
                                .map(|a| Self::rename_ty(a, ty_map, row_map))
                                .collect(),
                        })
                        .collect(),
                    effect_row_var: sig
                        .effect_row_var
                        .map(|id| row_map.get(&id).copied().unwrap_or(id)),
                };
                Ty::Fn(Box::new(new_sig))
            }
            Ty::Continuation(c) => {
                // Plan D Task 117 (a) — only Concrete scope ids
                // exist today (`check_handle` allocates them; no
                // scheme stores a Var ScopeId). Pass Concrete
                // through unchanged. Var would require region-
                // polymorphism wiring (Task 117 follow-up) — assert
                // here so the integration point surfaces loudly
                // when scheme-bearing Var ScopeIds are introduced.
                let scope_id = match &c.scope_id {
                    ScopeId::Concrete(n) => ScopeId::Concrete(*n),
                    ScopeId::Var(_) => unreachable!(
                        "rename_ty: ScopeId::Var reached scheme renaming — Task 117 (a) \
                         produces only Concrete scope ids; region-polymorphic continuation \
                         schemes are deferred to Task 117 follow-up which must wire ScopeId \
                         renaming before allocating Var"
                    ),
                };
                Ty::Continuation(Box::new(ContinuationTy {
                    op_ret: Self::rename_ty(&c.op_ret, ty_map, row_map),
                    ret: Self::rename_ty(&c.ret, ty_map, row_map),
                    scope_id,
                }))
            }
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
            Ty::Tuple(elems) => elems.iter().any(|e| self.occurs_in_ty(id, e)),
            Ty::Fn(sig) => {
                sig.params.iter().any(|p| self.occurs_in_ty(id, p))
                    || self.occurs_in_ty(id, &sig.ret)
            }
            Ty::Continuation(c) => {
                self.occurs_in_ty(id, &c.op_ret) || self.occurs_in_ty(id, &c.ret)
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
            // Plan C Task 63 — when binding two unbound type-vars,
            // prefer to point the higher-id var at the lower-id one.
            // Outer-fn type vars (collected into `outer_fn_var_ids`
            // from each fn's `Scheme.type_vars`) are allocated by
            // `check_fn`'s `fresh_generic_subst` BEFORE any body
            // fresh vars within that fn, so within a single fn body
            // lower-id is the outer-canonical representative.
            // Cross-arm unify in `check_match` unifies one arm's
            // free fresh-var (e.g. `Result[A, ?fE]`'s ?fE from
            // `Ok(x)`) with the other arm's outer-bound fresh var
            // (`Result[?fA, E]`'s already-bound ?fA from `Err(e)`).
            // Without this preference, the bind direction is
            // OUTER → FRESH (`subst[outer] = Var(fresh)`), and the
            // pending_ctor `apply_ty` walk returns the still-
            // unbound fresh var, firing E0132 even though the
            // program is well-typed under HM. Two-param sum types
            // (Result[A, E]) where each ctor arm only fixes one of
            // the two params are the canonical reproducer; List[A]
            // with a single param never tripped this because the
            // unbound arm-var never had a competing already-bound
            // counterpart at cross-arm time.
            //
            // Invariant pins (`mod tests`):
            //   - `fresh_ty_var_is_monotonic_counter` — the counter
            //     is strictly increasing.
            //   - `fresh_generic_subst_then_body_fresh_vars_have_higher_ids`
            //     — outer-fn vars allocate before body fresh-vars
            //     at the API level.
            //   - `bind_ty_var_with_two_unbound_vars_picks_lower_id_as_canonical`
            //     — this fn's load-bearing direction.
            //   - `outer_fn_vars_have_lower_ids_than_body_fresh_vars_after_typecheck`
            //     — end-to-end consecutiveness pin.
            //   - `two_param_sum_type_match_each_arm_constrains_one_param_typechecks`
            //     — the user-facing regression.
            let canonical = (*other).min(id);
            let other_id = (*other).max(id);
            if canonical != other_id {
                if self.occurs_in_ty(other_id, &Ty::Var(canonical)) {
                    self.push_error(
                        "E0126",
                        span.clone(),
                        format!(
                            "occurs check failed: cannot construct an infinite type `?{other_id} = ?{canonical}`"
                        ),
                    );
                    return false;
                }
                self.subst.tys.insert(other_id, Ty::Var(canonical));
            }
            return true;
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
        // Literal self-bind: `r` is exactly `?id` with no extra
        // effects. Skipping the deref here matters — a *transitive*
        // resolution to `?id` is a cycle, not a self-bind, and must
        // hit the occurs branch below.
        if r.tail == Some(id) && r.effects.is_empty() {
            return true;
        }
        let resolved = self.subst.apply_row(r);
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
            // Plan D Task 113 — tuple unification: arity must match;
            // elements unify position-by-position.
            (Ty::Tuple(elems_a), Ty::Tuple(elems_b)) => {
                if elems_a.len() != elems_b.len() {
                    self.push_error(
                        "E0044",
                        span.clone(),
                        format!(
                            "tuple arity mismatch: expected `{}`, got `{}`",
                            ty_display(&a),
                            ty_display(&b)
                        ),
                    );
                    return false;
                }
                let mut ok = true;
                for (ea, eb) in elems_a.iter().zip(elems_b.iter()) {
                    if !self.unify_ty(ea, eb, span) {
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
            // Plan D Task 117 — continuation unification. Two
            // `Ty::Continuation` values unify only if their scope_ids
            // match (concrete-vs-concrete must be equal; vars bind to
            // concretes or each other) AND their op_ret + ret
            // structurally unify. Different concrete scope_ids fire
            // E0145 ("continuations from different handles cannot
            // unify") rather than the generic E0044 — the user
            // doesn't think of these as type mismatches but as
            // escape-barrier violations.
            (Ty::Continuation(c_a), Ty::Continuation(c_b)) => {
                let scope_ok = match (&c_a.scope_id, &c_b.scope_id) {
                    (ScopeId::Concrete(n), ScopeId::Concrete(m)) => {
                        if n == m {
                            true
                        } else {
                            self.push_error(
                                "E0145",
                                span.clone(),
                                format!(
                                    "continuation from handle scope {n} cannot unify with \
                                     continuation from handle scope {m} — `k` cannot escape \
                                     its originating handle's arm body"
                                ),
                            );
                            false
                        }
                    }
                    // Plan D Task 117 (a) only produces Concrete
                    // scope ids (`check_handle` calls
                    // `Tc::fresh_scope_id()` which always returns
                    // Concrete; no scheme-instantiation path or
                    // surface syntax allocates Var). Reaching this
                    // arm means a Var leaked into typecheck without
                    // wiring the unification — assert to surface
                    // the integration point loudly when region-
                    // polymorphism work begins (Task 117 follow-up).
                    (ScopeId::Var(_), _) | (_, ScopeId::Var(_)) => unreachable!(
                        "ScopeId::Var reached unify_ty — Task 117 (a) produces only Concrete \
                         scope ids; region-polymorphic continuation schemes are deferred to \
                         Task 117 follow-up which must wire ScopeId unification before \
                         allocating Var"
                    ),
                };
                let op_ret_ok = self.unify_ty(&c_a.op_ret, &c_b.op_ret, span);
                let ret_ok = self.unify_ty(&c_a.ret, &c_b.ret, span);
                scope_ok && op_ret_ok && ret_ok
            }
            // Plan D Task 117 — escape barrier. A `Ty::Continuation`
            // unified against any non-Continuation, non-Var type is
            // an escape attempt: the continuation `k` would be
            // stored in (record field / ctor field / fn param / fn
            // return) something that isn't a continuation. Fire
            // E0145 with a uniform fix message rather than the
            // generic E0044 ("type mismatch"). Var-vs-Continuation
            // is handled higher up by the (Var, other) arm via
            // bind_ty_var — an unbound type var legitimately binds
            // to Continuation (e.g. `let f = k` infers `f`'s var to
            // Continuation); the escape only manifests when that
            // bound var later unifies against a non-Continuation
            // target, which re-enters this arm with both sides
            // resolved.
            (Ty::Continuation(_), _) | (_, Ty::Continuation(_)) => {
                self.push_error(
                    "E0145",
                    span.clone(),
                    format!(
                        "continuation `k` cannot escape its handle's arm body — tried to \
                         use a value of type `{}` where `{}` was expected. Keep `k` inside \
                         the handle's arm body (do not store it in a record/ctor field, \
                         pass it to a function expecting a non-continuation parameter, or \
                         return it from a function whose declared return type is not a \
                         continuation)",
                        ty_display(&a),
                        ty_display(&b)
                    ),
                );
                false
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
        // Plan D Stage 12 — name-based matching with arg unification.
        // For each effect-name shared between rows, unify the args
        // pairwise (E0044 if args mismatch). Effect-names appearing on
        // only one side go to `only_a` / `only_b`. This is the
        // unification-semantics fix for the structural-equality diff
        // that pre-Stage-12 produced spurious E0042/E0128 when two
        // rows shared a name but carried Ty::Var args (e.g.,
        // handle-discharge body row vs body's expected row inside
        // `catch[A, E](body: () -> A ![Raise[E]]) -> ...`).
        //
        // Caveat: each effect-name is assumed to appear at most once
        // per row. Multi-instantiation rows like `![Raise[Int],
        // Raise[String]]` are possible but unusual; the loop is
        // first-occurrence-wins (matched_b marks each b entry the
        // moment it pairs), so reversed b ordering would unify
        // Int vs String first → E0044. Pinned by the
        // `unify_row_multi_instantiation_*` regression tests in this
        // module; revisit them if future work changes the matching
        // strategy (e.g., exhaustive permutation).
        let mut only_a: Vec<EffectInst> = Vec::new();
        let mut matched_b: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
        // Plan D Stage 12 R3 — thread unify_ty's bool through; a
        // false from arg-unification is a real error and the
        // overall unify_row return must reflect it.
        let mut args_ok = true;
        for ea in &a.effects {
            match b
                .effects
                .iter()
                .enumerate()
                .find(|(idx, eb)| !matched_b.contains(idx) && eb.name == ea.name)
            {
                Some((idx, eb)) => {
                    matched_b.insert(idx);
                    if ea.args.len() == eb.args.len() {
                        for (ax, bx) in ea.args.iter().zip(eb.args.iter()) {
                            if !self.unify_ty(ax, bx, span) {
                                args_ok = false;
                            }
                        }
                    } else {
                        // Arg-arity mismatch between two same-named
                        // effect references in the same row — this
                        // is a real error, not graceful recovery.
                        // E0143 catches it at the row-decl site;
                        // here we surface a row-level diagnostic so
                        // the user sees the inconsistency.
                        self.push_error(
                            "E0128",
                            span.clone(),
                            format!(
                                "effect `{}` is referenced with {} type arg(s) on one side and {} on the other in the same unification — \
                                 the effect-decl's generic-param count must match consistently across both rows",
                                ea.name,
                                ea.args.len(),
                                eb.args.len(),
                            ),
                        );
                        args_ok = false;
                    }
                }
                None => only_a.push(ea.clone()),
            }
        }
        let only_b: Vec<EffectInst> = b
            .effects
            .iter()
            .enumerate()
            .filter(|(idx, _)| !matched_b.contains(idx))
            .map(|(_, eb)| eb.clone())
            .collect();
        match (a.tail, b.tail) {
            (None, None) => {
                // Both closed: must be set-equal.
                if !only_a.is_empty() || !only_b.is_empty() {
                    self.push_error(
                        "E0128",
                        span.clone(),
                        format!(
                            "effect row mismatch: closed row `![{}]` cannot unify with closed row `![{}]`",
                            effects_display(&a.effects),
                            effects_display(&b.effects)
                        ),
                    );
                    return false;
                }
                args_ok
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
                            effects_display(&a.effects),
                            effects_display(&only_b),
                            effects_display(&b.effects)
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
                ) && args_ok
            }
            (Some(a_tail), None) => {
                if !only_a.is_empty() {
                    self.push_error(
                        "E0128",
                        span.clone(),
                        format!(
                            "effect row mismatch: closed row `![{}]` is missing `{}` required by row `![{} | ?{a_tail}]`",
                            effects_display(&b.effects),
                            effects_display(&only_a),
                            effects_display(&a.effects)
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
                ) && args_ok
            }
            (Some(a_tail), Some(b_tail)) if a_tail == b_tail => {
                if !only_a.is_empty() || !only_b.is_empty() {
                    self.push_error(
                        "E0128",
                        span.clone(),
                        format!(
                            "effect row mismatch: rows share tail `?{a_tail}` but differ in known effects ({} vs {})",
                            effects_display(&a.effects),
                            effects_display(&b.effects)
                        ),
                    );
                    return false;
                }
                args_ok
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
                ok_a && ok_b && args_ok
            }
        }
    }

    /// Asymmetric row subsumption used at call sites (Plan B task
    /// 48 — reviewer follow-up). Different from `unify_row`:
    /// - Callee's effects must be a subset of caller's known effects
    ///   (closed callees fail loudly when the caller's row doesn't
    ///   permit them — E0042 with the legacy diagnostic vocabulary).
    /// - If the callee carries an open row variable, that variable
    ///   absorbs the caller's leftover effects (and the caller's
    ///   row tail, if any). This preserves the *caller's* row
    ///   variable for generalisation rather than collapsing it.
    /// - The caller's row variable is never bound from a call site.
    fn subsume_row(&mut self, callee_row: &Row, caller_row: &Row, span: &Span) -> bool {
        let callee = self.subst.apply_row(callee_row);
        let caller = self.subst.apply_row(caller_row);
        // Plan D Stage 12 — name-based matching with arg unification
        // (matches `unify_row`'s post-Stage-12 semantics). For each
        // callee effect, find a caller effect with the same name; if
        // matched, unify args (E0044 fires on mismatch). Names not
        // in the caller's row go to `missing` (E0042).
        //
        // Same multi-instantiation caveat as `unify_row`: the loop is
        // first-occurrence-wins (matched_caller marks each caller
        // entry the moment it pairs), so reversed caller-side ordering
        // would unify mismatched args first → E0044. Pinned by the
        // `subsume_row_multi_instantiation_*` regression tests in this
        // module; revisit them if future work changes the matching
        // strategy (e.g., exhaustive permutation).
        let mut missing: Vec<EffectInst> = Vec::new();
        let mut matched_caller: std::collections::BTreeSet<usize> =
            std::collections::BTreeSet::new();
        // Plan D Stage 12 R3 — thread unify_ty's bool through.
        let mut args_ok = true;
        for ce in &callee.effects {
            match caller
                .effects
                .iter()
                .enumerate()
                .find(|(idx, ec)| !matched_caller.contains(idx) && ec.name == ce.name)
            {
                Some((idx, ec)) => {
                    matched_caller.insert(idx);
                    if ce.args.len() == ec.args.len() {
                        for (ax, bx) in ce.args.iter().zip(ec.args.iter()) {
                            if !self.unify_ty(ax, bx, span) {
                                args_ok = false;
                            }
                        }
                    } else {
                        // Arg-arity mismatch between same-named
                        // effect refs in callee vs caller — real
                        // error, not graceful recovery.
                        self.push_error(
                            "E0042",
                            span.clone(),
                            format!(
                                "calling a function whose `{}` row entry has {} type arg(s) requires the enclosing function's `{}` row entry to have the same arity (got {})",
                                ce.name,
                                ce.args.len(),
                                ce.name,
                                ec.args.len(),
                            ),
                        );
                        args_ok = false;
                    }
                }
                None => missing.push(ce.clone()),
            }
        }
        if !missing.is_empty() {
            // Callee performs effects the caller doesn't permit.
            // Use the legacy E0042 diagnostic to keep error messages
            // consistent with Plan A1/A2/A3's effect-row check.
            for e in &missing {
                let s = e.display_str();
                self.push_error(
                    "E0042",
                    span.clone(),
                    format!(
                        "calling a function that performs `{s}` requires `{s}` in the enclosing function's effect row",
                    ),
                );
            }
            return false;
        }
        if let Some(callee_tail) = callee.tail {
            // Callee has an open row var — it absorbs caller's
            // leftover effects (caller-side names not matched to
            // any callee name) + caller's tail. This binds *only*
            // the callee's row var.
            let leftover: Vec<EffectInst> = caller
                .effects
                .iter()
                .enumerate()
                .filter(|(idx, _)| !matched_caller.contains(idx))
                .map(|(_, ec)| ec.clone())
                .collect();
            self.bind_row_var(
                callee_tail,
                &Row {
                    effects: leftover,
                    tail: caller.tail,
                },
                span,
            ) && args_ok
        } else {
            args_ok
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
                // Plan D Task 117 (continuation-surface) —
                // `Continuation[op_ret, ret]` is the surface form
                // for k's binding type. Validate the arity and the
                // arm-context location HERE so the diagnostic is
                // precise; `ty_from_type_expr_with_rows` returns
                // None silently in both failure modes (arity wrong
                // or no arm context) and would otherwise surface
                // as a generic E0112 "unknown type" miss.
                if name == "Continuation" {
                    if args.len() != 2 {
                        self.push_error(
                            "E0129",
                            span.clone(),
                            format!(
                                "type `Continuation` expects 2 type arguments \
                                 (op_ret, ret), got {}",
                                args.len()
                            ),
                        );
                        return;
                    }
                    if self.current_arm_scope_id.is_none() {
                        self.push_error(
                            "E0145",
                            span.clone(),
                            "Continuation annotations are only valid inside a handler arm \
                             body — `Continuation[op_ret, ret]` names the type of the \
                             handler's `k` binding, which exists only within that arm's \
                             dynamic extent. Move the annotation inside an arm body, or \
                             remove it (the type isn't user-constructible outside arm \
                             contexts)"
                                .to_string(),
                        );
                        return;
                    }
                    return;
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
            TypeExpr::Fn(fty) => {
                // Plan B' Stage 6.8 Task 103 — recurse into params +
                // ret so any nested unknown-type / Apply errors
                // still surface against the inner spans. The Fn
                // surface itself maps to Ty::Fn at
                // `ty_from_type_expr` (closed rows only in v1; row-
                // variable-bearing fn-types reject via E0137 below).
                for p in &fty.params {
                    self.check_type_expr_known(p);
                }
                self.check_type_expr_known(&fty.ret);
                // Plan D Task 114 — also walk effect-row entries so
                // EffectRef args get their own E0107 (unknown type)
                // diagnostics, plus arity-check the row's
                // generic-effect references against their declared
                // generic-param count (E0143).
                for eref in &fty.effects {
                    for a in &eref.args {
                        self.check_type_expr_known(a);
                    }
                    self.check_effect_ref_arity(eref);
                }
                // Plan D Task 116 — row-variable-bearing first-class
                // function types are now accepted. The row-var name
                // must already be bound at the enclosing fn's
                // `effect_row_var` (currently the only legal
                // binder; multi-row-var fns are deferred). When
                // unbound, fire E0137 with a precise diagnostic
                // pointing to the missing declaration.
                if let Some(rv) = &fty.effect_row_var {
                    if !self.current_row_var_subst.contains_key(&rv.name) {
                        self.push_error(
                            "E0137",
                            rv.span.clone(),
                            format!(
                                "row variable `{}` is not bound by the enclosing function — \
                                 declare it on the fn's row (e.g. `fn f(...) -> R ![| {}]` or \
                                 `![<effects> | {}]`) so the row variable can be referenced in \
                                 inner fn-type rows",
                                rv.name, rv.name, rv.name,
                            ),
                        );
                    }
                }
            }
            // Plan D Task 113 — recurse into tuple element types so
            // any nested unknown-type / Apply errors still surface
            // against the inner spans.
            TypeExpr::Tuple { elems, .. } => {
                for e in elems {
                    self.check_type_expr_known(e);
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
    /// Plan D Task 114 — validate one row-site `EffectRef` against
    /// its declared `EffectDecl`. Two checks:
    ///
    /// 1. **E0042** if the effect is not declared at all (mirrors
    ///    the existing legacy diagnostic vocabulary; we widen here
    ///    so unknown-effect references at row sites surface with
    ///    the same code paths that already exist for unknown
    ///    effects at perform sites).
    /// 2. **E0143** (Task 114; renamed from E0140 by Task 115's
    ///    audit — see catalog entry) if the row-site arg list arity
    ///    diverges from the decl's `generic_params`. A bare-name
    ///    reference to a generic effect-decl (`![Raise]` when the
    ///    decl is `Raise[E]`) and a referenced effect-decl with
    ///    too few or too many args (`Raise[Int, String]` when the
    ///    decl is `Raise[E]`) both fire E0143 with a precise
    ///    arity message. Bare-name refs to non-generic
    ///    effect-decls (the dominant pre-Task-114 surface)
    ///    continue to typecheck cleanly.
    fn check_effect_ref_arity(&mut self, eref: &crate::ast::EffectRef) {
        let decl = match self.effects.get(&eref.name) {
            Some(d) => d.clone(),
            None => {
                self.push_error(
                    "E0042",
                    eref.span.clone(),
                    format!(
                        "effect `{}` is not declared (declare it with `effect {} {{ ... }}` \
                         before referencing it in an effect row)",
                        eref.name, eref.name,
                    ),
                );
                return;
            }
        };
        let expected = decl.generic_params.len();
        let got = eref.args.len();
        if expected != got {
            let expected_names: Vec<&str> = decl
                .generic_params
                .iter()
                .map(|gp| gp.name.as_str())
                .collect();
            let header = if expected == 0 {
                format!(
                    "effect `{}` is not generic — drop the type-arg list to write `{}` instead of `{}`",
                    eref.name, eref.name, eref.name,
                )
            } else if got == 0 {
                format!(
                    "effect `{}` is generic over [{}] — write `{}[{}]` with explicit type arguments \
                     (bare `{}` refers to the un-instantiated declaration)",
                    eref.name,
                    expected_names.join(", "),
                    eref.name,
                    expected_names.join(", "),
                    eref.name,
                )
            } else {
                format!(
                    "effect `{}` is declared with {expected} type parameter(s) [{}], but {got} \
                     argument(s) were provided in the row site",
                    eref.name,
                    expected_names.join(", "),
                )
            };
            self.push_error("E0143", eref.span.clone(), header);
        }
    }

    fn resolve_ctor_unit_use(&mut self, name: &str, span: &Span) -> Option<Ty> {
        let info = self.ctors.get(name).cloned()?;
        let td = self.types.get(&info.type_name)?.clone();
        let variant = &td.variants[info.variant_index];
        match &variant.fields {
            VariantFields::Unit => {
                let (ty, _subst, fresh_ids) =
                    self.fresh_user_instance_with_subst_and_ids(&info.type_name, &td);
                // Plan B task 49 — record the use site so monomorph
                // can clone the type at the inferred type-arg tuple.
                // Empty `fresh_ids` for non-generic types is recorded
                // uniformly so monomorph keys all ctor sites the same.
                self.pending_ctor_instantiations.push((
                    span.clone(),
                    info.type_name.clone(),
                    fresh_ids,
                ));
                Some(ty)
            }
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
        row: &[EffectInst],
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
                let (result_ty, ctor_subst, fresh_ids) =
                    self.fresh_user_instance_with_subst_and_ids(&info.type_name, &td);
                // Plan B task 49 — record the construction site so
                // monomorph can clone the type at the inferred type-
                // arg tuple. Empty `fresh_ids` for non-generic types
                // is recorded uniformly.
                self.pending_ctor_instantiations.push((
                    span.clone(),
                    info.type_name.clone(),
                    fresh_ids,
                ));
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
        row: &[EffectInst],
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
        let (result_ty, ctor_subst, fresh_ids) =
            self.fresh_user_instance_with_subst_and_ids(&info.type_name, &td);
        // Plan B task 49 — record the construction site so monomorph
        // can clone the type at the inferred type-arg tuple.
        self.pending_ctor_instantiations
            .push((span.clone(), info.type_name.clone(), fresh_ids));
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
            // `fn main`'s signature shape is constrained: returns `Int`,
            // takes no params, effect row may only contain effects the
            // top-level `main` shim discharges. Plan B Task 57 expanded
            // the discharged set from `{IO}` to `{IO, ArithError}` —
            // both have top-level handler frames installed by the shim
            // (per `[DEVIATION Task 57] Top-level handler installation
            // in main shim` in PLAN_B_DEVIATIONS.md). Programs whose
            // `main` row references any other effect would `sigil_-
            // perform` against an unhandled effect at runtime.
            if !type_is(&f.return_type, "Int") {
                self.push_error(
                    "E0041",
                    f.span.clone(),
                    "`fn main` must return `Int` (expected `fn main() -> Int ![IO]`, \
                     `fn main() -> Int ![ArithError]`, `fn main() -> Int ![IO, ArithError]`, \
                     or `fn main() -> Int ![]`)",
                );
            }
            if !f.params.is_empty() {
                self.push_error(
                    "E0041",
                    f.span.clone(),
                    "`fn main` takes no parameters (expected `fn main() -> Int ![IO]` or similar)",
                );
            }
            for effect in &f.effects {
                let name = effect.name.as_str();
                if name != "IO" && name != "ArithError" && name != "Mem" {
                    self.push_error(
                        "E0041",
                        f.span.clone(),
                        format!(
                            "`fn main`'s effect row may only contain effects discharged by \
                             the top-level shim (`IO`, `ArithError`, or `Mem`); saw `{name}`",
                        ),
                    );
                }
            }
        }
        // Plan D Task 114 — body row carries args so per-call
        // subsumption sees `Raise[Int]` rather than bare `Raise`.
        let body_row = effect_refs_to_insts(&f.effects, &self.types, &self.current_generic_subst);
        let body_ty = self.check_block(&f.body, &body_row);

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
                effects: effect_refs_to_insts(&f.effects, &self.types, &self.current_generic_subst),
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
    fn check_block(&mut self, b: &Block, row: &[EffectInst]) -> Option<Ty> {
        for s in &b.stmts {
            match s {
                Stmt::Expr(e) => {
                    let _ = self.check_expr(e, row);
                }
                Stmt::Perform(p) => {
                    let _ = self.check_perform(p, row);
                }
                Stmt::Let(l) => {
                    // Plan B task 48: let-binding annotation may now
                    // reference in-scope generic parameters or
                    // generic-applied user types. Structurally
                    // validated here (E0112 / E0129 / E0131 fire on
                    // unknown name, arity mismatch, or applying a
                    // primitive). HM unification then checks the
                    // initializer against the declared type.
                    self.check_type_expr_known(&l.ty);
                    // Plan D Task 117 (continuation-surface, PR #62
                    // followup): when the let's declared type is
                    // `Continuation[op_ret, ret]`, the v1 desugar
                    // pre-pass only handles RHS shapes that are a
                    // bare `Expr::Ident` (the alias case). Other
                    // RHS shapes (`if cond { k } else { k }`,
                    // `match m { ... }`, parenthesized blocks,
                    // etc.) typecheck successfully (each branch is
                    // Ty::Continuation) but the desugar can't
                    // statically reduce them to the Slice B/C
                    // shapes the codegen recognizer supports. Push
                    // E0145 with a precise diagnostic at the
                    // annotation site so the user knows what's
                    // accepted; otherwise the compile fails later
                    // with a confusing codegen-walker reject on the
                    // surviving bare `k`.
                    let is_continuation_annotation = matches!(
                        &l.ty,
                        TypeExpr::Apply { name, args, .. }
                            if name == "Continuation" && args.len() == 2
                    );
                    if is_continuation_annotation && !matches!(&l.value, Expr::Ident(_, _)) {
                        self.push_error(
                            "E0145",
                            l.span.clone(),
                            "let-binding of type `Continuation[op_ret, ret]` requires \
                             the initializer to be a bare identifier (the arm's `k` \
                             or a previously-bound Continuation alias). Conditional/\
                             matched/expression-form initializers aren't supported in \
                             v1; pull the alias to a top-level `let` of the form \
                             `let f: Continuation[..] = k;` and use `f` directly"
                                .to_string(),
                        );
                    }
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

    /// Plan B Task 57 — verify that the surrounding fn's effect row
    /// declares `effect_name`. Used by both `check_perform` (for
    /// every `perform` site) and the `Expr::Binary` arm of
    /// `check_expr` (for `BinOp::Div`/`Mod` sites, which lower to
    /// `perform ArithError.{div,mod}_by_zero()` at elaborate-time).
    /// The deviation entry `[DEVIATION Task 57] BinOp::Div and
    /// BinOp::Mod elaborate to perform-bearing form` documents why
    /// the row introduction happens at typecheck rather than waiting
    /// for elaborate's rewrite.
    ///
    /// Emits E0042 with a context-specific message when missing;
    /// returns whether the row contains `effect_name` for callers
    /// that want to skip downstream registry lookups on missing rows
    /// (today no caller uses the return).
    fn register_effect_use(
        &mut self,
        effect_name: &str,
        row: &[EffectInst],
        span: Span,
        ctx: &str,
    ) {
        if !row.iter().any(|e| e.name == effect_name) {
            self.push_error(
                "E0042",
                span,
                format!("`{ctx}` requires `{effect_name}` in the enclosing function's effect row"),
            );
        }
    }

    fn check_perform(&mut self, p: &PerformExpr, row: &[EffectInst]) -> Option<Ty> {
        let ctx = format!("perform {}.{}", p.effect, p.op);
        self.register_effect_use(&p.effect, row, p.span.clone(), &ctx);
        // Plan B task 54 + Task 57 — every effect (including the
        // builtin `IO` and `ArithError`) dispatches through the
        // typechecker's effect registry built in the top-level
        // pre-pass. Task 57 dropped the Stage 1 IO hard-wire that
        // used to live above this comment; IO.println is now an
        // ordinary registry entry. The lookup needs to clone the
        // operation out of the registry to avoid borrowing
        // `self.effects` while the arg-typing recursion immutably
        // borrows `self`.
        let eff_decl = match self.effects.get(&p.effect).cloned() {
            Some(d) => d,
            None => {
                // Unknown effect. Mirror the existing IO-unknown-op
                // branch: emit E0042 specifically for the "not in
                // registry" case so a user who writes
                // `fn f() -> Int ![Foo] { perform Foo.bar() }`
                // (Foo listed in row but never declared) sees a
                // diagnostic pointing at the missing declaration
                // even though the row-membership check above
                // accepted Foo. Walk args for nested errors, then
                // recover.
                self.push_error(
                    "E0042",
                    p.span.clone(),
                    format!(
                        "unknown effect `{eff}` — declare it via `effect {eff} {{ ... }}` or remove the `perform`",
                        eff = p.effect,
                    ),
                );
                for a in &p.args {
                    let _ = self.check_expr(a, row);
                }
                return None;
            }
        };
        let op = match eff_decl.ops.iter().find(|o| o.name == p.op) {
            Some(o) => o.clone(),
            None => {
                self.push_error(
                    "E0042",
                    p.span.clone(),
                    format!(
                        "operation `{op}` is not declared on effect `{eff}`",
                        op = p.op,
                        eff = p.effect,
                    ),
                );
                for a in &p.args {
                    let _ = self.check_expr(a, row);
                }
                return None;
            }
        };
        // Compute op param types and return type under the effect's
        // own generic substitution + the per-op generic substitution
        // (Plan D Task 115).
        //
        // **Effect-decl substitution choice**:
        //   1. If a surrounding `handle X with { ... }` discharges
        //      this effect, reuse the handler's `effect_substs[X]`
        //      so cross-perform / cross-arm consistency holds.
        //   2. Else, if the surrounding fn's row carries an entry
        //      for `X` with concrete args (`![Raise[Int]]`), build
        //      a substitution from those args. Closes the Task 114
        //      R1 deferred gap: `perform Raise.fail("oops")` under
        //      `![Raise[Int]]` now instantiates `E := Int` and
        //      fails E0044 if the arg's type is wrong.
        //   3. Else, allocate fresh `Ty::Var`s per the effect-
        //      decl's `generic_params` (the unconstrained-call
        //      shape).
        //
        // **Per-op substitution**: independently, allocate fresh
        // `Ty::Var`s per the OP's `generic_params`. The combined
        // substitution governs op param / return resolution.
        let saved_subst = std::mem::take(&mut self.current_generic_subst);
        let mut eff_subst: BTreeMap<String, Ty> = self
            .handler_scopes
            .iter()
            .rev()
            .find_map(|scope| scope.effect_substs.get(&p.effect).cloned())
            .or_else(|| {
                // Plan D Task 114 R1 — find the surrounding fn's row
                // entry for this effect; if its args match the
                // effect-decl's arity, build the substitution from
                // them. Mismatched arity at the row site already
                // fired E0143 in `check_effect_ref_arity`, so we
                // skip silently here on len mismatch.
                row.iter()
                    .find(|e| e.name == p.effect)
                    .filter(|e| e.args.len() == eff_decl.generic_params.len())
                    .map(|e| {
                        eff_decl
                            .generic_params
                            .iter()
                            .zip(e.args.iter())
                            .map(|(gp, a)| (gp.name.clone(), a.clone()))
                            .collect()
                    })
            })
            .unwrap_or_else(|| self.fresh_generic_subst(&eff_decl.generic_params).0);
        // Plan D Task 115 — per-op generic params get their own
        // fresh Ty::Var allocations. Add to the effect-level
        // substitution (op-level shadows on collision; E0144 already
        // fired at the effect-decl pre-pass for shadowing).
        let (op_subst, _) = self.fresh_generic_subst(&op.generic_params);
        for (k, v) in op_subst {
            eff_subst.insert(k, v);
        }
        self.current_generic_subst = eff_subst;
        let param_tys: Vec<Option<Ty>> = op
            .params
            .iter()
            .map(|t| self.ty_from_type_expr_here(t))
            .collect();
        let op_ret_ty = self.ty_from_type_expr_here(&op.return_type);
        self.current_generic_subst = saved_subst;
        if p.args.len() != op.params.len() {
            self.push_error(
                "E0043",
                p.span.clone(),
                format!(
                    "`{eff}.{op}` takes {expected} argument(s); got {actual}",
                    eff = p.effect,
                    op = p.op,
                    expected = op.params.len(),
                    actual = p.args.len(),
                ),
            );
            for a in &p.args {
                let _ = self.check_expr(a, row);
            }
            return op_ret_ty.map(|t| self.deref(&t));
        }
        for (i, arg) in p.args.iter().enumerate() {
            let arg_ty = self.check_expr(arg, row);
            if let (Some(at), Some(pt)) = (arg_ty, param_tys.get(i).and_then(|x| x.clone())) {
                let _ = self.unify_ty(&pt, &at, &arg.span());
            }
        }
        op_ret_ty.map(|t| self.deref(&t))
    }

    fn check_expr(&mut self, e: &Expr, row: &[EffectInst]) -> Option<Ty> {
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
                    let (ty, fresh_ids) = self.instantiate_with_vars(&scheme);
                    // Plan B task 49 — capture every top-level-fn use
                    // site (callee in a Call, or value-position Ident
                    // referencing a fn). Empty `fresh_ids` is the
                    // non-generic case; recording it uniformly lets
                    // monomorphize key all top-level-fn references the
                    // same way regardless of genericity.
                    self.pending_call_instantiations
                        .push((span.clone(), name.clone(), fresh_ids));
                    return Some(ty);
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
            Expr::Perform(p) => self.check_perform(p, row),
            Expr::BoolLit(_, _) => Some(Ty::Bool),
            Expr::CharLit(_, _) => Some(Ty::Char),
            Expr::Binary { op, lhs, rhs, span } => {
                let lt = self.check_expr(lhs, row);
                let rt = self.check_expr(rhs, row);
                // Plan B Task 57 — `BinOp::Div` and `BinOp::Mod`
                // elaborate to a perform-bearing form (`if rhs == 0
                // { perform ArithError.{div,mod}_by_zero() } else {
                // … }`); the row introduction happens here at
                // typecheck because elaborate runs after typecheck
                // and cannot influence the row check upstream. See
                // `[DEVIATION Task 57] BinOp::Div and BinOp::Mod
                // elaborate to perform-bearing form` in
                // `PLAN_B_DEVIATIONS.md`.
                if matches!(op, BinOp::Div | BinOp::Mod) {
                    let opname = if matches!(op, BinOp::Div) { "/" } else { "%" };
                    let ctx = format!("operator `{opname}` (may abort with ArithError)");
                    self.register_effect_use("ArithError", row, span.clone(), &ctx);
                }
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
                // Plan D Task 114 — lambda's row carries args (same
                // shape as the enclosing fn's body row).
                let lambda_row =
                    effect_refs_to_insts(effects, &self.types, &self.current_generic_subst);
                self.check_lambda(params, return_type, &lambda_row, body, span.clone())
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
            // Plan B task 54 — `handle <body> with { ... }` runs
            // proper handler typing (env extension for op-arm params
            // and continuation `k`, body-row extension with
            // discharged effects, residual row equal to caller's
            // row, cross-arm unification through a single
            // handler-overall type, one-shot linearity check via
            // E0220). E0134 was lifted in Task 55 Phase 2 (`2d69b52`);
            // well-formed handle expressions now reach the codegen
            // path that wires the runtime handler-frame ABI.
            Expr::Handle {
                body,
                return_arm,
                op_arms,
                span,
            } => self.check_handle(body, return_arm.as_deref(), op_arms, row, span),
            // Plan D Task 113 — tuple value: `(e1, e2, ...)`. Check each
            // element under the active row; the tuple's type is the
            // element-wise Ty::Tuple. Empty tuples are rejected by the
            // parser (zero-arity reserved); single-element parens are
            // paren-grouping and never reach this arm.
            Expr::Tuple { elems, .. } => {
                let elem_tys: Vec<Ty> = elems
                    .iter()
                    .map(|e| {
                        self.check_expr(e, row)
                            .unwrap_or(Ty::Var(self.fresh_ty_var()))
                    })
                    .collect();
                Some(Ty::Tuple(elem_tys))
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
            BinOp::SdivUnchecked | BinOp::SremUnchecked => {
                // Plan B Task 57 — codegen-internal variants produced
                // only by elaborate's `BinOp::Div`/`Mod` rewrite. The
                // typechecker reaches this arm only via synthetic
                // walks (e.g. typecheck-tests-walking-elaborated-IR),
                // not via user source. Same operand/result types as
                // the pre-elaborate variants.
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
        row: &[EffectInst],
    ) -> Option<Ty> {
        let callee_ty = self.check_expr(callee, row);
        // Always type-check args so we surface any errors in them.
        let arg_tys: Vec<Option<Ty>> = args.iter().map(|a| self.check_expr(a, row)).collect();

        let sig = match callee_ty {
            Some(Ty::Fn(sig)) => sig,
            // Plan D Task 117 — calling a `Ty::Continuation` value
            // (i.e. an arm-bound `k` or a let-bound alias of one).
            // Synthesize a single-param FnSig from the continuation's
            // `op_ret` / `ret` so the rest of `check_call` (arity,
            // arg-type unify, ret deref) runs unchanged.
            //
            // Dynamic-extent enforcement is via E0145 only (broad
            // arm in unify_ty + bind_ty_var bypass closure below);
            // the synthetic `effects` / `effect_row_var` fields
            // exist solely for FnSig shape compatibility with
            // `subsume_row`. By constructing them with the call-
            // site row, the row check is structurally trivial — a
            // future reader removing the row fields wouldn't break
            // any safety contract, only the FnSig shape requirement
            // for `subsume_row`'s argument.
            Some(Ty::Continuation(c)) => Box::new(FnSig {
                params: vec![c.op_ret.clone()],
                ret: c.ret.clone(),
                effects: row.to_vec(),
                effect_row_var: self.lookup_active_row_var(),
            }),
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
        //
        // Plan D Task 117 — bind_ty_var bypass closure (review #3 at
        // HEAD `decb6d8`). Before unify_ty would silently bind a
        // freshly-instantiated generic var to `Ty::Continuation`, fire
        // E0145 explicitly. Without this gate, `id(k)` for generic
        // `fn id[A](x: A) -> A` propagates Continuation through the
        // (Var, other) bind_ty_var arm (the same arm `let f = k` uses
        // legitimately for local aliasing). Downstream USES are caught
        // by the broad arm in unify_ty, but the COMPILE PATH —
        // monomorphize cloning `id$$Continuation` and walking its body
        // via `rewrite_type_expr` → `ty_to_type_expr(Continuation)` —
        // panics with `unreachable!()` because Continuation has no
        // surface TypeExpr representation. Catching at the bind site
        // closes the panic surface AND surfaces a precise diagnostic.
        //
        // Discriminator vs `let f = k`: this gate fires only at the
        // call-site arg-unify against an instantiated generic param's
        // Var. The let-RHS path goes through the let-binding's own
        // unify (against a fresh let-var allocated outside check_call),
        // not through this loop, so let-aliasing stays untouched.
        for (i, (arg_ty, param_ty)) in arg_tys.iter().zip(sig.params.iter()).enumerate() {
            if let Some(at) = arg_ty {
                let arg_span = args.get(i).map(Expr::span).unwrap_or_else(|| span.clone());
                let resolved_at = self.deref(at);
                let resolved_param = self.deref(param_ty);
                if matches!(resolved_at, Ty::Continuation(_))
                    && matches!(resolved_param, Ty::Var(_))
                {
                    self.push_error(
                        "E0145",
                        arg_span.clone(),
                        "continuation `k` cannot escape its handle's arm body — \
                         passing `k` to a generic-typed parameter would propagate \
                         Continuation through the generic instantiation, escaping \
                         the arm body's dynamic extent. Keep `k` inside the \
                         handle's arm body (do not pass it to a function whose \
                         parameter type is a generic type variable)"
                            .to_string(),
                    );
                    continue;
                }
                if !self.unify_ty(param_ty, at, &arg_span) {
                    // unify_ty pushed the precise E0044 / E0145; nothing
                    // more to add here.
                }
            }
        }

        // (4) effect-row subsumption — Plan B task 48 (reviewer
        // follow-up): route through asymmetric `subsume_row` so the
        // *callee's* row var (came from instantiation) absorbs the
        // caller's leftover effects, while the *caller's* declared
        // row variable stays free for generalisation. Symmetric
        // `unify_row` here would silently bind the caller's row var
        // to whatever the callee left over, collapsing the caller's
        // declared row polymorphism.
        let caller_row = Row {
            effects: row.to_vec(),
            tail: self.lookup_active_row_var(),
        };
        let callee_row = Row {
            effects: sig.effects.clone(),
            tail: sig.effect_row_var,
        };
        self.subsume_row(&callee_row, &caller_row, &span);

        // (5) result type — derefed through the substitution so any
        // var bindings made by per-arg unification flow into the
        // returned type.
        let resolved_ret = self.deref(&sig.ret);

        // Plan B' Stage 6.8 Task 104 (R3 finding 4) — record the
        // resolved callee signature for indirect calls so Phase C+
        // codegen can resolve the call's signature via the side-table.
        // Skip direct calls — those resolve through `user_fn_refs` or
        // builtin handlers at codegen, no side-table entry needed.
        // Direct-call detection covers BOTH user-fn schemes
        // (`fn_schemes`) and seeded builtins (`fn_env`, e.g.
        // `int_to_string`); without the `fn_env` check, every
        // `int_to_string(n)` call would populate the side-table
        // wastefully. Resolution happens *after* arg-type unification
        // so generic-param `Ty::Var`s pick up their concrete
        // bindings.
        let is_direct_call = matches!(
            callee,
            Expr::Ident(name, _)
                if self.fn_schemes.contains_key(name) || self.fn_env.contains_key(name)
        );
        if !is_direct_call {
            let resolved_sig = FnSig {
                params: sig.params.iter().map(|p| self.deref(p)).collect(),
                ret: resolved_ret.clone(),
                effects: sig.effects.clone(),
                effect_row_var: sig.effect_row_var,
            };
            self.call_callee_tys
                .insert(span.clone(), Ty::Fn(Box::new(resolved_sig)));
        }

        Some(resolved_ret)
    }

    /// Helper: pick the single active row variable to thread
    /// through call-site row checks. Plan B task 48's surface
    /// admits at most one row variable per fn (`![ ... | e]`),
    /// enforced by `parse_effect_row`. We assert that invariant
    /// here so a future grammar change that allows multiple row
    /// vars trips loudly rather than silently picking one of them
    /// in `BTreeMap` sorted order.
    fn lookup_active_row_var(&self) -> Option<u32> {
        debug_assert!(
            self.current_row_var_subst.len() <= 1,
            "Plan B task 48: at most one row variable per fn (parser invariant); \
             grammar change that admits multiple needs a deliberate update here"
        );
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
        effects: &[EffectInst],
        body: &Expr,
        span: Span,
    ) -> Option<Ty> {
        // (1) build the signature up front so Ty::Fn is available for
        //     the lambda's own return type even if the body fails.
        //
        // Plan B task 48 (reviewer follow-up): lambdas inherit the
        // enclosing fn's `current_generic_subst`, so a lambda inside
        // `fn id[A](...)` can reference `A` in its param / return
        // types and have it resolve to the outer fn's `Ty::Var`. The
        // surface form does not yet allow lambdas to *declare* their
        // own generics (`(fn [B](x: B) ...)` would require parser
        // work in Stage 6+); this is the rank-1 ML choice. Row
        // variables on lambdas similarly inherit the active row-var
        // subst from the enclosing fn.
        let param_tys: Vec<Ty> = params
            .iter()
            .map(|p| self.ty_from_type_expr_here(&p.ty).unwrap_or(Ty::Unit))
            .collect();
        let ret_ty = self.ty_from_type_expr_here(return_type).unwrap_or(Ty::Unit);
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
                // Plan B' Stage 6.8 Phase B + C++ — deref the Ty
                // through the active substitution so any unresolved
                // `Ty::Var` (handler_overall_ty for k captured from
                // an arm body, or generic params resolved by
                // surrounding fn instantiation) is replaced with its
                // concrete binding before being recorded. Codegen's
                // `cranelift_ty_of_ty` rejects `Ty::Var(_)`, so
                // recording an unresolved var would surface as a
                // crash there rather than at the typecheck point.
                let ty = self.deref(&ty);
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

        // (5) restore and check return-type unification. Plan B
        //     task 48: route through `unify_ty` so an inferred body
        //     type that contains `Ty::Var`s (lambda inside a generic
        //     fn referencing outer `A`, future row-var positions)
        //     unifies cleanly rather than being rejected by raw
        //     `!=` against an unsubstituted ret_ty.
        self.env = saved_env;
        if let Some(bt) = body_ty {
            if !self.unify_ty(&ret_ty, &bt, &span) {
                // unify_ty already pushed the precise E0044; emit
                // the legacy E0069 alongside so existing diagnostics
                // continue to surface for plain lambda mismatches.
                self.push_error(
                    "E0069",
                    span.clone(),
                    format!(
                        "lambda body has type `{}` but the declared return type is `{}`",
                        ty_display(&self.deref(&bt)),
                        ty_display(&self.deref(&ret_ty)),
                    ),
                );
            }
        }

        Some(Ty::Fn(Box::new(sig)))
    }

    /// Plan B task 54 — full handler typing for `handle <body> with
    /// { return(v) => ..., Effect.op(p1, ..., k) => ..., ... }`.
    ///
    /// Walks the handler in three phases:
    ///
    /// 1. **Effect dispatch table.** Each op_arm names `Effect.op`; we
    ///    look the effect up in `self.effects`, look the operation up
    ///    in the effect's declared op list, and gather them into a
    ///    deduplicated `(effect, op) -> arm-info` index. Diagnostics
    ///    along the way: `E0138` (unknown effect), `E0139` (op not
    ///    declared on a known effect), `E0140` (two arms target the
    ///    same op). `E0141` fires when an arm's user-parameter count
    ///    disagrees with the operation's declared parameter count
    ///    (the trailing continuation `k` is implicit and always
    ///    present in the AST, so the comparison is `arm.params.len()
    ///    == op.params.len()`).
    ///
    /// 2. **Body walk.** The body is checked under a row that
    ///    includes the surrounding fn's row plus every discharged
    ///    effect (deduped). The handler-effect substitution stack is
    ///    pushed before the recursion so any `perform Effect.op(...)`
    ///    sites inside the body share the same fresh substitution
    ///    that the matching op-arm will install — this is what keeps
    ///    cross-arm / cross-perform generic-parameter instantiation
    ///    consistent inside a single handler.
    ///
    /// 3. **Arm walks + linearity check.** The handler's overall type
    ///    is allocated as a fresh type variable; the body's type
    ///    (or the return-arm's body type, when a `return` arm is
    ///    present) unifies into it. Each op-arm's bindings (user
    ///    params + the continuation `k`) are installed into a
    ///    snapshot/restore env scope; the arm body is checked under
    ///    the *caller's* row and unified against the handler's
    ///    overall type. Finally, one-shot linearity: arms for
    ///    effects declared **without** `resumes: many` walk a
    ///    syntactic-occurrence counter (`count_continuation_uses`)
    ///    that emits `E0220` when `k` is referenced more than once
    ///    along any path through the arm body, or anywhere inside a
    ///    lambda body (the conservative-capture rule).
    ///
    /// E0134 was lifted in Task 55 Phase 2 (`2d69b52`); well-formed
    /// handle expressions return the computed handler-overall type
    /// and flow into codegen. The codegen-entry guard
    /// `unsupported_handle_construct` rejects shapes still outside
    /// the supported subset (richer arm bodies, multi-effect, k
    /// usage, return arms — see Phase 4b–4f).
    fn check_handle(
        &mut self,
        body: &Expr,
        return_arm: Option<&HandleReturnArm>,
        op_arms: &[HandleOpArm],
        row: &[EffectInst],
        // Plan B Task 55 (Phase 4d): handle span used to key the
        // `handle_arm_captures` side-table populated below.
        handle_span: &Span,
    ) -> Option<Ty> {
        // Plan D Task 117 — allocate the handle's scope id once at
        // entry. Tags every `Ty::Continuation` bound for this
        // handle's arm `k` names, so unifying a `k` from this handle
        // with a `k` from a different handle fires E0145 (escape
        // barrier).
        let handle_scope_id = self.fresh_scope_id();

        // ---------- Phase 1: dispatch table ----------
        // Per-arm collected info that the arm walk consumes. Done
        // outside the arm loop so we see the full picture before
        // typing arm bodies (and so duplicates / arity mismatches
        // surface even when later analysis short-circuits).
        //
        // `have_op` distinguishes "effect+op resolved cleanly" (full
        // typing applies, linearity check runs) from "registry lookup
        // failed" (we still install Ty::Unit-fallback bindings + walk
        // the arm body so structural diagnostics inside the arm
        // surface alongside the registry diagnostic). The split keeps
        // Task 53's "walk all arm bodies" contract intact even when
        // E0138 / E0139 fire.
        struct ArmTyping {
            user_param_tys: Vec<Option<Ty>>,
            op_ret_ty: Option<Ty>,
            resumes_many: bool,
            have_op: bool,
        }
        let mut effect_substs: BTreeMap<String, BTreeMap<String, Ty>> = BTreeMap::new();
        let mut seen_arms: BTreeMap<(String, String), Span> = BTreeMap::new();
        let mut arm_typings: Vec<ArmTyping> = Vec::with_capacity(op_arms.len());
        let mut discharged: Vec<String> = Vec::new();
        for arm in op_arms {
            let mut user_param_tys: Vec<Option<Ty>> = Vec::new();
            let mut op_ret_ty: Option<Ty> = None;
            let mut resumes_many = false;
            let mut have_op = false;
            // Effect + op lookup (E0138 / E0139). Both diagnostics
            // run before the duplicate-arm check so a typo'd effect
            // surfaces both errors — the user should see the spelling
            // problem and the redundancy in one compile pass.
            match self.effects.get(&arm.effect).cloned() {
                None => {
                    self.push_error(
                        "E0138",
                        arm.effect_span.clone(),
                        format!(
                            "handler arm references unknown effect `{eff}` — declare it via `effect {eff} {{ ... }}`",
                            eff = arm.effect,
                        ),
                    );
                }
                Some(eff_decl) => {
                    resumes_many = eff_decl.resumes_many;
                    match eff_decl.ops.iter().find(|o| o.name == arm.op).cloned() {
                        None => {
                            self.push_error(
                                "E0139",
                                arm.op_span.clone(),
                                format!(
                                    "operation `{op}` is not declared on effect `{eff}`",
                                    op = arm.op,
                                    eff = arm.effect,
                                ),
                            );
                        }
                        Some(op) => {
                            have_op = true;
                            // Track discharged effect for body-row
                            // extension (dedupe by name; the seen_arms
                            // map below dedupes by (effect, op)).
                            if !discharged.contains(&arm.effect) {
                                discharged.push(arm.effect.clone());
                            }
                            // Allocate the effect's generic subst
                            // once across all arms in this handle and
                            // across `perform` sites in the body.
                            if !effect_substs.contains_key(&arm.effect) {
                                let (subst, _) = self.fresh_generic_subst(&eff_decl.generic_params);
                                effect_substs.insert(arm.effect.clone(), subst);
                            }
                            let eff_subst =
                                effect_substs.get(&arm.effect).cloned().unwrap_or_default();
                            // Compute op param tys + ret ty under the
                            // effect's subst. Save/restore so arm body
                            // checking (which runs with caller's
                            // subst) sees the surrounding fn's
                            // generics, not the effect's.
                            //
                            // Plan D Task 115 R1 — layer per-op generic
                            // params on top of the effect-decl subst
                            // for ops declared with their own generics
                            // (e.g. `fail[A]: (E) -> A`). Without this
                            // layer, `ty_from_type_expr_here` returns
                            // None for `A` references in op signatures
                            // and the user-param / k-arg types
                            // collapse to Ty::Unit, silently
                            // miscompiling arms over per-op-generic
                            // ops.
                            let mut arm_subst = eff_subst.clone();
                            let (op_subst, _) = self.fresh_generic_subst(&op.generic_params);
                            for (k, v) in op_subst {
                                arm_subst.insert(k, v);
                            }
                            let saved = std::mem::take(&mut self.current_generic_subst);
                            self.current_generic_subst = arm_subst;
                            user_param_tys = op
                                .params
                                .iter()
                                .map(|t| self.ty_from_type_expr_here(t))
                                .collect();
                            op_ret_ty = self.ty_from_type_expr_here(&op.return_type);
                            self.current_generic_subst = saved;
                            // Arity check between arm's user-binding
                            // count and op's declared param count.
                            // The continuation `k` is implicit and the
                            // parser guarantees it's always present,
                            // so we only compare user-side counts.
                            if arm.params.len() != op.params.len() {
                                self.push_error(
                                    "E0141",
                                    arm.span.clone(),
                                    format!(
                                        "handler arm for `{eff}.{op}` has {actual} parameter(s) before `{k}`, but `{op}` declares {expected}",
                                        eff = arm.effect,
                                        op = arm.op,
                                        k = arm.k_name,
                                        actual = arm.params.len(),
                                        expected = op.params.len(),
                                    ),
                                );
                            }
                        }
                    }
                }
            }
            // Duplicate-arm check (E0140). Always runs; even arms
            // whose effect+op didn't resolve still get duplicate-
            // checked so a typo that *coincidentally* matches an
            // earlier typo surfaces both diagnostics.
            let key = (arm.effect.clone(), arm.op.clone());
            match seen_arms.entry(key) {
                std::collections::btree_map::Entry::Vacant(e) => {
                    e.insert(arm.span.clone());
                }
                std::collections::btree_map::Entry::Occupied(_) => {
                    self.push_error(
                        "E0140",
                        arm.span.clone(),
                        format!(
                            "duplicate handler arm for `{eff}.{op}` — the first arm above wins; this one is unreachable",
                            eff = arm.effect,
                            op = arm.op,
                        ),
                    );
                }
            }
            arm_typings.push(ArmTyping {
                user_param_tys,
                op_ret_ty,
                resumes_many,
                have_op,
            });
        }

        // ---------- Phase 1.5: exhaustiveness check (E0142) ----------
        // Stage 6 cleanup (option 2 from `[DEVIATION Task 55] Phase 4f`):
        // a handle that lists any arm for `Effect` must cover every op
        // declared on `Effect`. The body's effect row treats `Effect`
        // as discharged, so any op NOT covered would runtime-abort if
        // performed. We surface this at compile time via E0142.
        //
        // Iterate `discharged` (the effects that have at least one
        // valid op-resolving arm — entries are added inside the
        // E0139-success branch above, so unknown-effect arms aren't
        // included). For each effect, check every declared op against
        // `seen_arms`'s `(effect, op)` keys. Missing op names are
        // collected and reported in alphabetical order (matches the
        // op_id assignment order; deterministic across runs).
        for eff_name in &discharged {
            let eff_decl = match self.effects.get(eff_name).cloned() {
                Some(d) => d,
                None => continue,
            };
            let mut missing: Vec<String> = Vec::new();
            for op in &eff_decl.ops {
                let key = (eff_name.clone(), op.name.clone());
                if !seen_arms.contains_key(&key) {
                    missing.push(op.name.clone());
                }
            }
            if !missing.is_empty() {
                // Anchor the diagnostic on the first arm targeting
                // this effect so the user sees where to add the
                // missing arms. seen_arms holds the first-arm span
                // per (effect, op) — pick the alphabetically-first
                // op's first-arm span as the anchor.
                let anchor_span = seen_arms
                    .iter()
                    .find(|((e, _), _)| e == eff_name)
                    .map(|(_, span)| span.clone())
                    .unwrap_or_else(|| handle_span.clone());
                let listed = if missing.len() == 1 {
                    format!("`{}.{}`", eff_name, missing[0])
                } else {
                    let parts: Vec<String> = missing
                        .iter()
                        .map(|op| format!("`{eff_name}.{op}`"))
                        .collect();
                    parts.join(", ")
                };
                self.push_error(
                    "E0142",
                    anchor_span,
                    format!(
                        "handler arms cover effect `{eff_name}` but do not exhaust its declared operations; \
                         missing arm(s) for {listed}. Add an arm for each unhandled op (use `=> k(default)` \
                         or `=> constant` for ops you don't expect to fire but need structurally present)",
                    ),
                );
            }
        }

        // ---------- Phase 2: body walk ----------
        // body_row = caller's literal effects ∪ discharged effects.
        // The contains check above already prevents duplicates from
        // appearing in the new tail entries; the surface row from
        // `parse_effect_row` is itself dedup'd by the parser, so no
        // post-loop sort/dedup is required. We don't thread a row
        // variable through this list either: the typechecker's per-
        // call row check is literal-membership only; the active row
        // variable on `self.current_row_var_subst` continues to
        // apply for downstream call-site subsumption.
        // Plan D Task 114 — body_row is a row over `EffectInst`,
        // not bare names. Discharged effects from `handle X.op =>
        // ...` enter the body row.
        //
        // Plan D Stage 12 R3 — when the discharged effect-decl is
        // generic (`effect Raise[E] { ... }`), use the active
        // handler subst (`effect_substs[name]`) to recover the
        // type-args at the discharge site. The handler arm's
        // op-typing code (lines 4347-4361 area) already allocated
        // these substs; reusing them here ensures the body row's
        // discharged `Raise[E_var]` matches body's expected
        // `Raise[E_body_var]` via subsume_row's arg unification.
        // Falls back to bare-name (`EffectInst::bare`) when no
        // generic_params declared — preserves the pre-Stage-12
        // behavior for non-generic effects (`IO`, `Mem`).
        let mut body_row: Vec<EffectInst> = row.to_vec();
        for e in &discharged {
            if !body_row.iter().any(|inst| inst.name == *e) {
                let args: Vec<Ty> = match (
                    self.effects.get(e).map(|d| d.generic_params.clone()),
                    effect_substs.get(e),
                ) {
                    (Some(gps), Some(subst)) if !gps.is_empty() => gps
                        .iter()
                        .map(|gp| subst.get(&gp.name).cloned().unwrap_or(Ty::Unit))
                        .collect(),
                    _ => Vec::new(),
                };
                body_row.push(EffectInst {
                    name: e.clone(),
                    args,
                });
            }
        }
        // Push handler scope for this body's `perform` sites.
        self.handler_scopes.push(HandlerScope {
            effect_substs: effect_substs.clone(),
        });
        let body_ty = self.check_expr(body, &body_row);
        self.handler_scopes.pop();

        // Plan B Stage 6 cleanup — populate the per-handle body type
        // side-table for the codegen pre-pass. Codegen reads this at
        // each `Expr::Handle` site to size the return-arm `v` binding's
        // Cranelift type correctly (Phase 4g shipped with `binding_ty
        // = I64` hardcoded, which produces verifier errors when the
        // body has a narrower type — Bool, Char — and the return arm
        // body uses `v` at narrow type). Resolves the `#[ignore]`'d
        // e2e `handle_with_bool_body_and_return_arm_uses_v_pending_-
        // proper_binding_ty`. Resolve through the substitution so the
        // recorded `Ty` is the post-inference concrete type, not a
        // raw type variable.
        if let Some(ref bt) = body_ty {
            self.handle_body_ty
                .insert(handle_span.clone(), self.deref(bt));
        }

        // ---------- Phase 3: handler-overall type + arm walks ----------
        // Allocate a fresh handler-overall var; cross-arm unification
        // collapses it to a concrete type whenever any branch pins
        // it. If the program is malformed enough that nothing pins
        // the var, `E0132` would fire upstream — but `handle` doesn't
        // have a generic-instantiation pending list, so an unpinned
        // overall type just stays as a `?N` in displays. Diagnostics
        // upstream will have fired before that ever surfaces.
        //
        // **Closure point (Plan B Task 55, Phase 4c):** the
        // unsolved-handler-overall edge case is **closed today** by
        // codegen, not typecheck — the codegen-entry guard
        // `unsupported_handle_construct` requires `IntLit`-only arm
        // bodies, which forces every arm to pin `handler_overall`
        // to `Int` in Phase 3 of this fn (the unification on every
        // arm's `Expr::IntLit` body solves the var). A program that
        // typechecks today and would otherwise leave
        // `handler_overall` unsolved (e.g.
        // `handle perform E.op() with { E.op(k) => k(0) }`) is
        // rejected at codegen entry by the IntLit-only check
        // before the `?N` could ever surface. Phase 4c lifts the
        // IntLit restriction in codegen; at that point this
        // typecheck path becomes reachable for non-IntLit arm
        // bodies and the choice is: pin via the body's perform-call
        // return-type constraint chain, or surface a "cannot infer
        // handler return type" diagnostic (E0132-style ambiguous
        // polymorphism). Tracked on the Phase 4c task list.
        let handler_overall_id = self.fresh_ty_var();
        let handler_overall = Ty::Var(handler_overall_id);

        // Body's type flows into the handler. With a `return` arm,
        // the body's value is bound to `v` and the return arm's body
        // is what unifies with the handler-overall; without a
        // `return` arm, the body's value flows directly through.
        match return_arm {
            Some(ra) => {
                // Bind v: body_ty (or a fresh var if body_ty is
                // unknown). The return arm body runs at caller's
                // row; install the binding into the env via a
                // snapshot/restore so it doesn't leak.
                let v_ty = body_ty
                    .clone()
                    .unwrap_or_else(|| Ty::Var(self.fresh_ty_var()));
                let saved_env = self.env.clone();

                // Plan B Task 55 (Phase 4g): collect this return arm
                // body's free-variable captures *before* installing
                // the `v` binding, so an `Ident` referencing `v`
                // doesn't get mis-classified as a capture. Mirrors
                // the Phase 4d op-arm capture collection at
                // `handle_arm_caps_accum.push(arm_captures)` below.
                // Names that resolve to top-level fns / ctors /
                // builtins (not in `saved_env`) pass through; codegen
                // resolves those via the user-fn / ctor / builtin
                // tables, not via closure env.
                let outer_names: std::collections::BTreeSet<String> =
                    saved_env.keys().cloned().collect();
                let mut binding_set: std::collections::BTreeSet<String> =
                    std::collections::BTreeSet::new();
                binding_set.insert(ra.binding.clone());
                let mut capture_names: Vec<String> = Vec::new();
                collect_free_vars(&ra.body, &outer_names, &binding_set, &mut capture_names);
                // Filter to names actually in `saved_env` (the
                // surrounding fn's lexical scope at the handle
                // expression). Mirrors the Phase 4d op-arm filter
                // (`capture_names.retain(|n| saved_env.contains_key(n))`).
                capture_names.retain(|n| saved_env.contains_key(n));
                let ra_captures: Vec<(String, Ty)> = capture_names
                    .into_iter()
                    .map(|name| {
                        let ty = saved_env.get(&name).cloned().unwrap_or_else(|| {
                            unreachable!(
                                "typecheck Phase 4g: retained return-arm capture \
                                 `{name}` must be in saved_env (filtered above by \
                                 `saved_env.contains_key`)"
                            )
                        });
                        (name, self.deref(&ty))
                    })
                    .collect();
                self.handle_return_arm_captures
                    .insert(handle_span.clone(), ra_captures);

                // Direct insert (not env_insert): the return-arm
                // binding is a fresh inner scope and may shadow an
                // outer name. The env_insert debug-assert exists for
                // the *function-level* no-shadowing contract.
                self.env.insert(ra.binding.clone(), v_ty);
                let ra_ty = self.check_expr(&ra.body, row);
                self.env = saved_env;
                if let Some(rt) = ra_ty {
                    let _ = self.unify_ty(&handler_overall, &rt, &ra.span);
                }
            }
            None => {
                if let Some(bt) = body_ty {
                    let _ = self.unify_ty(&handler_overall, &bt, &body.span());
                }
            }
        }

        // Plan B Task 55 (Phase 4d) — accumulator for per-arm
        // captures, keyed at the end of the loop into
        // `self.handle_arm_captures` under the handle's span. One
        // entry per arm in declaration order.
        let mut handle_arm_caps_accum: Vec<Vec<(String, Ty)>> = Vec::with_capacity(op_arms.len());

        // Op-arm walks. Every arm body is walked under bindings —
        // even arms whose registry lookup failed install Ty::Unit-
        // fallback bindings for their declared params + continuation
        // so structural diagnostics inside the arm body surface
        // alongside the registry diagnostic instead of cascading into
        // E0046s on the params/k references. The unify-with-overall
        // step and the one-shot linearity check are gated on
        // `have_op` (they only make sense once we know the op
        // signature).
        for (arm, typing) in op_arms.iter().zip(arm_typings.iter()) {
            let saved_env = self.env.clone();

            // Plan B Task 55 (Phase 4d): collect this arm body's
            // free-variable captures *before* installing arm
            // bindings, so an `Ident` referencing one of the arm's
            // own user-params or `k_name` doesn't get mis-classified
            // as a capture. Each captured name's `Ty` is looked up in
            // the saved env (the surrounding fn's lexical scope at
            // the handle expression). Names that resolve to top-level
            // fns / ctors / builtins (i.e., names not in `saved_env`)
            // pass through `collect_free_vars` because its
            // `outer_names` filter excludes them — codegen resolves
            // those via the user-fn / ctor / builtin tables, not via
            // closure env. Empty captures vec means the arm has no
            // outer-scope references; codegen will pass `null` as the
            // arm's `closure_ptr` (no allocation needed).
            let outer_names: std::collections::BTreeSet<String> =
                saved_env.keys().cloned().collect();
            let mut arm_param_set: std::collections::BTreeSet<String> =
                arm.params.iter().map(|p| p.name.clone()).collect();
            arm_param_set.insert(arm.k_name.clone());
            let mut capture_names: Vec<String> = Vec::new();
            collect_free_vars(&arm.body, &outer_names, &arm_param_set, &mut capture_names);
            // `collect_free_vars`'s `Expr::Lambda` arm widens
            // `outer_names` to include the *enclosing* `param_names`
            // (transitive-capture analysis: a nested lambda treats
            // the arm's params + `k` as visible-from-above and may
            // record them as captures). Filter those out here:
            // legitimate arm captures are exactly the names in
            // `saved_env` (the surrounding fn's lexical scope at
            // the handle expression). Names not in `saved_env` —
            // arm params, `k_name`, top-level fn names that were
            // never in the local env — are not captures.
            //
            // This is the test surface for
            // `linearity_lambda_capturing_k_is_e0220`: a lambda
            // inside an arm body that calls `k(0)` must produce a
            // clean `E0220` from the existing linearity check
            // (`count_continuation_uses` saturates to 2 on lambda
            // capture); the Phase 4d capture collection must NOT
            // record `k` as a capture and crash with `unreachable!`
            // before the linearity check has a chance to surface.
            capture_names.retain(|n| saved_env.contains_key(n));
            let arm_captures: Vec<(String, Ty)> = capture_names
                .into_iter()
                .map(|name| {
                    let ty = saved_env.get(&name).cloned().unwrap_or_else(|| {
                        unreachable!(
                            "typecheck Phase 4d: retained capture `{name}` must be \
                             in saved_env (filtered above by `saved_env.contains_key`)"
                        )
                    });
                    // Resolve type-vars through the current substitution
                    // so the side-table records a substitution-stable
                    // `Ty` (codegen-time `slot_kind_for_ty` requires
                    // resolved types to derive the GC bitmap).
                    (name, self.deref(&ty))
                })
                .collect();
            handle_arm_caps_accum.push(arm_captures);

            // Install user-param bindings; pad short user_param_tys
            // (registry lookup failed, or arity mismatched low) with
            // Ty::Unit so every declared name is bound.
            for (i, p) in arm.params.iter().enumerate() {
                let pty = typing
                    .user_param_tys
                    .get(i)
                    .and_then(|x| x.clone())
                    .unwrap_or(Ty::Unit);
                self.env.insert(p.name.clone(), pty);
            }
            // Continuation `k`: Plan D Task 117 — typed as
            // `Ty::Continuation { op_ret, ret: handler_overall,
            // scope_id }` rather than `Ty::Fn(...)`. The
            // Continuation type carries no effect row: `k` resumes
            // the surrounding fn's computation in place, so its
            // effects are exactly the caller's row by construction;
            // call sites that read `k`'s type fabricate the row at
            // use time. The fresh `handle_scope_id` (allocated at
            // the top of `check_handle`) tags the binding so
            // unifying a `k` from this handle with a `k` from a
            // different handle fires E0145.
            //
            // When op_ret_ty couldn't be resolved (E0138 / E0139
            // path), default to Ty::Unit; the binding still lets
            // references to `k` inside the arm body resolve without
            // firing spurious E0046.
            let k_param_ty = typing.op_ret_ty.clone().unwrap_or(Ty::Unit);
            let k_ty = Ty::Continuation(Box::new(ContinuationTy {
                op_ret: k_param_ty,
                ret: handler_overall.clone(),
                scope_id: ScopeId::Concrete(handle_scope_id),
            }));
            self.env.insert(arm.k_name.clone(), k_ty);
            // Plan D Task 117 (continuation-surface) — set the
            // current arm's scope_id so user-written
            // `Continuation[op_ret, ret]` annotations inside this
            // arm body resolve to a Continuation tagged with the
            // enclosing handle's scope. Saved/restored so nested
            // arm bodies (e.g., a handle inside this arm body)
            // inherit their own innermost scope_id and restore the
            // outer scope on exit.
            let saved_arm_scope_id = self.current_arm_scope_id;
            self.current_arm_scope_id = Some(handle_scope_id);
            // Arm body runs at caller's row (the discharged effect is
            // *not* in scope here — we are servicing it).
            let arm_ty = self.check_expr(&arm.body, row);
            self.current_arm_scope_id = saved_arm_scope_id;
            self.env = saved_env;
            // Unify arm body type with handler-overall only when the
            // op resolved cleanly; an arm whose registry lookup
            // failed has nothing meaningful to unify against (and
            // forcing the unification can produce cascade errors
            // against the fresh handler-overall var).
            if typing.have_op {
                if let Some(at) = arm_ty {
                    let _ = self.unify_ty(&handler_overall, &at, &arm.body.span());
                }
                // One-shot linearity: only meaningful for cleanly-
                // resolved arms. Multi-shot effects skip the check.
                if !typing.resumes_many {
                    let uses = count_continuation_uses(&arm.body, &arm.k_name);
                    if uses > 1 {
                        self.push_error(
                            "E0220",
                            arm.body.span(),
                            format!(
                                "one-shot continuation `{k}` used more than once along a path in this arm — `{eff}` is one-shot (default); declare `effect {eff} resumes: many {{ ... }}` to opt in to multi-shot semantics",
                                k = arm.k_name,
                                eff = arm.effect,
                            ),
                        );
                    }
                }
            }
        }

        // Plan B Task 55 (Phase 2 → 3b → 4a) — both staged gates
        // (`E0133`, `E0134`) lifted. Codegen lowers `handle BODY with
        // { arms }` through the runtime handler-frame ABI plus
        // synthetic CPS arm fns; the codegen-entry guard
        // `unsupported_handle_construct` rejects shapes still outside
        // the supported subset (richer arm bodies, multi-effect, k
        // usage, return arms, etc. — see Phase 4b–4g). Arm-body
        // diagnostics (E0046, E0220, E0044) and registry diagnostics
        // (E0138, E0139, E0140, E0141) emitted above continue to
        // surface unchanged.

        // Plan B Task 55 (Phase 4d): commit the per-arm captures
        // accumulator to the side-table, keyed by handle span.
        // Codegen reads this at each `Expr::Handle` site to build the
        // per-arm closure record (env_exprs sourced from the
        // surrounding fn's env at that capture's `Ty`).
        self.handle_arm_captures
            .insert(handle_span.clone(), handle_arm_caps_accum);

        Some(self.deref(&handler_overall))
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
        row: &[EffectInst],
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
                (Some(first), Some(t)) => {
                    // Plan B Task 51: cross-arm body-type consistency
                    // is a unification check, not a structural-equality
                    // check. Generic-fn-internal matches (e.g.
                    // `fn map[A](xs: List[A]) -> List[A] { match xs {
                    // Nil => Nil, Cons(h, t) => Cons(h, map(t)), } }`)
                    // produce arm body types containing fresh type
                    // variables — `Nil` resolves to `List[?6]` and
                    // `Cons(...)` to `List[?5]` via two distinct calls
                    // to `fresh_user_instance_with_subst`. Equality
                    // would reject `?6 != ?5` even though they unify.
                    //
                    // Snapshot the error list around `unify_ty`. On
                    // failure, `unify_ty` may push two error kinds at
                    // internal recursion sites: generic E0044 "type
                    // mismatch" (which we replace with arm-specific
                    // E0065) and E0126 occurs-check / E0127 row-occurs
                    // (which name a real soundness problem the
                    // generic E0065 wouldn't capture — preserve those).
                    // Drain new errors past baseline, keep occurs-
                    // check kinds, drop E0044, emit E0065 below.
                    //
                    // Subst is intentionally NOT rolled back on
                    // failure: in HM, partial bindings made during a
                    // failed compound unify (e.g. `?A := String`
                    // succeeds before `Int vs String` fails) are
                    // semantically correct constraints on the body's
                    // generic params; they surface at the function's
                    // call sites via the normal E0044/E0132 cascade,
                    // which is the diagnostic path we want. See
                    // `subst_pollution_from_partial_unify_surfaces_at_call_site`
                    // for the regression guard.
                    let pre_unify_errors = self.errors.len();
                    let unified = self.unify_ty(first, t, &arm.span);
                    if !unified {
                        let preserved: Vec<CompilerError> = self
                            .errors
                            .drain(pre_unify_errors..)
                            .filter(|e| e.code.as_str() != "E0044")
                            .collect();
                        self.errors.extend(preserved);
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
            // Plan D Task 113 — tuple scrutinee. A Pattern::Tuple of the
            // matching arity is the only constructor; if any of the
            // patterns is a wildcard / Var catchall, the
            // pattern_is_catchall guard above already returned None.
            // Otherwise the tuple has no other constructors, so we
            // descend element-wise to find the first un-covered element
            // and synthesize a witness with `_` placeholders elsewhere.
            Ty::Tuple(elem_tys) => {
                let tuple_rows: Vec<&Vec<Pattern>> = patterns
                    .iter()
                    .filter_map(|p| match p {
                        Pattern::Tuple(pats, _) if pats.len() == elem_tys.len() => Some(pats),
                        _ => None,
                    })
                    .collect();
                if tuple_rows.is_empty() {
                    let placeholders = vec!["_"; elem_tys.len()].join(", ");
                    return Some(format!("({placeholders})"));
                }
                for (elem_idx, elem_ty) in elem_tys.iter().enumerate() {
                    let elem_patterns: Vec<&Pattern> =
                        tuple_rows.iter().map(|row| &row[elem_idx]).collect();
                    if let Some(sub) = self.match_witness(elem_ty, &elem_patterns) {
                        let parts: Vec<String> = (0..elem_tys.len())
                            .map(|i| {
                                if i == elem_idx {
                                    sub.clone()
                                } else {
                                    "_".to_string()
                                }
                            })
                            .collect();
                        return Some(format!("({})", parts.join(", ")));
                    }
                }
                None
            }
            // Plan D Task 117 — Continuation scrutinees have no
            // user-constructible value form (no surface syntax
            // produces a Continuation literal); user code can only
            // bind `k` and pass it through let-aliases. A `match k`
            // expression has no destructuring rules and the only
            // pattern shape that would typecheck is `Pattern::Var`
            // (catchall, handled at the top of this fn). Any
            // structured pattern would have failed typecheck E0066
            // upstream. Return None (treat as exhaustive once a
            // catchall is present; otherwise the caller surfaces
            // the empty-witness case).
            Ty::Continuation(_) => None,
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
            // Plan D Task 113 — a tuple pattern is catchall iff every
            // sub-pattern is catchall against the corresponding tuple
            // element type. Arity must match the scrut_ty's arity.
            Pattern::Tuple(pats, _) => {
                if let Ty::Tuple(elem_tys) = scrut_ty {
                    if pats.len() == elem_tys.len() {
                        return pats
                            .iter()
                            .zip(elem_tys.iter())
                            .all(|(sub, ety)| self.pattern_is_catchall(sub, ety));
                    }
                }
                false
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
                // Plan D Task 113 — tuple pattern matches a Ty::Tuple
                // scrutinee element-wise. Arity must match; element
                // types unify position-by-position. Pattern arity ≥ 2
                // is the parser's invariant (single-elem parens are
                // paren-grouping, zero-elem rejected).
                let resolved = self.deref(scrut_ty);
                match resolved {
                    Ty::Tuple(elem_tys) => {
                        if pats.len() != elem_tys.len() {
                            self.push_error(
                                "E0117",
                                span.clone(),
                                format!(
                                    "tuple pattern with {} elements does not match scrutinee type `{}`",
                                    pats.len(),
                                    ty_display(scrut_ty)
                                ),
                            );
                            // Still recurse with the scrutinee type so
                            // inner shape errors surface.
                            for sub in pats {
                                self.check_pattern(sub, scrut_ty, bindings);
                            }
                        } else {
                            for (sub, elem_ty) in pats.iter().zip(elem_tys.iter()) {
                                self.check_pattern(sub, elem_ty, bindings);
                            }
                        }
                    }
                    _ => {
                        self.push_error(
                            "E0117",
                            span.clone(),
                            format!(
                                "tuple pattern does not match scrutinee type `{}` (expected a tuple type)",
                                ty_display(scrut_ty)
                            ),
                        );
                        for sub in pats {
                            self.check_pattern(sub, scrut_ty, bindings);
                        }
                    }
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
                // Plan B task 48 (reviewer follow-up): scrutinee
                // type args must propagate into ctor field type
                // resolution. For `match xs: List[Int] { Cons(h,
                // _) => ... }`, h's type comes from List's `A`
                // field at instantiation `[Int]`, so we extend
                // `current_generic_subst` with the type's generic
                // parameters bound to the scrutinee's args. Without
                // this, `ty_from_type_expr_here` falls through to
                // `unwrap_or(Ty::Unit)` and h types as Unit.
                let scrut_args: Vec<Ty> = match scrut_ty {
                    Ty::User(_, args) => args.clone(),
                    _ => Vec::new(),
                };
                let saved_generic_subst = self.current_generic_subst.clone();
                if td.generic_params.len() == scrut_args.len() {
                    for (gp, arg) in td.generic_params.iter().zip(scrut_args.iter()) {
                        self.current_generic_subst
                            .insert(gp.name.clone(), arg.clone());
                    }
                }
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
                self.current_generic_subst = saved_generic_subst;
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

fn type_name(t: &TypeExpr) -> String {
    // Plan D Task 113 — tuple types render in surface form
    // `(T1, T2, ...)` so error messages stay readable for the new
    // type surface. Other variants delegate to head_name (a stable
    // identifier-shaped string suitable for short error formats).
    match t {
        TypeExpr::Tuple { elems, .. } => {
            let parts: Vec<String> = elems.iter().map(type_name).collect();
            format!("({})", parts.join(", "))
        }
        TypeExpr::Fn(fty) => {
            let params: Vec<String> = fty.params.iter().map(type_name).collect();
            let ret = type_name(&fty.ret);
            format!("({}) -> {}", params.join(", "), ret)
        }
        TypeExpr::Apply { name, args, .. } => {
            let argstr: Vec<String> = args.iter().map(type_name).collect();
            format!("{}[{}]", name, argstr.join(", "))
        }
        TypeExpr::Named(n, _) => n.clone(),
    }
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
    let empty_rows: BTreeMap<String, u32> = BTreeMap::new();
    // External callers (non-Tc walks like builtin scheme registration)
    // never appear inside a handler arm body, so `arm_scope_id` is
    // None — any user `Continuation[op_ret, ret]` annotation reached
    // via these paths returns None, and `check_type_expr_known`
    // surfaces E0145 separately at the use site.
    ty_from_type_expr_with_rows(t, types, generic_subst, &empty_rows, None)
}

/// Plan D Task 116 — variant of `ty_from_type_expr` that threads a
/// row-variable substitution through. The row-var subst maps a
/// surface name (`e`) to its allocated id. Used by
/// `Tc::ty_from_type_expr_here` to resolve inner fn-type row-var
/// references against the enclosing fn's `current_row_var_subst`.
/// External callers (monomorphize / non-Tc walks) call the wrapper
/// `ty_from_type_expr` with an empty row-var map, falling back to
/// `effect_row_var: None` — those paths walk over already-resolved
/// or row-var-free shapes (the codegen-entry guard rejects any IR
/// still carrying surface row-var refs after monomorphize).
pub(crate) fn ty_from_type_expr_with_rows(
    t: &TypeExpr,
    types: &BTreeMap<String, TypeDecl>,
    generic_subst: &BTreeMap<String, Ty>,
    row_var_subst: &BTreeMap<String, u32>,
    // Plan D Task 117 (continuation-surface) — innermost enclosing
    // handler arm body's scope_id, threaded from
    // `Tc::ty_from_type_expr_here` via `Tc.current_arm_scope_id`.
    // When `Some(N)`, user-written `Continuation[op_ret, ret]`
    // annotations resolve to `Ty::Continuation { ..., scope_id:
    // Concrete(N) }`. When `None`, returns `None` for the
    // Continuation case — `check_type_expr_known` is the
    // authoritative diagnostic site (pushes E0145 with the
    // "Continuation annotations are only valid inside a handler
    // arm body" message).
    arm_scope_id: Option<u32>,
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
                        // Generic type used without arguments — the
                        // caller's diagnostic (the E0112 fall-through
                        // in `check_type_expr_known`) reports it.
                        None
                    }
                } else {
                    None
                }
            }
        },
        TypeExpr::Apply { name, args, .. } => {
            // Plan D Task 117 (continuation-surface) — `Continuation[
            // op_ret, ret]` is the surface form for k's binding type.
            // Type-position only (no value-position constructor —
            // `check_handle` stays the sole producer of Ty::Continuation
            // at the value level). scope_id is inferred from the
            // innermost enclosing handler arm body via the threaded
            // `arm_scope_id` param. When `arm_scope_id` is None
            // (annotation appears outside any arm body), return None;
            // `check_type_expr_known` is the diagnostic site for the
            // E0145 ("Continuation annotations are only valid inside a
            // handler arm body") message.
            if name == "Continuation" {
                if args.len() != 2 {
                    return None;
                }
                let scope_id = arm_scope_id?;
                let op_ret = ty_from_type_expr_with_rows(
                    &args[0],
                    types,
                    generic_subst,
                    row_var_subst,
                    arm_scope_id,
                )?;
                let ret = ty_from_type_expr_with_rows(
                    &args[1],
                    types,
                    generic_subst,
                    row_var_subst,
                    arm_scope_id,
                )?;
                return Some(Ty::Continuation(Box::new(ContinuationTy {
                    op_ret,
                    ret,
                    scope_id: ScopeId::Concrete(scope_id),
                })));
            }
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
                // Plan D Task 116 R1 — propagate row_var_subst into
                // Apply args so a row-var-bearing fn-type nested
                // inside a User type (`Box[() -> Int ![IO | r]]`)
                // doesn't silently lose the row-var binding.
                resolved_args.push(ty_from_type_expr_with_rows(
                    a,
                    types,
                    generic_subst,
                    row_var_subst,
                    arm_scope_id,
                )?);
            }
            Some(Ty::User(name.to_string(), resolved_args))
        }
        // Plan B' Stage 6.8 Task 103 — first-class function type
        // surface maps to Ty::Fn under HM.
        //
        // Plan D Task 116 — row variables in inner fn-type rows
        // (`(A) -> B ![ ... | r ]`) resolve through the empty
        // row-var subst at this `&` context (used by the
        // monomorphize-time / pre-pass walk where row-var
        // bindings are not in scope). Tc-method `ty_from_type_-
        // expr_here` calls `ty_from_type_expr_with_rows` instead,
        // passing `self.current_row_var_subst`. Inner row-vars
        // here either resolve via the supplied row-var subst or
        // fall back to `effect_row_var: None` — `check_type_-
        // expr_known`'s E0137 walk has already pushed a precise
        // diagnostic for unbound row-var names, so a `None` here
        // is a recovery shape, not a silent demotion.
        TypeExpr::Fn(fty) => {
            let mut params = Vec::with_capacity(fty.params.len());
            for p in &fty.params {
                params.push(ty_from_type_expr_with_rows(
                    p,
                    types,
                    generic_subst,
                    row_var_subst,
                    arm_scope_id,
                )?);
            }
            let ret = ty_from_type_expr_with_rows(
                &fty.ret,
                types,
                generic_subst,
                row_var_subst,
                arm_scope_id,
            )?;
            // Plan D Task 116 — resolve the inner fn-type's
            // `effect_row_var` (if any) by name through the supplied
            // row-var subst. None when no row-var subst entry exists
            // for the name (the row-var was unbound; E0137 already
            // surfaced it at `check_type_expr_known` time).
            let effect_row_var = fty
                .effect_row_var
                .as_ref()
                .and_then(|rv| row_var_subst.get(&rv.name).copied());
            Some(Ty::Fn(Box::new(FnSig {
                params,
                ret,
                effects: effect_refs_to_insts(&fty.effects, types, generic_subst),
                effect_row_var,
            })))
        }
        // Plan D Task 113 — TypeExpr::Tuple maps element-wise to Ty::Tuple.
        TypeExpr::Tuple { elems, .. } => {
            let mut elem_tys = Vec::with_capacity(elems.len());
            for e in elems {
                // Plan D Task 116 R1 — propagate row_var_subst into
                // tuple elements so a row-var-bearing fn-type nested
                // inside a tuple (`(Int, () -> Int ![IO | r])`)
                // doesn't silently lose the row-var binding.
                elem_tys.push(ty_from_type_expr_with_rows(
                    e,
                    types,
                    generic_subst,
                    row_var_subst,
                    arm_scope_id,
                )?);
            }
            Some(Ty::Tuple(elem_tys))
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
            let effects = effects_display(&sig.effects);
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
        Ty::Tuple(elems) => {
            let parts: Vec<String> = elems.iter().map(ty_display).collect();
            format!("({})", parts.join(", "))
        }
        Ty::Var(id) => format!("?{id}"),
        // Plan D Task 117 — render `Continuation` for diagnostics.
        // No user-source surface produces this type. The scope_id
        // is intentionally NOT rendered here: users have no mental
        // model for "scope N" and can't remediate by writing the
        // type they should have written. E0145 messages where the
        // scope_id is load-bearing (cross-handle escape) include
        // the numbers explicitly. Other diagnostics surface this
        // type as just `Continuation(op_ret) -> ret`.
        Ty::Continuation(c) => {
            format!(
                "Continuation({}) -> {}",
                ty_display(&c.op_ret),
                ty_display(&c.ret)
            )
        }
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
        // Plan B Task 57 — codegen-internal variants surface in
        // diagnostics only via post-elaborate IR walks; the surface
        // syntax is the same as their pre-elaborate counterparts.
        BinOp::SdivUnchecked => "/",
        BinOp::SremUnchecked => "%",
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

// ----------------------------------------------------------------
// Plan D Task 117 (continuation-surface) — let-bound k desugar.
//
// User-written `let f: Continuation[op_ret, ret] = k; ... f ...`
// is rewritten to `... k ...` so downstream codegen sees the
// existing supported "k as direct callee" arm-body shapes (Slice C
// 2-let multi-shot or basic single-shot tail-k(arg)). The let-stmt
// is elided; subsequent occurrences of `f` are renamed to `k_name`.
//
// Restrictions (v1):
//   - The let-stmt must appear at the top level of the arm body's
//     `Expr::Block`. Nested let-bound k (inside if/match/lambda
//     branches) is not desugared — typecheck still accepts the
//     shape (E0145 doesn't fire), but downstream codegen will
//     reject the surviving `Expr::Ident(k_name)` via
//     `arm_body_walk`.
//   - Subsequent shadowing of the let-binding name is NOT tracked
//     by the substitution (e.g., `let f: Cont = k; let f: Int = 0;
//     f` would substitute `f → k` in the inner-let's RHS and tail
//     incorrectly). For v1, this is documented as undefined and
//     not exercised by tests.
// ----------------------------------------------------------------

fn desugar_let_bound_continuations(program: &mut Program) {
    for item in &mut program.items {
        match item {
            Item::Fn(f) => desugar_block_handles(&mut f.body),
            // Plan D Task 117 (continuation-surface, PR #62
            // followup): only `Item::Fn` carries an `Expr::Handle`
            // today (handles only appear inside fn bodies). If a
            // future top-level Item kind (e.g., a `const` carrying
            // a computed handle expression) is added, this match
            // needs to extend. Other current Items (Type, Effect,
            // Import) carry no Expr at all.
            Item::Type(_) | Item::Effect(_) | Item::Import(_) => {}
        }
    }
}

/// Walk a `Block`, descending into every `Expr::Handle` to apply
/// `desugar_arm_body` to each op-arm's body. Other Expr shapes
/// (binop / call / match / etc.) are recursed into so nested
/// handles deep inside an outer scope still get processed.
fn desugar_block_handles(b: &mut Block) {
    for s in &mut b.stmts {
        match s {
            Stmt::Let(l) => desugar_expr_handles(&mut l.value),
            Stmt::Expr(e) => desugar_expr_handles(e),
            Stmt::Perform(p) => {
                for a in &mut p.args {
                    desugar_expr_handles(a);
                }
            }
        }
    }
    if let Some(t) = &mut b.tail {
        desugar_expr_handles(t);
    }
}

fn desugar_expr_handles(e: &mut Expr) {
    match e {
        Expr::IntLit(_, _)
        | Expr::BoolLit(_, _)
        | Expr::CharLit(_, _)
        | Expr::StringLit(_, _)
        | Expr::Ident(_, _)
        | Expr::ClosureEnvLoad { .. } => {}
        Expr::Call { callee, args, .. } => {
            desugar_expr_handles(callee);
            for a in args {
                desugar_expr_handles(a);
            }
        }
        Expr::Perform(p) => {
            for a in &mut p.args {
                desugar_expr_handles(a);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            desugar_expr_handles(lhs);
            desugar_expr_handles(rhs);
        }
        Expr::Unary { operand, .. } => desugar_expr_handles(operand),
        Expr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            desugar_expr_handles(cond);
            desugar_block_handles(then_block);
            desugar_block_handles(else_block);
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            desugar_expr_handles(scrutinee);
            for arm in arms {
                desugar_expr_handles(&mut arm.body);
            }
        }
        Expr::Block(b) => desugar_block_handles(b),
        Expr::Lambda { body, .. } => desugar_expr_handles(body),
        Expr::ClosureRecord { env_exprs, .. } => {
            for ee in env_exprs {
                desugar_expr_handles(ee);
            }
        }
        Expr::RecordLit { fields, .. } => {
            for f in fields {
                desugar_expr_handles(&mut f.value);
            }
        }
        Expr::Tuple { elems, .. } => {
            for el in elems {
                desugar_expr_handles(el);
            }
        }
        Expr::Handle {
            body,
            return_arm,
            op_arms,
            ..
        } => {
            // Recurse into the handle's body first (nested
            // handles in the body get their own desugaring at the
            // appropriate scope).
            desugar_expr_handles(body);
            // Per-arm: apply the let-bound k desugar.
            for arm in op_arms.iter_mut() {
                desugar_arm_body(&mut arm.body, &arm.k_name);
                desugar_expr_handles(&mut arm.body);
            }
            if let Some(ra) = return_arm {
                desugar_expr_handles(&mut ra.body);
            }
        }
    }
}

/// Apply the let-bound k desugar to a single arm body. Only the
/// top-level `Expr::Block` shape is handled; other arm-body shapes
/// (e.g. direct `Expr::Ident(k_name)` or `Expr::Call { callee:
/// Ident(k_name), .. }`) don't carry let-bound k aliases and pass
/// through unchanged.
///
/// Block-level scope tracking: the substitution accumulated within
/// this block invalidates entries when subsequent lets shadow
/// either the alias name (the substitution's key) or the original
/// k_name (the substitution's value). See `apply_subst_to_block`
/// for the per-stmt narrowing.
fn desugar_arm_body(body: &mut Expr, k_name: &str) {
    let Expr::Block(block) = body else {
        return;
    };
    // PR #62 followup case (d): if any top-level stmt of this block
    // shadows the arm's `k_name` (e.g. `let k: Int = 99`), refuse all
    // elision. Otherwise the substitution `f → k` would point at the
    // shadowed binding for any uses of `f` past the shadow point —
    // and dropping the subst at the shadow would leave `f`
    // references undefined since the let-stmt that bound it was
    // elided. The conservative "no elision in shadowed blocks"
    // policy preserves the original AST; the codegen-walker reject
    // surfaces the unsupported shape with the precise v1-restriction
    // message. Users can rename the local.
    let k_shadowed_at_top = block
        .stmts
        .iter()
        .any(|s| matches!(s, Stmt::Let(l) if l.name == k_name));
    if k_shadowed_at_top {
        return;
    }
    let mut subst: BTreeMap<String, String> = BTreeMap::new();
    let mut new_stmts: Vec<Stmt> = Vec::with_capacity(block.stmts.len());
    let raw_stmts = std::mem::take(&mut block.stmts);
    for s in raw_stmts {
        match s {
            Stmt::Let(l) if is_let_bound_continuation(&l, k_name) && !subst.is_empty() => {
                // Edge case: nested `let f: Continuation = k; let f:
                // Continuation = k` — the second let collapses into
                // the same alias. We just keep the latest entry
                // (semantically equivalent; both alias k).
                subst.insert(l.name, k_name.to_string());
            }
            Stmt::Let(l) if is_let_bound_continuation(&l, k_name) => {
                // Elide the let-stmt; record the substitution.
                subst.insert(l.name, k_name.to_string());
            }
            mut s => {
                if !subst.is_empty() {
                    apply_subst_to_stmt(&mut s, &subst);
                }
                // Per-stmt scope narrowing: if this stmt is a Let
                // whose binding name shadows an alias (key) or the
                // alias's target (value=k_name), drop the affected
                // entries before processing subsequent stmts.
                if let Stmt::Let(l) = &s {
                    subst.remove(&l.name);
                    subst.retain(|_, v| v != &l.name);
                }
                new_stmts.push(s);
            }
        }
    }
    if !subst.is_empty() {
        if let Some(t) = &mut block.tail {
            apply_subst_to_expr(t, &subst);
        }
    }
    block.stmts = new_stmts;
}

fn is_let_bound_continuation(l: &LetStmt, k_name: &str) -> bool {
    let ty_is_continuation = matches!(
        &l.ty,
        TypeExpr::Apply { name, args, .. } if name == "Continuation" && args.len() == 2
    );
    let value_is_k = matches!(&l.value, Expr::Ident(n, _) if n == k_name);
    ty_is_continuation && value_is_k
}

fn apply_subst_to_stmt(s: &mut Stmt, subst: &BTreeMap<String, String>) {
    match s {
        Stmt::Let(l) => apply_subst_to_expr(&mut l.value, subst),
        Stmt::Expr(e) => apply_subst_to_expr(e, subst),
        Stmt::Perform(p) => {
            for a in &mut p.args {
                apply_subst_to_expr(a, subst);
            }
        }
    }
}

/// Apply the substitution to an expression. Span on the rewritten
/// `Expr::Ident` is intentionally preserved — downstream side-tables
/// (handle_body_ty, capture_info, call_callee_tys, etc.) are
/// span-keyed and must continue resolving to the original alias-
/// reference's source location.
///
/// Scope discipline (PR #62 followup): every binder narrows the
/// active substitution before recursing. Specifically:
/// - `Expr::Lambda { params, body }` — drop subst entries whose
///   key (alias name) OR value (target k_name) matches a lambda
///   param. Without this, `let f = k; (fn (f: Int) => f + 1)(0)`
///   would silently rewrite the lambda body's `f` to `k`.
/// - `Expr::Match` — per arm, drop entries shadowed by the arm's
///   pattern bindings. Without this, `let f = k; match m { Some(f)
///   => f }` would rewrite the match-arm `f` to `k`.
/// - `Expr::Handle` — per op-arm, drop entries shadowed by the
///   arm's params or k_name (so an inner handle whose arm reuses
///   the outer alias name as its own k_name is unaffected).
/// - `Expr::Block` — let-stmt-by-let-stmt narrowing in
///   `apply_subst_to_block` handles the in-block shadowing case.
fn apply_subst_to_expr(e: &mut Expr, subst: &BTreeMap<String, String>) {
    match e {
        Expr::Ident(name, _) => {
            if let Some(target) = subst.get(name) {
                *name = target.clone();
            }
        }
        Expr::IntLit(_, _)
        | Expr::BoolLit(_, _)
        | Expr::CharLit(_, _)
        | Expr::StringLit(_, _)
        | Expr::ClosureEnvLoad { .. } => {}
        Expr::Call { callee, args, .. } => {
            apply_subst_to_expr(callee, subst);
            for a in args {
                apply_subst_to_expr(a, subst);
            }
        }
        Expr::Perform(p) => {
            for a in &mut p.args {
                apply_subst_to_expr(a, subst);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            apply_subst_to_expr(lhs, subst);
            apply_subst_to_expr(rhs, subst);
        }
        Expr::Unary { operand, .. } => apply_subst_to_expr(operand, subst),
        Expr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            apply_subst_to_expr(cond, subst);
            apply_subst_to_block(then_block, subst);
            apply_subst_to_block(else_block, subst);
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            apply_subst_to_expr(scrutinee, subst);
            for arm in arms {
                let mut bindings: std::collections::BTreeSet<String> =
                    std::collections::BTreeSet::new();
                pattern_bindings(&arm.pattern, &mut bindings);
                let inner = filter_subst(subst, &bindings);
                apply_subst_to_expr(&mut arm.body, &inner);
            }
        }
        Expr::Block(b) => apply_subst_to_block(b, subst),
        Expr::Lambda { params, body, .. } => {
            let shadowed: std::collections::BTreeSet<String> =
                params.iter().map(|p| p.name.clone()).collect();
            let inner = filter_subst(subst, &shadowed);
            apply_subst_to_expr(body, &inner);
        }
        Expr::ClosureRecord { env_exprs, .. } => {
            for ee in env_exprs {
                apply_subst_to_expr(ee, subst);
            }
        }
        Expr::RecordLit { fields, .. } => {
            for f in fields {
                apply_subst_to_expr(&mut f.value, subst);
            }
        }
        Expr::Tuple { elems, .. } => {
            for el in elems {
                apply_subst_to_expr(el, subst);
            }
        }
        Expr::Handle {
            body,
            return_arm,
            op_arms,
            ..
        } => {
            apply_subst_to_expr(body, subst);
            for arm in op_arms {
                let mut bindings: std::collections::BTreeSet<String> =
                    arm.params.iter().map(|p| p.name.clone()).collect();
                bindings.insert(arm.k_name.clone());
                let inner = filter_subst(subst, &bindings);
                apply_subst_to_expr(&mut arm.body, &inner);
            }
            if let Some(ra) = return_arm {
                let mut bindings: std::collections::BTreeSet<String> =
                    std::collections::BTreeSet::new();
                bindings.insert(ra.binding.clone());
                let inner = filter_subst(subst, &bindings);
                apply_subst_to_expr(&mut ra.body, &inner);
            }
        }
    }
}

/// Filter a substitution map: drop any entry whose key OR value is
/// in the `shadowed` set. Used at every binder boundary in
/// `apply_subst_to_expr`.
fn filter_subst(
    subst: &BTreeMap<String, String>,
    shadowed: &std::collections::BTreeSet<String>,
) -> BTreeMap<String, String> {
    subst
        .iter()
        .filter(|(k, v)| !shadowed.contains(k.as_str()) && !shadowed.contains(v.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

fn apply_subst_to_block(b: &mut Block, subst: &BTreeMap<String, String>) {
    let mut active = subst.clone();
    for s in &mut b.stmts {
        apply_subst_to_stmt(s, &active);
        // Per-stmt scope narrowing: if this stmt is a Let whose
        // binding name shadows an alias key (`f`) or the alias's
        // target (`k`), drop affected entries before processing
        // subsequent stmts.
        if let Stmt::Let(l) = s {
            active.remove(&l.name);
            active.retain(|_, v| v != &l.name);
        }
    }
    if let Some(t) = &mut b.tail {
        apply_subst_to_expr(t, &active);
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
            // Plan B task 53 — `handle <body> with { ... }` participates
            // in capture analysis like any other expression: the body
            // and arm bodies may reference outer-scope identifiers.
            // Each arm introduces its own bindings (the `return` arm's
            // single value, an op arm's parameter list and its
            // continuation `k`); snapshot/restore `locals` at every
            // arm boundary so the bindings don't leak out into peer
            // arms or the surrounding scope.
            //
            // Capture analysis runs strictly before closure conversion.
            // E0134 was lifted in Task 55 Phase 2 (`2d69b52`); a
            // `handle` reaching this code is now a well-formed
            // shape, and downstream passes need a structurally
            // correct walk regardless of which Phase-4 restrictions
            // the codegen-entry guard later rejects.
            Expr::Handle {
                body,
                return_arm,
                op_arms,
                ..
            } => {
                walk(body, outer_names, param_names, locals, captures);
                if let Some(ra) = return_arm {
                    let saved = locals.clone();
                    locals.insert(ra.binding.clone());
                    walk(&ra.body, outer_names, param_names, locals, captures);
                    *locals = saved;
                }
                for arm in op_arms {
                    let saved = locals.clone();
                    for p in &arm.params {
                        locals.insert(p.name.clone());
                    }
                    locals.insert(arm.k_name.clone());
                    walk(&arm.body, outer_names, param_names, locals, captures);
                    *locals = saved;
                }
            }
            // Plan D Task 113 — tuple values: walk each element to
            // collect captures from sub-expressions.
            Expr::Tuple { elems, .. } => {
                for e in elems {
                    walk(e, outer_names, param_names, locals, captures);
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
        // Snapshot/restore `locals` so a `let X` inside a nested
        // block does NOT leak `X` into the parent block's locals.
        // Pre-Phase-4d this leak was a no-op (capture analysis only
        // ran for `Expr::Lambda`'s overall capture set, not for
        // arm bodies); Phase 4d now consumes the captures list at
        // codegen-time to size per-arm closure records, so a missed
        // outer-scope capture (caused by a nested-block `let` shadow
        // leaking into the outer `locals` set and hiding a later
        // outer-scope reference from `captures`) is no longer a
        // no-op — the arm body's `Ident("X")` reaches codegen
        // without a `ClosureEnvLoad` rewrite and the synth-pass
        // lowerer panics on the unbound name. Mirror the
        // save/restore pattern the `Expr::Match` arm uses for
        // `Pattern::Var` bindings.
        let saved = locals.clone();
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
        *locals = saved;
    }
}

/// Plan D Task 113 — recognise patterns that are catchall against
/// `scrut_ty` at the simple-fallback exhaustiveness layer (no
/// access to the ctor registry; conservative for User types).
///
/// `Pattern::Wildcard` and `Pattern::Var` are catchall against any
/// non-User type. For User types we conservatively return false —
/// the User-type exhaustiveness path uses `match_witness` which
/// handles ctor-aware enumeration.
///
/// `Pattern::Tuple` is catchall against `Ty::Tuple` of matching
/// arity iff every sub-pattern is catchall against the matching
/// element type. Nested tuples are handled recursively.
fn pattern_is_simple_catchall(p: &Pattern, scrut_ty: &Ty) -> bool {
    match p {
        Pattern::Wildcard(_) => true,
        Pattern::Var(_, _) => !matches!(scrut_ty, Ty::User(_, _)),
        Pattern::Tuple(pats, _) => match scrut_ty {
            Ty::Tuple(elem_tys) if pats.len() == elem_tys.len() => pats
                .iter()
                .zip(elem_tys.iter())
                .all(|(sub, ety)| pattern_is_simple_catchall(sub, ety)),
            _ => false,
        },
        _ => false,
    }
}

fn is_exhaustive(scrut: &Ty, arms: &[MatchArm]) -> bool {
    if arms.is_empty() {
        return false;
    }
    let has_catchall = arms
        .iter()
        .any(|a| pattern_is_simple_catchall(&a.pattern, scrut));
    if has_catchall {
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
        // Plan D Task 113 — tuple scrutinees: only structural
        // exhaustiveness (Pattern::Tuple of matching arity covering
        // every cell) can saturate. Without a wildcard, the heuristic
        // here returns false; Maranget's algorithm in
        // `match_witness` does the actual coverage analysis.
        Ty::Tuple(_) => false,
        // Plan D Task 117 — Continuation scrutinees can't reach
        // shape-based exhaustiveness either. The user can't
        // construct a Continuation value in user surface, but the
        // typechecker may still produce one as a let-binding's
        // inferred type; if that's matched-on, only a wildcard
        // saturates.
        Ty::Continuation(_) => false,
        // Plan B task 48: still-polymorphic scrutinees can't reach
        // shape-based exhaustiveness; defer the diagnostic to the
        // upstream unification site that left the variable free.
        Ty::Var(_) => false,
    }
}

/// Plan B task 54 — count syntactic occurrences of `k_name` along
/// the maximal-use path through `expr`, used by the one-shot
/// linearity check (E0220).
///
/// Branching constructs (`if`, `match` arms, nested `handle` arm
/// bodies) take the **max** of branch counts; sequential composition
/// (block statements, binary/call argument lists, etc.) takes the
/// **sum**. Counts saturate at 2 — the linearity check only needs
/// the > 1 vs <= 1 distinction.
///
/// Lambdas: any reference to `k_name` from inside a lambda body
/// saturates the count to 2 (the conservative-capture rule). A
/// lambda's call frequency is not statically known — capturing `k`
/// into a closure could invoke `k` repeatedly even if the lambda
/// body has only a single syntactic `k(...)` call.
///
/// Shadowing: lexical bindings in patterns, arm-binding names,
/// nested handle arms' own `k_name`, and `let` statements all
/// suspend counting for their scope. The implementation keys on
/// the surface name only; resolve.rs's no-shadowing pass keeps
/// pattern-vs-let collisions out of the picture for ordinary
/// programs, and the inner-scope `Stmt::Let` shadowing here is the
/// stop-counting trigger for the rare deliberate-shadow case.
pub(crate) fn count_continuation_uses(e: &Expr, k_name: &str) -> usize {
    count_in_expr(e, k_name).min(2)
}

fn count_in_expr(e: &Expr, k_name: &str) -> usize {
    match e {
        Expr::IntLit(..) | Expr::StringLit(..) | Expr::BoolLit(..) | Expr::CharLit(..) => 0,
        Expr::Ident(name, _) => {
            if name == k_name {
                1
            } else {
                0
            }
        }
        Expr::Call { callee, args, .. } => {
            let mut n = count_in_expr(callee, k_name);
            for a in args {
                n = saturating_add(n, count_in_expr(a, k_name));
            }
            n
        }
        Expr::Perform(p) => {
            let mut n = 0usize;
            for a in &p.args {
                n = saturating_add(n, count_in_expr(a, k_name));
            }
            n
        }
        Expr::Binary { lhs, rhs, .. } => {
            saturating_add(count_in_expr(lhs, k_name), count_in_expr(rhs, k_name))
        }
        Expr::Unary { operand, .. } => count_in_expr(operand, k_name),
        Expr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            let nc = count_in_expr(cond, k_name);
            let nt = count_in_block(then_block, k_name);
            let ne = count_in_block(else_block, k_name);
            saturating_add(nc, nt.max(ne))
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            let ns = count_in_expr(scrutinee, k_name);
            let mut max_arm = 0usize;
            for arm in arms {
                let mut bindings = std::collections::BTreeSet::new();
                pattern_bindings(&arm.pattern, &mut bindings);
                if bindings.contains(k_name) {
                    // The pattern shadows `k_name`; uses inside the
                    // arm body resolve to the inner binding, not the
                    // outer continuation. Skip counting.
                    continue;
                }
                let n = count_in_expr(&arm.body, k_name);
                if n > max_arm {
                    max_arm = n;
                }
            }
            saturating_add(ns, max_arm)
        }
        Expr::Block(b) => count_in_block(b, k_name),
        Expr::Lambda { body, .. } => {
            // Any reference inside a lambda body saturates to 2 —
            // closure capture would let the surrounding code invoke
            // `k` an unknown number of times. See [DEVIATION Task 54]
            // entry "One-shot linearity check uses path-max
            // syntactic counting; lambda capture is conservative".
            if count_in_expr(body, k_name) > 0 {
                2
            } else {
                0
            }
        }
        // Post-CC nodes — typecheck runs strictly before closure
        // conversion, so these never appear here.
        Expr::ClosureRecord { .. } | Expr::ClosureEnvLoad { .. } => 0,
        Expr::RecordLit { fields, .. } => {
            let mut n = 0usize;
            for f in fields {
                n = saturating_add(n, count_in_expr(&f.value, k_name));
            }
            n
        }
        Expr::Handle {
            body,
            return_arm,
            op_arms,
            ..
        } => {
            // Body is its own scope but still references our k_name
            // (the inner handle's body and the outer arm's body are
            // both at the outer arm's caller-row level, so the outer
            // k flows through). Inner arms' bindings can shadow our
            // k_name — skip those.
            let nb = count_in_expr(body, k_name);
            let nra = match return_arm.as_ref() {
                Some(ra) if ra.binding != k_name => count_in_expr(&ra.body, k_name),
                _ => 0,
            };
            let mut max_arm = 0usize;
            for arm in op_arms {
                if arm.k_name == k_name || arm.params.iter().any(|p| p.name == k_name) {
                    continue;
                }
                let n = count_in_expr(&arm.body, k_name);
                if n > max_arm {
                    max_arm = n;
                }
            }
            saturating_add(nb, nra.max(max_arm))
        }
        // Plan D Task 113 — tuple values: each element is a sequential
        // sub-expression; sum k-counts across elements.
        Expr::Tuple { elems, .. } => {
            let mut n = 0usize;
            for e in elems {
                n = saturating_add(n, count_in_expr(e, k_name));
            }
            n
        }
    }
}

fn count_in_block(b: &Block, k_name: &str) -> usize {
    let mut n = 0usize;
    let mut shadowed = false;
    for s in &b.stmts {
        if shadowed {
            continue;
        }
        match s {
            Stmt::Let(l) => {
                n = saturating_add(n, count_in_expr(&l.value, k_name));
                if l.name == k_name {
                    shadowed = true;
                }
            }
            Stmt::Expr(e) => {
                n = saturating_add(n, count_in_expr(e, k_name));
            }
            Stmt::Perform(p) => {
                for a in &p.args {
                    n = saturating_add(n, count_in_expr(a, k_name));
                }
            }
        }
    }
    if !shadowed {
        if let Some(tail) = &b.tail {
            n = saturating_add(n, count_in_expr(tail, k_name));
        }
    }
    n
}

fn saturating_add(a: usize, b: usize) -> usize {
    a.saturating_add(b).min(2)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::{imports, lexer::lex, parser::parse, resolve::resolve};

    fn pipeline(src: &str) -> Vec<CompilerError> {
        let (toks, lex_errs) = lex("x.sigil", src);
        let (prog, parse_errs) = parse("x.sigil", &toks);
        let (prog, import_errs) = imports::resolve(prog);
        let (rp, res_errs) = resolve(prog);
        let (_tc, tc_errs) = typecheck(rp.program);
        let mut all = lex_errs;
        all.extend(parse_errs);
        all.extend(import_errs);
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
            // Plan B Task 55: both staged gates lifted. The first
            // program typechecks cleanly. The second program
            // surfaces E0138 (unknown effect arm `E`); typecheck no
            // longer fires E0134 for it. The "no E0001" discipline
            // rule still holds for both.
            "effect Raise { fail: (String) -> Int }\nfn main() -> Int ![] { 0 }\n",
            "fn main() -> Int ![] { handle 0 with { E.op(k) => 0 } }\n",
            // Plan B task 54: each new code added by handler typing
            // gets its own user-reachable program in this sweep so the
            // "no user-facing diagnostic uses E0001" discipline rule
            // actually covers all reachable codes from this PR.
            // E0136 — duplicate effect declaration:
            "effect Raise { fail: () -> Int }\n\
             effect Raise { other: () -> Int }\n\
             fn main() -> Int ![] { 0 }\n",
            // E0137 — duplicate operation in effect:
            "effect Choose { pick: () -> Int, pick: (Int) -> Int }\n\
             fn main() -> Int ![] { 0 }\n",
            // E0138 — handler arm references unknown effect:
            "fn main() -> Int ![] { handle 0 with { Raise.fail(msg, k) => 0 } }\n",
            // E0139 — unknown op on declared effect:
            "effect Raise { fail: (String) -> Int }\n\
             fn main() -> Int ![] { handle 0 with { Raise.panic(msg, k) => 0 } }\n",
            // E0140 — duplicate handler arm for same Effect.op:
            "effect Raise { fail: (String) -> Int }\n\
             fn main() -> Int ![] { handle 0 with { Raise.fail(m, k) => 0, Raise.fail(m2, k2) => 1 } }\n",
            // E0141 — handler arm parameter arity mismatch:
            "effect Raise { fail: (String) -> Int }\n\
             fn main() -> Int ![] { handle 0 with { Raise.fail(msg, extra, k) => 0 } }\n",
            // E0220 — one-shot continuation used more than once:
            "effect Raise { fail: (String) -> Int }\n\
             fn main() -> Int ![] { handle 0 with { Raise.fail(msg, k) => k(0) + k(1) } }\n",
            // Plan C Task 62.0 — stdlib import resolution:
            // E0032 — stdlib module not found:
            "import std.does_not_exist\nfn main() -> Int ![] { 0 }\n",
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
        let (prog, import_errs) = imports::resolve(prog);
        assert!(import_errs.is_empty(), "imports: {import_errs:?}");
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
        // `fn id[A](x: A) -> A ![] { x }` is the simplest Algorithm-W
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

    // Plan D Task 114 smoke gate — type-parameterized effect rows.

    #[test]
    fn generic_effect_decl_with_type_param_typechecks() {
        // `effect Raise[E] { fail: (E) -> Int }` declares fine —
        // op signatures already use the effect-decl's
        // generic_subst (line 941-948 in this file's pre-pass);
        // Task 114 just wires the row-site surface to substitute
        // the actual type-arg in.
        let src = "effect Raise[E] { fail: (E) -> Int }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "generic effect-decl `Raise[E]` should typecheck cleanly; got {errs:?}"
        );
    }

    #[test]
    fn type_parameterized_row_typechecks() {
        // `![Raise[Int]]` — bare-name effect with a type-arg list
        // resolves the effect-decl + substitutes Int for E. The
        // body's perform site is concrete-Int so check_perform's
        // op-arg unification accepts the call.
        let src = "effect Raise[E] { fail: (E) -> Int }\n\
                   fn risky() -> Int ![Raise[Int]] {\n  \
                     perform Raise.fail(42)\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "row `![Raise[Int]]` should typecheck; got {errs:?}"
        );
    }

    #[test]
    fn generic_effect_decl_with_wrong_arity_fires_e0143() {
        // `effect Raise[E] { ... }` declared with one type-param;
        // a row site uses two args (`Raise[Int, String]`). E0143
        // surfaces the mismatch.
        let src = "effect Raise[E] { fail: (E) -> Int }\n\
                   fn risky() -> Int ![Raise[Int, String]] {\n  \
                     0\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0143"),
            "row-arg arity mismatch should fire E0143; got {errs:?}"
        );
    }

    #[test]
    fn bare_name_reference_to_generic_effect_fires_e0143() {
        // `effect Raise[E] { ... }` declared generic; bare
        // `![Raise]` (no type-arg list) should fire E0143 because
        // E is unresolved at the row site.
        let src = "effect Raise[E] { fail: (E) -> Int }\n\
                   fn risky() -> Int ![Raise] {\n  \
                     0\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0143"),
            "bare `![Raise]` of a generic effect-decl should fire E0143; got {errs:?}"
        );
    }

    #[test]
    fn perform_site_e_substitution_closed_by_task_115() {
        // Plan D Task 114 R1 → Task 115 closure: perform-site
        // E-substitution previously failed to thread row-site
        // type-args into the op's signature. The op's signature
        // `fail: (E) -> Int` was checked under a fresh Ty::Var for
        // E, so a wrong-typed argument (`Raise.fail("wrong type")`
        // under `![Raise[Int]]`) bound `E := String` rather than
        // firing E0044 against the row-instantiated `E := Int`.
        //
        // Task 115 closed the gap: `check_perform` now consults
        // the surrounding fn's row entry for the effect, builds an
        // effect-decl substitution from its args, and applies it
        // before resolving the op's params. Wrong-typed args fire
        // E0044.
        //
        // INVERTED FROM `perform_site_e_substitution_deferred_to_task_115`
        // at Task 115 landing.
        let src = "effect Raise[E] { fail: (E) -> Int }\n\
                   fn risky() -> Int ![Raise[Int]] {\n  \
                     perform Raise.fail(\"wrong type\")\n  \
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0044"),
            "Task 115 closure: perform-site E-substitution should fire E0044 \
             for String arg vs Int param; got {errs:?}"
        );
    }

    // Plan D Task 115 smoke gate — per-op generic params on
    // user-declared effects.

    #[test]
    fn per_op_generic_params_typecheck() {
        // `fail[A]: (E) -> A` — A is bound only inside fail's
        // signature, distinct from the effect-decl's E.
        let src = "effect Raise[E] { fail[A]: (E) -> A }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "per-op generic params should typecheck cleanly; got {errs:?}"
        );
    }

    #[test]
    fn per_op_generic_param_shadowing_effect_decl_param_fires_e0144() {
        // `effect Raise[E] { fail[E]: (E) -> Int }` — per-op `E`
        // shadows effect-decl `E`. E0144 fires.
        let src = "effect Raise[E] { fail[E]: (E) -> Int }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0144"),
            "per-op generic param shadowing effect-decl param should \
             fire E0144; got {errs:?}"
        );
    }

    #[test]
    fn perform_site_per_op_generic_instantiates_at_use_site() {
        // `effect Raise[E] { fail[A]: (E) -> A }` — at the perform
        // site, the per-op `A` is fresh-instantiated and unifies
        // with the surrounding context. Here the let-binding's
        // declared type `Int` constrains A := Int.
        let src = "effect Raise[E] { fail[A]: (E) -> A }\n\
                   fn risky() -> Int ![Raise[String]] {\n  \
                     let r: Int = perform Raise.fail(\"oops\");\n  \
                     r\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "per-op generic A should instantiate to Int from let-binding \
             context; got {errs:?}"
        );
    }

    #[test]
    fn handle_arm_over_per_op_generic_op_typechecks() {
        // Plan D Task 115 R1 regression: `check_handle` previously
        // resolved op param/return types under the effect-decl
        // substitution only, so per-op generics like `A` in
        // `fail[A]: (E) -> A` resolved to `None` and silently
        // collapsed to `Ty::Unit`. The continuation `k`'s arg type
        // was `Unit` instead of `A_var`, and `k(42)` would have
        // fired E0044 (Int vs Unit).
        //
        // The fix layers per-op generics on top of the effect-decl
        // subst when typing each arm. The arm here invokes
        // `k(42)` — post-fix, `k` types as `Fn(A_var) -> Int`,
        // `A_var` unifies to `Int` from the call, and the arm
        // body's type matches `handler_overall = Int`.
        let src = "effect Raise[E] { fail[A]: (E) -> A }\n\
                   fn main() -> Int ![] {\n  \
                     handle 0 with {\n    \
                       Raise.fail(e, k) => k(42)\n  \
                     }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "handler arm over `fail[A]: (E) -> A` should typecheck; \
             pre-fix `k` typed as Fn(Unit) -> Int and `k(42)` fired E0044; \
             got {errs:?}"
        );
    }

    #[test]
    fn multi_arg_per_op_generic_typechecks() {
        // `effect Choose[A] { choose[B]: (B, B) -> A }` —
        // multiple per-op generics + an effect-decl generic.
        let src = "effect Choose[A] { choose[B]: (B, B) -> A }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "multi-arg per-op generics should typecheck; got {errs:?}"
        );
    }

    #[test]
    fn cross_fn_row_with_concrete_type_arg_unifies() {
        // Caller `![Raise[Int]]` calls callee declared `![Raise[Int]]`:
        // both sides carry the same `EffectInst { name: "Raise",
        // args: [Ty::Int] }`. unify_row's structural set diff
        // matches them as equal; subsume_row sees the callee's
        // effect already in the caller's row. Pinned so cross-fn
        // composition with type-parameterized effects doesn't
        // regress when the typecheck rewires its row machinery.
        let src = "effect Raise[E] { fail: (E) -> Int }\n\
                   fn risky() -> Int ![Raise[Int]] { 0 }\n\
                   fn outer() -> Int ![Raise[Int]] { risky() }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "cross-fn row `![Raise[Int]]` should unify; got {errs:?}"
        );
    }

    #[test]
    fn subsume_row_arg_arity_mismatch_fires_e0042() {
        // Plan D Stage 12 R3 regression — subsume_row's name-match
        // arg-unify path used to silently skip when arg-counts
        // differed. Now it fires E0042 with an arity-mismatch
        // message. This pin locks the behavior so a future
        // contributor doesn't accidentally restore the silent
        // skip (which had a soundness hole — discharged effects
        // could match no-args body_row entries against args-bearing
        // body rows without binding anything).
        //
        // Reaching this from surface code requires constructing a
        // row mismatch at typecheck time. We exercise it via a
        // direct unit test of subsume_row.
        let mut tc = fresh_tc();
        let span = Span::synthetic("x.sigil");
        let callee = Row::closed(vec![EffectInst {
            name: "Raise".to_string(),
            args: vec![Ty::Int],
        }]);
        let caller = Row::closed(vec![EffectInst {
            name: "Raise".to_string(),
            args: Vec::new(),
        }]);
        let ok = tc.subsume_row(&callee, &caller, &span);
        assert!(!ok, "arg-arity mismatch must fail subsumption");
        assert!(
            has_code(&tc.errors, "E0042"),
            "expected E0042 (arg-arity mismatch); got {:?}",
            tc.errors
        );
    }

    #[test]
    fn subsume_row_multi_instantiation_same_order_subsumes() {
        // Plan D Stage 12 R2 followup — mirror of the unify_row
        // multi-instantiation pin, applied to subsume_row's
        // analogous matched_caller set. Same-order callee/caller
        // rows for `![Raise[Int], Raise[String]]` subsume cleanly:
        // callee[0]=Raise[Int] matches caller[0]=Raise[Int] (idx 0
        // marked), then callee[1]=Raise[String] matches caller[1]=
        // Raise[String]. No diagnostics; missing is empty; closed
        // callee returns args_ok=true.
        let mut tc = fresh_tc();
        let span = Span::synthetic("x.sigil");
        let callee = Row::closed(vec![
            EffectInst {
                name: "Raise".to_string(),
                args: vec![Ty::Int],
            },
            EffectInst {
                name: "Raise".to_string(),
                args: vec![Ty::String],
            },
        ]);
        let caller = Row::closed(vec![
            EffectInst {
                name: "Raise".to_string(),
                args: vec![Ty::Int],
            },
            EffectInst {
                name: "Raise".to_string(),
                args: vec![Ty::String],
            },
        ]);
        let ok = tc.subsume_row(&callee, &caller, &span);
        assert!(
            ok,
            "same-order multi-instantiation rows must subsume; errors: {:?}",
            tc.errors
        );
        assert!(
            tc.errors.is_empty(),
            "no diagnostics expected; got {:?}",
            tc.errors
        );
    }

    #[test]
    fn subsume_row_multi_instantiation_reversed_order_fires_e0044() {
        // Plan D Stage 12 R2 followup — pair to
        // `subsume_row_multi_instantiation_same_order_subsumes`. With
        // caller reversed, callee[0]=Raise[Int] is matched against
        // the first unclaimed caller entry by name (caller[0]=
        // Raise[String]) — args unify Int vs String → E0044. Locks
        // in subsume_row's order-dependent semantics symmetrically
        // with the unify_row pins.
        let mut tc = fresh_tc();
        let span = Span::synthetic("x.sigil");
        let callee = Row::closed(vec![
            EffectInst {
                name: "Raise".to_string(),
                args: vec![Ty::Int],
            },
            EffectInst {
                name: "Raise".to_string(),
                args: vec![Ty::String],
            },
        ]);
        let caller = Row::closed(vec![
            EffectInst {
                name: "Raise".to_string(),
                args: vec![Ty::String],
            },
            EffectInst {
                name: "Raise".to_string(),
                args: vec![Ty::Int],
            },
        ]);
        let ok = tc.subsume_row(&callee, &caller, &span);
        assert!(
            !ok,
            "reversed multi-instantiation rows must fail (Int vs String at first match)"
        );
        assert!(
            has_code(&tc.errors, "E0044"),
            "expected E0044 from arg-unification at first by-name match; got {:?}",
            tc.errors,
        );
    }

    #[test]
    fn unify_row_multi_instantiation_same_order_unifies() {
        // Plan D Stage 12 R2 regression — pin the first-occurrence-
        // wins behavior of unify_row's name-match loop for rows
        // carrying the same effect-name twice with distinct args
        // (`![Raise[Int], Raise[String]]`). Same-side ordering
        // unifies cleanly: a[0]=Raise[Int] matches b[0]=Raise[Int]
        // (idx 0 marked in matched_b), then a[1]=Raise[String]
        // matches b[1]=Raise[String] (idx 1 in matched_b). The
        // matched_b set is what makes the order matter — without
        // it, a[1] could re-match b[0] and the args would diverge.
        let mut tc = fresh_tc();
        let span = Span::synthetic("x.sigil");
        let row_a = Row::closed(vec![
            EffectInst {
                name: "Raise".to_string(),
                args: vec![Ty::Int],
            },
            EffectInst {
                name: "Raise".to_string(),
                args: vec![Ty::String],
            },
        ]);
        let row_b = Row::closed(vec![
            EffectInst {
                name: "Raise".to_string(),
                args: vec![Ty::Int],
            },
            EffectInst {
                name: "Raise".to_string(),
                args: vec![Ty::String],
            },
        ]);
        let ok = tc.unify_row(&row_a, &row_b, &span);
        assert!(
            ok,
            "same-order multi-instantiation rows must unify; errors: {:?}",
            tc.errors
        );
        assert!(
            tc.errors.is_empty(),
            "no diagnostics expected; got {:?}",
            tc.errors
        );
    }

    #[test]
    fn unify_row_multi_instantiation_reversed_order_fires_e0044() {
        // Plan D Stage 12 R2 regression — pair to
        // `unify_row_multi_instantiation_same_order_unifies`. With
        // b reversed, a[0]=Raise[Int] is matched against the first
        // unclaimed b entry by name (b[0]=Raise[String]) — args
        // unify Int vs String → E0044. This locks in the order-
        // dependent semantics so a future "improvement" (e.g.,
        // exhaustive permutation matching) can't silently change
        // the diagnostic shape without un-ignoring this test.
        let mut tc = fresh_tc();
        let span = Span::synthetic("x.sigil");
        let row_a = Row::closed(vec![
            EffectInst {
                name: "Raise".to_string(),
                args: vec![Ty::Int],
            },
            EffectInst {
                name: "Raise".to_string(),
                args: vec![Ty::String],
            },
        ]);
        let row_b = Row::closed(vec![
            EffectInst {
                name: "Raise".to_string(),
                args: vec![Ty::String],
            },
            EffectInst {
                name: "Raise".to_string(),
                args: vec![Ty::Int],
            },
        ]);
        let ok = tc.unify_row(&row_a, &row_b, &span);
        assert!(
            !ok,
            "reversed multi-instantiation rows must fail (Int vs String at first match)"
        );
        assert!(
            has_code(&tc.errors, "E0044"),
            "expected E0044 from arg-unification at first by-name match; got {:?}",
            tc.errors,
        );
    }

    #[test]
    fn cross_fn_row_with_distinct_type_args_fires_e0044() {
        // Caller `![Raise[Int]]` calls callee `![Raise[String]]`:
        // they share the effect-decl name but instantiate it
        // distinctly. Plan D Stage 12 — the row-matching logic in
        // `subsume_row` matches by name then unifies args
        // pairwise; the type mismatch surfaces as **E0044** at the
        // arg-unify step (Int vs String), not E0042 (which fires
        // for "name not in caller's row"). The previous
        // structural-equality diff fired E0042 here; the
        // arg-unification semantics give a more precise
        // diagnostic — the user knows which arg type is wrong.
        let src = "effect Raise[E] { fail: (E) -> Int }\n\
                   fn risky_str() -> Int ![Raise[String]] { 0 }\n\
                   fn outer() -> Int ![Raise[Int]] { risky_str() }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0044"),
            "Raise[Int] caller calling Raise[String] callee should fire E0044 (Int vs String arg type mismatch); got {errs:?}"
        );
    }

    #[test]
    fn type_args_on_non_generic_effect_fires_e0143() {
        // Builtin `IO` is not generic; `![IO[Int]]` is malformed.
        let src = "fn main() -> Int ![IO[Int]] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0143"),
            "`![IO[Int]]` on non-generic IO should fire E0143; got {errs:?}"
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
    fn occurs_check_unify_ty_fires_e0126() {
        // The surface forms reachable from Sigil's current syntax
        // never produce a Var-vs-Fn(Var, ...) cycle (call sites
        // require a Fn callee; Sigil has no fn-type literal in
        // TypeExpr that would let the user write the cycle as a
        // type annotation). We exercise the occurs check by
        // constructing the Ty values directly and invoking
        // `unify_ty` against a synthetic span.
        let mut tc = fresh_tc();
        let v = Ty::Var(tc.fresh_ty_var());
        // Cyclic shape: ?V vs (?V) -> Int — solving this would
        // require ?V to expand infinitely.
        let cyclic = Ty::Fn(Box::new(FnSig {
            params: vec![v.clone()],
            ret: Ty::Int,
            effects: Vec::new(),
            effect_row_var: None,
        }));
        let span = Span::synthetic("x.sigil");
        let ok = tc.unify_ty(&v, &cyclic, &span);
        assert!(!ok, "unify_ty must reject the cycle");
        assert!(
            has_code(&tc.errors, "E0126"),
            "expected E0126 from occurs check; got {:?}",
            tc.errors,
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

    // ===== Plan B Task 48 review-follow-up tests =====

    #[test]
    fn forward_reference_to_generic_fn_typechecks() {
        // Reproducer from the PR review: `use_id` references `id`
        // before `id` is declared. Pre-pass scheme seeding (the
        // review fix) ensures the forward reference hits the
        // polymorphic `Scheme` rather than a Unit-fallback `fn_env`
        // entry. Without the fix, this emits two spurious E0044s.
        let src = "fn use_id[B](x: B) -> B ![] { id(x) }\n\
                   fn id[A](x: A) -> A ![] { x }\n\
                   fn main() -> Int ![] { use_id(0) }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "forward reference to generic fn should typecheck; got {errs:?}"
        );
    }

    #[test]
    fn pattern_binds_inner_var_with_scrutinee_type_arg() {
        // `match xs: List[Int] { Cons(h, _) => h + 1, Nil => 0 }`
        // — h must type as Int (List's A bound to Int by the
        // scrutinee). Without the per-pattern subst fix, h types
        // as Unit and the `h + 1` body emits E0060.
        let src = "type List[A] = | Nil | Cons(A, List[A])\n\
                   fn f(xs: List[Int]) -> Int ![] {\n  \
                     match xs { Nil => 0, Cons(h, _) => h + 1 }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "pattern-bind from generic ctor should typecheck; got {errs:?}"
        );
    }

    #[test]
    fn pair_instantiation_arg_mismatch_fires_e0044() {
        // `let p: Pair[Int, String] = P("a", 1)` — first arg should
        // be Int but is String. Per-call ctor subst maps X→Int,
        // Y→String; unifying field X with arg's String fails.
        let src = "type Pair[X, Y] = | P(X, Y)\n\
                   fn main() -> Int ![] {\n  \
                     let p: Pair[Int, String] = P(\"a\", 1);\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0044"),
            "expected E0044 from arg-vs-field type mismatch; got {errs:?}"
        );
    }

    #[test]
    fn self_recursive_generic_fn_typechecks() {
        // Self-recursion through fn_schemes: each recursive call
        // instantiates a fresh copy of the scheme rather than
        // colliding with the body's own bound vars. This exercises
        // the same path as the forward-reference test but along a
        // recursive edge.
        let src = "type List[A] = | Nil | Cons(A, List[A])\n\
                   fn length[A](xs: List[A]) -> Int ![] {\n  \
                     match xs { Nil => 0, Cons(_, tail) => length(tail) }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "self-recursive generic fn should typecheck; got {errs:?}"
        );
    }

    #[test]
    fn generic_match_returning_generic_unifies_arms() {
        // Plan B Task 51 regression: generic-fn-internal match where
        // arms return a generic-typed result (`List[A]`). `Nil` and
        // `Cons(...)` each generate fresh user-instance vars — pre-
        // fix, the cross-arm consistency check used structural Eq on
        // `Ty`, so two `List[?N]` arms with different fresh-var ids
        // tripped E0065 even when they trivially unify. Post-fix,
        // `unify_ty` runs and binds the vars together.
        let src = "type List[A] = | Nil | Cons(A, List[A])\n\
                   fn map[A](xs: List[A]) -> List[A] ![] {\n  \
                     match xs { Nil => Nil, Cons(h, t) => Cons(h, map(t)) }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "generic match with generic-typed arms should unify across arms; got {errs:?}"
        );
    }

    #[test]
    fn match_arm_type_unification_still_rejects_real_mismatch() {
        // Regression-guard: the unify-based cross-arm check must
        // still fire E0065 on a genuine arm-body type mismatch (Int
        // vs String). This is the same shape as the long-standing
        // `match_arm_types_must_unify_is_e0065` but expressed inside
        // a generic fn — so the post-fix path doesn't accidentally
        // accept rubbish that the pre-fix Eq check rejected.
        let src = "fn pick[A](b: Bool, x: A, y: A) -> A ![] {\n  \
                     match b { true => x, false => y }\n\
                   }\n\
                   fn main() -> Int ![] {\n  \
                     let n: Int = match 0 { 0 => 1, _ => \"x\" };\n  \
                     n\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0065"),
            "expected E0065 on Int-vs-String arm mismatch, got: {errs:?}"
        );
    }

    #[test]
    fn three_arm_generic_match_propagates_subst_across_all_arms() {
        // Reviewer-requested 3-arm regression test: pin that
        // cross-arm unify propagates substitutions across MORE than
        // two arms. Each `W(x)`-style positional ctor allocates a
        // fresh `Wrap[?N]` user instance, so arm 1 produces
        // `Wrap[?A]`, arm 2 `Wrap[?B]`, arm 3 `Wrap[?C]` — each
        // distinct fresh-var ids. The cross-arm check unifies
        // sequentially: arm1↔arm2 binds `?A := ?B`; arm1↔arm3 must
        // then unify `Wrap[?A]` (already deref'd to `Wrap[?B]`)
        // against `Wrap[?C]`. A naive 2-arm-only check would miss
        // a propagation bug on the third arm.
        //
        // All three vars eventually bind to the function's declared
        // `A` via the surrounding return-type unification, so the
        // match expression's overall type is `Wrap[A]`.
        let src = "type Wrap[A] = | W(A)\n\
                   fn three_arm[A](n: Int, x: A, y: A, z: A) -> Wrap[A] ![] {\n  \
                     match n {\n    \
                       0 => W(x),\n    \
                       1 => W(y),\n    \
                       _ => W(z),\n  \
                     }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "3-arm generic match must unify all arm-body fresh-var instances against each other; got {errs:?}"
        );
    }

    #[test]
    fn subst_pollution_from_partial_unify_surfaces_at_call_site() {
        // Reviewer-requested HM-semantics regression-guard. `unify_ty`
        // is non-transactional: it commits each successful binding to
        // `self.subst` as it recurses. Cross-arm unify in `check_match`
        // does NOT roll those back — this is HM-correct: a generic
        // body that constrains its own params via successful unifies
        // produces a real constraint that should surface at the
        // function's call sites via the normal E0044 / E0132 cascade.
        //
        // The discriminating program: `foo[A, B]`'s match has a first
        // arm `p` (type `Pair[A, B]`) and a second arm `Pair("x", 3)`
        // (type `Pair[String, Int]`). The cross-arm unify SUCCEEDS by
        // binding `A := String` and `B := Int`, so the body itself
        // typechecks clean — but the bindings remain in the global
        // subst. `foo`'s scheme is now over-constrained: `A` and `B`
        // are no longer free type vars. `caller` calls `foo(Pair(1, 2),
        // 0)` which forces `A := Int, B := Int` at the instantiation
        // site, and the over-constraint shows up as E0044 (type
        // mismatch) + E0132 (ambiguous polymorphism: A and B
        // unconstrained at the call site, because `foo`'s scheme
        // generalization can no longer abstract over them).
        //
        // A future refactor that adds subst snapshot/restore around
        // `unify_ty` would silently lose this. With rollback: arm 2's
        // bindings are discarded; `foo`'s scheme stays generic
        // `forall A B. Pair[A, B] -> Pair[A, B]`; `caller`'s call
        // accepts `Pair[Int, Int]` cleanly with NO errors. CI would
        // not catch the regression. This test pins the contract that
        // the cascade fires by asserting both E0044 (call-site
        // concrete mismatch) and E0132 (scheme over-constraint
        // surfacing as ambiguous polymorphism).
        let src = "type Pair[A, B] = | Pair(A, B)\n\
                   fn foo[A, B](p: Pair[A, B], k: Int) -> Pair[A, B] ![] {\n  \
                     match k {\n    \
                       0 => p,\n    \
                       _ => Pair(\"x\", 3),\n  \
                     }\n\
                   }\n\
                   fn caller() -> Int ![] {\n  \
                     let _q: Pair[Int, Int] = foo(Pair(1, 2), 0);\n  \
                     0\n\
                   }\n\
                   fn main() -> Int ![] { caller() }\n";
        let errs = pipeline(src);
        // Non-empty diagnostic list: a rollback regression would
        // produce empty errors here.
        assert!(
            !errs.is_empty(),
            "subst pollution from foo's body must surface at caller's call site; empty errs would indicate rollback hiding the over-constraint"
        );
        // E0044: the body bound A := String, B := Int; caller's
        // Pair(1, 2) (Int, Int) now mismatches against the over-
        // constrained scheme.
        assert!(
            has_code(&errs, "E0044"),
            "expected E0044 cascade at call site (caller's Pair[Int, Int] vs foo's over-constrained Pair[String, Int]); got: {errs:?}"
        );
        // E0132: scheme generalization sees A and B already bound
        // and emits ambiguous-polymorphism at the call site. This is
        // the most discriminating regression signal — it fires only
        // when the body's partial unifies persist into scheme generation.
        assert!(
            has_code(&errs, "E0132"),
            "expected E0132 ambiguous-polymorphism at call site (proves subst pollution survived into scheme generalization); got: {errs:?}"
        );
        // Body-level cleanliness: the discriminating contract is that
        // the body's match itself does NOT error (both arms unify via
        // the new cross-arm path). E0065 here would mean either pre-
        // fix Eq behavior (regression in the opposite direction) OR
        // a hypothetical "stricter rollback that emits a body error"
        // regression. Neither is correct.
        assert!(
            !has_code(&errs, "E0065"),
            "body's match must succeed (cross-arm unify binds A := String, B := Int); E0065 means a regression in the opposite direction. got: {errs:?}"
        );
    }

    #[test]
    fn lambda_inside_generic_fn_references_outer_type_var() {
        // Plan B task 48 (reviewer follow-up): a lambda nested
        // inside `fn id[A]` should be able to reference `A` in
        // its own param/return positions and have it resolve to
        // id's `Ty::Var`. Before the fix, check_lambda used an
        // empty generic_subst and `A` would E0112 inside the
        // lambda. The applied lambda is constrained to the same
        // A, so the body returns x.
        let src = "fn apply_self[A](x: A) -> A ![] {\n  \
                     (fn (y: A) -> A ![] => y)(x)\n\
                   }\n\
                   fn main() -> Int ![] { apply_self(0) }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "lambda referencing outer fn's generic param should typecheck; got {errs:?}"
        );
    }

    #[test]
    fn closed_row_with_extra_fires_e0042() {
        // Closed-row callee `![IO]`, caller `![]`. subsume_row
        // pushes E0042 for the missing IO. Pinned to ensure the
        // asymmetric subsumption replaces unify_row's old behavior
        // without losing the diagnostic.
        let src = "fn f() -> Int ![IO] { 0 }\n\
                   fn caller() -> Int ![] {\n  \
                     let x: Int = f();\n  \
                     x\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0042"),
            "expected E0042 (closed row missing required effect); got {errs:?}"
        );
    }

    #[test]
    fn closed_row_callees_unify_via_unify_row_under_e0128() {
        // Place two fns with mismatched closed effect rows in
        // positions that force `unify_row` to compare them
        // symmetrically. The let-binding annotation `Fn`-typed
        // value path doesn't exist in v1, so we exercise the
        // E0128 branch directly via a unit-style test below.
        // This source-level test confirms the standard
        // closed-vs-closed subsumption path stays clean for the
        // matching case.
        let src = "fn f() -> Int ![IO] { 0 }\n\
                   fn g() -> Int ![IO] { 0 }\n\
                   fn main() -> Int ![IO] {\n  \
                     let a: Int = f();\n  \
                     let b: Int = g();\n  \
                     a + b\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "matching closed rows should typecheck; got {errs:?}"
        );
    }

    #[test]
    fn unify_row_closed_vs_closed_mismatch_fires_e0128() {
        // Unit-style test against `unify_row` directly with two
        // distinct closed rows that cannot unify. Reachable from
        // the surface only when two Fn-typed values flow through
        // the same unification (Stage 6 effect-handler work);
        // pinned now so the E0128 emission path stays exercised.
        let mut tc = fresh_tc();
        let span = Span::synthetic("x.sigil");
        let row_a = Row::closed(vec![EffectInst::bare("IO")]);
        let row_b = Row::closed(vec![EffectInst::bare("Raise")]);
        let ok = tc.unify_row(&row_a, &row_b, &span);
        assert!(!ok, "two distinct closed rows must not unify");
        assert!(
            has_code(&tc.errors, "E0128"),
            "expected E0128; got {:?}",
            tc.errors,
        );
    }

    #[test]
    fn unify_row_closed_vs_open_absorbs_difference() {
        // Open `![IO, Raise | r]` vs closed `![IO, Raise]`: r
        // gets bound to closed[]. Confirm unify_row succeeds and
        // the substitution resolves r to a closed empty row.
        let mut tc = fresh_tc();
        let span = Span::synthetic("x.sigil");
        let r = tc.fresh_row_var();
        let open = Row::open(vec![EffectInst::bare("IO"), EffectInst::bare("Raise")], r);
        let closed = Row::closed(vec![EffectInst::bare("IO"), EffectInst::bare("Raise")]);
        let ok = tc.unify_row(&open, &closed, &span);
        assert!(ok, "open(IO,Raise|r) must unify with closed(IO,Raise)");
        let resolved = tc.subst.apply_row(&Row {
            effects: Vec::new(),
            tail: Some(r),
        });
        assert!(resolved.tail.is_none(), "r must resolve to closed");
        assert!(resolved.effects.is_empty(), "r must absorb no extras");
    }

    #[test]
    fn unify_row_closed_vs_open_missing_effect_fires_e0128() {
        // Open `![IO | r]` vs closed `![]`: closed side is missing
        // IO that the open side requires (sets-side); E0128 fires.
        let mut tc = fresh_tc();
        let span = Span::synthetic("x.sigil");
        let r = tc.fresh_row_var();
        let open = Row::open(vec![EffectInst::bare("IO")], r);
        let closed = Row::closed(Vec::new());
        let ok = tc.unify_row(&open, &closed, &span);
        assert!(!ok, "closed `[]` cannot supply `IO` to open `[IO | r]`");
        assert!(
            has_code(&tc.errors, "E0128"),
            "expected E0128; got {:?}",
            tc.errors,
        );
    }

    #[test]
    fn unify_row_open_vs_open_shared_tail_succeeds() {
        // Two distinct open rows with no overlap: a fresh shared
        // tail absorbs both sides. After unification, both
        // original tails resolve to a row that includes the
        // other side's effects.
        let mut tc = fresh_tc();
        let span = Span::synthetic("x.sigil");
        let a_tail = tc.fresh_row_var();
        let b_tail = tc.fresh_row_var();
        let row_a = Row::open(vec![EffectInst::bare("IO")], a_tail);
        let row_b = Row::open(vec![EffectInst::bare("Raise")], b_tail);
        let ok = tc.unify_row(&row_a, &row_b, &span);
        assert!(ok, "open(IO|a) must unify with open(Raise|b)");
        // After the merge, a_tail's resolution should mention Raise.
        let a_resolved = tc.subst.apply_row(&Row {
            effects: Vec::new(),
            tail: Some(a_tail),
        });
        assert!(
            a_resolved.effects.iter().any(|e| e.name == "Raise"),
            "a_tail should absorb Raise; got {:?}",
            a_resolved
        );
    }

    #[test]
    fn unify_row_row_occurs_check_fires_e0127() {
        // Bind row var r := open([], r) — a self-cycle. The row
        // occurs check rejects this with E0127.
        let mut tc = fresh_tc();
        let span = Span::synthetic("x.sigil");
        let r = tc.fresh_row_var();
        // The rows we're going to unify both share tail r; we
        // synthesise a cycle by trying to bind r against a row
        // whose tail is r itself.
        let cyclic = Row {
            effects: Vec::new(),
            tail: Some(r),
        };
        // Unify open([IO]|r) with open([]|r) directly: tails
        // share, sets differ → E0128 (which is a different
        // enforcement). To exercise E0127, call bind_row_var
        // directly with a self-referential row that's already
        // got the var as its tail (not the same id we're binding).
        // Setup: pretend r resolves to s, then bind s := tail=r.
        let s = tc.fresh_row_var();
        tc.subst.rows.insert(s, cyclic.clone());
        // Now binding r := open([]|s) → resolving s gives tail=r.
        let synthetic = Row {
            effects: Vec::new(),
            tail: Some(s),
        };
        let ok = tc.bind_row_var(r, &synthetic, &span);
        assert!(!ok, "row occurs check must reject the cycle");
        assert!(
            has_code(&tc.errors, "E0127"),
            "expected E0127 from row occurs; got {:?}",
            tc.errors,
        );
    }

    /// Build a freshly-initialised `Tc` with empty registries.
    /// Used by the unit-style row/type tests above.
    fn fresh_tc() -> Tc {
        Tc {
            errors: Vec::new(),
            string_literals: Vec::new(),
            lambda_captures: Vec::new(),
            fn_env: BTreeMap::new(),
            env: BTreeMap::new(),
            types: BTreeMap::new(),
            ctors: BTreeMap::new(),
            match_scrut_tys: BTreeMap::new(),
            call_callee_tys: BTreeMap::new(),
            fn_schemes: BTreeMap::new(),
            next_ty_var: 0,
            next_row_var: 0,
            next_scope_id: 0,
            current_arm_scope_id: None,
            subst: Subst::new(),
            current_generic_subst: BTreeMap::new(),
            current_row_var_subst: BTreeMap::new(),
            pending_call_instantiations: Vec::new(),
            pending_ctor_instantiations: Vec::new(),
            effects: BTreeMap::new(),
            handler_scopes: Vec::new(),
            handle_arm_captures: BTreeMap::new(),
            handle_return_arm_captures: BTreeMap::new(),
            handle_body_ty: BTreeMap::new(),
        }
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

    // ===== Plan B Task 49 — E0132 ambiguous polymorphism =====

    #[test]
    fn ambiguous_polymorphism_at_call_site_is_e0132() {
        // `nothing[A]()` declared with no params using `A` in the
        // signature. Calling `nothing()` with no constraint on `A`
        // means inference can't pin `A` to anything, and
        // monomorphization would silently mangle to a placeholder.
        // E0132 catches this at end-of-typecheck.
        let src = "fn nothing[A]() -> Int ![] { 0 }\n\
                   fn main() -> Int ![] { nothing() }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0132"),
            "expected E0132 ambiguous polymorphism; got: {errs:?}"
        );
    }

    #[test]
    fn polymorphism_constrained_at_call_site_is_clean() {
        // Counterpart to the above: `id[A](x: A) -> A` instantiated
        // at Int via the arg `42` typechecks cleanly — `A` is pinned
        // by the input type. Regression guard for over-eager E0132.
        let src = "fn id[A](x: A) -> A ![] { x }\n\
                   fn main() -> Int ![] { id(42) }\n";
        let errs = pipeline(src);
        let hard: Vec<_> = errs
            .iter()
            .filter(|e| matches!(e.severity, Severity::Error))
            .collect();
        assert!(
            hard.is_empty(),
            "constrained polymorphism should not fire E0132; got: {hard:?}"
        );
    }

    #[test]
    fn polymorphism_inside_generic_fn_body_is_clean() {
        // Inside a generic body, an inner generic call's type-args
        // resolve to the *outer* fn's free vars (e.g., `id(x)` inside
        // `fn use_id[B](y: B)` produces inner-A := outer-B). Those
        // outer vars are listed in `fn_schemes[*].type_vars`, so the
        // E0132 check correctly classifies them as legitimate (not
        // ambiguous). Regression guard.
        let src = "fn id[A](x: A) -> A ![] { x }\n\
                   fn use_id[B](y: B) -> B ![] { id(y) }\n\
                   fn main() -> Int ![] { use_id(42) }\n";
        let errs = pipeline(src);
        let hard: Vec<_> = errs
            .iter()
            .filter(|e| matches!(e.severity, Severity::Error))
            .collect();
        assert!(
            hard.is_empty(),
            "outer-fn-bound polymorphism should not fire E0132; got: {hard:?}"
        );
    }

    // ===== Plan B Task 55 — both staged-feature gates lifted ==============
    //
    // Task 53 introduced E0133 (effect decl) and E0134 (handle expr) as
    // staged-feature gates so partial Plan B programs could not reach
    // the CPS-pending codegen path. Task 55's foundation phase
    // (`b3af204`) lifted E0133; Phase 2 (`2d69b52`) lifted E0134; both
    // gate diagnostics no longer fire. The historic "asymmetric" tests
    // below remain as positive coverage of the now-lifted state — each
    // exercises a well-formed effect decl + handle program and asserts
    // neither gate fires. The codegen-entry guard
    // `unsupported_handle_construct` rejects shapes still outside the
    // Phase 3b/4a supported subset (richer arm bodies, multi-effect,
    // k usage, return arms — see Phase 4b–4f).

    #[test]
    fn well_formed_effect_decl_typechecks_cleanly() {
        let src = "effect Raise { fail: (String) -> Int }\nfn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "effect decl + main should typecheck with no errors; got: {errs:?}"
        );
    }

    #[test]
    fn effect_ids_assigned_alphabetically_per_program() {
        // Plan B Task 55 + Task 57 — user effect_ids are assigned in
        // alphabetical order, **starting at `BUILTIN_EFFECT_NAMES.len()`**.
        // Plan C Task 66 added `Mem` as a third builtin (zero-op
        // marker effect), bumping the user-id start to 3. Reserved
        // low ids: 0 (`ArithError`), 1 (`IO`), 2 (`Mem`).
        let src = "effect Zeta { z: () -> Int }\n\
                   effect Alpha { a: () -> Int }\n\
                   effect Mu { m: () -> Int }\n\
                   fn main() -> Int ![] { 0 }\n";
        let (cp, errs) = pipeline_checked(src);
        assert!(errs.is_empty(), "expected clean typecheck; got: {errs:?}");
        // Builtins occupy 0, 1, 2.
        assert_eq!(cp.effect_ids.get("ArithError"), Some(&0));
        assert_eq!(cp.effect_ids.get("IO"), Some(&1));
        assert_eq!(cp.effect_ids.get("Mem"), Some(&2));
        // User effects start at 3 in alphabetical order.
        assert_eq!(cp.effect_ids.get("Alpha"), Some(&3));
        assert_eq!(cp.effect_ids.get("Mu"), Some(&4));
        assert_eq!(cp.effect_ids.get("Zeta"), Some(&5));
    }

    #[test]
    fn builtin_effects_present_in_every_program() {
        // Plan B Task 57 — even programs that declare no effects of
        // their own carry the synthetic builtins `IO` (effect_id 1)
        // and `ArithError` (effect_id 0) in `tc.effects`. The `main`
        // shim hardcodes these effect_ids when emitting top-level
        // handler frames; this test pins the convention.
        let src = "fn main() -> Int ![] { 0 }\n";
        let (cp, errs) = pipeline_checked(src);
        assert!(errs.is_empty(), "expected clean typecheck; got: {errs:?}");
        assert_eq!(cp.effect_ids.get("ArithError"), Some(&0));
        assert_eq!(cp.effect_ids.get("IO"), Some(&1));
        // ArithError op_ids: div_by_zero (alphabetically first), mod_by_zero.
        assert_eq!(
            cp.op_ids
                .get(&("ArithError".to_string(), "div_by_zero".to_string())),
            Some(&0)
        );
        assert_eq!(
            cp.op_ids
                .get(&("ArithError".to_string(), "mod_by_zero".to_string())),
            Some(&1)
        );
        // IO op_ids (alphabetical, post-Task-70):
        // 0=print, 1=println, 2=read_file, 3=read_line, 4=write_file.
        assert_eq!(
            cp.op_ids.get(&("IO".to_string(), "print".to_string())),
            Some(&0)
        );
        assert_eq!(
            cp.op_ids.get(&("IO".to_string(), "println".to_string())),
            Some(&1)
        );
        assert_eq!(
            cp.op_ids.get(&("IO".to_string(), "read_file".to_string())),
            Some(&2)
        );
        assert_eq!(
            cp.op_ids.get(&("IO".to_string(), "read_line".to_string())),
            Some(&3)
        );
        assert_eq!(
            cp.op_ids.get(&("IO".to_string(), "write_file".to_string())),
            Some(&4)
        );
    }

    #[test]
    fn user_redeclaring_io_triggers_e0136() {
        // Plan B Task 57 — synthetic builtin `IO` is injected into
        // `tc.effects` before user effects are walked, so a user
        // declaring `effect IO { ... }` hits the existing E0136
        // duplicate-effect path. Same for `ArithError`.
        let src = "effect IO { println: (String) -> Unit }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0136"),
            "expected E0136 for user redeclaring builtin IO; got: {errs:?}"
        );
    }

    #[test]
    fn op_ids_assigned_alphabetically_per_effect() {
        // Plan B Task 55 — op_ids are assigned alphabetically within
        // each effect, regardless of source order. Declaration order
        // (`get` before `put` here, but `get` < `put` alphabetically
        // either way) does not affect IDs; the State effect's `set`
        // and `get` are picked deliberately so source order would
        // disagree with sorted order.
        let src = "effect State { set: (Int) -> Int, get: () -> Int }\n\
                   fn main() -> Int ![] { 0 }\n";
        let (cp, errs) = pipeline_checked(src);
        assert!(errs.is_empty(), "expected clean typecheck; got: {errs:?}");
        // `get` < `set` alphabetically.
        assert_eq!(
            cp.op_ids.get(&("State".to_string(), "get".to_string())),
            Some(&0)
        );
        assert_eq!(
            cp.op_ids.get(&("State".to_string(), "set".to_string())),
            Some(&1)
        );
    }

    #[test]
    fn op_ids_namespaced_per_effect() {
        // Plan B Task 55 — `(effect_name, op_name)` is the lookup key,
        // so two effects can each have an op named `fail` and they
        // both get op_id 0 within their own effect.
        let src = "effect Raise { fail: () -> Int }\n\
                   effect Catch { fail: () -> Int }\n\
                   fn main() -> Int ![] { 0 }\n";
        let (cp, errs) = pipeline_checked(src);
        assert!(errs.is_empty(), "expected clean typecheck; got: {errs:?}");
        assert_eq!(
            cp.op_ids.get(&("Raise".to_string(), "fail".to_string())),
            Some(&0)
        );
        assert_eq!(
            cp.op_ids.get(&("Catch".to_string(), "fail".to_string())),
            Some(&0)
        );
        // Effect IDs are still distinct.
        assert_ne!(cp.effect_ids.get("Raise"), cp.effect_ids.get("Catch"));
    }

    #[test]
    fn effect_decl_with_invalid_op_type_emits_e0112() {
        // Op signatures are still walked even though E0133 is gone, so
        // an unknown type in an op return position still fires E0112.
        let src = "effect E { fail: () -> Bogus }\nfn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0112"), "expected E0112: {errs:?}");
        assert!(
            !has_code(&errs, "E0133"),
            "E0133 was lifted in Task 55 but still firing: {errs:?}"
        );
    }

    #[test]
    fn well_formed_handle_expr_typechecks_cleanly() {
        // E0134 lifted in Task 55 (Phase 2). A well-formed `handle`
        // expression typechecks cleanly; codegen then either lowers
        // it as a body-pass-through (when the body has no non-IO
        // perform) or reports a clear codegen-time error pointing at
        // the in-progress task.
        let src = "effect E { op: () -> Int }\n\
                   fn main() -> Int ![] { handle 0 with { E.op(k) => 0 } }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "well-formed handle should typecheck cleanly; got: {errs:?}"
        );
    }

    #[test]
    fn handle_expr_with_nested_body_type_error_surfaces() {
        // Confirms `check_handle` walks the body so a nested binop
        // type error surfaces independently of any handler-specific
        // diagnostic. `true && 1` mismatches at the binop's RHS.
        let src = "effect E { op: () -> Int }\n\
                   fn main() -> Int ![] {\n\
                     let n: Int = handle (true && 1) with { E.op(k) => 0 };\n\
                     n\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            errs.iter()
                .any(|e| e.code.as_str().starts_with("E006")
                    || e.code.as_str().starts_with("E004")),
            "expected nested binop type error: {errs:?}"
        );
    }

    #[test]
    fn handle_arm_body_type_mismatch_surfaces_e0044_or_e0065() {
        // Arm bodies are walked; an arm-body type that doesn't match
        // the cross-arm overall type fires E0044 or E0065. Here the
        // handle body is `0: Int` (taken as the implicit return arm)
        // and the op-arm body is `true: Bool` — cross-arm unify fails.
        let src = "effect E { op: () -> Int }\n\
                   fn main() -> Int ![] {\n\
                     let n: Int = handle 0 with { E.op(k) => true };\n\
                     n\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            errs.iter()
                .any(|e| e.code.as_str() == "E0044" || e.code.as_str() == "E0065"),
            "expected arm-body mismatch (E0044/E0065): {errs:?}"
        );
    }

    // ===== Plan B task 54 — effect registry / handler typing / E0220 =====

    #[test]
    fn duplicate_effect_decl_is_e0136() {
        // Two `effect Raise { ... }` declarations in one program. The
        // second offender's name span gets E0136; the first declaration
        // wins in the registry.
        let src = "effect Raise { fail: () -> Int }\n\
                   effect Raise { other: () -> Int }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0136"), "expected E0136: {errs:?}");
    }

    #[test]
    fn duplicate_op_in_effect_decl_is_e0137() {
        // Two `pick` ops on `effect Choose { ... }`. The second
        // occurrence's name span gets E0137; the first op wins inside
        // the canonical EffectDecl.
        let src = "effect Choose {\n\
                     pick: () -> Int,\n\
                     pick: (Int) -> Int,\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0137"), "expected E0137: {errs:?}");
    }

    #[test]
    fn handle_unknown_effect_arm_is_e0138() {
        // `Raise` is not declared anywhere; the arm's effect span
        // gets E0138.
        let src = "fn main() -> Int ![] {\n\
                     handle 0 with { Raise.fail(msg, k) => 0 }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0138"), "expected E0138: {errs:?}");
    }

    #[test]
    fn handle_unknown_op_on_known_effect_is_e0139() {
        // `Raise` is declared with `fail` only; `panic` is unknown.
        let src = "effect Raise { fail: (String) -> Int }\n\
                   fn main() -> Int ![] {\n\
                     handle 0 with { Raise.panic(msg, k) => 0 }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0139"), "expected E0139: {errs:?}");
    }

    #[test]
    fn handle_op_on_mem_marker_effect_is_e0139() {
        // Mem is a builtin marker effect with zero declared ops. Any
        // `Mem.X(...)` arm names an op not declared on the effect, so
        // E0139 must fire — `[DEVIATION Task 66]` calls this out as
        // the expected diagnostic for users who try to mock Mem via
        // a handler. Pins that future v2 generic-Mem work hasn't
        // silently changed Mem's op surface in v1.
        let src = "fn main() -> Int ![Mem] {\n\
                     handle 0 with { Mem.new_array(len, fill, k) => 0 }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0139"),
            "expected E0139 for handle on marker-effect Mem: {errs:?}"
        );
    }

    #[test]
    fn duplicate_handle_arm_is_e0140() {
        // Two arms for the same `Raise.fail`; second is unreachable.
        let src = "effect Raise { fail: (String) -> Int }\n\
                   fn main() -> Int ![] {\n\
                     handle 0 with {\n\
                       Raise.fail(msg, k) => 0,\n\
                       Raise.fail(msg2, k2) => 1,\n\
                     }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0140"), "expected E0140: {errs:?}");
    }

    #[test]
    fn handle_arm_arity_mismatch_is_e0141() {
        // `fail: (String) -> Int` declares 1 user param; arm binds 2
        // user params before `k`.
        let src = "effect Raise { fail: (String) -> Int }\n\
                   fn main() -> Int ![] {\n\
                     handle 0 with { Raise.fail(msg, extra, k) => 0 }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0141"), "expected E0141: {errs:?}");
    }

    #[test]
    fn handle_single_op_effect_with_single_arm_no_e0142() {
        // Stage 6 cleanup: E0142 fires only for multi-op effects with
        // partial coverage. A single-op effect with a single arm is
        // exhaustive by construction; no E0142.
        let src = "effect Raise { fail: () -> Int }\n\
                   fn main() -> Int ![] {\n\
                     handle 0 with { Raise.fail(k) => 1 }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            !has_code(&errs, "E0142"),
            "single-op effect single-arm handler is exhaustive; \
             E0142 should not fire: {errs:?}"
        );
    }

    #[test]
    fn handle_multi_op_effect_with_full_coverage_no_e0142() {
        // Stage 6 cleanup: a multi-op effect with arms covering every
        // declared op is exhaustive; no E0142.
        let src = "effect Choose { left: () -> Int, right: () -> Int }\n\
                   fn main() -> Int ![] {\n\
                     handle 0 with {\n\
                       Choose.left(k) => 1,\n\
                       Choose.right(k) => 2,\n\
                     }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            !has_code(&errs, "E0142"),
            "multi-op effect with full coverage is exhaustive; \
             E0142 should not fire: {errs:?}"
        );
    }

    #[test]
    fn handle_multi_op_effect_with_partial_coverage_emits_e0142() {
        // Stage 6 cleanup: a multi-op effect with arms covering only
        // a subset of declared ops emits E0142 naming the unhandled
        // op(s). Mirrors the option-2 resolution from the Phase 4f
        // latent op_id/arm_count constraint deviation.
        let src = "effect Choose { left: () -> Int, right: () -> Int }\n\
                   fn main() -> Int ![] {\n\
                     handle 0 with { Choose.right(k) => 2 }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0142"), "expected E0142: {errs:?}");
        let msg = errs
            .iter()
            .find(|e| e.code.as_str() == "E0142")
            .map(|e| e.message.clone())
            .unwrap_or_default();
        assert!(
            msg.contains("Choose.left"),
            "E0142 message should name the unhandled op `Choose.left`; got: {msg}"
        );
    }

    #[test]
    fn handle_multi_op_effect_with_no_arms_for_unrelated_effect_no_e0142() {
        // Stage 6 cleanup: E0142 only fires for effects that have AT
        // LEAST ONE arm in the handler. A multi-op effect that the
        // handler doesn't target at all is not "discharged" — the
        // body's row keeps the effect open, no exhaustiveness check
        // applies.
        let src = "effect Raise { fail: () -> Int }\n\
                   effect Choose { left: () -> Int, right: () -> Int }\n\
                   fn main() -> Int ![Choose] {\n\
                     handle 0 with { Raise.fail(k) => 1 }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            !has_code(&errs, "E0142"),
            "Choose has no arms in the handler — exhaustiveness check should \
             not apply to Choose; E0142 should not fire: {errs:?}"
        );
    }

    #[test]
    fn handle_arm_param_bindings_no_spurious_e0046() {
        // Task 53 review item 10: the arm body's references to op-
        // declared params (`msg`) must resolve to a binding installed
        // by the handler-typing walk. Without item 10, this would
        // fire spurious E0046 alongside E0134.
        let src = "effect Raise { fail: (String) -> Int }\n\
                   fn use_msg(s: String) -> Int ![] { 0 }\n\
                   fn main() -> Int ![] {\n\
                     handle 0 with { Raise.fail(msg, k) => use_msg(msg) }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            !has_code(&errs, "E0046"),
            "arm-param binding `msg` must not fire E0046; got: {errs:?}"
        );
    }

    #[test]
    fn handle_arm_k_binding_no_spurious_e0046() {
        // Task 53 review item 10: arm body references to `k` must
        // resolve to the continuation binding installed by check_handle.
        let src = "effect Raise { fail: (String) -> Int }\n\
                   fn main() -> Int ![] {\n\
                     handle 0 with { Raise.fail(msg, k) => k(0) }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            !has_code(&errs, "E0046"),
            "continuation `k` binding must not fire E0046; got: {errs:?}"
        );
    }

    #[test]
    fn handle_return_arm_v_binding_no_spurious_e0046() {
        // Task 53 review item 10 for the return-arm side: `v` must be
        // bound in the return-arm body's env.
        let src = "fn main() -> Int ![] {\n\
                     handle 0 with {\n\
                       return(v) => v,\n\
                       E.op(k) => 0,\n\
                     }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            !has_code(&errs, "E0046"),
            "return-arm binding `v` must not fire E0046; got: {errs:?}"
        );
    }

    #[test]
    fn handle_arm_param_typed_from_op_decl() {
        // The arm's `msg` param is bound to `String` (from `fail`'s
        // signature). Calling a `(Int) -> Int` fn with `msg` fires
        // E0044 because msg is String, not Int — confirming the
        // binding carries the op-declared type, not just `Unit`.
        let src = "effect Raise { fail: (String) -> Int }\n\
                   fn double(n: Int) -> Int ![] { n + n }\n\
                   fn main() -> Int ![] {\n\
                     handle 0 with { Raise.fail(msg, k) => double(msg) }\n\
                   }\n";
        let errs = pipeline(src);
        // E0044 (or E0065 depending on path) for String passed where
        // Int expected.
        assert!(
            errs.iter().any(|e| e.code.as_str() == "E0044"),
            "expected E0044 for String→Int mismatch: {errs:?}"
        );
    }

    #[test]
    fn handle_body_can_perform_discharged_effect() {
        // The handle body sees the discharged effect in its row, so
        // `perform Raise.fail(...)` inside the body does not fire
        // E0042 (effect-not-in-row) even though `Raise` is not in the
        // surrounding fn's row.
        let src = "effect Raise { fail: (String) -> Int }\n\
                   fn main() -> Int ![] {\n\
                     handle (perform Raise.fail(\"x\")) with {\n\
                       Raise.fail(msg, k) => 0,\n\
                     }\n\
                   }\n";
        let errs = pipeline(src);
        // E0134 was lifted in Phase 2 (`2d69b52`) so it doesn't fire
        // here. No E0042 should fire for the body's perform — Raise
        // is in the body's row (caller's row plus discharged effects)
        // even though it isn't in the surrounding fn's row.
        assert!(
            !has_code(&errs, "E0042"),
            "handle body's perform of discharged effect must not E0042: {errs:?}"
        );
    }

    #[test]
    fn handle_arm_body_perform_of_discharged_effect_fires_e0042() {
        // Arm bodies run at caller's row level — the discharged
        // effect is *not* in scope inside an arm body. This test
        // pins that contract: a perform of `Raise.fail` inside the
        // arm body fires E0042 because Raise is not in the
        // surrounding fn's row (only the body sees it).
        let src = "effect Raise { fail: (String) -> Int }\n\
                   fn main() -> Int ![] {\n\
                     handle 0 with {\n\
                       Raise.fail(msg, k) => perform Raise.fail(\"loop\"),\n\
                     }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0042"),
            "arm-body perform of discharged effect must E0042 (caller's row only): {errs:?}"
        );
    }

    #[test]
    fn perform_via_registry_typechecks() {
        // A user-declared non-IO effect can be performed from a fn
        // whose row lists it. The E0042 / IO special case do not
        // apply; the registry route handles it. E0133 was lifted in
        // the Task 55 foundation phase (`b3af204`).
        let src = "effect Log { write: (String) -> Unit }\n\
                   fn main() -> Int ![Log] {\n\
                     perform Log.write(\"hi\");\n\
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            !has_code(&errs, "E0042"),
            "registered effect's perform must not E0042: {errs:?}"
        );
    }

    #[test]
    fn perform_via_registry_arity_mismatch_is_e0043() {
        // `Log.write: (String) -> Unit` declares 1 arg; calling with
        // 2 args fires E0043.
        let src = "effect Log { write: (String) -> Unit }\n\
                   fn main() -> Int ![Log] {\n\
                     perform Log.write(\"hi\", \"extra\");\n\
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0043"), "expected E0043: {errs:?}");
    }

    #[test]
    fn perform_via_registry_arg_type_mismatch_is_e0044() {
        // `Log.write: (String) -> Unit` declares String; calling
        // with Int fires E0044 via unify_ty.
        let src = "effect Log { write: (String) -> Unit }\n\
                   fn main() -> Int ![Log] {\n\
                     perform Log.write(42);\n\
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0044"), "expected E0044: {errs:?}");
    }

    #[test]
    fn linearity_zero_uses_is_fine() {
        // Early-exit handler (no `k` reference) is fine: zero uses.
        let src = "effect Raise { fail: (String) -> Int }\n\
                   fn main() -> Int ![] {\n\
                     handle 0 with { Raise.fail(msg, k) => 0 }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            !has_code(&errs, "E0220"),
            "zero uses of k is fine, must not E0220: {errs:?}"
        );
    }

    #[test]
    fn linearity_one_use_is_fine() {
        // Single `k` invocation is fine.
        let src = "effect Raise { fail: (String) -> Int }\n\
                   fn main() -> Int ![] {\n\
                     handle 0 with { Raise.fail(msg, k) => k(0) }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            !has_code(&errs, "E0220"),
            "single use of k is fine, must not E0220: {errs:?}"
        );
    }

    #[test]
    fn linearity_two_uses_in_sequence_is_e0220() {
        // Sequential composition (block stmt + tail; binary op) sums
        // counts. Two uses on the same path: E0220.
        let src = "effect Raise { fail: (String) -> Int }\n\
                   fn main() -> Int ![] {\n\
                     handle 0 with { Raise.fail(msg, k) => k(0) + k(1) }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0220"), "expected E0220: {errs:?}");
    }

    #[test]
    fn linearity_branches_use_k_independently_is_fine() {
        // if-then-else: max across branches. Each branch uses k once
        // (max=1). No E0220.
        let src = "effect Raise { fail: (String) -> Int }\n\
                   fn main() -> Int ![] {\n\
                     handle 0 with {\n\
                       Raise.fail(msg, k) => if true { k(0) } else { k(1) },\n\
                     }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            !has_code(&errs, "E0220"),
            "branch-disjoint uses of k are fine, must not E0220: {errs:?}"
        );
    }

    #[test]
    fn linearity_branch_then_extra_use_is_e0220() {
        // if-then-else where one branch uses k AND a sequential `k`
        // follows the if: max(branch)=1 + 1 outside = 2. E0220.
        let src = "effect Raise { fail: (String) -> Int }\n\
                   fn main() -> Int ![] {\n\
                     handle 0 with {\n\
                       Raise.fail(msg, k) => if true { k(0) } else { 0 } + k(1),\n\
                     }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(has_code(&errs, "E0220"), "expected E0220: {errs:?}");
    }

    #[test]
    fn linearity_lambda_capturing_k_is_e0220() {
        // Conservative-capture rule: any reference to k inside a
        // lambda body saturates to 2. E0220 fires regardless of how
        // many syntactic occurrences appear inside the lambda.
        let src = "effect Raise { fail: (String) -> Int }\n\
                   fn main() -> Int ![] {\n\
                     handle 0 with {\n\
                       Raise.fail(msg, k) => (fn () -> Int ![] => k(0))(),\n\
                     }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0220"),
            "lambda capturing k must E0220 (conservative rule): {errs:?}"
        );
    }

    #[test]
    fn linearity_multi_shot_skips_check() {
        // `effect Choose resumes: many` opts in to multi-shot; the
        // linearity check is skipped so k(0) + k(1) is fine.
        let src = "effect Choose resumes: many { pick: () -> Int }\n\
                   fn main() -> Int ![] {\n\
                     handle 0 with { Choose.pick(k) => k(1) + k(2) }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            !has_code(&errs, "E0220"),
            "multi-shot effect must skip linearity check: {errs:?}"
        );
    }

    #[test]
    fn linearity_zero_uses_with_branches_is_fine() {
        // Both branches return 0; neither uses k. Max=0, no E0220.
        let src = "effect Raise { fail: (String) -> Int }\n\
                   fn main() -> Int ![] {\n\
                     handle 0 with {\n\
                       Raise.fail(msg, k) => if true { 0 } else { 1 },\n\
                     }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            !has_code(&errs, "E0220"),
            "branches that don't use k are fine: {errs:?}"
        );
    }

    #[test]
    fn linearity_count_in_expr_zero_in_lit() {
        // Direct unit test of the counter on a literal — no `k` ref.
        let span = Span::synthetic("x.sigil");
        let e = Expr::IntLit(0, span);
        assert_eq!(count_continuation_uses(&e, "k"), 0);
    }

    #[test]
    fn linearity_count_in_expr_saturates_at_two() {
        // Direct test that sequential composition saturates at 2.
        // `(k + k) + k` has 3 syntactic uses but the saturating-add
        // collapses early; the helper returns at most 2 so the
        // > 1 vs <= 1 caller distinction is preserved with no overflow.
        let span = Span::synthetic("x.sigil");
        let k = || Expr::Ident("k".to_string(), span.clone());
        let lhs = Expr::Binary {
            op: BinOp::Add,
            lhs: Box::new(k()),
            rhs: Box::new(k()),
            span: span.clone(),
        };
        let three = Expr::Binary {
            op: BinOp::Add,
            lhs: Box::new(lhs),
            rhs: Box::new(k()),
            span,
        };
        assert_eq!(count_continuation_uses(&three, "k"), 2);
    }

    #[test]
    fn linearity_count_in_expr_lambda_capture_returns_two() {
        // Direct test: a lambda body containing `k` returns 2 even
        // for a single syntactic occurrence inside the lambda.
        let span = Span::synthetic("x.sigil");
        let body = Expr::Ident("k".to_string(), span.clone());
        let lambda = Expr::Lambda {
            params: Vec::new(),
            return_type: TypeExpr::Named("Int".to_string(), span.clone()),
            effects: Vec::new(),
            effect_row_var: None,
            body: Box::new(body),
            span,
        };
        assert_eq!(count_continuation_uses(&lambda, "k"), 2);
    }

    #[test]
    fn linearity_count_in_expr_branches_take_max() {
        // Direct test: if-then-else of single-`k`-each branches takes
        // max, which is 1. The cond also contributes 0 here.
        let span = Span::synthetic("x.sigil");
        let k_block = Block {
            stmts: Vec::new(),
            tail: Some(Expr::Ident("k".to_string(), span.clone())),
            span: span.clone(),
        };
        let if_expr = Expr::If {
            cond: Box::new(Expr::BoolLit(true, span.clone())),
            then_block: Box::new(k_block.clone()),
            else_block: Box::new(k_block),
            span,
        };
        assert_eq!(count_continuation_uses(&if_expr, "k"), 1);
    }

    #[test]
    fn handle_overall_type_is_body_type_without_return_arm() {
        // Without a return arm, the handler returns the body's type.
        // Here body is Int (literal 0); handle's overall type is Int.
        // The let-binding accepts Int with no E0044/E0045.
        let src = "effect Raise { fail: (String) -> Int }\n\
                   fn main() -> Int ![] {\n\
                     let n: Int = handle 0 with { Raise.fail(msg, k) => 0 };\n\
                     n\n\
                   }\n";
        let errs = pipeline(src);
        // Internally the handler-overall type *should* be Int. The
        // let-binding diagnostic only fires when types disagree —
        // this assertion is the absence of a mismatch.
        assert!(
            !errs.iter().any(|e| e.code.as_str() == "E0045"),
            "let n: Int = ... where handle returns Int must not E0045: {errs:?}"
        );
    }

    #[test]
    fn handle_overall_type_uses_return_arm_when_present() {
        // With a return arm `return(v) => "x"`, the handler returns
        // String regardless of the body's Int. The let-binding
        // declared as `String` is consistent with this.
        let src = "effect Raise { fail: (String) -> Int }\n\
                   fn main() -> Int ![] {\n\
                     let s: String = handle 0 with {\n\
                       return(v) => \"x\",\n\
                       Raise.fail(msg, k) => \"err\",\n\
                     };\n\
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            !errs.iter().any(|e| e.code.as_str() == "E0045"),
            "let s: String = handle ... with return arm returning String must not E0045: {errs:?}"
        );
    }

    #[test]
    fn handle_arm_body_unifies_with_handler_overall() {
        // Two arms must produce the same handler-overall type. Here
        // body is Int (from `0`) and arm returns String — unify_ty
        // fails and emits E0044 against the arm's body span.
        let src = "effect Raise { fail: (String) -> Int }\n\
                   fn main() -> Int ![] {\n\
                     let n: Int = handle 0 with {\n\
                       Raise.fail(msg, k) => \"err\",\n\
                     };\n\
                     n\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            errs.iter().any(|e| e.code.as_str() == "E0044"),
            "arm body String must not unify with body Int (handler-overall): {errs:?}"
        );
    }

    // ===== Plan B task 54 — review-fixup follow-ups (PR #20) =============

    #[test]
    fn linearity_shadowed_k_does_not_count() {
        // Direct unit test on `count_continuation_uses` for the
        // `Stmt::Let` shadow-suspension logic in `count_in_block`.
        // Block: `let k: Int = 1; k + k` — the let shadows `k_name`
        // so the post-let `k + k` uses don't count toward the outer
        // continuation. Final count = 0.
        //
        // The full-pipeline equivalent (a `let k = 5` inside a
        // handler arm body whose arm declares `k`) would trip
        // `env_insert`'s debug-assert against shadowing — that's a
        // *typecheck-side* invariant unrelated to the linearity
        // counter's own shadow handling. Direct unit test on the
        // helper avoids the env_insert path.
        let span = Span::synthetic("x.sigil");
        let block = Block {
            stmts: vec![Stmt::Let(LetStmt {
                name: "k".to_string(),
                ty: TypeExpr::Named("Int".to_string(), span.clone()),
                value: Expr::IntLit(1, span.clone()),
                span: span.clone(),
            })],
            tail: Some(Expr::Binary {
                op: BinOp::Add,
                lhs: Box::new(Expr::Ident("k".to_string(), span.clone())),
                rhs: Box::new(Expr::Ident("k".to_string(), span.clone())),
                span: span.clone(),
            }),
            span,
        };
        let block_expr = Expr::Block(Box::new(block));
        assert_eq!(count_continuation_uses(&block_expr, "k"), 0);
    }

    #[test]
    fn linearity_count_in_block_sums_uses() {
        // Direct unit test pinning sequential composition: a Block
        // with two `Stmt::Expr(Ident("k"))` returns 2 (each stmt
        // contributes 1; `count_in_block` sums them).
        let span = Span::synthetic("x.sigil");
        let k = || Expr::Ident("k".to_string(), span.clone());
        let block = Block {
            stmts: vec![Stmt::Expr(k()), Stmt::Expr(k())],
            tail: None,
            span,
        };
        let block_expr = Expr::Block(Box::new(block));
        assert_eq!(count_continuation_uses(&block_expr, "k"), 2);
    }

    #[test]
    fn linearity_nested_handle_inner_arm_shadow_does_not_count_outer_k() {
        // Direct linearity-pipeline test for the `Expr::Handle` case
        // in `count_in_expr`: an inner handle's op-arm rebinds
        // `k_name`, so syntactic `k` references inside the inner arm
        // body resolve to the inner k, not the outer continuation.
        // Outer count for `Raise.fail`'s k stays at 0 even though
        // there is a syntactic `k(...)` inside the nested arm tree.
        let src = "effect Raise { fail: (String) -> Int }\n\
                   effect Other { ping: () -> Int }\n\
                   fn main() -> Int ![] {\n\
                     handle 0 with {\n\
                       Raise.fail(msg, k) => handle 0 with {\n\
                         Other.ping(k) => k(0),\n\
                       },\n\
                     }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            !has_code(&errs, "E0220"),
            "inner-arm rebinding of `k` must shadow outer `k` for linearity: {errs:?}"
        );
    }

    #[test]
    fn cross_arm_state_generic_param_consistency_fires_e0044() {
        // The `effect_substs` cache is the load-bearing mechanism
        // for keeping `effect State[A]`'s `A` consistent across
        // `get` and `set` arms in a single handle. Without it, each
        // arm would allocate its own fresh `Ty::Var` for `A` and
        // the program below would silently typecheck.
        //
        // Discriminating shape: `State.get(k) => k(42)` pins
        // `A = Int` via `k`'s param-type unification; `State.set(v, k)`
        // therefore binds `v: A = Int`, so `use_str(v)` fires E0044
        // (Int passed where String expected). If the cache were
        // broken, `v: A_fresh` would unify cleanly with String at
        // the `use_str` call site and no E0044 would surface — that
        // would be a soundness regression.
        let src = "effect State[A] {\n\
                     get: () -> A,\n\
                     set: (A) -> Unit,\n\
                   }\n\
                   fn use_str(s: String) -> Int ![] { 0 }\n\
                   fn main() -> Int ![] {\n\
                     handle 0 with {\n\
                       State.get(k) => k(42),\n\
                       State.set(v, k) => use_str(v),\n\
                     }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0044"),
            "State[A] cross-arm cache must propagate A=Int from get to set so use_str(v) fires E0044: {errs:?}"
        );
    }

    #[test]
    fn handle_arm_k_call_with_wrong_arg_is_e0044() {
        // `k`'s type is `Continuation(op_ret_ty) -> handler_overall`
        // post-Task-117. For `Raise.fail: (String) -> Int`, op_ret is
        // Int (the op's return). Calling k with a String fires E0044
        // via check_call's Continuation arm (synthesizes a single-param
        // FnSig from op_ret/ret and unifies arg against op_ret). Both
        // sides of the failing unify are non-Continuation primitives,
        // so the E0145 broad arm doesn't kick in — generic E0044
        // ("type mismatch") is the right diagnostic.
        let src = "effect Raise { fail: (String) -> Int }\n\
                   fn main() -> Int ![] {\n\
                     handle 0 with {\n\
                       Raise.fail(msg, k) => k(\"not_int\"),\n\
                     }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0044"),
            "k call with wrong arg type must E0044 (Continuation's op_ret unifies arg \
             against the op's return type, both non-Continuation): {errs:?}"
        );
    }

    // ----------------------------------------------------------------
    // Plan D Task 117 — E0145 escape barrier (PR #60 substrate tests)
    // ----------------------------------------------------------------
    //
    // These tests pin the E0145 broad-arm in `unify_ty` (added in
    // PR #60 fix `503308d`), the cross-handle scope_id mismatch arm
    // (added in `a3de60f`), and the `ty_display` scope-omission fix
    // (added in `6c6fb41`).
    //
    // Positive shapes (`let f = k; f(arg)` single-shot + multi-shot)
    // are out of scope here — they need the codegen-walker delta +
    // closure_convert + codegen let-bound k dispatch, which ship in
    // a follow-up PR. Lambda-capture-of-k → E0145 is also out of
    // scope; today the linearity check fires E0220 for one-shot and
    // multi-shot bypasses linearity (the existing
    // `linearity_lambda_capturing_k_is_e0220` test pins one-shot
    // E0220; lambda-capture-k inheritance with E0145 ships in PR (b)
    // per `[DEVIATION Task 117]`).

    #[test]
    fn k_returned_from_fn_with_non_continuation_ret_fires_e0145() {
        // `fn ret_k() -> Int` whose body's value is `k`. Arm body's
        // type is `Continuation(op_ret) -> handler_overall`.
        // handler_overall var binds to Continuation via the arm-body
        // unify. The handle's overall type is Continuation. The fn
        // body's type is Continuation. `check_fn` unifies the fn's
        // declared return type (Int) against the body type:
        // `unify_ty(Int, Continuation)` → E0145 broad arm fires
        // ("continuation `k` cannot escape its handle's arm body").
        let src = "effect Raise { fail: () -> Int }\n\
                   fn ret_k() -> Int ![Raise] {\n\
                     handle 0 with {\n\
                       Raise.fail(k) => k,\n\
                     }\n\
                   }\n\
                   fn main() -> Int ![Raise] { ret_k() }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0145"),
            "returning `k` from a fn whose declared ret type is non-Continuation must \
             fire E0145 (escape barrier): {errs:?}"
        );
    }

    #[test]
    fn k_passed_as_fn_arg_of_non_continuation_param_fires_e0145() {
        // `fn take_fn(f: (Int) -> Int ![]) -> Int ![]`. Calling
        // `take_fn(k)` from inside a handler arm: check_call's
        // arg-unify pass walks `unify_ty(Fn(Int) -> Int, Continuation)`
        // → E0145 broad arm fires. The Fn-vs-Continuation unify fails
        // structurally (different type ctors), but the broad arm
        // catches it before the generic E0044 catchall.
        let src = "effect Raise { fail: () -> Int }\n\
                   fn take_fn(f: (Int) -> Int ![]) -> Int ![] { f(0) }\n\
                   fn main() -> Int ![Raise] {\n\
                     handle 0 with {\n\
                       Raise.fail(k) => take_fn(k),\n\
                     }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0145"),
            "passing `k` as a fn-arg of non-Continuation declared type must fire E0145 \
             (escape barrier): {errs:?}"
        );
    }

    #[test]
    fn k_stored_in_user_type_field_fires_e0145() {
        // ctor-construction site for a non-generic user type whose
        // field has a Fn-typed declared TypeExpr. Storing `k` at that
        // field unifies `Fn(Int) -> Int` against `Continuation(Int) ->
        // handler_overall` → E0145 broad arm. Distinct from the
        // generic-instantiation case (`Wrap[A]` with `Wrap(k)`): a
        // generic field would resolve A → Continuation via the
        // (Var, other) bind_ty_var arm and bypass E0145 here; that
        // bypass is the bind_ty_var precision fix deferred to PR (b)
        // ("complete the E0145 escape barrier") per the DEVIATIONS
        // entry.
        let src = "type WrapFn = | WrapFn((Int) -> Int ![])\n\
                   effect Raise { fail: () -> Int }\n\
                   fn main() -> Int ![Raise] {\n\
                     handle 0 with {\n\
                       Raise.fail(k) => {\n\
                         let _: WrapFn = WrapFn(k);\n\
                         0\n\
                       },\n\
                     }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0145"),
            "storing `k` in a user-type field whose declared type is Fn (not generic) \
             must fire E0145: {errs:?}"
        );
    }

    #[test]
    fn cross_handle_k_unification_fires_e0145_with_scope_mismatch() {
        // Two nested `handle` expressions, both discharging `E1`.
        // Inner handle has two arms; arm 1's body returns the OUTER
        // handle's `k1` (a Continuation tagged with the outer
        // handle's scope_id), arm 2's body returns the INNER handle's
        // `ki2` (tagged with the inner handle's scope_id). Cross-arm
        // unification of the two arm bodies' types into the inner
        // handle's `handler_overall` var produces a
        // `(Ty::Continuation, Ty::Continuation)` unify with mismatched
        // Concrete scope_ids — fires the specific cross-handle E0145
        // arm (`scope {n} cannot unify with scope {m}`).
        let src = "effect E1 { op1: () -> Int, op2: () -> Int }\n\
                   fn main() -> Int ![] {\n\
                     handle 0 with {\n\
                       E1.op1(k1) =>\n\
                         handle 0 with {\n\
                           E1.op1(ki1) => k1,\n\
                           E1.op2(ki2) => ki2,\n\
                         },\n\
                       E1.op2(k2) => 0,\n\
                     }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0145"),
            "cross-handle k unification with mismatched scope_ids must fire E0145: \
             {errs:?}"
        );
    }

    #[test]
    fn k_passed_to_generic_fn_param_fires_e0145() {
        // PR #60 review #3 (HEAD `decb6d8`) high-severity blocker:
        // close the bind_ty_var bypass. `id(k)` for generic
        // `fn id[A](x: A) -> A` instantiates A := fresh Var; arg-
        // unify would normally bind A → Continuation through the
        // (Var, other) bind_ty_var arm — the same arm `let f = k`
        // legitimately uses for local aliasing. Without the
        // precision check at check_call, A binds, `id(k)` returns
        // Continuation, and monomorphize later panics in
        // `ty_to_type_expr(Continuation)` because Continuation has
        // no surface TypeExpr. The check_call precision gate
        // catches this BEFORE the bind, firing E0145 and skipping
        // unify_ty so A stays unbound.
        //
        // Test shape uses `discard[A](x: A) -> Int` so the result
        // type is structurally Int, NOT Continuation — without the
        // precision check, typecheck would silently pass (broad
        // unify_ty arm doesn't fire on Int return), and mono would
        // crash with `unreachable!()` on Continuation in
        // ty_to_type_expr. With the precision check, E0145 fires
        // at the call site and the cascade is short-circuited.
        // Tight regression: removing the precision check makes this
        // test fail (E0145 stops appearing in pipeline errors).
        let src = "fn id[A](x: A) -> A ![] { x }\n\
                   fn discard[A](x: A) -> Int ![] { 0 }\n\
                   effect Raise { fail: () -> Int }\n\
                   fn main() -> Int ![Raise] {\n\
                     handle 0 with {\n\
                       Raise.fail(k) => discard(id(k)),\n\
                     }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0145"),
            "passing `k` to a generic-typed param must fire E0145 at the bind \
             site — closes the bind_ty_var bypass that would otherwise propagate \
             Continuation through generics and panic in mono::ty_to_type_expr. \
             This shape (discard returns Int, not the generic) doesn't trigger \
             the broad unify arm; only the precision check at check_call fires \
             E0145: {errs:?}"
        );
    }

    #[test]
    fn k_let_aliased_then_passed_to_generic_fn_fires_e0145() {
        // Regression: ensure the precision check fires even when k
        // flows through an intermediate let-binding (the let-RHS
        // path is intentionally untouched by the precision check —
        // `let f = k` is the legitimate aliasing case — but `f`
        // then passed to a generic param re-enters check_call's
        // gate). Same `discard[A] -> Int` discriminator as above:
        // removing the precision check would silently typecheck-pass
        // and panic in mono.
        let src = "fn id[A](x: A) -> A ![] { x }\n\
                   fn discard[A](x: A) -> Int ![] { 0 }\n\
                   effect Raise { fail: () -> Int }\n\
                   fn main() -> Int ![Raise] {\n\
                     handle 0 with {\n\
                       Raise.fail(k) => {\n\
                         let f: Int = discard(id(k));\n\
                         f\n\
                       },\n\
                     }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0145"),
            "passing `k` (or a let-aliased k) into a generic-typed param chain \
             must fire E0145 (the precision check sees the resolved arg type \
             post-deref): {errs:?}"
        );
    }

    // ----------------------------------------------------------------
    // Plan D Task 117 (continuation-surface) — Continuation type-
    // position surface form. Pins the parser + typecheck-level
    // behavior of `Continuation[op_ret, ret]` annotations: in-arm
    // resolves to Ty::Continuation; outside-arm fires E0145; wrong
    // arity fires E0129.
    // ----------------------------------------------------------------

    #[test]
    fn continuation_annotation_inside_arm_body_typechecks() {
        // `let f: Continuation[Int, Int] = k;` inside the arm body
        // resolves the annotation to Ty::Continuation tagged with
        // the enclosing handle's scope_id. The let-RHS is k (also
        // Ty::Continuation with the same scope_id); unify_ty
        // succeeds. Body returns 0 — handle returns 0.
        let src = "effect Raise { fail: () -> Int }\n\
                   fn main() -> Int ![Raise] {\n\
                     handle 0 with {\n\
                       Raise.fail(k) => {\n\
                         let f: Continuation[Int, Int] = k;\n\
                         0\n\
                       },\n\
                     }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            !has_code(&errs, "E0145") && !has_code(&errs, "E0044"),
            "Continuation[Int, Int] annotation inside arm body must typecheck \
             cleanly (resolves to the same Ty::Continuation k is bound at): \
             {errs:?}"
        );
    }

    #[test]
    fn continuation_annotation_outside_arm_body_fires_e0145() {
        // Annotation outside any handler arm body — fires E0145
        // ("Continuation annotations are only valid inside a
        // handler arm body"). The diagnostic is precise: it points
        // at the annotation's span, not at a generic "unknown
        // type Continuation" miss.
        let src = "fn main() -> Int ![] {\n\
                     let f: Continuation[Int, Int] = 0;\n\
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0145"),
            "Continuation annotation outside arm body must fire E0145: {errs:?}"
        );
    }

    #[test]
    fn continuation_annotation_wrong_arity_fires_e0129() {
        // Continuation expects exactly 2 type args (op_ret, ret).
        // Anything else fires E0129 ("type expects N type
        // argument(s), got M") — same diagnostic shape as user
        // generic types.
        let src = "effect Raise { fail: () -> Int }\n\
                   fn main() -> Int ![Raise] {\n\
                     handle 0 with {\n\
                       Raise.fail(k) => {\n\
                         let f: Continuation[Int] = k;\n\
                         0\n\
                       },\n\
                     }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0129"),
            "Continuation with wrong arity must fire E0129: {errs:?}"
        );
    }

    #[test]
    fn continuation_annotation_with_mismatched_scope_fires_e0145_via_unify() {
        // Inner arm body has `let f: Continuation[Int, Int] = k_outer;`
        // where k_outer comes from the surrounding outer handle's arm.
        // The annotation resolves to Ty::Continuation tagged with the
        // INNER scope_id; k_outer is tagged with the OUTER scope_id.
        // unify_ty fires the cross-handle E0145 (the specific
        // n != m message). This pins the interaction between the
        // type-position surface and the existing cross-handle arm
        // in unify_ty.
        let src = "effect E1 { op1: () -> Int }\n\
                   fn main() -> Int ![] {\n\
                     handle 0 with {\n\
                       E1.op1(k_outer) =>\n\
                         handle 0 with {\n\
                           E1.op1(k_inner) => {\n\
                             let f: Continuation[Int, Int] = k_outer;\n\
                             0\n\
                           },\n\
                         },\n\
                     }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0145"),
            "Continuation annotation with mismatched enclosing scope vs RHS k must \
             fire E0145 via cross-handle unify: {errs:?}"
        );
    }

    // ----------------------------------------------------------------
    // Plan D Task 117 (continuation-surface) — PR #62 followup
    // hardening tests. These pin the shadowing-hygiene fix in
    // `apply_subst_to_expr`/`apply_subst_to_block`, the type-decl
    // pre-pass reservation of `Continuation`, the non-bare-Ident-RHS
    // rejection at the let-stmt typecheck site, and the per-arity
    // diagnostic for `Continuation[..]` surface form.
    // ----------------------------------------------------------------

    #[test]
    fn continuation_user_type_decl_is_reserved_e0113() {
        // Reserved name: `type Continuation[A, B] { ... }` would
        // silently lose under `ty_from_type_expr`'s special-case
        // shunt; reject at the type-decl pre-pass with E0113.
        let src = "type Continuation = | Foo\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0113"),
            "user `type Continuation = ...` must fire E0113 (reserved name): {errs:?}"
        );
    }

    #[test]
    fn continuation_arity_one_fires_e0129() {
        let src = "effect Raise { fail: () -> Int }\n\
                   fn main() -> Int ![Raise] {\n\
                     handle 0 with {\n\
                       Raise.fail(k) => {\n\
                         let f: Continuation[Int] = k;\n\
                         0\n\
                       },\n\
                     }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0129"),
            "Continuation arity=1 must fire E0129: {errs:?}"
        );
    }

    #[test]
    fn continuation_arity_three_fires_e0129() {
        let src = "effect Raise { fail: () -> Int }\n\
                   fn main() -> Int ![Raise] {\n\
                     handle 0 with {\n\
                       Raise.fail(k) => {\n\
                         let f: Continuation[Int, Int, Int] = k;\n\
                         0\n\
                       },\n\
                     }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0129"),
            "Continuation arity=3 must fire E0129: {errs:?}"
        );
    }

    #[test]
    fn continuation_let_with_non_bare_ident_rhs_fires_e0145() {
        // PR #62 review #4: typecheck-vs-codegen gap. `let f:
        // Continuation = if cond { k } else { k }` typechecks (RHS
        // is Ty::Continuation each branch) but the desugar can't
        // statically reduce conditional/matched RHS shapes. Fire
        // E0145 at the let-stmt with a precise diagnostic so the
        // user knows what's accepted.
        let src = "effect Raise { fail: () -> Int }\n\
                   fn main() -> Int ![Raise] {\n\
                     handle 0 with {\n\
                       Raise.fail(k) => {\n\
                         let f: Continuation[Int, Int] = if true { k } else { k };\n\
                         f(0)\n\
                       },\n\
                     }\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0145"),
            "let-binding of Continuation type with non-bare-Ident RHS must fire \
             E0145 (only bare-Ident RHS is supported in v1): {errs:?}"
        );
    }

    #[test]
    fn shadowing_hygiene_lambda_param_shadow_typechecks_cleanly() {
        // PR #62 review case (a): a lambda inside an arm body whose
        // param name shadows the let-bound continuation alias. Prior
        // to the shadowing fix in `apply_subst_to_expr`'s Lambda
        // arm, the desugar rewrote the lambda body's `f → k`,
        // producing `fn (f: Int) -> Int => k + 1` — `k` is
        // Continuation; `+1` against Continuation would fire E0044
        // (or E0145 broad arm). Post-fix, the lambda param shadows
        // the alias, the subst is filtered out, and the lambda body
        // stays unchanged.
        let src = "effect Raise { fail: () -> Int }\n\
                   fn run() -> Int ![] {\n\
                     handle 0 with {\n\
                       Raise.fail(k) => {\n\
                         let f: Continuation[Int, Int] = k;\n\
                         let l: (Int) -> Int ![] = fn (f: Int) -> Int ![] => f + 1;\n\
                         l(0)\n\
                       },\n\
                     }\n\
                   }\n\
                   fn main() -> Int ![] { run() }\n";
        let errs = pipeline(src);
        assert!(
            !has_code(&errs, "E0044") && !has_code(&errs, "E0145"),
            "lambda param `f` shadowing the Continuation alias must NOT corrupt the \
             lambda body via desugar substitution (no E0044 / E0145 from synthetic \
             substitution sites): {errs:?}"
        );
    }

    #[test]
    fn shadowing_hygiene_match_pattern_shadow_typechecks_cleanly() {
        // PR #62 review case (b): a match arm pattern binding
        // shadows the let-bound continuation alias. Prior to the
        // shadowing fix in `apply_subst_to_expr`'s Match arm, the
        // desugar rewrote `Some(f) => f` to `Some(f) => k`, breaking
        // the pattern binding. Post-fix, the pattern bindings are
        // filtered from the subst before recursing into arm bodies.
        let src = "type Maybe = | None | Some(Int)\n\
                   effect Raise { fail: () -> Int }\n\
                   fn run() -> Int ![] {\n\
                     handle 0 with {\n\
                       Raise.fail(k) => {\n\
                         let f: Continuation[Int, Int] = k;\n\
                         match Some(7) {\n\
                           Some(f) => f,\n\
                           None => 0,\n\
                         }\n\
                       },\n\
                     }\n\
                   }\n\
                   fn main() -> Int ![] { run() }\n";
        let errs = pipeline(src);
        assert!(
            !has_code(&errs, "E0044") && !has_code(&errs, "E0145"),
            "match arm pattern binding `f` shadowing the Continuation alias must NOT \
             corrupt the arm body via desugar substitution (no E0044 / E0145 from \
             synthetic substitution sites): {errs:?}"
        );
    }

    #[test]
    fn shadowing_hygiene_inner_handle_k_name_reuse_typechecks_cleanly() {
        // PR #62 review case (c): an inner handle's arm reuses the
        // outer alias name as its own k_name. Prior to the
        // shadowing fix in `apply_subst_to_expr`'s Handle arm, the
        // outer subst `f → k_outer` was applied to the inner arm
        // body's `f(0)` → `k_outer(0)`. The inner arm's k_name was
        // `f`, so `k_outer` was a free reference at the inner arm,
        // and the inner walker accepted it as a regular ident →
        // miscompile. Post-fix, the inner arm's k_name is filtered
        // from the subst before recursing.
        let src = "effect Outer { op_o: () -> Int }\n\
                   effect Inner { op_i: () -> Int }\n\
                   fn run() -> Int ![] {\n\
                     handle 0 with {\n\
                       Outer.op_o(k_outer) => {\n\
                         let f: Continuation[Int, Int] = k_outer;\n\
                         handle 0 with {\n\
                           Inner.op_i(f) => f(0),\n\
                         }\n\
                       },\n\
                     }\n\
                   }\n\
                   fn main() -> Int ![] { run() }\n";
        let errs = pipeline(src);
        // Inner k_name `f` shadows the outer alias key `f`. The
        // subst is filtered at the inner Handle's op-arm; inner
        // arm body's `f(0)` stays as `f(0)`, resolving to inner k.
        // Should typecheck cleanly (no E0044 / E0145 / E0046 from
        // a corrupted reference).
        assert!(
            !has_code(&errs, "E0044") && !has_code(&errs, "E0145") && !has_code(&errs, "E0046"),
            "inner handle reusing the outer alias name as its k_name must NOT \
             corrupt the inner arm body via desugar substitution: {errs:?}"
        );
    }

    // Note: PR #62 review case (d) — `let k: Int = 99` shadowing
    // the arm's `k` inside the same arm body — is currently gated
    // at `Tc::env_insert`'s debug_assert (resolve.rs has a gap and
    // doesn't catch this shadow, so typecheck panics in debug
    // builds when the let-stmt's `env_insert` finds an existing
    // binding for `k`). The shadowing fix in `desugar_arm_body`'s
    // pre-scan abort + `apply_subst_to_block`'s per-stmt subst
    // narrowing are kept as defense-in-depth for the release-build
    // case (where the assertion compiles out and the env's last
    // insertion wins). No unit test exercises this path because it
    // panics before reaching the desugar in debug builds; release-
    // build behavior with the pre-scan abort: original AST
    // preserved, codegen-walker catches the surviving let-
    // Continuation shape with the v1-restriction diagnostic.

    #[test]
    fn ty_display_continuation_omits_scope_id() {
        // PR #60 review 2 issue #5 regression: `ty_display` MUST NOT
        // render `<scope=N>` in user-facing diagnostics — users have
        // no mental model for "scope N" and can't remediate by
        // writing the type they should have written. Scope numbers
        // are reserved for E0145 cross-handle messages where they
        // explain the violation.
        let ct = Ty::Continuation(Box::new(ContinuationTy {
            op_ret: Ty::Int,
            ret: Ty::Bool,
            scope_id: ScopeId::Concrete(42),
        }));
        let rendered = ty_display(&ct);
        assert!(
            !rendered.contains("scope"),
            "ty_display must not leak scope_id to user diagnostics; got: {rendered}"
        );
        assert_eq!(rendered, "Continuation(Int) -> Bool");
    }

    // ----------------------------------------------------------------
    // Plan B' Stage 6.8 Task 103 — TypeExpr::Fn typecheck integration
    // ----------------------------------------------------------------

    /// Helper: pipeline that returns the inferred Ty of `fn f`'s
    /// only parameter. Asserts no errors. Used by the Phase B tests
    /// to verify TypeExpr::Fn maps to Ty::Fn. The src is augmented
    /// with a stub `fn main` so typecheck doesn't trip E0040.
    fn fn_param0_ty(src_fn_f: &str) -> Ty {
        let src = format!("{src_fn_f}fn main() -> Int ![] {{ 0 }}\n");
        let (toks, _) = lex("t.sigil", &src);
        let (prog, _) = parse("t.sigil", &toks);
        let (rp, _) = resolve(prog);
        let (tc, errs) = typecheck(rp.program);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
        let scheme = tc
            .fn_schemes
            .get("f")
            .unwrap_or_else(|| panic!("fn `f` not found in schemes"));
        let Ty::Fn(sig) = &scheme.body else {
            panic!("expected scheme body Ty::Fn, got {:?}", scheme.body)
        };
        sig.params[0].clone()
    }

    #[test]
    fn fn_type_zero_param_in_param_position_resolves_to_ty_fn() {
        let src = "fn f(g: () -> Int ![]) -> Int ![] { 0 }\n";
        let ty = fn_param0_ty(src);
        let Ty::Fn(sig) = ty else {
            panic!("expected Ty::Fn for fn-typed param, got {ty:?}")
        };
        assert!(sig.params.is_empty());
        assert_eq!(sig.ret, Ty::Int);
        assert!(sig.effects.is_empty());
        assert!(sig.effect_row_var.is_none());
    }

    #[test]
    fn fn_type_one_param_with_effect_resolves_to_ty_fn() {
        let src = "fn f(g: (Int) -> String ![IO]) -> Int ![] { 0 }\n";
        let ty = fn_param0_ty(src);
        let Ty::Fn(sig) = ty else {
            panic!("expected Ty::Fn, got {ty:?}")
        };
        assert_eq!(sig.params, vec![Ty::Int]);
        assert_eq!(sig.ret, Ty::String);
        assert_eq!(sig.effects, vec![EffectInst::bare("IO")]);
    }

    #[test]
    fn fn_type_with_generic_param_resolves_to_ty_fn_with_var() {
        let src = "fn f[A](g: (A) -> A ![]) -> Int ![] { 0 }\n";
        let ty = fn_param0_ty(src);
        let Ty::Fn(sig) = ty else {
            panic!("expected Ty::Fn, got {ty:?}")
        };
        // params[0] and ret are both Ty::Var(_) — same id since `A`
        // resolves to the same fresh var across both positions.
        let Ty::Var(p_id) = &sig.params[0] else {
            panic!("expected Ty::Var for `A` param, got {:?}", sig.params[0])
        };
        let Ty::Var(r_id) = &sig.ret else {
            panic!("expected Ty::Var for `A` return, got {:?}", sig.ret)
        };
        assert_eq!(p_id, r_id, "shared `A` must resolve to the same Ty::Var");
    }

    #[test]
    fn fn_type_with_unbound_row_variable_fires_e0137() {
        // Plan D Task 116 — E0137 now fires only when the inner
        // fn-type's row variable is NOT bound by the enclosing fn's
        // row-var (the outer fn here declares `![]`, so no row var
        // is in scope; `r` inside g's row is unbound).
        let src = "fn f(g: (Int) -> Int ![IO | r]) -> Int ![] { 0 }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0137"),
            "unbound inner-fn-type row variable must E0137: {errs:?}"
        );
    }

    // Plan D Task 116 smoke gate — row-polymorphic Fn parameters.

    #[test]
    fn fn_type_with_row_var_bound_by_enclosing_fn_typechecks() {
        // The row var `r` on `g`'s inner fn-type row references the
        // outer fn's row var (declared via `f`'s `!r` row). Both
        // should resolve to the same row id at typecheck.
        let src = "fn f(g: (Int) -> Int ![IO | r]) -> Int ![IO | r] { 0 }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "row-var-bearing fn-type bound by enclosing fn should typecheck; got {errs:?}"
        );
    }

    #[test]
    fn row_polymorphic_callee_called_from_earlier_caller_typechecks() {
        // Plan D Task 116 R1 regression — pre-pass-1 (Scheme
        // builder) must seed `current_row_var_subst` with the
        // SAME row-var id allocated for `Scheme.row_vars`. Without
        // it, a forward-ref caller declared earlier than the
        // row-poly callee instantiates a Scheme whose outer
        // `effect_row_var = Some(id)` but inner FnTypeExpr's
        // `effect_row_var = None` — a mismatch that
        // rename_ty's row_map miss leaves None, silently coercing
        // the row-poly param's row tail to closed.
        //
        // Caller declared BEFORE callee on purpose; this is the
        // shape that exposes the bug.
        let src = "effect Raise[E] { fail: (E) -> Int }\n\
                   fn caller() -> Int ![Raise[Int]] {\n  \
                     passthrough(my_body)\n\
                   }\n\
                   fn my_body() -> Int ![Raise[Int]] {\n  \
                     0\n\
                   }\n\
                   fn passthrough[A](body: () -> A ![Raise[Int] | e]) -> A ![Raise[Int] | e] {\n  \
                     body()\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "forward-ref caller of row-poly callee should typecheck; got {errs:?}"
        );
    }

    #[test]
    fn row_polymorphic_passthrough_signature_typechecks() {
        // Plan D Task 116 smoke gate — row var on a fn-typed
        // parameter shares an id with the outer fn's row var.
        // Calling `body()` performs Raise[String] + e effects;
        // those flow to the enclosing fn's row, which declares
        // the same Raise[String] + e. Cross-fn subsumption with
        // row vars unifies cleanly.
        //
        // Note: `e` is bound by the outer fn's `![| e]`, NOT by
        // generic_params. Including `e` in `generic_params`
        // would allocate it as a Ty::Var (unconstrained, fires
        // E0132 at call sites). The R1 reviewer flagged this
        // ergonomics — v1's row-var binder is the outer fn's
        // `effect_row_var`, not generic_params.
        let src = "effect Raise[E] { fail: (E) -> Int }\n\
                   fn passthrough[A](body: () -> A ![Raise[String] | e]) -> A ![Raise[String] | e] {\n  \
                     body()\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "row-polymorphic passthrough signature should typecheck; got {errs:?}"
        );
    }

    #[test]
    fn fn_type_in_let_binding_position_typechecks() {
        // The let RHS is the same fn `id_fn`, so its scheme matches
        // the let-binding's annotated type.
        let src = "fn id_fn(x: Int) -> Int ![] { x }\n\
                   fn main() -> Int ![] { let f: (Int) -> Int ![] = id_fn; 0 }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "fn-typed let binding must typecheck cleanly: {errs:?}"
        );
    }

    #[test]
    fn fn_type_let_binding_mismatch_is_e0044() {
        // Annotated as `(Int) -> String` but RHS returns Int.
        let src = "fn id_fn(x: Int) -> Int ![] { x }\n\
                   fn main() -> Int ![] { let f: (Int) -> String ![] = id_fn; 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0044"),
            "fn-typed let mismatch must E0044: {errs:?}"
        );
    }

    // ----------------------------------------------------------------
    // R1 finding 3 — exercise HM unification on user-surface Ty::Fn
    // values. Pre-Phase-B, Ty::Fn was internal-only; Phase B is the
    // first surface use, so we pin: (a) generic `A` shared across
    // both fn-type positions unifies through, (b) effect-row order
    // is irrelevant under unification, (c) effect-row width
    // mismatch fails cleanly.
    // ----------------------------------------------------------------

    #[test]
    fn fn_type_unification_through_generic_position() {
        // `apply[A, B](f: (A) -> B ![], x: A) -> B ![]` invoked with
        // a concrete fn whose `A`/`B` map to Int/String pins both
        // positions through unification. Typechecks cleanly iff
        // unification flows the concrete types through `A` + `B`.
        let src = "fn int_to_string(n: Int) -> String ![] { \"x\" }\n\
                   fn apply[A, B](f: (A) -> B ![], x: A) -> B ![] { f(x) }\n\
                   fn main() -> Int ![] { let _: String = apply(int_to_string, 42); 0 }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "generic apply with fn-typed arg must typecheck via unification: {errs:?}"
        );
    }

    #[test]
    fn fn_type_unification_effect_row_width_mismatch_is_e0128() {
        // Annotation says fn-type with `![]` (empty effects); RHS is
        // a fn whose declared effects are `![IO]`. Unification must
        // reject. Note: the root cause is E0128 ("effect row
        // mismatch: closed row `![]` cannot unify with closed row
        // `![IO]`") fired by the row-unification path; E0045 ("let
        // binding declared type but initializer has type") fires as
        // a follow-on when the let-decl-vs-init types differ.
        let src = "fn pr(s: String) -> Int ![IO] { perform IO.println(s); 0 }\n\
                   fn main() -> Int ![IO] { let _: (String) -> Int ![] = pr; 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0128"),
            "fn-type effect-row width mismatch must E0128 (row-unify): {errs:?}"
        );
    }

    #[test]
    fn fn_type_unification_effect_row_order_independent() {
        // Effect rows are semantically a set; declaration order is
        // irrelevant. Wrapper fn `caller` has `![IO, Choose]` row;
        // RHS `pr` is declared `![Choose, IO]`. Unification accepts
        // either ordering. Wrapped in `caller` (not `main`) since
        // `fn main` rejects non-{IO, ArithError} effects.
        //
        // NOTE: under v1's representation `effects: Vec<String>` is
        // ordered — typecheck unifies it as a set. If ordering ever
        // becomes load-bearing, this test will trip and force the
        // discussion.
        let src = "effect Choose { flip: () -> Bool }\n\
                   fn pr(s: String) -> Int ![Choose, IO] { 0 }\n\
                   fn caller() -> Int ![IO, Choose] { \
                     let _: (String) -> Int ![IO, Choose] = pr; 0 }\n\
                   fn main() -> Int ![IO] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "fn-type effect-row order should not affect unification: {errs:?}"
        );
    }

    #[test]
    fn fn_type_nested_fn_in_param_resolves() {
        // `((Int) -> Int ![]) -> Int ![]` — the param is itself a
        // fn-type. Unify: outer Ty::Fn whose params[0] is inner
        // Ty::Fn(Int -> Int).
        let src = "fn f(g: ((Int) -> Int ![]) -> Int ![]) -> Int ![] { 0 }\n";
        // fn_param0_ty appends fn main; this src has the fn-type in
        // the param position of the outer fn `f`.
        let ty = fn_param0_ty(src);
        let Ty::Fn(outer_sig) = ty else {
            panic!("expected outer Ty::Fn, got {ty:?}")
        };
        let Ty::Fn(inner_sig) = &outer_sig.params[0] else {
            panic!("expected inner Ty::Fn, got {:?}", outer_sig.params[0])
        };
        assert_eq!(inner_sig.params, vec![Ty::Int]);
        assert_eq!(inner_sig.ret, Ty::Int);
        assert_eq!(outer_sig.ret, Ty::Int);
    }

    // ===== Plan C Task 62 — std/option typecheck-level coverage =====
    //
    // E2E run-and-check-output tests live in `compiler/tests/e2e.rs`
    // (CI-only on the headless pod per the memory-pressure rule).
    // These typecheck-only tests give fast pod-side feedback that
    // `std/option.sigil` is valid sigil syntax + that the import
    // resolver (Task 62.0) loads it cleanly.

    #[test]
    fn import_std_option_typechecks_cleanly() {
        let src = "import std.option\n\
                   fn main() -> Int ![IO] {\n  \
                     let o: Option[Int] = Some(42);\n  \
                     let v: Int = unwrap_or(o, 0);\n  \
                     perform IO.println(int_to_string(v));\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn import_std_option_map_and_and_then_typecheck_cleanly() {
        // `safe_pos` mimics a partial transformation without using `/`
        // (which would require `![ArithError]` on the row per Plan B
        // Task 57). `map` and `and_then` are pure-row helpers; this
        // test confirms generic instantiation at A=Int, B=Int compiles.
        let src = "import std.option\n\
                   fn double(n: Int) -> Int ![] { n + n }\n\
                   fn safe_pos(n: Int) -> Option[Int] ![] {\n  \
                     match n { 0 => None, _ => Some(n * 2) }\n\
                   }\n\
                   fn main() -> Int ![IO] {\n  \
                     let a: Option[Int] = map(Some(21), double);\n  \
                     let b: Option[Int] = and_then(Some(5), safe_pos);\n  \
                     let v: Int = unwrap_or(a, 0) + unwrap_or(b, 0);\n  \
                     perform IO.println(int_to_string(v));\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn option_helpers_unavailable_without_import() {
        // Without `import std.option`, the user program cannot
        // reference `Some`, `None`, `unwrap_or`, etc. The diagnostic
        // wording varies by which lookup fires first; this test
        // pins that compilation fails (does not silently succeed).
        let src = "fn main() -> Int ![IO] {\n  \
                     let o: Option[Int] = Some(42);\n  \
                     let v: Int = unwrap_or(o, 0);\n  \
                     perform IO.println(int_to_string(v));\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            !errs.is_empty(),
            "expected the program to fail without `import std.option`; got clean errs"
        );
    }

    // ===== Plan C Task 63 — std/result typecheck-level coverage =====

    #[test]
    fn fresh_ty_var_is_monotonic_counter() {
        // Plan C Task 63 invariant pin. `bind_ty_var`'s lower-id-is-
        // outer-canonical direction fix relies on `fresh_ty_var`
        // returning monotonically-increasing IDs so that
        // `fresh_generic_subst`-allocated outer-fn vars (called first
        // in `check_fn`) have lower IDs than any subsequent body
        // fresh-var. If a future refactor adds ID reuse / recycling
        // here, this assertion fires and the bind direction needs
        // re-checking.
        let mut tc = fresh_tc();
        let a = tc.fresh_ty_var();
        let b = tc.fresh_ty_var();
        let c = tc.fresh_ty_var();
        assert!(
            a < b,
            "fresh_ty_var must be strictly monotonic: a={a}, b={b}"
        );
        assert!(
            b < c,
            "fresh_ty_var must be strictly monotonic: b={b}, c={c}"
        );
        assert_eq!(b, a + 1, "fresh_ty_var must increment by 1");
        assert_eq!(c, b + 1, "fresh_ty_var must increment by 1");
    }

    #[test]
    fn fresh_generic_subst_then_body_fresh_vars_have_higher_ids() {
        // Plan C Task 63 invariant pin (API level). Within a single
        // `check_fn` invocation, outer-fn type vars allocated via
        // `fresh_generic_subst` must precede any body-walk fresh-vars
        // so that lower-id-is-outer-canonical holds in `bind_ty_var`.
        // This test simulates the calling order at the API level: a
        // future refactor that allocates body fresh-vars BEFORE the
        // outer-fn subst within `check_fn` would invert the property
        // silently in production (since `check_fn` is a private
        // method that can't be unit-tested in isolation), but this
        // test would still pass — the test is a pin on the API
        // contract, not the call-site discipline. Pair with
        // `outer_fn_vars_have_lower_ids_than_body_fresh_vars_after_typecheck`
        // for end-to-end coverage.
        let mut tc = fresh_tc();
        let span = Span::synthetic("test.sigil");
        let gps = vec![
            crate::ast::GenericParam {
                name: "A".to_string(),
                span: span.clone(),
            },
            crate::ast::GenericParam {
                name: "E".to_string(),
                span,
            },
        ];
        let (_subst, outer_ids) = tc.fresh_generic_subst(&gps);
        let body_id_1 = tc.fresh_ty_var();
        let body_id_2 = tc.fresh_ty_var();
        let outer_max = *outer_ids.iter().max().expect("outer_ids non-empty");
        assert!(
            body_id_1 > outer_max,
            "body fresh-var must allocate AFTER all outer-fn vars: \
             outer_ids={outer_ids:?}, body_id_1={body_id_1}"
        );
        assert!(body_id_2 > body_id_1);
    }

    #[test]
    fn bind_ty_var_with_two_unbound_vars_picks_lower_id_as_canonical() {
        // Plan C Task 63 direction-fix pin. Verifies the load-bearing
        // half of the fix: when `bind_ty_var(id, Var(other))` runs
        // with both `id` and `other` unbound and distinct, the
        // resulting substitution maps the higher-id var to the
        // lower-id var, NOT the other way around. Pairs with
        // `two_param_sum_type_match_each_arm_constrains_one_param_typechecks`
        // (the regression test for the user-facing surface).
        let span = Span::synthetic("test.sigil");
        let mut tc = fresh_tc();
        let outer = tc.fresh_ty_var(); // simulates outer-fn var, lower id.
        let body = tc.fresh_ty_var(); // simulates body fresh-var, higher id.
        assert!(outer < body, "test setup: outer must have lower id");
        // Bind body := Var(outer): the implementation should pick
        // `min(body, outer) = outer` as canonical and store
        // subst[body] = Var(outer).
        let ok = tc.bind_ty_var(body, &Ty::Var(outer), &span);
        assert!(ok);
        assert_eq!(
            tc.subst.tys.get(&body),
            Some(&Ty::Var(outer)),
            "bind_ty_var must map higher-id (body) to lower-id (outer); \
             got subst.tys = {:?}",
            tc.subst.tys
        );
        assert!(
            !tc.subst.tys.contains_key(&outer),
            "bind_ty_var must NOT map lower-id (outer) to higher-id (body); \
             got subst.tys = {:?}",
            tc.subst.tys
        );

        // Symmetric case: same call with arguments swapped should
        // produce the same substitution direction.
        let mut tc2 = fresh_tc();
        let outer2 = tc2.fresh_ty_var();
        let body2 = tc2.fresh_ty_var();
        let ok2 = tc2.bind_ty_var(outer2, &Ty::Var(body2), &span);
        assert!(ok2);
        assert_eq!(
            tc2.subst.tys.get(&body2),
            Some(&Ty::Var(outer2)),
            "bind_ty_var(outer, Var(body)) must still map body → outer"
        );
        assert!(!tc2.subst.tys.contains_key(&outer2));
    }

    #[test]
    fn outer_fn_vars_have_lower_ids_than_body_fresh_vars_after_typecheck() {
        // Plan C Task 63 invariant pin (end-to-end). Typecheck a
        // generic fn whose body triggers ctor-site instantiations —
        // the path that allocates body fresh-vars during
        // `instantiate_with_vars`. After typecheck, the fn's
        // `Scheme.type_vars` IDs must be the lowest IDs allocated
        // for that fn's typecheck, sitting consecutively at the
        // base of its allocation range. The post-typecheck
        // `Tc.subst` is private (not surfaced through
        // `CheckedProgram`) so this test pins the property via
        // outer-id consecutiveness + the program-wide scheme
        // layout: `id`'s outer vars must be lower than `main`'s
        // (no) vars. The regression-test's clean typecheck under
        // the same bind direction is the user-facing pin.
        let src = "type Result[A, E] = | Ok(A) | Err(E)\n\
                   fn id[A, E](r: Result[A, E]) -> Result[A, E] ![] {\n  \
                     match r {\n    \
                       Ok(x) => Ok(x),\n    \
                       Err(e) => Err(e),\n  \
                     }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let (toks, _) = lex("t.sigil", src);
        let (prog, _) = parse("t.sigil", &toks);
        let (rp, _) = resolve(prog);
        let (cp, errs) = typecheck(rp.program);
        assert!(errs.is_empty(), "unexpected typecheck errors: {errs:?}");
        let id_scheme = cp
            .fn_schemes
            .get("id")
            .expect("scheme `id` should be registered");
        let outer_ids: Vec<u32> = id_scheme.type_vars.clone();
        assert_eq!(
            outer_ids.len(),
            2,
            "fn `id[A, E]` should have 2 outer-fn type vars; got {outer_ids:?}"
        );

        // Sanity: outer IDs are consecutive (allocation-order
        // invariant — `fresh_generic_subst` allocates them as a
        // contiguous block).
        let mut sorted_outer = outer_ids.clone();
        sorted_outer.sort();
        for w in sorted_outer.windows(2) {
            assert_eq!(
                w[1],
                w[0] + 1,
                "outer-fn vars must be allocated consecutively; got {sorted_outer:?}"
            );
        }
    }

    #[test]
    fn two_param_sum_type_match_each_arm_constrains_one_param_typechecks() {
        // Plan C Task 63 regression-pin for the `bind_ty_var` order
        // fix. Before the fix, this program tripped E0132 because
        // `Ok(x)` constrained only A and `Err(e)` constrained only
        // E; cross-arm unify bound the outer-fn vars to the
        // ctor-instance fresh vars, leaving them unconstrained at
        // the pending E0132 sweep. List[A] never tripped this
        // because there's only one type-param so an unbound arm-
        // var has no competing already-bound counterpart. This
        // test is a free-standing pin; the user-observable surface
        // ships in `std/result.sigil` and `tests/std_result_*`.
        let src = "type Result[A, E] = | Ok(A) | Err(E)\n\
                   fn id[A, E](r: Result[A, E]) -> Result[A, E] ![] {\n  \
                     match r {\n    \
                       Ok(x) => Ok(x),\n    \
                       Err(e) => Err(e),\n  \
                     }\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "two-param sum match should typecheck cleanly under bind_ty_var fix; got {errs:?}"
        );
    }

    #[test]
    fn import_std_result_typechecks_cleanly() {
        let src = "import std.result\n\
                   fn main() -> Int ![IO] {\n  \
                     let r: Result[Int, String] = Ok(42);\n  \
                     match r {\n    \
                       Ok(v) => perform IO.println(int_to_string(v)),\n    \
                       Err(_) => perform IO.println(\"err\"),\n  \
                     };\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    // ===== Plan C Task 64 — std/list typecheck-level coverage =====

    // ===== Plan C Task 65 — builtin Array typecheck-level coverage =====
    //
    // Array is registered as a builtin generic type via builtin_types().
    // Array operations (`array_alloc`, `array_empty`, `array_length`,
    // `array_get`, `array_set`) are registered as builtin generic
    // schemes via register_builtin_array_schemes(). Both are accessible
    // without an `import std.array` statement (Array is a primitive at
    // the typecheck level).

    #[test]
    fn array_alloc_get_set_typechecks_cleanly() {
        let src = "fn main() -> Int ![IO] {\n  \
                     let arr: Array[Int] = array_alloc(3, 0);\n  \
                     let v: Int = array_get(arr, 0);\n  \
                     let arr2: Array[Int] = array_set(arr, 1, 42);\n  \
                     let n: Int = array_length(arr2);\n  \
                     perform IO.println(int_to_string(n));\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn array_empty_typechecks_with_explicit_type_annotation() {
        // array_empty[A]() with no value args needs an explicit type
        // annotation at the let-binding to pin A.
        let src = "fn main() -> Int ![IO] {\n  \
                     let arr: Array[Int] = array_empty();\n  \
                     let n: Int = array_length(arr);\n  \
                     perform IO.println(int_to_string(n));\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn array_of_string_typechecks_cleanly() {
        let src = "fn main() -> Int ![IO] {\n  \
                     let arr: Array[String] = array_alloc(2, \"hi\");\n  \
                     let s: String = array_get(arr, 0);\n  \
                     perform IO.println(s);\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn array_get_arg_type_mismatch_fires_e0044() {
        // array_get(arr, "not int") — the index must be Int.
        let src = "fn main() -> Int ![] {\n  \
                     let arr: Array[Int] = array_alloc(1, 0);\n  \
                     let v: Int = array_get(arr, \"x\");\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0044"),
            "expected E0044 from array_get's index arg mismatch; got {errs:?}"
        );
    }

    #[test]
    fn user_redeclares_array_type_fires_e0113() {
        let src = "type Array = | Foo\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0113"),
            "expected E0113 (duplicate type Array); got {errs:?}"
        );
    }

    // ===== Plan C Task 66 — MutArray + Mem effect typecheck-level coverage =====

    #[test]
    fn mut_array_new_get_set_typechecks_under_mem_row() {
        let src = "fn main() -> Int ![IO, Mem] {\n  \
                     let arr: MutArray[Int] = mut_array_new(3, 0);\n  \
                     mut_array_set(arr, 1, 42);\n  \
                     let v: Int = mut_array_get(arr, 1);\n  \
                     perform IO.println(int_to_string(v));\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn mut_array_set_without_mem_in_row_fires_e0042() {
        // Without Mem in main's row, mut_array_set's `![Mem]` row
        // requirement isn't satisfied — typecheck rejects.
        let src = "fn main() -> Int ![IO] {\n  \
                     let arr: MutArray[Int] = mut_array_new(3, 0);\n  \
                     mut_array_set(arr, 1, 42);\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0042"),
            "expected E0042 from missing Mem in row; got {errs:?}"
        );
    }

    #[test]
    fn user_redeclares_mut_array_type_fires_e0113() {
        let src = "type MutArray = | Foo\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0113"),
            "expected E0113 (duplicate type MutArray); got {errs:?}"
        );
    }

    #[test]
    fn main_with_mem_only_in_row_typechecks() {
        // Mem alone (no IO) is acceptable in main's row.
        let src = "fn main() -> Int ![Mem] {\n  \
                     let arr: MutArray[Int] = mut_array_new(2, 5);\n  \
                     mut_array_set(arr, 0, 99);\n  \
                     mut_array_get(arr, 0)\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn import_std_list_typechecks_cleanly() {
        let src = "import std.list\n\
                   fn main() -> Int ![IO] {\n  \
                     let xs: List[Int] = Cons(10, Cons(20, Cons(30, Nil)));\n  \
                     let n: Int = length(xs);\n  \
                     perform IO.println(int_to_string(n));\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn import_std_list_higher_order_helpers_typecheck_cleanly() {
        // Predicate avoids `/` and `%` (both require ArithError per
        // Plan B Task 57); a positive-test predicate keeps the
        // closed `![]` row clean.
        let src = "import std.list\n\
                   fn double(n: Int) -> Int ![] { n + n }\n\
                   fn is_pos(n: Int) -> Bool ![] {\n  \
                     match n { 0 => false, _ => true }\n\
                   }\n\
                   fn add(acc: Int, x: Int) -> Int ![] { acc + x }\n\
                   fn main() -> Int ![IO] {\n  \
                     let xs: List[Int] = range(1, 5);\n  \
                     let mapped: List[Int] = map(xs, double);\n  \
                     let kept: List[Int] = filter(xs, is_pos);\n  \
                     let total: Int = fold(xs, 0, add);\n  \
                     let rev: List[Int] = reverse(xs);\n  \
                     let combined: List[Int] = append(xs, rev);\n  \
                     perform IO.println(int_to_string(length(mapped) + length(kept) + total + length(combined)));\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    // ===== Plan C Task 66.5 — ByteArray + Byte helper builtins =====

    #[test]
    fn byte_array_alloc_get_typechecks_cleanly() {
        // Build a 5-byte array filled with 0x42, read it back. The
        // builtin schemes guarantee `byte_array_alloc(Int, Byte) ->
        // ByteArray` and `byte_array_get(ByteArray, Int) -> Byte`.
        // Construct the Byte via byte_truncate (after byte_in_range
        // gating) — a runtime-primitive-only path that doesn't need
        // std.option.
        let src = "fn main() -> Int ![IO] {\n  \
                     let in_range: Bool = byte_in_range(66);\n  \
                     let b: Byte = byte_truncate(66);\n  \
                     let ba: ByteArray = byte_array_alloc(5, b);\n  \
                     let n: Int = byte_array_length(ba);\n  \
                     match in_range {\n    \
                       true => perform IO.println(int_to_string(n)),\n    \
                       false => perform IO.println(\"oor\"),\n  \
                     };\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn byte_array_alloc_with_int_fill_fires_e0044() {
        // byte_array_alloc(len, fill) requires fill: Byte, not Int.
        // Pass a literal `0` (Int) to provoke E0044.
        let src = "fn main() -> Int ![] {\n  \
                     let _ba: ByteArray = byte_array_alloc(3, 0);\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0044"),
            "expected E0044 from byte_array_alloc Int-vs-Byte mismatch; got {errs:?}"
        );
    }

    #[test]
    fn byte_array_concat_and_slice_typecheck_cleanly() {
        let src = "fn main() -> Int ![IO] {\n  \
                     let b: Byte = byte_truncate(1);\n  \
                     let a: ByteArray = byte_array_alloc(3, b);\n  \
                     let b2: ByteArray = byte_array_alloc(2, b);\n  \
                     let c: ByteArray = byte_array_concat(a, b2);\n  \
                     let s: ByteArray = byte_array_slice(c, 1, 4);\n  \
                     perform IO.println(int_to_string(byte_array_length(s)));\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn string_from_bytes_validate_alloc_typechecks_cleanly() {
        // Pin the runtime-primitive UTF-8 round-trip surface. User-
        // side `string_from_bytes` wrapper (returning Result) is
        // deferred per `[DEVIATION Task 66.5]`; the primitives
        // themselves are usable directly.
        let src = "fn main() -> Int ![IO] {\n  \
                     let bytes: ByteArray = string_to_bytes(\"hi\");\n  \
                     let v: Int = string_from_bytes_validate(bytes);\n  \
                     match v {\n    \
                       -1 => perform IO.println(string_from_bytes_alloc(bytes)),\n    \
                       _ => perform IO.println(\"bad\"),\n  \
                     };\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn import_std_byte_array_is_doc_only_skip_list() {
        // `import std.byte_array` is a no-op (skip-list path). The
        // ByteArray surface is available unconditionally via builtin
        // injection; the import provides documentation alignment
        // only. Mirrors the std.array / std.mut_array pattern.
        let src = "import std.byte_array\n\
                   fn main() -> Int ![] {\n  \
                     let _ba: ByteArray = byte_array_empty();\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn user_redeclares_byte_array_type_fires_e0113() {
        let src = "type ByteArray = | Foo\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0113"),
            "expected E0113 (duplicate type ByteArray); got {errs:?}"
        );
    }

    // ===== Plan C Task 66.6 — MutByteArray + Mem effect =====

    #[test]
    fn mut_byte_array_new_get_set_typechecks_under_mem_row() {
        let src = "fn main() -> Int ![IO, Mem] {\n  \
                     let b: Byte = byte_truncate(7);\n  \
                     let ba: MutByteArray = mut_byte_array_new(3, b);\n  \
                     mut_byte_array_set(ba, 1, byte_truncate(99));\n  \
                     let v: Byte = mut_byte_array_get(ba, 1);\n  \
                     perform IO.println(int_to_string(byte_to_int(v)));\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn mut_byte_array_set_without_mem_in_row_fires_e0042() {
        // Without Mem in main's row, mut_byte_array_set's `![Mem]`
        // requirement isn't satisfied — typecheck rejects.
        let src = "fn main() -> Int ![IO] {\n  \
                     let b: Byte = byte_truncate(7);\n  \
                     let ba: MutByteArray = mut_byte_array_new(3, b);\n  \
                     mut_byte_array_set(ba, 1, byte_truncate(99));\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0042"),
            "expected E0042 from missing Mem in row; got {errs:?}"
        );
    }

    #[test]
    fn mut_byte_array_alloc_with_int_fill_fires_e0044() {
        // mut_byte_array_new(len, fill) requires fill: Byte.
        let src = "fn main() -> Int ![Mem] {\n  \
                     let _ba: MutByteArray = mut_byte_array_new(3, 0);\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0044"),
            "expected E0044 from mut_byte_array_new Int-vs-Byte mismatch; got {errs:?}"
        );
    }

    #[test]
    fn import_std_mut_byte_array_is_doc_only_skip_list() {
        let src = "import std.mut_byte_array\n\
                   fn main() -> Int ![Mem] {\n  \
                     let b: Byte = byte_truncate(0);\n  \
                     let _ba: MutByteArray = mut_byte_array_new(0, b);\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn user_redeclares_mut_byte_array_type_fires_e0113() {
        let src = "type MutByteArray = | Foo\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0113"),
            "expected E0113 (duplicate type MutByteArray); got {errs:?}"
        );
    }

    // ===== Plan C Task 68 — extended String primitives =====

    #[test]
    fn string_concat_typechecks_cleanly() {
        let src = "fn main() -> Int ![IO] {\n  \
                     let s: String = string_concat(\"hello, \", \"world\");\n  \
                     perform IO.println(s);\n  \
                     perform IO.println(int_to_string(string_length(s)));\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn string_compare_returns_int_typechecks() {
        let src = "fn main() -> Int ![IO] {\n  \
                     let c: Int = string_compare(\"a\", \"b\");\n  \
                     perform IO.println(int_to_string(c));\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn string_predicates_typecheck() {
        let src = "fn main() -> Int ![IO] {\n  \
                     let sw: Bool = string_starts_with(\"hello\", \"he\");\n  \
                     let ew: Bool = string_ends_with(\"hello\", \"lo\");\n  \
                     let ct: Bool = string_contains(\"hello\", \"ell\");\n  \
                     match sw {\n    \
                       true => perform IO.println(\"sw\"),\n    \
                       false => perform IO.println(\"!sw\"),\n  \
                     };\n  \
                     match ew {\n    \
                       true => perform IO.println(\"ew\"),\n    \
                       false => perform IO.println(\"!ew\"),\n  \
                     };\n  \
                     match ct {\n    \
                       true => perform IO.println(\"ct\"),\n    \
                       false => perform IO.println(\"!ct\"),\n  \
                     };\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn string_byte_at_returns_byte_typechecks() {
        let src = "fn main() -> Int ![IO] {\n  \
                     let b: Byte = string_byte_at(\"abc\", 1);\n  \
                     perform IO.println(int_to_string(byte_to_int(b)));\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn string_to_int_validate_parse_pair_typechecks() {
        // The validate / parse pair is the v1 surface for parsing
        // — user-side wrappers compose it into Result[Int, ...].
        let src = "fn main() -> Int ![IO] {\n  \
                     let v: Int = string_to_int_validate(\"42\");\n  \
                     match v {\n    \
                       0 => perform IO.println(int_to_string(string_to_int_parse(\"42\"))),\n    \
                       _ => perform IO.println(\"err\"),\n  \
                     };\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn import_std_string_is_doc_only_skip_list() {
        let src = "import std.string\n\
                   fn main() -> Int ![IO] {\n  \
                     perform IO.println(string_concat(\"a\", \"b\"));\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn string_concat_with_int_arg_fires_e0044() {
        let src = "fn main() -> Int ![] {\n  \
                     let _s: String = string_concat(\"hello\", 42);\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0044"),
            "expected E0044 from string_concat Int-vs-String mismatch; got {errs:?}"
        );
    }

    // ===== Plan C Task 70 — IO extensions =====

    #[test]
    fn io_print_typechecks_under_io_row() {
        let src = "fn main() -> Int ![IO] {\n  \
                     perform IO.print(\"no newline\");\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn io_read_line_returns_string() {
        let src = "fn main() -> Int ![IO] {\n  \
                     let line: String = perform IO.read_line();\n  \
                     perform IO.println(line);\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn io_read_file_takes_path_returns_contents() {
        let src = "fn main() -> Int ![IO] {\n  \
                     let s: String = perform IO.read_file(\"/dev/null\");\n  \
                     perform IO.println(s);\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn io_write_file_takes_path_and_data() {
        let src = "fn main() -> Int ![IO] {\n  \
                     perform IO.write_file(\"/tmp/x\", \"hi\");\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn io_print_without_io_row_fires_e0042() {
        let src = "fn main() -> Int ![] {\n  \
                     perform IO.print(\"hi\");\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0042"),
            "expected E0042 (missing IO in row); got {errs:?}"
        );
    }

    // ===== Plan C Task 75 — Random effect =====

    #[test]
    fn import_std_random_typechecks_cleanly() {
        // Loads the Random effect declaration + `random_int()` +
        // `run_pseudo_random` higher-order helper. Exercises the
        // typechecker's user-effect handling path for a stdlib-
        // declared effect with a tail-`k` resume arm.
        let src = "import std.random\n\
                   fn main() -> Int ![IO] {\n  \
                     let n: Int = run_pseudo_random(random_int);\n  \
                     perform IO.println(int_to_string(n));\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn random_int_without_random_in_row_fires_e0042() {
        let src = "import std.random\n\
                   fn main() -> Int ![IO] {\n  \
                     let _n: Int = random_int();\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0042"),
            "expected E0042 (missing Random in row); got {errs:?}"
        );
    }

    // ===== Plan C Task 76 — Clock effect =====

    #[test]
    fn import_std_clock_typechecks_cleanly() {
        let src = "import std.clock\n\
                   fn main() -> Int ![IO] {\n  \
                     let t: Int = run_os_clock(now);\n  \
                     perform IO.println(int_to_string(t));\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn now_without_clock_in_row_fires_e0042() {
        let src = "import std.clock\n\
                   fn main() -> Int ![IO] {\n  \
                     let _t: Int = now();\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0042"),
            "expected E0042 (missing Clock in row); got {errs:?}"
        );
    }

    // ===== Plan C Task 71 — Raise effect + catch =====

    #[test]
    fn import_std_raise_typechecks_cleanly() {
        // Pin the `Raise` + `raise` + `catch` surface end-to-end:
        // a fallible `parse_pos` returns `Int ![Raise]` via
        // `raise(...)`; the caller wraps with `catch` to get
        // `Result[Int, String]`.
        let src = "import std.raise\n\
                   fn parse_pos(n: Int) -> Int ![Raise[String]] {\n  \
                     match n {\n    \
                       0 => raise(\"zero\"),\n    \
                       _ => n,\n  \
                     }\n\
                   }\n\
                   fn parse_pos_three() -> Int ![Raise[String]] { parse_pos(3) }\n\
                   fn main() -> Int ![IO] {\n  \
                     let r: Result[Int, String] = catch(parse_pos_three);\n  \
                     match r {\n    \
                       Ok(v) => perform IO.println(int_to_string(v)),\n    \
                       Err(_) => perform IO.println(\"err\"),\n  \
                     };\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn raise_without_raise_in_row_fires_e0042() {
        let src = "import std.raise\n\
                   fn bad() -> Int ![] {\n  \
                     raise(\"oops\")\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0042"),
            "expected E0042 (missing Raise in row); got {errs:?}"
        );
    }

    #[test]
    fn raise_with_int_arg_fires_e0044() {
        // raise[A, E] takes an E-typed error. The enclosing fn
        // declares `![Raise[String]]`, so the row site fixes
        // E := String. Calling `raise(42)` infers E := Int from
        // the arg, which conflicts with the row's Raise[String].
        // Plan D Stage 12's name-based arg-unification fires
        // E0044 (Int vs String).
        let src = "import std.raise\n\
                   fn fail_with_code() -> Int ![Raise[String]] {\n  \
                     raise(42)\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0044"),
            "expected E0044 (Int arg vs String row instantiation); got {errs:?}"
        );
    }

    // ===== Plan C Task 72 — State effect + run_state =====

    #[test]
    fn import_std_state_typechecks_cleanly() {
        // Pin `State` + `run_state` surface end-to-end. Body sets
        // state to 10, gets it back, returns get-result + 1;
        // run_state(5, comp) discharges and threads state. Same
        // shape as examples/state.sigil's canonical trace
        // (Plan B' Stage 6.8 demo). Uses direct `perform State.x`
        // invocations per `[DEVIATION Task 72]` v1 constraint #3
        // (wrapper fns don't compose with the discharge-with-
        // lambda pattern in v1).
        let src = "import std.state\n\
                   fn comp() -> Int ![State] {\n  \
                     let _: Int = perform State.set(10);\n  \
                     let v: Int = perform State.get();\n  \
                     v + 1\n\
                   }\n\
                   fn main() -> Int ![IO] {\n  \
                     let result: Int = run_state(5, comp);\n  \
                     perform IO.println(int_to_string(result));\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn perform_state_get_without_state_in_row_fires_e0042() {
        let src = "import std.state\n\
                   fn bad() -> Int ![] {\n  \
                     perform State.get()\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0042"),
            "expected E0042 (missing State in row); got {errs:?}"
        );
    }

    #[test]
    fn perform_state_set_with_string_arg_fires_e0044() {
        // State.set takes Int; passing String should fire E0044.
        let src = "import std.state\n\
                   fn bad() -> Int ![State] {\n  \
                     perform State.set(\"not an int\")\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0044"),
            "expected E0044 (State.set expects Int, got String); got {errs:?}"
        );
    }

    #[test]
    fn raise_in_string_returning_fn_typechecks_post_task_115() {
        // Plan C Task 71's v1 gap is **closed by Plan D Task 115**:
        // `Raise.fail` no longer declares a fixed `Int` return —
        // it's `fail[A]: (E) -> A`, so `raise[A, E](e: E) -> A`
        // is polymorphic in the return type. A String-returning
        // fn that calls `raise(s)` now typechecks cleanly because
        // A instantiates to String at the use site.
        //
        // Was `raise_int_return_in_string_returning_fn_fires_e0044_v1_gap_pin`
        // pre-Stage-12; renamed and inverted at Stage 12 review.
        let src = "import std.raise\n\
                   fn parse_or_fail(s: String) -> String ![Raise[String]] {\n  \
                     raise(s)\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            errs.is_empty(),
            "raise's per-op A return should fit any context; \
             got {errs:?}"
        );
    }

    // ===== Plan C Task 69 — Boxed Int64 =====

    #[test]
    fn import_std_int64_typechecks_cleanly() {
        // Pin the boxed Int64 surface end-to-end: construct from
        // Int, perform arithmetic + comparison, convert back, and
        // stringify.
        let src = "import std.int64\n\
                   fn double_it(n: Int64) -> Int64 ![] {\n  \
                     int64_add(n, n)\n\
                   }\n\
                   fn check(a: Int64, b: Int64) -> Int ![] {\n  \
                     let _e: Bool = int64_eq(a, b);\n  \
                     let _l: Bool = int64_lt(a, b);\n  \
                     let _le: Bool = int64_le(a, b);\n  \
                     let _g: Bool = int64_gt(a, b);\n  \
                     let _ge: Bool = int64_ge(a, b);\n  \
                     int64_to_int(int64_neg(int64_mod(int64_div(int64_mul(int64_sub(a, b), b), a), b)))\n\
                   }\n\
                   fn main() -> Int ![IO] {\n  \
                     let big: Int64 = int64_from_int(100);\n  \
                     let bigger: Int64 = double_it(big);\n  \
                     let formatted: String = int64_to_string(bigger);\n  \
                     perform IO.println(formatted);\n  \
                     check(big, bigger)\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn int64_from_int_with_string_arg_fires_e0044() {
        let src = "import std.int64\n\
                   fn bad() -> Int64 ![] {\n  \
                     int64_from_int(\"not an int\")\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0044"),
            "expected E0044 (int64_from_int expects Int, got String); got {errs:?}"
        );
    }

    #[test]
    fn int64_add_with_int_arg_fires_e0044() {
        // int64_add takes Int64; passing Int (not yet boxed) should fail.
        let src = "import std.int64\n\
                   fn bad(n: Int64) -> Int64 ![] {\n  \
                     int64_add(n, 7)\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0044"),
            "expected E0044 (int64_add expects Int64, got Int); got {errs:?}"
        );
    }

    #[test]
    fn user_redeclares_int64_type_fires_e0113() {
        // Int64 is a builtin opaque type; user-side `type Int64 = ...`
        // collides with the builtin and should fire E0113 (duplicate
        // type). Mirrors the Array / MutByteArray redeclare gate.
        let src = "type Int64 = | Foo\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0113"),
            "expected E0113 (duplicate Int64 declaration); got {errs:?}"
        );
    }

    // ===== Plan C Task 67 — StringBuilder (Mem-gated) =====

    #[test]
    fn import_std_string_builder_typechecks_cleanly() {
        let src = "import std.string_builder\n\
                   fn render() -> String ![Mem] {\n  \
                     let sb: StringBuilder = sb_new();\n  \
                     sb_append(sb, \"hello, \");\n  \
                     sb_append(sb, \"world\");\n  \
                     sb_finalize(sb)\n\
                   }\n\
                   fn main() -> Int ![Mem, IO] {\n  \
                     let s: String = render();\n  \
                     perform IO.println(s);\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn sb_new_without_mem_in_row_fires_e0042() {
        let src = "import std.string_builder\n\
                   fn bad() -> StringBuilder ![] {\n  \
                     sb_new()\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0042"),
            "expected E0042 (sb_new requires Mem in row); got {errs:?}"
        );
    }

    #[test]
    fn sb_append_with_int_arg_fires_e0044() {
        let src = "import std.string_builder\n\
                   fn bad(sb: StringBuilder) -> Int ![Mem] {\n  \
                     sb_append(sb, 42);\n  \
                     0\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0044"),
            "expected E0044 (sb_append expects String, got Int); got {errs:?}"
        );
    }

    #[test]
    fn user_redeclares_string_builder_type_fires_e0113() {
        let src = "type StringBuilder = | Foo\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0113"),
            "expected E0113 (duplicate StringBuilder declaration); got {errs:?}"
        );
    }

    // ===== Plan C Task 73 — Choose effect (decl-only, dischargers v2) =====

    #[test]
    fn import_std_choose_typechecks_cleanly() {
        // Pin the v1 `Choose` surface: effect declaration + direct
        // `perform Choose.choose / Choose.fail` invocations under an
        // inline single-shot handler. Per `[DEVIATION Task 73]`,
        // `all_choices` / `first_choice` dischargers and perform
        // wrappers are deferred to v2 — users handle Choose inline
        // until first-class continuations land.
        let src = "import std.choose\n\
                   fn pick_one() -> Int ![Choose] {\n  \
                     perform Choose.choose(2)\n\
                   }\n\
                   fn main() -> Int ![IO] {\n  \
                     let value: Int = handle pick_one() with {\n    \
                       Choose.choose(arg, k) => k(0),\n    \
                       Choose.fail(k) => 0,\n  \
                     };\n  \
                     perform IO.println(int_to_string(value));\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }

    #[test]
    fn perform_choose_choose_without_choose_in_row_fires_e0042() {
        let src = "import std.choose\n\
                   fn bad() -> Int ![] {\n  \
                     perform Choose.choose(3)\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0042"),
            "expected E0042 (missing Choose in row); got {errs:?}"
        );
    }

    #[test]
    fn perform_choose_choose_with_string_arg_fires_e0044() {
        // Choose.choose takes Int; passing String should fire E0044.
        let src = "import std.choose\n\
                   fn bad() -> Int ![Choose] {\n  \
                     perform Choose.choose(\"two\")\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let errs = pipeline(src);
        assert!(
            has_code(&errs, "E0044"),
            "expected E0044 (Choose.choose expects Int, got String); got {errs:?}"
        );
    }

    #[test]
    fn import_std_result_map_map_err_and_then_typecheck_cleanly() {
        let src = "import std.result\n\
                   fn double(n: Int) -> Int ![] { n + n }\n\
                   fn err_to_default(_e: String) -> Int ![] { 0 }\n\
                   fn safe_pos(n: Int) -> Result[Int, String] ![] {\n  \
                     match n { 0 => Err(\"zero\"), _ => Ok(n * 3) }\n\
                   }\n\
                   fn main() -> Int ![IO] {\n  \
                     let a: Result[Int, String] = map(Ok(21), double);\n  \
                     let b: Result[Int, Int] = map_err(Err(\"oops\"), err_to_default);\n  \
                     let c: Result[Int, String] = and_then(Ok(5), safe_pos);\n  \
                     match a {\n    \
                       Ok(v) => perform IO.println(int_to_string(v)),\n    \
                       Err(_) => perform IO.println(\"err\"),\n  \
                     };\n  \
                     match b {\n    \
                       Ok(_) => perform IO.println(\"ok\"),\n    \
                       Err(n) => perform IO.println(int_to_string(n)),\n  \
                     };\n  \
                     match c {\n    \
                       Ok(v) => perform IO.println(int_to_string(v)),\n    \
                       Err(_) => perform IO.println(\"err\"),\n  \
                     };\n  \
                     0\n\
                   }\n";
        let errs = pipeline(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }
}
