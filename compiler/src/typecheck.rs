//! Type checker for the Stage-1 subset — plan A1 task 7.
//!
//! Stage 1's type surface is: `Int`, `String`, `Unit`; one implicit effect
//! (`IO`) used as a runtime-intrinsic shortcut until Plan B generalizes
//! effects. We verify:
//!
//! - `fn main() -> Int ![IO]` is present and well-formed.
//! - Every `perform IO.println(s)` call has a String argument and `IO` in
//!   the enclosing function's effect row.
//! - Integer and string literals are well-typed.
//!
//! Recovery: a single expression's type error is recorded and checking
//! continues on sibling expressions so one compile reports as many errors
//! as possible.

use crate::ast::*;
use crate::errors::{catalog::ErrorCode, CompilerError, Severity, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    Int,
    String,
    Unit,
}

#[derive(Clone, Debug)]
pub struct CheckedProgram {
    pub program: Program,
    /// Ordered list of (string-literal span, literal value). Codegen uses
    /// this to interleave static-data sections with the pipeline output.
    pub string_literals: Vec<(Span, String)>,
}

pub fn typecheck(program: Program) -> (CheckedProgram, Vec<CompilerError>) {
    let mut tc = Tc {
        errors: Vec::new(),
        string_literals: Vec::new(),
    };
    // Validate main: for Stage 1 there must be exactly one fn named "main".
    for item in &program.items {
        match item {
            Item::Fn(f) => tc.check_fn(f),
            Item::Import(_) => {}
        }
    }
    let has_main = program
        .items
        .iter()
        .any(|i| matches!(i, Item::Fn(f) if f.name == "main"));
    if !has_main {
        let span = Span::synthetic(&program.file);
        tc.push_error(
            "E0001",
            span,
            "program has no `fn main`; every Sigil program needs `fn main() -> Int ![IO]`",
        );
    }

    (
        CheckedProgram {
            program,
            string_literals: tc.string_literals,
        },
        tc.errors,
    )
}

struct Tc {
    errors: Vec<CompilerError>,
    string_literals: Vec<(Span, String)>,
}

impl Tc {
    fn push_error(&mut self, code: &'static str, span: Span, msg: impl Into<String>) {
        let c = match ErrorCode::new(code) {
            Some(c) => c,
            None => panic!("catalog is missing {code}"),
        };
        self.errors
            .push(CompilerError::new(Severity::Error, c, span, msg));
    }

    fn check_fn(&mut self, f: &FnDecl) {
        if f.name == "main" {
            // Return type must be Int.
            if !type_is(&f.return_type, "Int") {
                self.push_error(
                    "E0001",
                    f.span.clone(),
                    "`main` must return `Int` in Plan A1",
                );
            }
            // Stage 1 allows [] or [IO] — both are valid.
        }
        self.check_block(&f.body, &f.effects);
    }

    fn check_block(&mut self, b: &Block, row: &[String]) {
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
                        if !type_matches(&l.ty, got_ty) {
                            self.push_error(
                                "E0001",
                                l.span.clone(),
                                format!("let binding `{}` has declared type mismatch", l.name),
                            );
                        }
                    }
                }
            }
        }
        if let Some(tail) = &b.tail {
            let _ = self.check_expr(tail, row);
        }
    }

    fn check_perform(&mut self, p: &PerformExpr, row: &[String]) {
        if !row.iter().any(|e| e == &p.effect) {
            self.push_error(
                "E0001",
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
                    "E0001",
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
                        "E0001",
                        p.span.clone(),
                        format!("`IO.println` requires a `String` argument; got `{other:?}`"),
                    );
                }
                None => {}
            }
        } else if p.effect == "IO" {
            self.push_error(
                "E0001",
                p.span.clone(),
                format!("`IO.{}` is not a Plan A1 operation", p.op),
            );
        } else {
            // Non-IO perform sites arrive in later plans; Stage 1 treats
            // them as unknown and recovers.
            self.push_error(
                "E0001",
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
            Expr::Ident(_, _) => Some(Ty::Unit),
            Expr::Call { .. } => {
                self.push_error(
                    "E0001",
                    e.span(),
                    "function calls are Stage-2+; Plan A1 supports only `perform IO.println`",
                );
                None
            }
            Expr::Perform(p) => {
                self.check_perform(p, row);
                Some(Ty::Unit)
            }
        }
    }
}

fn type_is(t: &TypeExpr, name: &str) -> bool {
    match t {
        TypeExpr::Named(n, _) => n == name,
    }
}

fn type_matches(expected: &TypeExpr, actual: Ty) -> bool {
    match (expected, actual) {
        (TypeExpr::Named(n, _), Ty::Int) => n == "Int",
        (TypeExpr::Named(n, _), Ty::String) => n == "String",
        (TypeExpr::Named(n, _), Ty::Unit) => n == "Unit",
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
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

    #[test]
    fn hello_world_typechecks() {
        let src = "import std.io\nfn main() -> Int ![IO] { perform IO.println(\"hi\"); 0 }\n";
        assert!(pipeline(src).is_empty(), "{:?}", pipeline(src));
    }

    #[test]
    fn perform_without_io_in_row_errors() {
        let src = "fn main() -> Int ![] { perform IO.println(\"hi\"); 0 }\n";
        let errs = pipeline(src);
        assert!(!errs.is_empty());
    }

    #[test]
    fn perform_non_io_effect_errors() {
        let src = "fn main() -> Int ![Foo] { perform Foo.bar(\"x\"); 0 }\n";
        let errs = pipeline(src);
        assert!(!errs.is_empty());
    }
}
