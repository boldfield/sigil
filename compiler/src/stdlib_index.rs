//! Stdlib symbol index — maps each top-level stdlib name (function /
//! type / sum constructor) back to the module(s) that declare it,
//! for use in unknown-name diagnostics (E0046 / E0112 / E0114).
//!
//! When the typechecker hits `unknown identifier string_to_int`, the
//! emit site asks this index "which stdlib module(s) declare that
//! name?" and attaches an actionable `import std.X` hint to the
//! diagnostic.
//!
//! ## Population
//!
//! Walks the embedded stdlib corpus (`stdlib_embed::STD`), parses
//! each `std/*.sigil` source via the existing lexer + parser, and
//! extracts:
//!
//! - top-level `fn name` declarations
//! - top-level `type Name = ...` declarations
//! - sum-type constructor names (`| Ok(A) | Err(E)` → `Ok`, `Err`)
//!
//! Names beginning with `__` are excluded — by Sigil stdlib
//! convention these are internal helpers (`__string_split_acc`,
//! `__tag_to_fs_error`) that should not surface as suggestions.
//!
//! Items that ship as builtins-only (registered via
//! `register_builtin_*_schemes` in `typecheck.rs` rather than
//! declared in source — `array_get`, `byte_array_alloc`, etc.) are
//! NOT in this index. Those names are always in scope and never
//! trigger E0046, so a "did you mean to import" hint would be
//! actively misleading.
//!
//! ## Lifetime
//!
//! The index is built lazily on first lookup via `OnceLock` and
//! retained for the process lifetime. A clean build of the stdlib
//! is small enough (30 files, ~4k lines) that the one-time parse
//! cost is negligible (microseconds in release mode); diagnostic
//! lookups are O(log n) BTreeMap searches.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use crate::ast::Item;
use crate::{lexer, parser, stdlib_embed};

/// `name -> {bare module path}`. The `.sigil` suffix is stripped
/// from the module side; e.g. `string_to_int -> {"string"}`,
/// `Ok -> {"result"}`, `map -> {"list", "option", "result"}`.
static INDEX: OnceLock<BTreeMap<String, BTreeSet<String>>> = OnceLock::new();

/// Build the index. Called exactly once per process.
fn build() -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for file in stdlib_embed::STD.files() {
        let path_str = match file.path().to_str() {
            Some(p) => p,
            None => continue,
        };
        let module = match path_str.strip_suffix(".sigil") {
            Some(m) => m.to_string(),
            None => continue,
        };
        let source = match file.contents_utf8() {
            Some(s) => s,
            None => continue,
        };
        let (toks, lex_errs) = lexer::lex(path_str, source);
        if !lex_errs.is_empty() {
            // Stdlib should always lex; skip if broken in this build
            // (the test suite catches actual stdlib lex/parse breakage
            // separately — silent skip here keeps user diagnostics
            // working even on a partly-broken stdlib).
            continue;
        }
        let (prog, parse_errs) = parser::parse(path_str, &toks);
        if !parse_errs.is_empty() {
            continue;
        }
        for item in &prog.items {
            match item {
                Item::Fn(f) => {
                    insert_if_public(&mut out, &f.name, &module);
                }
                Item::Type(t) => {
                    insert_if_public(&mut out, &t.name, &module);
                    for v in &t.variants {
                        insert_if_public(&mut out, &v.name, &module);
                    }
                }
                Item::Import(_) | Item::Effect(_) => {
                    // Imports don't declare names; effect names live in
                    // their own diagnostic surface (E0138) which doesn't
                    // route through this index.
                }
            }
        }
    }
    out
}

fn insert_if_public(out: &mut BTreeMap<String, BTreeSet<String>>, name: &str, module: &str) {
    if name.starts_with("__") {
        return;
    }
    out.entry(name.to_string())
        .or_default()
        .insert(module.to_string());
}

/// Look up the stdlib modules that declare `name`. Returns a sorted
/// list of bare module names (without `.sigil` suffix or `std.`
/// prefix); empty if the name is not from stdlib.
pub fn modules_declaring(name: &str) -> Vec<&'static str> {
    let idx = INDEX.get_or_init(build);
    match idx.get(name) {
        Some(set) => set.iter().map(|s| s.as_str()).collect(),
        None => Vec::new(),
    }
}

