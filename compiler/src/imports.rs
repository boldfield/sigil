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
/// `array.sigil`, `mut_array.sigil`, `byte_array.sigil`,
/// `mut_byte_array.sigil`, `string.sigil`, and `mem.sigil` are
/// documentation-only today (zero items declared) but listed here
/// proactively: a future doctest tooling pass (Plan C Task 77) may
/// emit `@example` blocks as standalone fns, and unguarded loading
/// would let any future fn item in those files pollute every
/// importer's flat namespace silently.
const BUILTIN_INJECTED: &[&str] = &[
    "io.sigil",
    "array.sigil",
    "mut_array.sigil",
    "byte_array.sigil",
    "mut_byte_array.sigil",
    "string.sigil",
    "mem.sigil",
];

/// Resolve every `Item::Import` in `program` against the embedded stdlib.
/// Returns a new `Program` with imported items appended, plus diagnostics
/// for missing modules (E0032) and circular imports (E0033).
pub fn resolve(program: Program) -> (Program, Vec<CompilerError>) {
    resolve_with_source(program, &|m| stdlib_embed::get(m).map(String::from))
}

/// Same as [`resolve`] but with the source lookup injected — used by
/// tests to construct synthetic stdlib modules (e.g. for cycle-detection
/// regression coverage that doesn't require touching `std/`). Source is
/// returned as `String` so the closure can synthesise it on demand
/// without lifetime gymnastics; production callers wrap
/// `stdlib_embed::get` and copy the static string.
pub(crate) fn resolve_with_source(
    program: Program,
    get_source: &dyn Fn(&str) -> Option<String>,
) -> (Program, Vec<CompilerError>) {
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
                get_source,
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

/// Wrap a lex / parse error from a stdlib module load with an
/// "internal compiler error" framing so users can tell the failure
/// is in stdlib code, not their own. The original message is
/// preserved verbatim after the framing prefix; the span points at
/// the stdlib file for stdlib-author debugging.
fn wrap_stdlib_error(err: CompilerError, module_pretty: &str) -> CompilerError {
    let new_message = format!(
        "internal compiler error in stdlib module `{module_pretty}`: {}",
        err.message
    );
    let mut wrapped = CompilerError::new(err.severity, err.code, err.span, new_message);
    wrapped.hint = err.hint.or_else(|| {
        Some(
            "this is a sigil compiler bug — please report at the sigil repo with \
             the failing program attached"
                .to_string(),
        )
    });
    wrapped
}

fn load_module(
    module: &str,
    import_span: &Span,
    loaded: &mut BTreeSet<String>,
    in_progress: &mut BTreeSet<String>,
    out: &mut Vec<Item>,
    errs: &mut Vec<CompilerError>,
    get_source: &dyn Fn(&str) -> Option<String>,
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
    let src = match get_source(module) {
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

    let module_pretty = render_module_for_diagnostic(module);

    // Transform lex / parse errors that originate from stdlib source
    // so users see "internal compiler error in stdlib module `std.X`"
    // framing instead of a raw lex/parse diagnostic over a path they
    // didn't write. CI catches stdlib breakage before release; this
    // path is the in-development safety net for stdlib-author edits.
    let (tokens, lex_errs) = lexer::lex(module, &src);
    errs.extend(
        lex_errs
            .into_iter()
            .map(|e| wrap_stdlib_error(e, &module_pretty)),
    );
    let (subprog, parse_errs) = parser::parse(module, &tokens);
    errs.extend(
        parse_errs
            .into_iter()
            .map(|e| wrap_stdlib_error(e, &module_pretty)),
    );

    for sub_item in &subprog.items {
        if let Item::Import(decl) = sub_item {
            let sub_module = match path_to_module(&decl.path) {
                Some(m) => m,
                None => continue,
            };
            if BUILTIN_INJECTED.contains(&sub_module.as_str()) {
                continue;
            }
            load_module(
                &sub_module,
                &decl.span,
                loaded,
                in_progress,
                out,
                errs,
                get_source,
            );
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
    fn duplicate_import_appended_items_dedupe() {
        // Through the test source-injection path: two imports of the
        // same synthetic non-skip-list module load that module once.
        // This exercises the `loaded.contains(module)` early return in
        // `load_module` (the skip-list path bypasses load entirely;
        // this is a real load + early-return). Asserts the resolved
        // program's appended items appear once, not twice.
        let get_source = |m: &str| match m {
            "phantom.sigil" => Some("fn phantom_helper() -> Int ![] { 7 }\n".to_string()),
            _ => None,
        };
        let user_src = "import std.phantom\n\
                        import std.phantom\n\
                        fn main() -> Int ![] { 0 }\n";
        let (toks, lex_errs) = lexer::lex("user.sigil", user_src);
        assert!(lex_errs.is_empty(), "lex errs: {lex_errs:?}");
        let (prog, parse_errs) = parser::parse("user.sigil", &toks);
        assert!(parse_errs.is_empty(), "parse errs: {parse_errs:?}");
        let (resolved, errs) = resolve_with_source(prog, &get_source);
        assert!(errs.is_empty(), "errs: {errs:?}");
        // Original 3 user items + 1 appended (phantom_helper, loaded once).
        assert_eq!(resolved.items.len(), 4);
        let helper_count = resolved
            .items
            .iter()
            .filter(|i| matches!(i, Item::Fn(f) if f.name == "phantom_helper"))
            .count();
        assert_eq!(
            helper_count, 1,
            "phantom_helper must be appended exactly once"
        );
    }

    #[test]
    fn circular_stdlib_import_is_e0033() {
        // Two synthetic phantom modules that import each other:
        //   phantom_a imports std.phantom_b
        //   phantom_b imports std.phantom_a
        // The user program imports phantom_a; load_module recurses
        // into phantom_b which tries to load phantom_a (already in
        // `in_progress`) — E0033 fires. Real cycle, no skip-list
        // shortcut. Pins the cycle-detection branch in `load_module`.
        let get_source = |m: &str| match m {
            "phantom_a.sigil" => {
                Some("import std.phantom_b\nfn a_helper() -> Int ![] { 1 }\n".to_string())
            }
            "phantom_b.sigil" => {
                Some("import std.phantom_a\nfn b_helper() -> Int ![] { 2 }\n".to_string())
            }
            _ => None,
        };
        let user_src = "import std.phantom_a\nfn main() -> Int ![] { 0 }\n";
        let (toks, lex_errs) = lexer::lex("user.sigil", user_src);
        assert!(lex_errs.is_empty(), "lex errs: {lex_errs:?}");
        let (prog, parse_errs) = parser::parse("user.sigil", &toks);
        assert!(parse_errs.is_empty(), "parse errs: {parse_errs:?}");
        let (_resolved, errs) = resolve_with_source(prog, &get_source);
        assert!(
            has_code(&errs, "E0033"),
            "expected E0033 for circular import: {errs:?}"
        );
        let msg = errs
            .iter()
            .find(|e| e.code.as_str() == "E0033")
            .map(|e| e.message.clone())
            .unwrap_or_default();
        assert!(
            msg.contains("std.phantom_a"),
            "diagnostic should name the cycle-closing module; got: {msg}"
        );
    }

    #[test]
    fn stdlib_lex_or_parse_failure_wraps_with_internal_framing() {
        // When a synthetic stdlib module has a lex/parse failure,
        // the propagated diagnostic must carry "internal compiler
        // error in stdlib module `std.X`" framing so the user
        // doesn't think it's their code. CI catches real stdlib
        // breakage before release; this path is the safety-net for
        // stdlib-author edits in development.
        let get_source = |m: &str| match m {
            "broken.sigil" => Some("@@!! not valid sigil ##^^\n".to_string()),
            _ => None,
        };
        let user_src = "import std.broken\nfn main() -> Int ![] { 0 }\n";
        let (toks, lex_errs) = lexer::lex("user.sigil", user_src);
        assert!(lex_errs.is_empty(), "user lex errs: {lex_errs:?}");
        let (prog, parse_errs) = parser::parse("user.sigil", &toks);
        assert!(parse_errs.is_empty(), "user parse errs: {parse_errs:?}");
        let (_resolved, errs) = resolve_with_source(prog, &get_source);
        assert!(!errs.is_empty(), "broken stdlib should produce errors");
        let any_internal_framed = errs.iter().any(|e| {
            e.message
                .contains("internal compiler error in stdlib module `std.broken`")
        });
        assert!(
            any_internal_framed,
            "at least one diagnostic should carry internal-stdlib framing; \
             got: {errs:?}"
        );
        let any_with_hint = errs.iter().any(|e| {
            e.hint
                .as_deref()
                .is_some_and(|h| h.contains("compiler bug"))
        });
        assert!(
            any_with_hint,
            "wrapped diagnostics should carry the 'report at the sigil repo' hint"
        );
    }

    #[test]
    fn self_import_cycle_is_e0033() {
        // Smallest possible cycle: a single module imports itself.
        // load_module recurses into itself; the second entry hits the
        // `in_progress` early return and fires E0033.
        let get_source = |m: &str| match m {
            "phantom.sigil" => Some("import std.phantom\nfn h() -> Int ![] { 1 }\n".to_string()),
            _ => None,
        };
        let user_src = "import std.phantom\nfn main() -> Int ![] { 0 }\n";
        let (toks, lex_errs) = lexer::lex("user.sigil", user_src);
        assert!(lex_errs.is_empty(), "lex errs: {lex_errs:?}");
        let (prog, parse_errs) = parser::parse("user.sigil", &toks);
        assert!(parse_errs.is_empty(), "parse errs: {parse_errs:?}");
        let (_resolved, errs) = resolve_with_source(prog, &get_source);
        assert!(
            has_code(&errs, "E0033"),
            "expected E0033 for self-import cycle: {errs:?}"
        );
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
