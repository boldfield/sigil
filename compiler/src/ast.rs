//! Surface AST for the Stage-1 subset.
//!
//! The AST is intentionally small. Later plans will grow the type here;
//! for Plan A1 we represent exactly what hello-world needs plus the
//! near-neighbours that the error-recovery story needs to produce
//! well-shaped partial ASTs.

use crate::errors::Span;

#[derive(Clone, Debug)]
pub struct Program {
    pub items: Vec<Item>,
    pub file: String,
}

#[derive(Clone, Debug)]
pub enum Item {
    Import(ImportDecl),
    Fn(Box<FnDecl>),
    /// User-defined nominal type declaration — plan A3 task 37.
    ///
    /// Surface forms:
    /// - `type Name = | Ctor | Ctor(T1, T2) | Ctor { f: T, ... }` — sum type.
    /// - `type Name = { f: T, ... }` — single-constructor shorthand,
    ///   desugars to one named variant `Name { ... }`.
    ///
    /// The typechecker (task 38) treats these nominally: a sum type
    /// with name `N` is distinct from any other type of the same shape
    /// but a different name. Codegen (task 41) bakes a per-program
    /// type tag (0x10+) into each allocation site.
    Type(Box<TypeDecl>),
    /// User-defined effect declaration — plan B task 53.
    ///
    /// Surface forms:
    /// - `effect Name[T1, ...] { op: (T, ...) -> R, ... }` — default
    ///   one-shot continuations. Each operation arm in a handler must
    ///   use the continuation `k` at most once along every path
    ///   (linearity check lands in Task 54 / E0220).
    /// - `effect Name[T1, ...] resumes: many { op: ... -> R, ... }` —
    ///   opt-in multi-shot. Handler arms may invoke `k` any number
    ///   of times.
    ///
    /// Task 53 (this commit) ships the parser surface only; the
    /// typechecker emits `E0133` for any `Item::Effect` it sees,
    /// preventing well-formed effect declarations from reaching
    /// downstream passes until Task 54 lands the row-polymorphic
    /// effect-checker and effect registry.
    Effect(Box<EffectDecl>),
}

