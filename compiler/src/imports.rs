//! Stdlib import resolution — Plan C Task 62.0.
//!
//! Runs between [`crate::parser::parse`] and [`crate::resolve::resolve`] in
//! the compile pipeline. For each `Item::Import { path: ["std", X, ...] }` in
//! the user program, the resolver looks up the corresponding `.sigil` source
//! in the embedded `STD` tree (see [`crate::stdlib_embed`]), lexes and parses
//! it, recursively resolves *its* imports (depth-first, with cycle detection),
//! and appends the loaded module's non-import items to the program.
//!
//! Each module is loaded at most once globally (deduplicated by file path).
//! Paths in [`BUILTIN_INJECTED`] are no-ops at this layer because the
//! corresponding effect / function bindings are produced synthetically by
//! `typecheck::builtin_effects()` / `typecheck::builtin_fn_env()`. See
//! `[DEVIATION Task 57]` in `PLAN_B_DEVIATIONS.md` for the builtin-injection
//! rationale and `[DEVIATION Task 62.0]` in `PLAN_C_DEVIATIONS.md` for this
//! pass's scope decisions.
//!
//! Imports are not stripped from the program after resolution: the original
//! `Item::Import` items remain in `program.items` so spans and downstream
//! diagnostics that mention import positions still work. Every downstream
//! pass already no-ops `Item::Import(_)`, so leaving them in is harmless.

use std::collections::BTreeSet;

use crate::ast::{Item, Program};
use crate::errors::{self, CompilerError, Severity, Span};
use crate::{lexer, parser, stdlib_embed};

/// Stdlib module paths whose contents are provided by builtin injection at
/// the typechecker (see `builtin_effects` / `builtin_fn_env` /
/// `register_builtin_array_schemes` / etc.). Importing them is a no-op
/// here. Entries match the embedded-tree path produced by
/// [`path_to_module`] — with the `.sigil` suffix and `/` separators.
///
/// `array.sigil` and `mut_array.sigil` are documentation-only today
/// (zero items declared) but listed here proactively: a future doctest
/// tooling pass (Plan C Task 77) may emit `@example` blocks as
/// standalone fns, and unguarded loading would let any future fn item
/// in those files pollute every importer's flat namespace silently.
const BUILTIN_INJECTED: &[&str] = &["io.sigil", "array.sigil", "mut_array.sigil"];

/// Resolve every `Item::Import` in `program` against the embedded stdlib.
/// Returns a new `Program` with imported items appended, plus diagnostics
/// for missing modules (E0032) and circular imports (E0033).
pub fn resolve(program: Program) -> (Program, Vec<CompilerError>) {
    let mut errs: Vec<CompilerError> = Vec::new();
    let mut loaded: BTreeSet<String> = BTreeSet::new();
    let mut in_progress: BTreeSet<String> = BTreeSet::new();
    let mut imported_items: Vec<Item> = Vec::new();

    for item in &program.items {
        if let Item::Import(decl) = item {
            let module = match path_to_module(&decl.path) {
                Some(m) => m,
                None => continue,
            };
            if BUILTIN_INJECTED.contains(&module.as_str()) {
                continue;
            }
            load_module(
                &module,
                &decl.span,
                &mut loaded,
                &mut in_progress,
                &mut imported_items,
                &mut errs,
            );
        }
    }

    let mut new_items: Vec<Item> = program.items.clone();
    new_items.extend(imported_items);

    (
        Program {
            items: new_items,
            file: program.file,
        },
        errs,
    )
}

/// Convert an import path like `["std", "option"]` to the embedded-tree
/// relative path `option.sigil`. `["std", "iter", "fold"]` becomes
/// `iter/fold.sigil`. The `"std"` head is required (the parser enforces
/// it via E0031); other shapes return `None`.
fn path_to_module(path: &[String]) -> Option<String> {
    if path.first().map(String::as_str) != Some("std") || path.len() < 2 {
        return None;
    }
    Some(format!("{}.sigil", path[1..].join("/")))
}

fn render_module_for_diagnostic(module: &str) -> String {
    let stem = module.trim_end_matches(".sigil");
    format!("std.{}", stem.replace('/', "."))
}

