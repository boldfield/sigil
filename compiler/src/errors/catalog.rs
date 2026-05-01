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
        code: "E0032",
        short: "stdlib module not found",
        long: "An `import std.X` (or `import std.X.Y`) named a module path with no \
               matching file in the embedded standard library tree. Sigil's stdlib \
               is a fixed set of modules baked into the compiler binary; only modules \
               that ship with the compiler can be imported. Check the path for typos, \
               or remove the import if the module name is wrong.",
        fix_example: "// Wrong:\n// import std.optionz\n\n\
                      // Right:\nimport std.option",
    },
    ErrorEntry {
        code: "E0033",
        short: "circular stdlib import",
        long: "Two or more stdlib modules import each other directly or via a chain. \
               The import resolver loads each module exactly once via depth-first \
               traversal; a cycle would loop forever. This is a stdlib bug, not a \
               user error: a stdlib commit introduced an `import std.X` whose \
               transitive imports re-enter the importer. Report the diagnostic with \
               the module names from the cycle so the stdlib import graph can be \
               un-cycled.",
        fix_example: "// Stdlib bug. No user-side fix. Report with the cycle named in the diagnostic.",
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
        code: "E0110",
        short: "pattern form not supported in v1",
        long: "Plan A3 pattern matching deliberately excludes three ergonomic \
               extensions that other ML/Rust-family languages include:\n\n\
               - **Or-patterns** `p1 | p2 => body`. Write each variant as a \
                 separate arm. Rationale: or-patterns obscure which names \
                 bind where and make exhaustiveness errors harder to read; \
                 explicit arms keep the match intent obvious.\n\
               - **Pattern guards** `pat if cond => body`. Move the \
                 condition into the arm body (an `if`) or into an explicit \
                 nested match. Rationale: guards turn exhaustiveness checks \
                 from trivial into a decidable-but-subtle problem, and they \
                 hide flow control behind seemingly-declarative syntax.\n\
               - **As-bindings** `pat as name`. Destructure via constructor \
                 / tuple patterns that already name the pieces you want. \
                 Rationale: as-bindings introduce a second, redundant \
                 binding path that makes reading a match arm harder.\n\n\
               These are all \"fight-the-priors\" decisions: the extensions \
               are popular but they consistently degrade the code they \
               appear in. A3 patterns are literal, wildcard `_`, fresh \
               variable, constructor `Ctor(..)` / `Ctor { .. }`, and tuple \
               `(pat, pat)`. That surface handles the full set of data \
               shapes Plan A3 introduces.\n\n\
               If a v1 program reaches a point where an or-pattern or guard \
               would be the right tool, the answer is to restructure the \
               match into multiple arms or to use a nested `match` / `if` \
               in the arm body — the cost is a few extra lines; the \
               benefit is that the pattern shape fully determines which \
               names bind and which arm runs.",
        fix_example: "// Or-pattern — instead of `Red | Green => ...`:\n\
                      match c {\n  Red => handle_primary(),\n  Green => handle_primary(),\n  Blue => handle_blue(),\n}\n\n\
                      // Guard — instead of `Some(n) if n > 0 => ...`:\n\
                      match o {\n  Some(n) => if n > 0 { positive() } else { non_positive() },\n  None => default(),\n}\n\n\
                      // As-binding — instead of `Pair(a, b) as whole => ...`:\n\
                      match p {\n  Pair(a, b) => use_parts(a, b),  // reference `a`, `b` directly\n}",
    },
    ErrorEntry {
        code: "E0112",
        short: "unknown type name",
        long: "A `TypeExpr` referenced a type name that is neither a Plan A2 \
               primitive (`Int`, `String`, `Unit`, `Bool`, `Char`, `Byte`) nor \
               a user-defined type declared in the current program via \
               `type Name = ...`.\n\n\
               Plan A3 resolves type names in a single pre-pass before any \
               function body is typechecked; forward references are therefore \
               fine, but the referenced name must still exist somewhere in \
               the program. Typos, a missing `type` declaration, and imports \
               that never landed (imports are Plan A1 stdlib-only in v1) are \
               the common causes.\n\n\
               When E0112 fires on a function signature, the checker falls \
               back to `Unit` for the unresolved type so body-level type \
               errors still surface on the same compile — downstream \
               diagnostics may therefore reference `Unit` where the source \
               said the missing name.",
        fix_example: "// Declare the type before (or after) using it:\n\
                      type Option = | None | Some(Int)\n\
                      fn unwrap_or(o: Option, d: Int) -> Int ![] { d }",
    },
    ErrorEntry {
        code: "E0114",
        short: "unknown constructor",
        long: "A constructor application referenced a name that does not belong \
               to any registered user-defined type's variant list. Plan A3 \
               registers constructor names in a single flat namespace across \
               all `type` declarations in the program; a missing `type` decl, \
               a typo in the constructor name, or a constructor defined in a \
               separate file (imports are Plan A1 stdlib-only) all trip this \
               diagnostic.\n\n\
               E0114 fires regardless of how the constructor is applied: \
               bare identifier (for nullary constructors), `Foo(args)` \
               positional call, or `Foo { fields }` record form. If the \
               constructor exists under a different name, or if the call \
               shape doesn't match the declared shape, E0115 surfaces that \
               distinct problem.",
        fix_example: "type Option = | None | Some(Int)\n\
                      fn f() -> Option ![] { Some(1) }  // Some is a registered ctor\n\n\
                      // NOT:\n\
                      // fn g() -> Option ![] { Maybe(1) }  // E0114: Maybe unknown",
    },
    ErrorEntry {
        code: "E0115",
        short: "constructor application shape mismatch",
        long: "A constructor application used a form (bare identifier, \
               positional call, record literal) that does not match the \
               constructor's declared variant shape. Each variant declares \
               exactly one shape in its `type` declaration:\n\n\
               - Unit variants (`| None`) apply as bare identifiers: `None`.\n\
               - Positional variants (`| Some(Int)`) apply as function-call \
                 syntax: `Some(42)`.\n\
               - Record variants (`| Point { x: Int, y: Int }`) apply as \
                 record-literal syntax: `Point { x: 1, y: 2 }`.\n\n\
               E0115 also fires on positional-arity mismatch (wrong number \
               of arguments for a positional variant) and on record-field \
               mismatches (missing, unknown, or duplicate field name for a \
               record variant). The mismatch kind is named in the message.",
        fix_example: "type Point = { x: Int, y: Int }\n\
                      // p: Point = Point(1, 2);   // E0115: record shape expected\n\
                      // p: Point = Point { x: 1 }; // E0115: missing field `y`\n\
                      let p: Point = Point { x: 1, y: 2 };  // correct",
    },
    ErrorEntry {
        code: "E0117",
        short: "pattern shape does not match scrutinee type",
        long: "A constructor, tuple, or variable pattern in a `match` arm \
               names or structures a value that cannot belong to the \
               scrutinee's type. Plan A3 verifies pattern shape against \
               the scrutinee's type after Task 38 resolves the scrutinee's \
               nominal type:\n\n\
               - A constructor pattern `Ctor(...)` or `Ctor { .. }` matches \
                 only a scrutinee of the user-defined type that declared \
                 `Ctor`. Matching `Some(n)` against an `Int` fires E0117.\n\
               - A tuple pattern `(a, b)` requires a tuple-typed scrutinee. \
                 Plan A3 v1 has no tuple types in the surface, so every \
                 tuple pattern fires E0117 against every scrutinee.\n\
               - Constructor-argument sub-patterns are checked recursively \
                 against the declared field types; a mismatched sub-pattern \
                 fires E0117 with the sub-scrutinee position.\n\n\
               E0064 (Plan A2) handles the literal-pattern-vs-primitive \
               mismatch case (`0 => ...` against a String scrutinee). \
               E0117 is the Plan A3 counterpart for the structural pattern \
               forms introduced in task 37.",
        fix_example: "type Option = | None | Some(Int)\n\
                      // match opt { None => 0, Some(n) => n }  // ok\n\
                      // match n { Some(k) => k, _ => 0 }       // E0117: Some not a ctor of Int\n\
                      // match opt { (a, b) => a, _ => 0 }     // E0117: no tuple type",
    },
    ErrorEntry {
        code: "E0118",
        short: "duplicate constructor name across types",
        long: "Two user-defined types declared variants with the same \
               constructor name. Plan A3 registers constructor names in a \
               single flat namespace across all `type` declarations; a \
               constructor name like `Some` therefore belongs to exactly \
               one type program-wide.\n\n\
               Rename one of the colliding variants. Future plans may \
               introduce path-qualified syntax (`Option::Some`) to \
               disambiguate, but v1 keeps the surface flat to match the \
               rest of the identifier namespace.",
        fix_example: "type Option = | None | Some(Int)\n\
                      type Result = | Ok(Int) | Err(String)    // different names, fine\n\n\
                      // NOT:\n\
                      // type Option = | None | Some(Int)\n\
                      // type Maybe = | Nothing | Some(Int)  // E0118: Some collides",
    },
    ErrorEntry {
        code: "E0113",
        short: "duplicate type declaration",
        long: "Two `type` declarations in the same program share a name. Plan \
               A3 registers user types in a single flat namespace keyed by \
               name; there is no module scoping in v1 and no shadowing of a \
               prior type declaration. Rename the second declaration or \
               delete it if it is redundant.\n\n\
               A duplicate-type diagnostic is distinct from the redefinition \
               rule for identifiers (E0020): a `type Foo = ...` and a value \
               binding `let Foo: Int = 1` do not collide (distinct \
               namespaces), but two `type Foo = ...` lines do.",
        fix_example: "// Rename or remove one of:\n\
                      type Option = | None | Some(Int)\n\
                      type Result = | Ok(Int) | Err(String)   // different name, fine\n\n\
                      // NOT:\n\
                      // type Option = | None | Some(Int)\n\
                      // type Option = | Nope | Yep(Int)  // E0113: duplicate `Option`",
    },
    ErrorEntry {
        code: "E0120",
        short: "non-exhaustive match on user-defined type",
        long: "A `match` expression on a user-defined (nominal) type does \
               not cover every constructor of the type. Plan A3 requires \
               user-type matches to be structurally exhaustive — either a \
               wildcard arm (`_ => ...`) or a variable-pattern arm that \
               binds the whole scrutinee must be present, OR every \
               declared variant must appear as a dedicated arm. Missing \
               variants are named in the diagnostic message with their \
               field positions filled in by wildcards so the user can \
               paste the witness directly into a new arm.\n\n\
               Related codes:\n\
               - E0066: non-exhaustive match on a primitive scrutinee \
                 (Plan A2 rule — wildcard required except for `Bool` where \
                 both `true` and `false` literals may cover).\n\
               - E0117: pattern shape does not match scrutinee type \
                 (different failure mode — well-formed exhaustiveness \
                 implies well-formed shapes first).\n\n\
               Plan A3 shipped top-level exhaustiveness only. Plan B \
               extends the check with full nested Maranget: \
               `match o { Some(true) => .., None => .. }` on \
               `Option = | None | Some(Bool)` now produces E0120 with \
               witness `Some(false)` at compile time rather than \
               falling through to the runtime `TRAP_NONEXHAUSTIVE_MATCH` \
               trap. Nested witness formats follow the same paste-able \
               rules: positional field holes with the uncovered value \
               in place (`Some(false)`, `Holds(Node(_, _, _))`) and \
               record fields (`P { a: false, b: _ }`) in declared \
               field order. Infinite-domain primitive fields (Int, \
               Char, String, Byte, Fn) still require a wildcard to \
               cover — their witnesses use `_` since no concrete \
               counterexample is surfaceable.\n\n\
               E0120 is suppressed when any match arm emits a \
               pattern-check or body-check error (E0117, E0115, E0065, \
               or any other code from `check_pattern`/`check_expr`). \
               Fix the arm-level error first; re-running typecheck \
               re-evaluates exhaustiveness against the corrected arms. \
               The suppression mirrors the common cascade pattern where \
               a mistyped arm looks like a \"missing variant\" to the \
               exhaustiveness pass and produces a noisy double-fire.",
        fix_example: "type Option = | None | Some(Int)\n\
                      // match o { None => 0 }  // E0120: missing `Some(_)`\n\
                      match o {\n  None => 0,\n  Some(_) => 1,\n}  // exhaustive",
    },
    ErrorEntry {
        code: "E0130",
        short: "user-type layout too large (reserved)",
        long: "Plan A3 user types whose payload word count exceeds the \
               6-bit field in the object header (>63 payload words) need \
               the external-descriptor escape hatch (tag `0xFF`), which \
               ships in Plan B. v1 emits E0130 at codegen when a type's \
               computed layout would require it, so the user sees a clear \
               size ceiling rather than a silent header truncation. In \
               practice Plan A3's surface syntax (records + positional \
               variants with primitive or user-type fields) rarely \
               approaches the ceiling; the guard exists primarily for \
               safety, not as a regular user-facing diagnostic.\n\n\
               This catalog entry is reserved: Plan A3 registers the \
               diagnostic without emitting it in Stage 4 code paths \
               (Task 40's codegen layout check is the emission site and \
               it fires only at the 64-word boundary). Presence here \
               keeps `sigil explain E0130` informative if a user ever \
               trips it.",
        fix_example: "// Refactor the type to nest records instead of\n\
                      // flattening: a `Page { lines: Lines }` with\n\
                      // `Lines = { l0: ..., l1: ..., .. }` pushes the\n\
                      // top-level payload under the 64-word ceiling.",
    },
    ErrorEntry {
        code: "E0126",
        short: "occurs check failed (recursive type)",
        long: "HM unification (Plan B task 48) tried to bind a type \
               variable to a structure that mentions the same \
               variable, which would create an infinite type. The \
               classic example is unifying `?A` with `?A -> Int`: \
               solving the equation requires `?A = (?A -> Int) = \
               ((?A -> Int) -> Int) = ...` ad infinitum. The checker \
               rejects the binding rather than diverge.\n\n\
               Common cause: a generic function used non-uniformly. \
               For example, `fn loop[A](x: A) -> A { loop(loop) }` \
               unifies `A` with `A -> A`, which fails the occurs \
               check. The fix is usually to split the recursive \
               case into two separate generic parameters or to \
               rethink the recursion's type shape.",
        fix_example: "// Avoid unifying a variable with itself wrapped\n\
                      // in a constructor — split the recursion's\n\
                      // generic params so each occurrence is fresh.",
    },
    ErrorEntry {
        code: "E0127",
        short: "row occurs check failed (recursive effect row)",
        long: "HM row unification (Plan B task 48) tried to bind a \
               row variable to a row that mentions the same \
               variable, which would create an infinite effect row. \
               This typically only arises through accidental \
               aliasing during inference; a clean program rarely \
               hits this directly.\n\n\
               If you see E0127 it usually points to a row-polymorphic \
               function being called with mutually-recursive row \
               constraints. The fix is to declare separate row \
               variables on each generic position rather than \
               sharing one across nested calls.",
        fix_example: "// Use distinct row variables on each generic\n\
                      // position rather than sharing one through\n\
                      // mutually-recursive callers.",
    },
    ErrorEntry {
        code: "E0128",
        short: "effect row mismatch",
        long: "HM row unification (Plan B task 48) found two effect \
               rows that cannot be reconciled. The closed-row \
               discipline is the most common source: a function \
               declared with a closed row `![IO]` cannot absorb \
               additional effects, so unifying `![IO]` with `![IO, \
               Raise[String]]` fails — the closed row says \"these \
               are the only effects this function performs\".\n\n\
               Open rows (those declared with an explicit row \
               variable, `![IO | e]`) can absorb additional effects \
               at unification time. Mixing closed and open is fine \
               as long as the closed side covers the open side's \
               known effects.\n\n\
               Fix: either add the missing effect to the closed-row \
               function's declared effect list, or add an explicit \
               row variable so the row becomes open.",
        fix_example: "// closed row — only IO, fails to absorb Raise:\n\
                      // fn f() -> Int ![IO] { ... }\n\
                      // open row — accepts richer caller rows:\n\
                      // fn f[e]() -> Int ![IO | e] { ... }",
    },
    ErrorEntry {
        code: "E0129",
        short: "type-argument arity mismatch",
        long: "Plan B task 48: a generic type was applied with a \
               number of type arguments that doesn't match its \
               declaration. `type List[A] = ...` requires exactly \
               one argument; writing `List[Int, String]` or `List` \
               (no args) fails this check.\n\n\
               Plan B v1 does not infer omitted type arguments at \
               type-name positions; every `Apply` must list every \
               declared parameter. Future plans may add inference \
               for omitted arguments, but the explicit form remains \
               canonical.",
        fix_example: "// Declared:  type List[A] = | Nil | Cons(A, List[A])\n\
                      // Wrong:     fn use(xs: List)            // E0129\n\
                      // Wrong:     fn use(xs: List[Int, Int])  // E0129\n\
                      // Right:     fn use(xs: List[Int])",
    },
    ErrorEntry {
        code: "E0131",
        short: "primitive or generic-parameter type cannot take type arguments",
        long: "Plan B task 48: only declared generic types accept \
               type arguments via the `Name[T1, T2]` syntax. \
               Primitives (`Int`, `String`, `Unit`, `Bool`, `Char`, \
               `Byte`) are atomic — they have no parameters. \
               Generic parameters (the `A` in `fn id[A](x: A)`) \
               are placeholders for types and likewise cannot be \
               applied to other types.\n\n\
               If you want to wrap a primitive in a generic \
               container, use a declared generic type and apply \
               *it*: `Option[Int]` or `List[Int]`, not `Int[Foo]`.",
        fix_example: "// Wrong:  fn f(x: Int[Foo]) -> Int ![] { x }   // E0131\n\
                      // Right:  fn f(x: Option[Int]) -> Int ![] { ... }",
    },
    ErrorEntry {
        code: "E0132",
        short: "ambiguous polymorphism: type parameter is unconstrained at this call site",
        long: "Plan B task 49: when a generic function is called with no \
               argument that pins one of its declared type parameters, \
               the type parameter has no concrete value to specialise \
               at. Monomorphization can't generate a clone, and the \
               program would silently render with a placeholder name \
               that two distinct unresolved sites might collide at.\n\n\
               This fires for fns like `fn nothing[A]() -> Unit ![] { ... }` \
               called as `nothing()` — `A` has no input that constrains \
               it. The fix is either to give the call site a context \
               that pins the parameter (e.g. via a let-binding's \
               annotation, an `if`-branch unification, or by passing a \
               value of the right shape), or to drop the parameter \
               from the function's signature when the body doesn't \
               need it.\n\n\
               This diagnostic only fires at end-of-typecheck, after \
               the substitution has been fully resolved — so it picks \
               up only genuinely-unconstrained parameters, not ones \
               that are still free because they reference an enclosing \
               generic fn's parameters (those resolve when \
               monomorphization clones the enclosing fn).",
        fix_example: "// Wrong:\n\
                      //   fn nothing[A]() -> Unit ![] { unit_value }\n\
                      //   fn main() -> Int ![] {\n\
                      //     nothing();   // E0132 — what is A?\n\
                      //     0\n\
                      //   }\n\n\
                      // Right:  drop the unused parameter\n\
                      //   fn nothing() -> Unit ![] { unit_value }\n\n\
                      // Right:  pin via context\n\
                      //   fn id[A](x: A) -> A ![] { x }\n\
                      //   let v: Int = id(42);  // pins A := Int",
    },
    // E0133 (staged effect-declaration gate) and E0134 (staged
    // handle-expression gate) were introduced in Plan B Task 53 to
    // keep partial Plan B programs out of the CPS-pending codegen
    // path. Plan B Task 55 lifts both gates: codegen now lowers
    // `Item::Effect` (no codegen output, registered for perform
    // dispatch at typecheck time) and `Expr::Handle` (Phase 2:
    // body-pass-through when the body has no non-IO perform; Phase
    // 3+: full handler-frame setup + CPS calling convention). Both
    // catalog entries removed accordingly. Code numbers E0133 and
    // E0134 stay retired (per the
    // `seed_entries_are_unique_and_non_empty` discipline rule, never
    // renumber a once-shipped code).
    ErrorEntry {
        code: "E0136",
        short: "duplicate effect declaration",
        long: "Plan B task 54: an effect name is declared by more than one \
               `effect Name { ... }` item in the program. Effect names \
               share a single flat namespace; duplicates would make \
               handler dispatch ambiguous because a handler arm's \
               `Effect.op` reference cannot pick between two declarations \
               of `Effect`. Reported against the second (and subsequent) \
               offender's name span; the first declaration wins in the \
               registry so downstream typing always picks a single \
               canonical operation set per effect name.",
        fix_example: "// Wrong:\n\
                      effect Raise { fail: (String) -> Int }\n\
                      effect Raise { other: () -> Int }   // E0136 here\n\n\
                      // Right (rename the second):\n\
                      effect Raise { fail: (String) -> Int }\n\
                      effect Other { other: () -> Int }",
    },
    ErrorEntry {
        code: "E0137",
        short: "duplicate operation name in effect declaration",
        long: "Plan B task 54: an operation name appears more than once \
               in the same `effect Name { ... }` body. Within a single \
               effect declaration, operation names share a flat \
               namespace — handler arms `Name.op(...)` reference an \
               operation by name and cannot pick between two declarations \
               of `op` on the same effect. Reported against the second \
               occurrence's name span; the first wins.",
        fix_example: "// Wrong:\n\
                      effect Choose {\n\
                        pick: () -> Int,\n\
                        pick: (Int) -> Int,   // E0137 here\n\
                      }\n\n\
                      // Right (rename, or merge):\n\
                      effect Choose {\n\
                        pick: () -> Int,\n\
                        pick_in: (Int) -> Int,\n\
                      }",
    },
    ErrorEntry {
        code: "E0138",
        short: "handler arm references unknown effect",
        long: "Plan B task 54: a `handle ... with { Effect.op(...) => ... }` \
               arm names an `Effect` that is not declared anywhere in the \
               program. Effect declarations are top-level items \
               (`effect Effect { ... }`) and are visible everywhere in the \
               file once present.\n\n\
               This fires when the user typo's an effect name or forgets \
               to declare it before writing the handler. The fix is either \
               to add the matching `effect Effect { ... }` declaration or \
               to correct the spelling.",
        fix_example: "// Wrong (no `effect Raise`):\n\
                      fn safe(x: Int) -> Int ![] {\n\
                        handle x with { Raise.fail(msg, k) => 0 }   // E0138\n\
                      }\n\n\
                      // Right:\n\
                      effect Raise { fail: (String) -> Int }\n\
                      fn safe(x: Int) -> Int ![] {\n\
                        handle x with { Raise.fail(msg, k) => 0 }\n\
                      }",
    },
    ErrorEntry {
        code: "E0139",
        short: "handler arm references unknown operation on declared effect",
        long: "Plan B task 54: a `handle ... with { Effect.op(...) => ... }` \
               arm names an `Effect.op` whose `op` is not declared in \
               `Effect`'s body. The effect itself is recognised; the \
               operation is the part that doesn't match.\n\n\
               This fires when the user typo's an operation name or \
               references an operation that hasn't been added to the \
               effect declaration yet. The fix is either to add the \
               missing operation to the `effect` body or to correct the \
               spelling.",
        fix_example: "// Wrong (Raise has only `fail`, not `panic`):\n\
                      effect Raise { fail: (String) -> Int }\n\
                      handle x with { Raise.panic(msg, k) => 0 }   // E0139\n\n\
                      // Right (correct the op name):\n\
                      handle x with { Raise.fail(msg, k) => 0 }",
    },
    ErrorEntry {
        code: "E0140",
        short: "duplicate handler arm for the same Effect.op",
        long: "Plan B task 54: a `handle ... with { ... }` expression has \
               two arms that both name the same `Effect.op`. Handler \
               dispatch is by op identity; two arms for the same op are \
               redundant — the second is unreachable.\n\n\
               Reported against the second (and subsequent) duplicate's \
               span. The first declaration wins. The fix is to delete one \
               of the redundant arms or merge their bodies.",
        fix_example: "// Wrong:\n\
                      handle x with {\n\
                        Raise.fail(msg, k) => 0,\n\
                        Raise.fail(msg2, k2) => 1,   // E0140 here\n\
                      }\n\n\
                      // Right (one arm per op):\n\
                      handle x with {\n\
                        Raise.fail(msg, k) => 0,\n\
                      }",
    },
    ErrorEntry {
        code: "E0141",
        short: "handler arm parameter count does not match operation declaration",
        long: "Plan B task 54: a `handle ... with { Effect.op(p1, ..., k) }` \
               arm binds N user-parameters before the trailing continuation \
               `k`, but the operation `Effect.op` declares M parameters. \
               The arm's binding shape must match: one binding per declared \
               operation parameter, plus exactly one trailing continuation \
               parameter.\n\n\
               This fires when the user adds or removes a parameter from \
               an effect declaration without updating handler arms, or \
               when an arm forgets the trailing `k`. The fix is to align \
               the arm's binding count with the operation's declared \
               parameter count.",
        fix_example: "// Wrong (op declares 1 param, arm binds 2):\n\
                      effect Raise { fail: (String) -> Int }\n\
                      handle x with {\n\
                        Raise.fail(msg, extra, k) => 0,   // E0141 here\n\
                      }\n\n\
                      // Right (one binding per declared param + k):\n\
                      handle x with {\n\
                        Raise.fail(msg, k) => 0,\n\
                      }",
    },
    ErrorEntry {
        code: "E0142",
        short: "handler does not cover every operation declared on the discharged effect",
        long: "Plan B Stage 6 cleanup: a `handle ... with { ... }` block \
               that lists arms for an effect must cover every operation \
               declared on that effect. The body's effect row treats the \
               effect as discharged, so any operation NOT covered by an \
               arm would runtime-abort if performed. The exhaustiveness \
               check surfaces this at compile time instead of at runtime.\n\n\
               This rule applies per discharged effect: if the handler \
               targets `Effect.op_a` but `Effect` declares both `op_a` \
               and `op_b`, an arm for `op_b` is required even if the \
               body never performs `op_b` (the body's row claims the \
               full effect is discharged). The fix is to add the missing \
               arm(s); use `=> k(default_value)` or `=> some_constant` \
               for ops you don't expect to fire but need structurally \
               present.\n\n\
               Pre-Stage-6-cleanup, this constraint was a runtime abort \
               from `sigil_perform`'s null-arm-slot path (pinned by the \
               `#[ignore]`'d e2e `partial_handler_of_multi_op_effect_-\
               aborts_at_runtime_pending_resolution`). The Stage 6 \
               cleanup PR resolves it via this typecheck-time check, \
               un-ignores the test, and inverts it to assert E0142.",
        fix_example: "// Wrong (handler covers `div_by_zero` but not `mod_by_zero`):\n\
                      effect ArithError { div_by_zero: () -> Int, mod_by_zero: () -> Int }\n\
                      handle safe_divide(a, b) with {\n\
                        ArithError.div_by_zero(k) => 0,   // E0142 here\n\
                      }\n\n\
                      // Right (cover both ops; mod_by_zero arm is unreachable\n\
                      // in this program but its presence keeps coverage exhaustive):\n\
                      handle safe_divide(a, b) with {\n\
                        ArithError.div_by_zero(k) => 0,\n\
                        ArithError.mod_by_zero(k) => 0,\n\
                      }",
    },
    ErrorEntry {
        code: "E0143",
        short: "row-site effect-arg arity does not match the effect-decl's generic-param count",
        long: "Plan D Task 114 introduced this check; Plan D Task 115 \
               (PR #55) renamed the code from E0140 → E0143 to \
               disambiguate from the existing E0140 (duplicate-handler-\
               arm) — a future agent reading older PR descriptions / \
               commit messages will see references to the original \
               E0140 number and should treat them as referring to this \
               diagnostic. A row entry references an effect with a \
               type-arg list (e.g. `![Raise[Int]]`) whose arity does \
               not match the effect-decl's `generic_params` count. \
               Three shapes fire E0143:\n\n\
               1. **Non-generic effect-decl referenced with args** — \
                  e.g. `![IO[Int]]` when `IO` is non-generic.\n\
               2. **Generic effect-decl referenced bare** — e.g. \
                  `![Raise]` when the decl is `effect Raise[E]`. The \
                  bare reference has no way to instantiate `E`, so \
                  perform sites of `Raise.fail` would fail elsewhere; \
                  E0143 surfaces the issue at the row site.\n\
               3. **Wrong arity** — e.g. `![Raise[Int, String]]` when \
                  the decl is `effect Raise[E]` (one type param). The \
                  row-site arg count must match the decl's \
                  `generic_params` length.\n\n\
               The fix is to align the row-site arg list with the \
               effect-decl's declared generics.",
        fix_example: "// Wrong (Raise[E] is generic, ![Raise] doesn't instantiate E):\n\
                      effect Raise[E] { fail: (E) -> Int }\n\
                      fn risky() -> Int ![Raise] { 0 }   // E0143 here\n\n\
                      // Right (instantiate E at the row site):\n\
                      effect Raise[E] { fail: (E) -> Int }\n\
                      fn risky() -> Int ![Raise[Int]] { 0 }",
    },
    ErrorEntry {
        code: "E0144",
        short: "per-op generic parameter shadows an effect-decl generic parameter",
        long: "Plan D Task 115: a per-op generic parameter declared on an \
               effect operation has the same name as a generic parameter \
               on the enclosing effect declaration. Shadowing per-op \
               generics is forbidden because the effect-decl's parameter \
               is in scope inside the op's signature, and an inner shadow \
               would silently mask it for the op's params / return.\n\n\
               The fix is to use a distinct name for the per-op generic \
               (the canonical Koka / Effekt idiom uses single-letter \
               names: `E` for the effect-decl-level generic, `A` / `B` \
               for per-op generics).",
        fix_example: "// Wrong (per-op `E` shadows effect-decl `E`):\n\
                      effect Raise[E] { fail[E]: (E) -> Int }   // E0144 here\n\n\
                      // Right (use distinct names):\n\
                      effect Raise[E] { fail[A]: (E) -> A }",
    },
    ErrorEntry {
        code: "E0145",
        short: "continuation `k` cannot escape its handle's arm body",
        long: "Plan D Task 117: a `Ty::Continuation` value (the `k` \
               binding inside a handler arm body, or any let-binding \
               that aliases one) is dynamic-extent: it remains valid \
               only inside the arm body that introduced it. Storing \
               `k` in any cross-storage / cross-fn position lets it \
               escape to a context where its originating handle's \
               frame may have been popped, producing undefined \
               behavior at runtime.\n\n\
               Four escape shapes fire E0145:\n\n\
               1. Returning `k` (or a let-binding aliased to `k`) \
                  from a fn whose declared return type is not a \
                  matching `Continuation`.\n\
               2. Storing `k` in a record/ctor field whose declared \
                  type is not a matching `Continuation`.\n\
               3. Passing `k` to a fn parameter whose declared type \
                  is not a matching `Continuation`.\n\
               4. Cross-handle: unifying a `Continuation` with \
                  another `Continuation` whose `scope_id` originates \
                  from a different `handle` expression. This catches \
                  `k`-from-outer-handle leaking into inner-handler-\
                  -arm contexts where the inner handle's frame would \
                  pop the outer's `k` out of dynamic extent.\n\n\
               The fix in all cases is to keep `k` inside the handle's \
               arm body. Multi-shot resumption (`effect E resumes: \
               many`) lifts the linearity rule (E0220) but does NOT \
               lift the escape barrier — `k` may be invoked multiple \
               times within the arm, but cannot be smuggled out.\n\n\
               Lambda capture of `k` is rejected today via E0220 \
               (one-shot effects) or via the `ArmKPairCapture` \
               discharge-with-lambda machinery (multi-shot effects \
               that lift the lambda back into the same arm body). \
               Lambda-capture-k-with-E0145 inheritance and the \
               generic-instantiation bypass (`id(k)` for generic \
               `id[A]`) are deferred to a follow-up PR titled \
               \"complete the E0145 escape barrier.\"",
        fix_example: "// Wrong (returning k from fn whose ret is non-Continuation):\n\
                      effect Raise { fail: () -> Int }\n\
                      fn ret_k() -> Int ![Raise] {\n\
                        handle 0 with {\n\
                          Raise.fail(k) => k,   // E0145 here\n\
                        }\n\
                      }\n\n\
                      // Right (invoke k inside the arm; return its result):\n\
                      fn ret_k() -> Int ![Raise] {\n\
                        handle 0 with {\n\
                          Raise.fail(k) => k(42),\n\
                        }\n\
                      }\n\n\
                      // Wrong (k stored in a record field):\n\
                      type WrapFn = | WrapFn((Int) -> Int ![])\n\
                      handle 0 with {\n\
                        Raise.fail(k) => {\n\
                          let _: WrapFn = WrapFn(k);   // E0145 here\n\
                          0\n\
                        },\n\
                      }",
    },
    ErrorEntry {
        code: "E0146",
        short: "type name `Continuation` is reserved",
        long: "Plan D Task 117 PR #62 followup: the `Continuation` \
               name is reserved for the type-position continuation \
               surface form (`Continuation[op_ret, ret]` names a \
               handler arm's `k` binding type — see error E0145's \
               long-form for the surrounding mechanism). User type \
               declarations cannot use this name; the typecheck \
               surface-form path would silently shunt every reference \
               into the surface-form path, ignoring the user-\
               declared type, and outside a handler arm body the \
               user would see a misleading E0145 (\"Continuation \
               annotations are only valid inside a handler arm \
               body\") for what they think is *their* type.\n\n\
               The fix is to rename the user type. The reserved \
               name is one specific identifier (`Continuation`); \
               variations like `Cont` / `Continuation2` / `MyCont` \
               are unaffected.",
        fix_example: "// Wrong (collides with the reserved name):\n\
                      type Continuation = | Foo   // E0146 here\n\n\
                      // Right (any other name works):\n\
                      type Cont = | Foo",
    },
    ErrorEntry {
        code: "E0220",
        short: "one-shot continuation used more than once on a code path",
        long: "Plan B task 54: in a handler arm for a one-shot effect, \
               the continuation `k` must be invoked or referenced at most \
               once along every path through the arm body. One-shot is \
               the default for effects declared as `effect Name { ... }`; \
               opt-in multi-shot via `effect Name resumes: many { ... }` \
               relaxes this rule.\n\n\
               Zero uses of `k` (early-exit handlers) is fine; one use is \
               fine; any path that uses `k` more than once is an error. \
               Sibling branches of an `if` or `match` may each use `k` \
               once independently — only sequential composition along a \
               single path counts as accumulation.\n\n\
               References to `k` from inside a `lambda` body are \
               conservatively rejected: a lambda's call count is not \
               statically known, so capturing `k` into a closure could \
               invoke it any number of times. Move the `k` invocation out \
               of the lambda, or annotate the effect with \
               `resumes: many` if the closure-capture pattern is \
               intended.",
        fix_example: "// Wrong (k used twice on the same path):\n\
                      effect Raise { fail: (String) -> Int }\n\
                      handle x with {\n\
                        Raise.fail(msg, k) => k(0) + k(1),   // E0220\n\
                      }\n\n\
                      // Right (each branch uses k at most once):\n\
                      handle x with {\n\
                        Raise.fail(msg, k) => if msg == \"x\" { k(0) } else { k(1) },\n\
                      }\n\n\
                      // Also right (multi-shot opt-in):\n\
                      effect Choose resumes: many { pick: () -> Int }\n\
                      handle x with {\n\
                        Choose.pick(k) => k(1) + k(2),   // OK, multi-shot\n\
                      }",
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