#[derive(Clone, Debug)]
pub struct ImportDecl {
    pub path: Vec<String>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FnDecl {
    pub name: String,
    pub name_span: Span,
    /// Plan B Task 47 — generic type parameters declared on this `fn`.
    /// Empty for non-generic functions (every Plan A1/A2/A3 fn). The
    /// parser captures these; semantic consumption (HM unification +
    /// monomorphization) lands in Tasks 48-49.
    pub generic_params: Vec<GenericParam>,
    pub params: Vec<Param>,
    pub return_type: TypeExpr,
    pub effects: Vec<EffectRef>,
    /// Plan B Task 47 — explicit row variable in the effect row,
    /// `![IO | e]` introduces row variable `e`. `None` means a
    /// closed row (default before Plan B). Semantic consumption (row
    /// polymorphism in HM unification) lands in Task 48.
    pub effect_row_var: Option<RowVar>,
    pub body: Block,
    pub span: Span,
}

/// Plan B Task 47 — generic type parameter declaration on a `fn` or
/// `type` declaration. Identifies an HM-bound type variable in the
/// declaration's signature scope.
#[derive(Clone, Debug)]
pub struct GenericParam {
    pub name: String,
    pub span: Span,
}

/// Plan B Task 47 — explicit row variable in an effect row.
///
/// `![IO | e]` introduces a row variable `e` that the type checker
/// (Task 48) treats as a free row to be unified or generalised.
/// Closed rows like `![IO]` carry `effect_row_var: None`.
#[derive(Clone, Debug)]
pub struct RowVar {
    pub name: String,
    pub span: Span,
}

/// Plan D Task 114 — effect-row entry: a reference to an effect at a
/// row site (`![Raise]`, `![Raise[E]]`, etc.). The pre-Task-114 row
/// representation was `Vec<String>` over bare effect names; Task 114
/// extends it to a structured shape so type-parameterized effects
/// can be referenced in rows. `args.is_empty()` is the bare-name
/// case (e.g., `IO`, `ArithError`); non-empty `args` represents a
/// type-parameterized reference (e.g., `Raise[E]`, `State[Int]`).
///
/// Each effect-decl carries `generic_params: Vec<GenericParam>`; a
/// row-site `EffectRef` must match the decl's arity post-parse, with
/// element-wise type-arg substitution via the surrounding fn's
/// generic-param scope. Bare-name refs to a generic effect-decl are
/// rejected at typecheck (E0143 — introduced by Task 114 as E0140,
/// renamed in Task 115's audit to disambiguate from the existing
/// E0140 duplicate-handler-arm code).
///
/// Spans cover the entire reference: for `Raise[E]` the span runs
/// from the `R` of `Raise` through the closing `]` of the arg list.
#[derive(Clone, Debug)]
pub struct EffectRef {
    pub name: String,
    pub args: Vec<TypeExpr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub ty: TypeExpr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum TypeExpr {
    /// Bare type name with no type arguments: `Int`, `String`,
    /// `MyType`, or a generic-parameter reference inside a generic
    /// function/type's signature scope.
    Named(String, Span),
    /// Plan B Task 47 / 48 — generic type application: `List[Int]`,
    /// `Map[String, List[Int]]`. Parser produces this whenever a
    /// type-name token is followed by `[...]`. Plan B Task 48
    /// resolves it through HM unification: `ty_from_type_expr`
    /// validates head-name arity against the declared type's
    /// `generic_params` and recurses into args. Codegen never sees
    /// `TypeExpr::Apply` — the codegen-entry walker rejects any
    /// program whose AST still carries it (monomorphization in
    /// Task 49 erases generics into concrete clones).
    Apply {
        name: String,
        args: Vec<TypeExpr>,
        span: Span,
    },
    /// Plan B' Stage 6.8 Task 102 — first-class function type
    /// surface: `(T1, ..., Tn) -> R ![E1, ..., En]` or
    /// `(T1, ..., Tn) -> R ![E1, ..., En | r]`. Allowed in any type
    /// position: fn parameter types, fn return types, `let`-binding
    /// annotations, type-decl field types. Parser produces this
    /// whenever a type expression starts with `(`. Phase B
    /// (Task 103) maps it to `Ty::Fn` for HM unification;
    /// closure-convert + codegen (Phase C, Task 104) emit indirect
    /// calls for fn-typed values via the closure record's `code_ptr`.
    ///
    /// The payload is boxed so `TypeExpr` stays small (otherwise it
    /// pushes `Stmt::Let` and `Expr::Lambda` past clippy's
    /// `large_enum_variant` limit).
    Fn(Box<FnTypeExpr>),
    /// Plan D Task 113 — tuple type: `(T1, T2, ...)`. Arity ≥ 2;
    /// single-element parens fall through to paren-grouping over the
    /// inner type, and zero-element parens are reserved (parser
    /// rejects). Allowed in any type position. Maps to
    /// [`crate::typecheck::Ty::Tuple`] for HM unification (element-
    /// wise); codegen emits a heap-allocated record with one slot
    /// per element.
    Tuple { elems: Vec<TypeExpr>, span: Span },
}

/// Boxed payload of [`TypeExpr::Fn`]. See that variant's docstring
/// for the surface grammar and phasing.
#[derive(Clone, Debug)]
pub struct FnTypeExpr {
    pub params: Vec<TypeExpr>,
    pub ret: TypeExpr,
    pub effects: Vec<EffectRef>,
    pub effect_row_var: Option<RowVar>,
    pub span: Span,
}

impl TypeExpr {
    /// Span of this type expression (whichever variant).
    pub fn span(&self) -> Span {
        match self {
            TypeExpr::Named(_, s) => s.clone(),
            TypeExpr::Apply { span, .. } => span.clone(),
            TypeExpr::Fn(fty) => fty.span.clone(),
            TypeExpr::Tuple { span, .. } => span.clone(),
        }
    }