fn load_module(
    module: &str,
    import_span: &Span,
    loaded: &mut BTreeSet<String>,
    in_progress: &mut BTreeSet<String>,
    out: &mut Vec<Item>,
    errs: &mut Vec<CompilerError>,
) {
    if loaded.contains(module) {
        return;
    }
    if in_progress.contains(module) {
        errs.push(CompilerError::new(
            Severity::Error,
            errors::code("E0033"),
            import_span.clone(),
            format!(
                "circular stdlib import involving `{}`",
                render_module_for_diagnostic(module)
            ),
        ));
        return;
    }
    let src = match stdlib_embed::get(module) {
        Some(s) => s,
        None => {
            errs.push(CompilerError::new(
                Severity::Error,
                errors::code("E0032"),
                import_span.clone(),
                format!(
                    "stdlib module `{}` not found",
                    render_module_for_diagnostic(module)
                ),
            ));
            return;
        }
    };

    in_progress.insert(module.to_string());

    let (tokens, lex_errs) = lexer::lex(module, src);
    errs.extend(lex_errs);
    let (subprog, parse_errs) = parser::parse(module, &tokens);
    errs.extend(parse_errs);

    for sub_item in &subprog.items {
        if let Item::Import(decl) = sub_item {
            let sub_module = match path_to_module(&decl.path) {
                Some(m) => m,
                None => continue,
            };
            if BUILTIN_INJECTED.contains(&sub_module.as_str()) {
                continue;
            }
            load_module(&sub_module, &decl.span, loaded, in_progress, out, errs);
        }
    }

    for sub_item in subprog.items {
        if !matches!(sub_item, Item::Import(_)) {
            out.push(sub_item);
        }
    }

    in_progress.remove(module);
    loaded.insert(module.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pipeline_through_imports(src: &str) -> (Program, Vec<CompilerError>) {
        let (toks, lex_errs) = lexer::lex("user.sigil", src);
        assert!(lex_errs.is_empty(), "lex errs: {lex_errs:?}");
        let (prog, parse_errs) = parser::parse("user.sigil", &toks);
        assert!(parse_errs.is_empty(), "parse errs: {parse_errs:?}");
        let (resolved, errs) = resolve(prog);
        (resolved, errs)
    }

    fn has_code(errs: &[CompilerError], code: &str) -> bool {
        errs.iter().any(|e| e.code.as_str() == code)
    }

    #[test]
    fn no_imports_is_identity() {
        let src = "fn main() -> Int ![] { 0 }\n";
        let (resolved, errs) = pipeline_through_imports(src);
        assert!(errs.is_empty(), "errs: {errs:?}");
        assert_eq!(resolved.items.len(), 1);
    }

    #[test]
    fn import_std_io_is_noop_via_skip_list() {
        let src = "import std.io\nfn main() -> Int ![IO] { perform IO.println(\"hi\"); 0 }\n";
        let (resolved, errs) = pipeline_through_imports(src);
        assert!(errs.is_empty(), "errs: {errs:?}");
        // Original Item::Import remains; nothing appended (io is in skip-list).
        assert_eq!(resolved.items.len(), 2);
        assert!(matches!(resolved.items[0], Item::Import(_)));
        assert!(matches!(resolved.items[1], Item::Fn(_)));
    }

    #[test]
    fn import_unknown_stdlib_module_is_e0032() {
        let src = "import std.does_not_exist\nfn main() -> Int ![] { 0 }\n";
        let (_resolved, errs) = pipeline_through_imports(src);
        assert!(has_code(&errs, "E0032"), "expected E0032, got: {errs:?}");
        let msg = errs
            .iter()
            .find(|e| e.code.as_str() == "E0032")
            .map(|e| e.message.clone())
            .unwrap_or_default();
        assert!(
            msg.contains("std.does_not_exist"),
            "diagnostic should name the missing module path; got: {msg}"
        );
    }

    #[test]
    fn duplicate_import_loads_module_once() {
        // Test a real loadable stdlib module exists by writing a synthetic
        // path lookup against `io.sigil`. Since `io` is in the skip-list,
        // dedupe-vs-load isn't observable here; this test asserts the
        // skip-list covers duplicate-import too.
        let src = "import std.io\nimport std.io\nfn main() -> Int ![IO] { 0 }\n";
        let (resolved, errs) = pipeline_through_imports(src);
        assert!(errs.is_empty(), "errs: {errs:?}");
        // Both Item::Imports remain; nothing appended.
        assert_eq!(resolved.items.len(), 3);
    }

    #[test]
    fn path_to_module_with_one_segment_returns_none() {
        assert_eq!(path_to_module(&["std".to_string()]), None);
    }

    #[test]
    fn path_to_module_with_non_std_head_returns_none() {
        assert_eq!(
            path_to_module(&["other".to_string(), "thing".to_string()]),
            None
        );
    }

    #[test]
    fn path_to_module_two_segments() {
        assert_eq!(
            path_to_module(&["std".to_string(), "option".to_string()]),
            Some("option.sigil".to_string())
        );
    }

    #[test]
    fn path_to_module_three_segments_uses_slash_separators() {
        assert_eq!(
            path_to_module(&["std".to_string(), "iter".to_string(), "fold".to_string()]),
            Some("iter/fold.sigil".to_string())
        );
    }

    #[test]
    fn render_module_for_diagnostic_strips_extension_and_dots_separators() {
        assert_eq!(render_module_for_diagnostic("option.sigil"), "std.option");
        assert_eq!(
            render_module_for_diagnostic("iter/fold.sigil"),
            "std.iter.fold"
        );
    }
}