/// Format an `import` suggestion suitable for an unknown-name
/// diagnostic hint. Returns `None` if `name` is not declared in any
/// stdlib module.
///
/// Single-module hits produce a directive form ("add `import
/// std.X`"). Multi-module hits list every candidate so the user can
/// pick the right one — the typechecker can't disambiguate which
/// module the user meant from name alone.
pub fn format_import_hint(name: &str) -> Option<String> {
    let modules = modules_declaring(name);
    match modules.len() {
        0 => None,
        1 => Some(format!(
            "`{name}` is declared in `std.{module}` — add `import std.{module}` at the top of the file",
            module = modules[0],
        )),
        _ => {
            let candidates: Vec<String> =
                modules.iter().map(|m| format!("`std.{m}`")).collect();
            Some(format!(
                "`{name}` is declared in {} — add the matching `import` line at the top of the file",
                candidates.join(", "),
            ))
        }
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;

    #[test]
    fn string_to_int_resolves_to_std_string() {
        let modules = modules_declaring("string_to_int");
        assert_eq!(modules, vec!["string"], "string_to_int → std.string");
    }

    #[test]
    fn prelude_types_not_in_index() {
        // After the Option/Result prelude (2026-05-10), the source-
        // level type declarations were removed from std/option.sigil
        // and std/result.sigil; the type+constructor decls now live
        // in `typecheck::builtin_types`. The index walks stdlib
        // sources, so it correctly returns empty for prelude names —
        // this means the import-hint diagnostic does NOT fire for
        // prelude names (because they're never "unknown" in the
        // first place).
        assert!(
            modules_declaring("Option").is_empty(),
            "Option is prelude — not in module index"
        );
        assert!(
            modules_declaring("Result").is_empty(),
            "Result is prelude — not in module index"
        );
        assert!(modules_declaring("Some").is_empty(), "Some is prelude");
        assert!(modules_declaring("None").is_empty(), "None is prelude");
        assert!(modules_declaring("Ok").is_empty(), "Ok is prelude");
        assert!(modules_declaring("Err").is_empty(), "Err is prelude");
    }

    #[test]
    fn list_resolves_to_std_list() {
        // `List`/`Cons`/`Nil` are NOT in the prelude (per the
        // Option/Result design's scope hard-limit) — they still
        // require explicit `import std.list` and the index keeps
        // suggesting that import.
        assert_eq!(modules_declaring("List"), vec!["list"]);
        assert_eq!(modules_declaring("Cons"), vec!["list"]);
        assert_eq!(modules_declaring("Nil"), vec!["list"]);
    }

    #[test]
    fn map_lists_all_three_declaring_modules() {
        // `map` is declared in std.list, std.option, std.result.
        let modules = modules_declaring("map");
        assert!(
            modules.contains(&"list"),
            "map should include list: {modules:?}"
        );
        assert!(
            modules.contains(&"option"),
            "map should include option: {modules:?}"
        );
        assert!(
            modules.contains(&"result"),
            "map should include result: {modules:?}"
        );
    }

    #[test]
    fn internal_helpers_excluded() {
        // `__string_split_acc` is an internal helper; should not be in
        // the index (no leaking-suggestion hazard if a user wrote that
        // name unintentionally).
        assert!(modules_declaring("__string_split_acc").is_empty());
    }

    #[test]
    fn typo_returns_empty() {
        // Unknown name → no entry → no suggestion.
        assert!(modules_declaring("ghost_function_no_such_thing").is_empty());
    }

    #[test]
    fn parse_error_resolves_to_std_string() {
        // `ParseError` ships in std/string.sigil after PR #136.
        assert_eq!(modules_declaring("ParseError"), vec!["string"]);
    }

    #[test]
    fn parse_error_constructors_resolve_to_std_string() {
        assert_eq!(modules_declaring("Empty"), vec!["string"]);
        assert_eq!(modules_declaring("NonDecimal"), vec!["string"]);
        assert_eq!(modules_declaring("Overflow"), vec!["string"]);
    }

    #[test]
    fn format_import_hint_single_module() {
        let hint = format_import_hint("string_to_int").expect("hint should exist");
        assert!(
            hint.contains("import std.string"),
            "hint missing import line: {hint}"
        );
        assert!(hint.contains("string_to_int"), "hint missing name: {hint}");
    }

    #[test]
    fn format_import_hint_multi_module() {
        let hint = format_import_hint("map").expect("hint should exist");
        // Multi-module form lists every candidate; should mention all
        // three declaring modules.
        assert!(hint.contains("std.list"), "hint missing list: {hint}");
        assert!(hint.contains("std.option"), "hint missing option: {hint}");
        assert!(hint.contains("std.result"), "hint missing result: {hint}");
    }

    #[test]
    fn format_import_hint_unknown_returns_none() {
        assert!(format_import_hint("ghost_function_no_such_thing").is_none());
    }
}