    /// Head name of this type expression. For `Named` it's the name
    /// itself; for `Apply` it's the constructor's name (the part
    /// before the `[...]`); for `Fn` it's the synthetic `"Fn"`
    /// marker (no surface name — fn-typed values lower to closure-
    /// record pointers, and the codegen catchall already routes
    /// unknown head-names to `pointer_ty`). Convenience for the
    /// many call sites that only need the head and not the type
    /// arguments.
    pub fn head_name(&self) -> &str {
        match self {
            TypeExpr::Named(n, _) => n,
            TypeExpr::Apply { name, .. } => name,
            TypeExpr::Fn(_) => "Fn",
            TypeExpr::Tuple { .. } => "Tuple",
        }
    }
}

/// Plan B task 53 — user-defined effect declaration.
///
/// `effect Name[T1, ...] { op: (T, ...) -> R, ... }` declares an
/// algebraic effect with one or more operations. The optional
/// `resumes: many` annotation between the generic-param header and
/// the body's opening `{` switches the effect from one-shot (default)
/// to multi-shot continuations.
///
/// Generic parameters declared on the effect (`T1`, ...) are in scope
/// in every operation's parameter and return types but not in the
/// effect's row-polymorphism story (effects in v1 do not themselves
/// have an effect-row signature).
///
/// Semantic consumption — registry build, row-polymorphic checking,
/// linearity check on the per-arm continuation — lands in Task 54.
/// Until then, the typechecker emits `E0133` for every effect
/// declaration so a partially-implemented program cannot reach
/// downstream passes.
#[derive(Clone, Debug)]
pub struct EffectDecl {
    pub name: String,
    pub name_span: Span,
    /// Generic-parameter list `[T1, ...]`. Empty when the effect is
    /// declared without any generic parameters.
    pub generic_params: Vec<GenericParam>,
    /// `true` when the source carries `resumes: many` between the
    /// (possibly empty) generic-param header and the operation list.
    /// Default `false` (one-shot).
    pub resumes_many: bool,
    pub ops: Vec<EffectOp>,
    pub span: Span,
}

/// Plan B task 53 — a single operation declared inside an
/// `effect` body: `name : ( T1, T2, ... ) -> R`. Empty parameter
/// lists are written `name: () -> R`.
///
/// Plan D Task 115 — per-op generic params: each op may declare its
/// own generic parameters bound *only* inside the op's signature,
/// distinct from the enclosing effect-decl's `generic_params`. The
/// canonical shape is `fail[A]: (E) -> A` — `A` is the op's per-call
/// return-type generic (Koka's "never returns" idiom), bound only
/// inside `fail`'s signature; `E` is the effect-decl's generic
/// param bound at `effect Raise[E] { ... }`. Empty for ops without
/// per-op generics (the dominant pre-Task-115 surface). Per-op
/// generics shadow effect-decl-level generics (E0144 fires when a
/// per-op generic param has the same name as an effect-decl one).
#[derive(Clone, Debug)]
pub struct EffectOp {
    pub name: String,
    pub name_span: Span,
    pub generic_params: Vec<GenericParam>,
    pub params: Vec<TypeExpr>,
    pub return_type: TypeExpr,
    pub span: Span,
}

/// Plan A3 task 37 — user-defined nominal type declaration.
#[derive(Clone, Debug)]
pub struct TypeDecl {
    pub name: String,
    pub name_span: Span,
    /// Plan B Task 47 — generic type parameters declared on this
    /// `type`. Empty for non-generic types (every Plan A3 type).
    /// Semantic consumption lands with monomorphization in Task 49.
    pub generic_params: Vec<GenericParam>,
    pub variants: Vec<Variant>,
    pub span: Span,
}

/// A single constructor (variant) in a `type` declaration.
#[derive(Clone, Debug)]
pub struct Variant {
    pub name: String,
    pub name_span: Span,
    pub fields: VariantFields,
    pub span: Span,
}

/// Three shapes a variant's payload may take.
#[derive(Clone, Debug)]
pub enum VariantFields {
    /// `| None` — no payload.
    Unit,
    /// `| Some(Int)` / `| Pair(Int, Int)` — positional fields.
    Positional(Vec<TypeExpr>),
    /// `| Point { x: Int, y: Int }` — named fields. Also the shape used
    /// by the single-constructor record shorthand `type Name = { ... }`.
    Record(Vec<RecordFieldDecl>),
}

/// A named field inside a variant's record payload.
#[derive(Clone, Debug)]
pub struct RecordFieldDecl {
    pub name: String,
    pub ty: TypeExpr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub tail: Option<Expr>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Let(LetStmt),
    Expr(Expr),
    Perform(PerformExpr),
}

#[derive(Clone, Debug)]
pub struct LetStmt {
    pub name: String,
    pub ty: TypeExpr,
    pub value: Expr,
    pub span: Span,
}

/// Cranelift-level classification of a closure env slot. Set by closure
/// conversion (plan A2 task 31) from each capture's Sigil `Ty`; consumed
/// by codegen (task 32) to pick the right load/store width and to decide
/// whether to set the corresponding bit in the closure header's GC pointer
/// bitmap. Lives in `ast.rs` (not `typecheck.rs`) so the new `Expr`
/// variants can carry slot metadata without pulling the typechecker's
/// `Ty` into the AST layer.
///
/// Every env slot stores its value in a fixed 8-byte word. Smaller types
/// (`Bool`, `Byte`, `Char`, `Unit`) are zero-extended on store and
/// truncated on load. `String` and `Closure` are word-sized GC pointers
/// and are the only variants that set a bitmap bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvSlotKind {
    Int,
    Bool,
    Char,
    Byte,
    Unit,
    String,
    Closure,
    /// Nominal user-type value (Plan A3). Heap-allocated record with an
    /// 8-byte header at word 0. GC pointer, same bitmap treatment as
    /// `String`/`Closure`.
    User,
}

impl EnvSlotKind {
    /// Whether this slot holds a GC-managed pointer. Codegen ORs bit `k+1`
    /// (past the code_ptr word at bit 0) into the closure header's pointer
    /// bitmap iff this returns `true` for slot `k`.
    ///
    /// Plan C addendum (Char): `EnvSlotKind::Char` is pointer-typed
    /// because boxed `Char` (`TAG_CHAR`) is heap-allocated. Slot
    /// widening / narrowing skips the I32 narrow path; the bitmap bit
    /// is set so a precise GC walker would trace the boxed Char.
    pub fn is_pointer(self) -> bool {
        matches!(
            self,
            EnvSlotKind::Char | EnvSlotKind::String | EnvSlotKind::Closure | EnvSlotKind::User
        )
    }
}

#[derive(Clone, Debug)]
pub enum Expr {
    IntLit(i64, Span),
    FloatLit(f64, Span),
    StringLit(String, Span),
    BoolLit(bool, Span),
    CharLit(char, Span),
    UnitLit(Span),
    Ident(String, Span),
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
        span: Span,
    },
    Perform(PerformExpr),
    // Stage 2 additions (plan A2 task 21).
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        span: Span,
    },
    Unary {
        op: UnOp,
        operand: Box<Expr>,
        span: Span,
    },
    If {
        cond: Box<Expr>,
        then_block: Box<Block>,
        else_block: Box<Block>,
        span: Span,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
        span: Span,
    },
    /// Block-as-expression, introduced by elaboration (plan A2 task 23)
    /// when desugaring `if/else` whose branches contain statements into
    /// `match` arm bodies. The surface parser does not accept this form
    /// as a primary — user-authored match arm bodies are `Expr`, not
    /// `Block`. Elaborate may wrap a `Block` with multiple statements
    /// into `Expr::Block(block)` so the resulting `MatchArm` body can
    /// carry a statement sequence without changing `MatchArm::body`'s
    /// type.
    Block(Box<Block>),
    /// Lambda expression `fn (x: T, ...) -> U !E => body`. Surface
    /// form introduced by plan A2 task 29. Unlike top-level `FnDecl`,
    /// a lambda's body is a single expression rather than a block —
    /// the grammar choice that most closely matches canonical
    /// anonymous-function forms in ML-family languages. Callers that
    /// want multiple statements inside a lambda can wrap the body in
    /// a match-arm-style expression once elaboration lands the
    /// appropriate desugaring.
    ///
    /// Typecheck, closure conversion, and codegen for lambdas land in
    /// plan A2 tasks 30, 31, 32 respectively; the AST variant is
    /// introduced in Task 29 (parser) so the grammar can ship without
    /// waiting for the full closure machinery.
    Lambda {
        params: Vec<Param>,
        return_type: TypeExpr,
        effects: Vec<EffectRef>,
        /// Plan B Task 47 — explicit row variable in the lambda's
        /// effect row. Mirrors `FnDecl::effect_row_var`.
        effect_row_var: Option<RowVar>,
        body: Box<Expr>,
        span: Span,
    },
    /// Post-closure-conversion node (plan A2 task 31). Replaces the
    /// original `Expr::Lambda` at its source position. At runtime,
    /// codegen (task 32) allocates a flat closure record on the GC
    /// heap: `{header, code_ptr, env[0], ..., env[N-1]}`. `code_fn_name`
    /// names a synthetic top-level `FnDecl` hoisted into the program's
    /// `items` list by closure conversion; its body holds the lambda's
    /// original body with captured-variable `Ident`s rewritten into
    /// `ClosureEnvLoad`s. `env_exprs[k]` evaluates (in the scope where
    /// the original lambda was written) to the captured value stored at
    /// env slot `k`; `env_slot_kinds[k]` tells codegen the Cranelift
    /// store width and whether to flip the GC bitmap bit for that slot.
    ///
    /// The parser never produces this variant — it only appears after
    /// closure conversion runs. Pre-CC passes (typecheck, elaborate)
    /// treat it as unreachable.
    ClosureRecord {
        code_fn_name: String,
        env_exprs: Vec<Expr>,
        env_slot_kinds: Vec<EnvSlotKind>,
        span: Span,
    },
    /// Post-closure-conversion node (plan A2 task 31). Inside a
    /// synthetic lambda fn hoisted by closure conversion, every original
    /// `Expr::Ident` that named a captured outer variable is rewritten
    /// into a `ClosureEnvLoad`. Codegen (task 32) lowers this to a load
    /// from `closure_ptr + 16 + 8*index` (past the 8-byte header and
    /// the 8-byte code_ptr word), narrowed or zero-extended as `kind`
    /// dictates. `name` is kept for diagnostics and pretty-printing only.
    ClosureEnvLoad {
        name: String,
        index: usize,
        kind: EnvSlotKind,
        span: Span,
    },
    /// Plan A3 task 37 — record-style constructor application:
    /// `Ctor { f: v, ... }`. Distinct from positional constructor
    /// application `Ctor(v, ...)` which parses as an `Expr::Call` on
    /// an `Expr::Ident` callee and is disambiguated by the
    /// typechecker (task 38) against the registered type symbol
    /// table. A `RecordLit` is only produced for identifier + `{`
    /// in expression position.
    RecordLit {
        name: String,
        fields: Vec<RecordFieldLit>,
        span: Span,
    },
    /// Plan B task 53 — handler expression:
    ///
    /// ```text
    /// handle <body> with {
    ///   return(<binding>) => <expr>,
    ///   <Effect>.<op>(<param>, ..., <k>) => <expr>,
    ///   ...
    /// }
    /// ```
    ///
    /// Each operation arm names the discharged effect explicitly via
    /// `Effect.op` so a single `handle` can dispatch operations from
    /// more than one effect. The trailing parameter `k` is the
    /// continuation, bound as a regular value inside the arm body
    /// (post-CPS, continuations are values). Operation arms must
    /// declare exactly one continuation parameter — Task 54 emits
    /// `E0220` if the linearity check rejects multi-use along any
    /// path of a one-shot effect's arm.
    ///
    /// The optional `return(v) => body` arm runs when the wrapped
    /// expression evaluates to a value normally (no `perform`); `v`
    /// is bound to that value. When omitted, the handler returns the
    /// body's value unchanged.
    ///
    /// Task 53 (this commit) ships the parser surface only; the
    /// typechecker emits `E0134` for every `Expr::Handle` it sees so
    /// that no well-formed handler reaches downstream passes until
    /// Task 54 / 55 / 56 land the typing rules, CPS transform, and
    /// runtime support.
    Handle {
        body: Box<Expr>,
        return_arm: Option<Box<HandleReturnArm>>,
        op_arms: Vec<HandleOpArm>,
        span: Span,
    },
    /// Plan D Task 113 — tuple value: `(e1, e2, ...)`. Arity ≥ 2;
    /// single-element parens fall through to paren-grouping (the
    /// parser returns the inner expression directly), zero-element
    /// `()` is reserved for a future Unit-literal spelling, and
    /// `(e,)` with a trailing comma is rejected (Plan D Task 113 R1
    /// finding 1: trailing-comma single-element tuples are not a
    /// valid syntax). Maps to [`crate::typecheck::Ty::Tuple`] for
    /// HM unification (element-wise inference); codegen allocates a
    /// heap record `{header, elem[0], ..., elem[N-1]}` with elements
    /// at offsets `8 + 8*i` (no discriminant word — tuples have one
    /// constructor per arity, unlike sum-type ctors which lay
    /// fields out at `16 + 8*i` after the discriminant). The GC
    /// pointer bitmap reflects per-slot pointer-ness.
    Tuple {
        elems: Vec<Expr>,
        span: Span,
    },
}

