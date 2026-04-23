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
use crate::errors::{self, CompilerError, Severity, Span};
use std::collections::BTreeMap;

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
        env: BTreeMap::new(),
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
            "E0040",
            span,
            "program has no `fn main`; every Sigil program needs `fn main() -> Int ![IO]` or `fn main() -> Int ![]`",
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
    /// Type environment for the currently-checked function. Populated with
    /// parameters on entry to `check_fn` and extended by `let` bindings as
    /// each statement is checked. Cleared between functions. A BTreeMap
    /// keeps iteration order stable (the catalog-integrity discipline).
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
        // Fresh environment per function. Sigil has no closures in Plan A1
        // (closure conversion is a no-op stub), so lexical scopes do not
        // nest across function boundaries.
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
                                "E0045",
                                l.span.clone(),
                                format!(
                                    "let binding `{}` has declared type `{}` but initializer has type `{:?}`",
                                    l.name,
                                    type_name(&l.ty),
                                    got_ty,
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
        if let Some(tail) = &b.tail {
            let _ = self.check_expr(tail, row);
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
                        format!("`IO.println` requires a `String` argument; got `{other:?}`"),
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
            Expr::Ident(name, span) => match self.env.get(name).copied() {
                Some(ty) => Some(ty),
                None => {
                    self.push_error(
                        "E0046",
                        span.clone(),
                        format!("unknown identifier `{name}`"),
                    );
                    None
                }
            },
            Expr::Call { .. } => {
                self.push_error(
                    "E0043",
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

fn type_name(t: &TypeExpr) -> &str {
    match t {
        TypeExpr::Named(n, _) => n.as_str(),
    }
}

/// Lift a surface `TypeExpr` into the checker's `Ty` lattice. Unknown
/// type names return `None`; Plan A1's surface only names `Int`,
/// `String`, and `Unit`, so any other name is a no-op here (the checker
/// elsewhere emits a diagnostic for the surrounding declaration).
fn ty_from_type_expr(t: &TypeExpr) -> Option<Ty> {
    match t {
        TypeExpr::Named(n, _) => match n.as_str() {
            "Int" => Some(Ty::Int),
            "String" => Some(Ty::String),
            "Unit" => Some(Ty::Unit),
            _ => None,
        },
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
        // checker actually flags.
        let programs = [
            "fn not_main() -> Int ![] { 0 }\n",
            "fn main() -> String ![] { \"x\" }\n",
            "fn main(x: Int) -> Int ![] { 0 }\n",
            "fn main() -> Int ![] { perform IO.println(\"hi\"); 0 }\n",
            "fn main() -> Int ![IO] { perform IO.println(); 0 }\n",
            "fn main() -> Int ![IO] { perform IO.println(42); 0 }\n",
            "fn main() -> Int ![] { let x: String = 42; 0 }\n",
        ];
        for src in programs {
            let errs = pipeline(src);
            assert!(
                !errs.iter().any(|e| e.code.as_str() == "E0001"),
                "program surfaced E0001 (internal-only): src={src:?} errs={errs:?}",
            );
        }
    }
}
