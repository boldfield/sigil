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
/// `Fn` for user function/lambda values.
///
/// Plan A2's type surface remains monomorphic: no generics, no type
/// variables, no subtyping. Type equality is structural and cheap —
/// direct `PartialEq` on `Ty`.
///
/// `Ty` no longer derives `Copy` starting from task 30 because `Ty::Fn`
/// carries owned `Vec`s (parameter list and effect row). The small
/// primitive cases still benefit from `Clone`, and every call site
/// that needed a by-value copy now uses `.clone()` explicitly.
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
}

/// Structural function signature. Used in `Ty::Fn` and built for
/// every top-level `FnDecl` plus every `Expr::Lambda`.
///
/// Effects are stored as `Vec<String>` rather than a dedicated enum
/// so the runtime row-extension rules (v2+) can add new effect names
/// without breaking the typechecker. Plan A2 only ever sees `IO` in
/// an effect row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FnSig {
    pub params: Vec<Ty>,
    pub ret: Ty,
    pub effects: Vec<String>,
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
}

pub fn typecheck(program: Program) -> (CheckedProgram, Vec<CompilerError>) {
    // Pre-pass: build a global environment from every top-level
    // `FnDecl`'s declared signature. This lets recursive and mutually-
    // recursive user functions reference each other by name during
    // `check_fn`'s body walk. Any `FnDecl` whose declared types don't
    // resolve to known `Ty`s is recorded with its best-effort partial
    // signature; the full diagnostic surfaces when the decl's own body
    // is checked.
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
            let sig = FnSig {
                params: f
                    .params
                    .iter()
                    .map(|p| ty_from_type_expr(&p.ty).unwrap_or(Ty::Unit))
                    .collect(),
                ret: ty_from_type_expr(&f.return_type).unwrap_or(Ty::Unit),
                effects: f.effects.clone(),
            };
            fn_env.insert(f.name.clone(), Ty::Fn(Box::new(sig)));
        }
    }

    let mut tc = Tc {
        errors: Vec::new(),
        string_literals: Vec::new(),
        lambda_captures: Vec::new(),
        fn_env,
        env: BTreeMap::new(),
    };
    for item in &program.items {
        match item {
            Item::Fn(f) => tc.check_fn(f),
            Item::Import(_) => {}
            // Plan A3: user-defined types are registered in a pre-pass
            // (task 38 fleshes out the nominal-types symbol table).
            // For task 37 (parser) the arm is empty — AST variants
            // arrive first so the parser can ship without waiting for
            // the full typechecker. A temporary E0001 will fire from
            // downstream passes if a `type` decl is actually reached
            // in a compiled program before task 38 lands.
            Item::Type(_) => {}
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
    /// Type environment for the currently-checked function. Initialised
    /// from `fn_env` on entry to `check_fn` (so user code can reference
    /// sibling and own-recursive functions) and extended by parameters
    /// and `let` bindings as each statement is checked. Reset between
    /// functions. A `BTreeMap` keeps iteration order stable (the
    /// catalog-integrity discipline).
    env: BTreeMap<String, Ty>,
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
        self.env.clear();
        for p in &f.params {
            if let Some(ty) = ty_from_type_expr(&p.ty) {
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
        let _ = self.check_block(&f.body, &f.effects);
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
                    let got = self.check_expr(&l.value, row);
                    if let Some(got_ty) = got {
                        if !type_matches(&l.ty, &got_ty) {
                            self.push_error(
                                "E0045",
                                l.span.clone(),
                                format!(
                                    "let binding `{}` has declared type `{}` but initializer has type `{}`",
                                    l.name,
                                    type_name(&l.ty),
                                    ty_display(&got_ty),
                                ),
                            );
                        }
                    }
                    // Extend the environment with the declared type, so
                    // subsequent statements can resolve references. We use
                    // the declaration rather than the inferred type so a
                    // type mismatch in the initializer doesn't cascade into
                    // spurious E0044/E0046 at downstream sites.
                    if let Some(ty) = ty_from_type_expr(&l.ty) {
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
                // through to the global fn_env for top-level
                // function references. Keeping the two maps separate
                // lets `let foo: Int = ...` locally shadow a
                // top-level `fn foo() -> ...` without `env_insert`'s
                // debug-assert tripping (see PR #6 review).
                match self
                    .env
                    .get(name)
                    .or_else(|| self.fn_env.get(name))
                    .cloned()
                {
                    Some(ty) => Some(ty),
                    None => {
                        self.push_error(
                            "E0046",
                            span.clone(),
                            format!("unknown identifier `{name}`"),
                        );
                        None
                    }
                }
            }
            Expr::Call { callee, args, span } => self.check_call(callee, args, span.clone(), row),
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
                body,
                span,
            } => self.check_lambda(params, return_type, effects, body, span.clone()),
            // `ClosureRecord` / `ClosureEnvLoad` are post-closure-
            // conversion nodes synthesized by plan A2 task 31. They
            // never appear in a parser-produced AST; typecheck runs
            // strictly before closure conversion.
            Expr::ClosureRecord { .. } | Expr::ClosureEnvLoad { .. } => {
                unreachable!("typecheck: closure-conversion nodes should not appear pre-CC")
            }
            // Plan A3 task 37: record literal `Ctor { f: v, ... }`.
            // Task 38 replaces this stub with real nominal-type
            // resolution (look up `name` in the registered types,
            // check field names and value types, return the sum
            // type). For task 37, any program using this syntax is
            // rejected with a staged E0111 diagnostic so the pipeline
            // does not silently accept un-typechecked record data.
            // E0001 is reserved for compiler-internal contract
            // violations (see errors/catalog.rs); a user-reachable
            // "not yet implemented" path must carry its own code.
            Expr::RecordLit { name, span, .. } => {
                self.push_error(
                    "E0111",
                    span.clone(),
                    format!(
                        "record literal `{name} {{ .. }}` requires Plan A3 Task 38's nominal-type checker; not yet implemented"
                    ),
                );
                None
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

        // (3) arg types
        for (i, (arg_ty, param_ty)) in arg_tys.iter().zip(sig.params.iter()).enumerate() {
            if let Some(at) = arg_ty {
                if at != param_ty {
                    let arg_span = args.get(i).map(Expr::span).unwrap_or_else(|| span.clone());
                    self.push_error(
                        "E0044",
                        arg_span,
                        format!(
                            "argument {} has type `{}` but the callee expects `{}`",
                            i,
                            ty_display(at),
                            ty_display(param_ty),
                        ),
                    );
                }
            }
        }

        // (4) effect-row containment
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

        // (5) result type
        Some(sig.ret.clone())
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
        let param_tys: Vec<Ty> = params
            .iter()
            .map(|p| ty_from_type_expr(&p.ty).unwrap_or(Ty::Unit))
            .collect();
        let ret_ty = ty_from_type_expr(return_type).unwrap_or(Ty::Unit);
        let sig = FnSig {
            params: param_tys.clone(),
            ret: ret_ty.clone(),
            effects: effects.to_vec(),
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

        if arms.is_empty() {
            self.push_error("E0066", span, "`match` must have at least one arm");
            return None;
        }

        // (1) pattern types vs scrutinee
        if let Some(ref st) = scrut_ty {
            for arm in arms {
                if let Some(pt) = pattern_ty(&arm.pattern) {
                    if pt != *st {
                        self.push_error(
                            "E0064",
                            arm.pattern.span(),
                            format!(
                                "pattern type `{}` does not match scrutinee type `{}`",
                                ty_display(&pt),
                                ty_display(st),
                            ),
                        );
                    }
                }
            }
        }

        // (2) arm body unification
        let mut result_ty: Option<Ty> = None;
        for arm in arms {
            let body_ty = self.check_expr(&arm.body, row);
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
        }

        // (3) exhaustiveness
        if let Some(ref st) = scrut_ty {
            if !is_exhaustive(st, arms) {
                self.push_error(
                    "E0066",
                    span,
                    format!("`match` on `{}` is not exhaustive", ty_display(st)),
                );
            }
        }

        result_ty
    }
}

fn type_is(t: &TypeExpr, name: &str) -> bool {
    match t {
        TypeExpr::Named(n, _) => n == name,
    }
}

fn type_name(t: &TypeExpr) -> &str {
    match t {
        TypeExpr::Named(n, _) => n.as_str(),
    }
}

/// Lift a surface `TypeExpr` into the checker's `Ty` lattice. Unknown
/// type names return `None`; Plan A2's surface names `Int`, `String`,
/// `Unit`, `Bool`, `Char`, `Byte`. Any other name is a no-op here (the
/// checker elsewhere emits a diagnostic for the surrounding declaration).
fn ty_from_type_expr(t: &TypeExpr) -> Option<Ty> {
    match t {
        TypeExpr::Named(n, _) => match n.as_str() {
            "Int" => Some(Ty::Int),
            "String" => Some(Ty::String),
            "Unit" => Some(Ty::Unit),
            "Bool" => Some(Ty::Bool),
            "Char" => Some(Ty::Char),
            "Byte" => Some(Ty::Byte),
            _ => None,
        },
    }
}

fn type_matches(expected: &TypeExpr, actual: &Ty) -> bool {
    match (expected, actual) {
        (TypeExpr::Named(n, _), Ty::Int) => n == "Int",
        (TypeExpr::Named(n, _), Ty::String) => n == "String",
        (TypeExpr::Named(n, _), Ty::Unit) => n == "Unit",
        (TypeExpr::Named(n, _), Ty::Bool) => n == "Bool",
        (TypeExpr::Named(n, _), Ty::Char) => n == "Char",
        (TypeExpr::Named(n, _), Ty::Byte) => n == "Byte",
        // `TypeExpr` does not yet admit a function-type surface
        // syntax (deferred from Task 30's minimum scope — the
        // `FnSig`-bearing `Ty::Fn` lives entirely in the checker for
        // now, constructed by `check_expr` on `Expr::Lambda` and by
        // the global fn-env pre-pass). A `let f: Foo = <fn-typed
        // expr>;` therefore never matches, so a named left-hand
        // type against a `Ty::Fn` right-hand is a hard mismatch.
        (TypeExpr::Named(_, _), Ty::Fn(_)) => false,
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
            format!("({params}) -> {ret} ![{effects}]")
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
    }
}

/// Type of a pattern, for pattern-vs-scrutinee compatibility. Wildcard
/// returns `None` (it is not a type error against any scrutinee).
fn pattern_ty(p: &Pattern) -> Option<Ty> {
    match p {
        Pattern::IntLit(_, _) => Some(Ty::Int),
        Pattern::BoolLit(_, _) => Some(Ty::Bool),
        Pattern::CharLit(_, _) => Some(Ty::Char),
        Pattern::Wildcard(_) => None,
        // Plan A3 task 37: new pattern variants. A `Var` / `Tuple` /
        // `Ctor` pattern's type is not a simple scalar — it depends
        // on the nominal type of the scrutinee, which task 38's
        // nominal-types symbol table resolves. Returning `None`
        // here keeps the coarse pattern-vs-scrutinee check (which
        // uses this function) in sync with the "wildcard matches
        // any type" case; task 38 will replace this coarse check
        // with a structural one that uses the symbol table.
        Pattern::Var(_, _) | Pattern::Tuple(..) | Pattern::Ctor { .. } => None,
    }
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
                    walk(&arm.body, outer_names, param_names, locals, captures);
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
            // Plan A3 task 37: record literal is a user-reachable surface
            // form whose typecheck stub lives behind E0111, not E0001.
            // Review of PR #12 flagged the original E0001 regression here.
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
}