/// Plan B task 53 — the optional `return(v) => body` arm of a
/// `handle` expression.
#[derive(Clone, Debug)]
pub struct HandleReturnArm {
    pub binding: String,
    pub binding_span: Span,
    pub body: Expr,
    pub span: Span,
}

/// Plan B task 53 — one operation arm of a `handle` expression:
/// `Effect.op(p1, p2, ..., k) => body`.
///
/// Arms are stored separately from the optional `return` arm because
/// the typechecker (Task 54) treats the two roles distinctly: the
/// return arm consumes the body's value, while operation arms
/// consume `perform Effect.op(...)` calls from the body.
#[derive(Clone, Debug)]
pub struct HandleOpArm {
    pub effect: String,
    pub effect_span: Span,
    pub op: String,
    pub op_span: Span,
    /// Operation parameters bound by the arm, in source order. Their
    /// types are pinned by the operation's declaration in
    /// `EffectDecl::ops` and are filled in by Task 54's typechecker.
    pub params: Vec<HandleArmParam>,
    /// Continuation binding name. Always present and is the trailing
    /// parameter of the arm's parameter list at the surface level.
    /// Bound as a regular value inside the arm body.
    pub k_name: String,
    pub k_span: Span,
    pub body: Expr,
    pub span: Span,
}

