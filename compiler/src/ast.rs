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
    pub params: Vec<Param>,
    pub return_type: TypeExpr,
    pub effects: Vec<String>,
    pub body: Block,
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
    Named(String, Span),
}

/// Plan A3 task 37 — user-defined nominal type declaration.
#[derive(Clone, Debug)]
pub struct TypeDecl {
    pub name: String,
    pub name_span: Span,
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
    pub fn is_pointer(self) -> bool {
        matches!(
            self,
            EnvSlotKind::String | EnvSlotKind::Closure | EnvSlotKind::User
        )
    }
}

#[derive(Clone, Debug)]
pub enum Expr {
    IntLit(i64, Span),
    StringLit(String, Span),
    BoolLit(bool, Span),
    CharLit(char, Span),
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
        effects: Vec<String>,
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
            | Expr::StringLit(_, s)
            | Expr::BoolLit(_, s)
            | Expr::CharLit(_, s)
            | Expr::Ident(_, s)
            | Expr::Call { span: s, .. }
            | Expr::Binary { span: s, .. }
            | Expr::Unary { span: s, .. }
            | Expr::If { span: s, .. }
            | Expr::Match { span: s, .. }
            | Expr::Lambda { span: s, .. }
            | Expr::ClosureRecord { span: s, .. }
            | Expr::ClosureEnvLoad { span: s, .. }
            | Expr::RecordLit { span: s, .. } => s.clone(),
            Expr::Perform(p) => p.span.clone(),
            Expr::Block(b) => b.span.clone(),
        }
    }
}
