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
            | Expr::Match { span: s, .. } => s.clone(),
            Expr::Perform(p) => p.span.clone(),
            Expr::Block(b) => b.span.clone(),
        }
    }
}
