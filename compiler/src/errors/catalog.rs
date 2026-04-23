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
    ErrorEntry {
        code: "E0050",
        short: "integer literal out of range",
        long: "An integer literal exceeds the range representable by the Plan A1 \
               `Int` type, which is a signed 64-bit two's-complement integer \
               (range -2^63 .. 2^63-1). Literals that do not fit must be expressed \
               differently — split across arithmetic, stored as a bignum once v2 \
               introduces one, or encoded as a `String` if the value is a textual \
               constant rather than a number used in arithmetic.",
        fix_example: "let n: Int = 9223372036854775807;  // i64::MAX, fits",
    },
    ErrorEntry {
        code: "E0060",
        short: "binary operator operand type mismatch",
        long: "A binary operator was applied to an operand of the wrong type. \
               Sigil's binary operators are monomorphic in Plan A2:\n\n\
               - `+ - * / %` require both operands to be `Int` and return `Int`.\n\
               - `< > <= >=` require both operands to be `Int` and return `Bool`.\n\
               - `&& ||` require both operands to be `Bool` and return `Bool`.\n\
               - `== !=` require both operands to have the same primitive type \
                 (`Int`, `Bool`, `Char`, `Byte`, `String`, or `Unit`) and return \
                 `Bool`.\n\n\
               Sigil performs no implicit conversions between types. If you need \
               to compare a `Byte` and an `Int` numerically, convert the `Byte` \
               first using `byte_to_int`. There is no `String`-to-`Int` parse in \
               Plan A2.",
        fix_example: "let n: Int = 1 + 2;            // Int + Int\n\
                      let b: Bool = 3 < 4;           // Int < Int\n\
                      let p: Bool = true && false;   // Bool && Bool\n\
                      let e: Bool = 1 == 1;          // primitive == primitive (same type)",
    },
    ErrorEntry {
        code: "E0061",
        short: "unary operator operand type mismatch",
        long: "A prefix unary operator was applied to an operand of the wrong type. \
               `-` (negation) requires an `Int` operand and returns `Int`; `!` \
               (logical not) requires a `Bool` operand and returns `Bool`. Sigil \
               performs no implicit conversions.\n\n\
               Integer-literal negation is constant-folded at parse time: `-3` is \
               tokenised as `Minus Int(3)` then folded to `IntLit(-3)` in the \
               parser, so a literal negation never reaches the typechecker as a \
               `Unary`. A `Unary::Neg` therefore always wraps a non-literal \
               expression whose type is checked here.",
        fix_example: "let n: Int = -x;    // x must be Int\n\
                      let b: Bool = !p;   // p must be Bool",
    },
    ErrorEntry {
        code: "E0062",
        short: "`if` condition is not `Bool`",
        long: "The condition expression of an `if/else` form must have type \
               `Bool`. Plan A2 does not coerce `Int` or other types to `Bool` — \
               an `if` condition must be produced by a comparison (`< > == !=`), \
               a boolean literal (`true`/`false`), or an identifier bound to a \
               `Bool` value.\n\n\
               Elaboration (Task 23) desugars `if/else` into a `match` on `Bool`, \
               so the `Bool` constraint here is structural: no `Bool`, no \
               desugaring path.",
        fix_example: "if n == 0 { \"zero\" } else { \"nonzero\" }  // n == 0 is Bool",
    },
    ErrorEntry {
        code: "E0063",
        short: "`if` branches have incompatible types",
        long: "The `then` and `else` branches of an `if/else` form must have the \
               same type; `if/else` is an expression and its type is the common \
               branch type. Sigil performs no branch-level type widening in Plan \
               A2 — `Int` and `String` are disjoint and no `if/else` produces \
               either one based on the condition. Refactor to two separate \
               statements, or make both branches produce the same type.",
        fix_example: "let s: String = if ok { \"yes\" } else { \"no\" };",
    },
    ErrorEntry {
        code: "E0064",
        short: "match pattern type does not match scrutinee",
        long: "Each pattern in a `match` form must describe a value of the \
               scrutinee's type. Plan A2 patterns are literal patterns \
               (integer, boolean, character) and the wildcard pattern `_`. A \
               literal pattern is only valid against a scrutinee of the \
               matching primitive type: `IntLit` against `Int`, `BoolLit` \
               against `Bool`, `CharLit` against `Char`. Wildcard `_` matches \
               any scrutinee type.\n\n\
               `Byte` has no literal pattern form in Plan A2, so matches on a \
               `Byte` scrutinee must be wildcard-only in the current surface.",
        fix_example: "match n {\n  0 => \"zero\",\n  _ => \"other\",\n}  // scrutinee: Int, patterns: IntLit + wildcard",
    },
    ErrorEntry {
        code: "E0065",
        short: "match arms have incompatible types",
        long: "All arms of a `match` expression must produce the same type; the \
               `match` form is an expression and its type is the common arm \
               type. The first arm's body type is taken as the expected type for \
               the remaining arms, and any arm whose body type does not match \
               produces E0065. Refactor arms to produce a common type.",
        fix_example: "let name: String = match n {\n  0 => \"zero\",\n  _ => \"other\",\n};",
    },
    ErrorEntry {
        code: "E0066",
        short: "non-exhaustive match",
        long: "A `match` expression must cover every possible value of its \
               scrutinee. Plan A2 exhaustiveness is structural and deliberately \
               coarse:\n\n\
               - `Bool`: exhaustive iff both `true` and `false` are covered, or \
                 a wildcard `_` arm is present.\n\
               - `Int`, `Char`, `String`, `Byte`: exhaustive iff a wildcard `_` \
                 arm is present (these scrutinees have infinite or effectively- \
                 infinite value domains in Plan A2's surface syntax).\n\
               - `Unit`: exhaustive iff the arm list is non-empty (only one \
                 `Unit` value exists), though in practice patterns here are \
                 wildcards.\n\n\
               An empty arm list is always non-exhaustive. Plan A3 introduces \
               sum types and refines this check; Plan A2's rule is intentionally \
               simple so `match` on primitives is usable without the full \
               decision-procedure machinery.",
        fix_example: "match b {\n  true => 1,\n  false => 0,\n}        // Bool exhaustive: both values covered\n\n\
                      match n {\n  0 => \"zero\",\n  _ => \"other\",\n}  // Int exhaustive: wildcard covers the rest",
    },
    ErrorEntry {
        code: "E0068",
        short: "cannot apply a non-function value",
        long: "A call-site expression `callee(args...)` requires the callee \
               to have a function type. Plan A2 function types are built \
               from `fn` declarations (top-level or lambdas) and have the \
               shape `(param_tys) -> ret_ty ![effects]`. Applying a \
               non-function value — an `Int`, `Bool`, `String`, or any \
               other primitive — is a type error.\n\n\
               Common causes: a typo in the callee name that resolved to a \
               user variable; a parenthesised expression whose result \
               happens to be a primitive; or a lambda-bound name that \
               was later shadowed by a `let` of a non-function type.",
        fix_example: "fn inc(x: Int) -> Int ![] { x + 1 }\n\
                      fn main() -> Int ![] { inc(41) }",
    },
    ErrorEntry {
        code: "E0069",
        short: "lambda body type does not match declared return type",
        long: "A lambda expression `fn (params) -> R ![E] => body` requires \
               `typeof(body)` to match the declared return type `R`. The \
               checker does not infer a lambda's return type; it verifies \
               the programmer's annotation. Adjust either the annotation or \
               the body so the two agree.\n\n\
               The check fires in-place when the lambda is type-checked — \
               before the lambda is assigned or passed as an argument. A \
               separate diagnostic (E0044) handles the case where the \
               lambda's overall function type is passed to a callee whose \
               parameter expects a different function type.",
        fix_example: "let inc = fn (x: Int) -> Int ![] => x + 1;  // body is Int, matches",
    },
    ErrorEntry {
        code: "E0401",
        short: "runtime arithmetic abort",
        long: "A division or modulo operation was performed with a zero \
               divisor, or another runtime arithmetic trap fired. The \
               runtime prints `sigil: arithmetic error: <reason>` to stderr \
               and exits with status 2. This is a **v1-only** surface: Plan \
               B replaces it with a `Raise[ArithError]` effect that the \
               language can catch with a handler. Until then, dividing by \
               zero (or modulo by zero) terminates the process.\n\n\
               Avoid the trap by guarding with an `if` that checks the \
               divisor before the division.\n\n\
               `E0401` is a **runtime** code — unlike `E00xx` (compile-time \
               diagnostics), it is emitted by the runtime library when the \
               compiled program traps, not by the compiler. Its presence in \
               this catalog lets `sigil explain E0401` describe the \
               condition without needing a separate runtime catalog.",
        fix_example: "let q: Int = if d == 0 { 0 } else { n / d };",
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
