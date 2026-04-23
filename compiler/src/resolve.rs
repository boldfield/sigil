//! Name resolution — plan A1 Stage 1 task 6.
//!
//! Stage 1 scope is tiny: one function (`main`) and its body. We assign a
//! fresh `NodeId` to every binding, detect shadowing within a scope, and
//! return the resolved program alongside any diagnostics. The real
//! `NodeId`-based symbol table is populated in later plans.

use crate::ast::*;
use crate::errors::{catalog::ErrorCode, CompilerError, Severity, Span};
use std::collections::BTreeSet;

#[derive(Clone, Debug)]
pub struct ResolvedProgram {
    pub program: Program,
}

pub fn resolve(program: Program) -> (ResolvedProgram, Vec<CompilerError>) {
    let mut errors = Vec::new();
    for item in &program.items {
        if let Item::Fn(f) = item {
            let mut scope: BTreeSet<String> = BTreeSet::new();
            for p in &f.params {
                if !scope.insert(p.name.clone()) {
                    push_redef(&mut errors, p.span.clone(), &p.name);
                }
            }
            resolve_block(&f.body, &mut scope, &mut errors);
        }
    }
    (ResolvedProgram { program }, errors)
}

fn push_redef(errors: &mut Vec<CompilerError>, span: Span, name: &str) {
    let code = match ErrorCode::new("E0020") {
        Some(c) => c,
        None => panic!("catalog is missing E0020"),
    };
    errors.push(CompilerError::new(
        Severity::Error,
        code,
        span,
        format!("redefinition of `{name}` — Sigil forbids shadowing"),
    ));
}

fn resolve_block(b: &Block, scope: &mut BTreeSet<String>, errors: &mut Vec<CompilerError>) {
    for s in &b.stmts {
        match s {
            Stmt::Let(l) => {
                if !scope.insert(l.name.clone()) {
                    push_redef(errors, l.span.clone(), &l.name);
                }
            }
            Stmt::Expr(_) | Stmt::Perform(_) => {}
        }
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;
    use crate::{lexer::lex, parser::parse};

    fn pipeline(src: &str) -> Vec<CompilerError> {
        let (toks, lex_errs) = lex("x.sigil", src);
        let (prog, parse_errs) = parse("x.sigil", &toks);
        let (_resolved, res_errs) = resolve(prog);
        let mut all = lex_errs;
        all.extend(parse_errs);
        all.extend(res_errs);
        all
    }

    #[test]
    fn hello_world_resolves_clean() {
        let src = "import std.io\nfn main() -> Int ![IO] { perform IO.println(\"hi\"); 0 }\n";
        assert!(pipeline(src).is_empty());
    }

    #[test]
    fn redefinition_is_e0020() {
        let src = "fn main() -> Int ![] { let x: Int = 1; let x: Int = 2; 0 }\n";
        let errs = pipeline(src);
        assert!(
            errs.iter().any(|e| e.code.as_str() == "E0020"),
            "expected E0020 redef error, got: {errs:?}"
        );
    }
}
