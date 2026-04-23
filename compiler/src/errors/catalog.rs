//! Error code catalog — single source of truth for diagnostic codes.
//!
//! Every diagnostic the compiler emits carries a stable `ErrorCode` (the
//! literal `&'static str` form; `E0010`, `E0042`, etc.). Codes point into
//! this catalog which carries a short message, a long-form explanation, and
//! a canonical fix example. `sigil explain <code>` prints the long form.
//!
//! Stages beyond Plan A1 add entries here; none are ever renumbered once
//! committed. Seed entries below establish the pattern.

/// Stable textual diagnostic code (e.g. `"E0010"`). The `ErrorCode` newtype
/// exists so the type system forbids constructing a `CompilerError` without
/// one: every `CompilerError` takes an `ErrorCode` in its constructor, and
/// the `ErrorCode` constructor only admits strings registered in `CATALOG`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ErrorCode(&'static str);

impl ErrorCode {
    /// Obtain an `ErrorCode` by code literal. Returns `None` if the code is
    /// not registered in `CATALOG`.
    pub fn new(code: &str) -> Option<Self> {
        CATALOG
            .iter()
            .find(|entry| entry.code == code)
            .map(|entry| ErrorCode(entry.code))
    }

    pub fn as_str(self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// One row in the catalog.
#[derive(Clone, Copy, Debug)]
pub struct ErrorEntry {
    pub code: &'static str,
    pub short: &'static str,
    pub long: &'static str,
    pub fix_example: &'static str,
}

/// Look up the catalog entry for a given code string.
pub fn lookup(code: &str) -> Option<&'static ErrorEntry> {
    CATALOG.iter().find(|entry| entry.code == code)
}

/// Seed catalog. Later plans populate this file directly; never dynamic.
pub const CATALOG: &[ErrorEntry] = &[
    ErrorEntry {
        code: "E0001",
        short: "internal compiler error",
        long: "The compiler hit a code path that is believed to be unreachable. \
               This is always a compiler bug, not a user error. Please report it \
               with the smallest input that reproduces the message. Compiler-internal \
               contracts (for example: an AST node expected to have been desugared \
               reaching codegen in original form) produce this error; no user program \
               should ever trigger it.",
        fix_example: "Report the error with the program source and the full stderr \
                      output of the compile command. There is no user-side fix.",
    },
    ErrorEntry {
        code: "E0010",
        short: "parser syntax error",
        long: "The parser encountered a token it could not incorporate into the \
               grammar. Sigil's grammar is strict and intentionally anti-ergonomic; \
               most syntactic missteps are real errors, not the parser being \
               pedantic. Common causes: a missing `;` between statements, a missing \
               effect row on a function signature (every `fn` must carry an `![...]` \
               suffix, even `![]` for pure functions), or a missing `-> ReturnType` \
               between the argument list and the effect row.\n\n\
               The parser recovers at `;` and `}` boundaries and continues so a \
               single compile run reports every syntactic error, not just the first.",
        fix_example: "fn main() -> Int ![IO] {\n  perform IO.println(\"hi\");\n  0\n}",
    },
    ErrorEntry {
        code: "E0020",
        short: "unknown identifier or redefinition",
        long: "Either a name was referenced before being bound, or a name was bound \
               twice in the same scope. Sigil forbids shadowing of any identifier; \
               every name is bound exactly once. If you need to rebind the \
               'logical' value, use a different name (for example: `count` and \
               `count_next`).\n\n\
               Name resolution records the error and continues so downstream type \
               errors still surface.",
        fix_example: "// Wrong — redefinition:\n// let x: Int = 1;\n// let x: Int = 2;\n\n\
                      // Right — fresh names:\nlet x: Int = 1;\nlet y: Int = 2;",
    },
    ErrorEntry {
        code: "E0031",
        short: "user-code imports are not supported in v1",
        long: "Plan A1 restricts imports to the Sigil standard library. User-code \
               imports (cross-file imports between user modules) ship in v2. If you \
               need functionality from another module, inline it into the current \
               file for now, or import the matching capability from `std.*` if one \
               exists.",
        fix_example: "import std.io",
    },
    ErrorEntry {
        code: "E0040",
        short: "program has no `fn main`",
        long: "Every Sigil program is a standalone executable and must declare a \
               function named `main`. Plan A1 fixes its signature as either \
               `fn main() -> Int ![IO]` (when the body performs any IO effect) or \
               `fn main() -> Int ![]` (pure). `main` takes no parameters and the \
               `Int` it returns becomes the process exit status.",
        fix_example: "fn main() -> Int ![IO] {\n  perform IO.println(\"hello\");\n  0\n}",
    },
    ErrorEntry {
        code: "E0041",
        short: "`fn main` has the wrong signature",
        long: "`main` must be declared `fn main() -> Int ![IO]` (when the body \
               performs IO) or `fn main() -> Int ![]` (pure). Other return types, \
               parameter lists, or effect rows are rejected in Plan A1 so the \
               runtime's C-callable `main` shim can always rely on an `Int` exit \
               status.",
        fix_example: "fn main() -> Int ![IO] {\n  perform IO.println(\"hi\");\n  0\n}",
    },
    ErrorEntry {
        code: "E0042",
        short: "effect used but not declared in the enclosing function's row",
        long: "Every `perform E.op(..)` call site requires the effect `E` to appear \
               in the enclosing function's `![..]` effect row. Effect rows are the \
               static contract that makes handler dispatch sound; silently widening \
               a function's effect row at the call site would defeat the point. \
               Either add the missing effect to the function's row, or factor the \
               perform into a helper function that declares it.",
        fix_example: "fn main() -> Int ![IO] {\n  perform IO.println(\"hi\");\n  0\n}",
    },
    ErrorEntry {
        code: "E0043",
        short: "wrong argument count at call site",
        long: "A call supplied a different number of arguments than the callee \
               declares. Sigil has no variadics and no default parameters in Plan \
               A1; each call site must match the declared arity exactly. For \
               `perform IO.println(..)` in Plan A1 the declared arity is one.",
        fix_example: "perform IO.println(\"one String argument\");",
    },
    ErrorEntry {
        code: "E0044",
        short: "argument type mismatch at call site",
        long: "A call passed an argument whose type does not match the callee's \
               declared parameter type. Sigil performs no implicit conversions in \
               Plan A1 — `Int`, `String`, and `Unit` are disjoint and the checker \
               will not coerce between them. Adjust the argument to match the \
               declared type.",
        fix_example: "perform IO.println(\"hi\");  // String is required",
    },
    ErrorEntry {
        code: "E0045",
        short: "let-binding declared type does not match initializer",
        long: "A `let <name>: <DeclaredType> = <expr>;` form requires \
               `typeof(<expr>)` to equal `<DeclaredType>`. Plan A1 does not infer \
               binding types when they are declared, and does not coerce between \
               `Int`, `String`, and `Unit`. Either change the declared type to \
               match the initializer, or change the initializer to produce the \
               declared type.",
        fix_example: "let greeting: String = \"hello\";",
    },
    ErrorEntry {
        code: "E0046",
        short: "unknown identifier",
        long: "An identifier was referenced that resolves to no binding in scope. \
               Plan A1 does not introduce user-bound locals through shadowing; every \
               binding must be declared via `let` (or appear as a function \
               parameter) earlier in the same block. Check for a typo in the \
               identifier, or add the missing binding before use.",
        fix_example: "let count: Int = 1;\nlet total: Int = count;  // count is now in scope",
    },
];

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;

    #[test]
    fn seed_entries_are_unique_and_non_empty() {
        let mut codes: Vec<&str> = CATALOG.iter().map(|e| e.code).collect();
        codes.sort();
        codes.dedup();
        assert_eq!(
            codes.len(),
            CATALOG.len(),
            "duplicate error codes in CATALOG"
        );
        for e in CATALOG {
            assert!(!e.short.is_empty(), "{} has empty short", e.code);
            assert!(!e.long.is_empty(), "{} has empty long", e.code);
            assert!(
                !e.fix_example.is_empty(),
                "{} has empty fix_example",
                e.code
            );
            assert!(e.code.starts_with('E'), "{} is not an E-code", e.code);
        }
    }

    #[test]
    fn new_resolves_known_codes() {
        assert!(ErrorCode::new("E0001").is_some());
        assert!(ErrorCode::new("E0010").is_some());
        assert!(ErrorCode::new("E9999").is_none());
    }
}
