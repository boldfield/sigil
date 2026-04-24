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
}

impl EnvSlotKind {
    /// Whether this slot holds a GC-managed pointer. Codegen ORs bit `k+1`
    /// (past the code_ptr word at bit 0) into the closure header's pointer
    /// bitmap iff this returns `true` for slot `k`.
    pub fn is_pointer(self) -> bool {
        matches!(self, EnvSlotKind::String | EnvSlotKind::Closure)
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
}

impl Pattern {
    pub fn span(&self) -> Span {
        match self {
            Pattern::IntLit(_, s)
            | Pattern::BoolLit(_, s)
            | Pattern::CharLit(_, s)
            | Pattern::Wildcard(s) => s.clone(),
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
            | Expr::ClosureEnvLoad { span: s, .. } => s.clone(),
            Expr::Perform(p) => p.span.clone(),
            Expr::Block(b) => b.span.clone(),
        }
    }
}