/// Plan B task 53 — a named parameter binding in a handler op arm.
/// The corresponding type comes from the matching operation's
/// declaration in Task 54; the parser only records the binding name.
#[derive(Clone, Debug)]
pub struct HandleArmParam {
    pub name: String,
    pub span: Span,
}

/// A `field: value` pair in a record literal `Ctor { f: v, ... }`.
#[derive(Clone, Debug)]
pub struct RecordFieldLit {
    pub name: String,
    pub value: Expr,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    NotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    And,
    Or,
    /// Plan B Task 57 — codegen-internal `sdiv` without zero check;
    /// produced only by elaborate's `BinOp::Div` rewrite when the
    /// divisor is statically guaranteed non-zero on this branch (the
    /// `else` arm of the `if rhs == 0 { perform … } else { __unchecked
    /// }` rewrite). Never produced by parser; surface syntax has no
    /// way to express it. Codegen emits a plain `sdiv` for this
    /// variant. See `[DEVIATION Task 57] BinOp::Div and BinOp::Mod
    /// elaborate to perform-bearing form` in `PLAN_B_DEVIATIONS.md`.
    SdivUnchecked,
    /// Plan B Task 57 — codegen-internal `srem` without zero check;
    /// the `Mod` parallel of `SdivUnchecked`. Same elaborate-time
    /// rewrite shape, same codegen treatment (plain `srem`).
    SremUnchecked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Clone, Debug)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub body: Expr,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Pattern {
    IntLit(i64, Span),
    BoolLit(bool, Span),
    CharLit(char, Span),
    Wildcard(Span),
    /// Plan A3 task 37 — fresh variable binding in a pattern.
    ///
    /// `match x { y => y + 1 }` binds `y` to the scrutinee value
    /// inside the arm body. The parser emits `Pattern::Var` for
    /// any bare identifier in pattern position that is not `_`.
    /// The typechecker (task 38) may reinterpret a `Pattern::Var`
    /// as a nullary constructor pattern when the bare name matches
    /// a registered constructor for the scrutinee's type — that
    /// resolution is delayed so the parser does not need a symbol
    /// table.
    Var(String, Span),
    /// Plan A3 task 37 — tuple pattern `(pat, pat, ...)`.
    Tuple(Vec<Pattern>, Span),
    /// Plan A3 task 37 — constructor pattern:
    /// - Unit: `None`
    /// - Positional: `Some(n)` / `Pair(a, b)`
    /// - Record: `Point { x, y }` (field-pun: each name binds a
    ///   fresh variable with the same name) or
    ///   `Point { x: px, y }` (rename: `px` binds).
    ///
    /// Unit-shape constructors (no fields, no parentheses) never
    /// reach this variant from the parser — the parser emits
    /// `Pattern::Var(name)` for a bare identifier, and the
    /// typechecker promotes it to a nullary `Ctor` when the name
    /// resolves to a constructor.
    Ctor {
        name: String,
        fields: CtorPatternFields,
        span: Span,
    },
}

