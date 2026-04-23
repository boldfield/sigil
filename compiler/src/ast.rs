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
    Ident(String, Span),
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
        span: Span,
    },
    Perform(PerformExpr),
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
            | Expr::Ident(_, s)
            | Expr::Call { span: s, .. } => s.clone(),
            Expr::Perform(p) => p.span.clone(),
        }
    }
}