/// Plan A3 task 37 — the three possible payload shapes of a
/// constructor pattern, mirroring `VariantFields`.
#[derive(Clone, Debug)]
pub enum CtorPatternFields {
    Unit,
    Positional(Vec<Pattern>),
    Record(Vec<CtorPatternField>),
}

/// A named field inside a record constructor pattern.
/// `Point { x, y }` field-puns `x` and `y` as fresh binders of
/// the same name. `Point { x: px }` renames field `x` to binding
/// `px`.
#[derive(Clone, Debug)]
pub struct CtorPatternField {
    pub name: String,
    pub pattern: Pattern,
    pub span: Span,
}

impl Pattern {
    pub fn span(&self) -> Span {
        match self {
            Pattern::IntLit(_, s)
            | Pattern::BoolLit(_, s)
            | Pattern::CharLit(_, s)
            | Pattern::Wildcard(s)
            | Pattern::Var(_, s)
            | Pattern::Tuple(_, s)
            | Pattern::Ctor { span: s, .. } => s.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PerformExpr {
    pub effect: String,
    pub op: String,
    pub args: Vec<Expr>,
    pub span: Span,
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::IntLit(_, s)
            | Expr::FloatLit(_, s)
            | Expr::StringLit(_, s)
            | Expr::BoolLit(_, s)
            | Expr::CharLit(_, s)
            | Expr::UnitLit(s)
            | Expr::Ident(_, s)
            | Expr::Call { span: s, .. }
            | Expr::Binary { span: s, .. }
            | Expr::Unary { span: s, .. }
            | Expr::If { span: s, .. }
            | Expr::Match { span: s, .. }
            | Expr::Lambda { span: s, .. }
            | Expr::ClosureRecord { span: s, .. }
            | Expr::ClosureEnvLoad { span: s, .. }
            | Expr::RecordLit { span: s, .. }
            | Expr::Handle { span: s, .. }
            | Expr::Tuple { span: s, .. } => s.clone(),
            Expr::Perform(p) => p.span.clone(),
            Expr::Block(b) => b.span.clone(),
        }
    }
}
