# Sigil — Language Specification (v1)

Sigil is a compiled, statically-typed programming language designed to
be reliably authored by large language models. Programs are parsed by
a strict recursive-descent parser, type-checked by a Hindley–Milner
checker extended with effect rows, lowered to Cranelift IR, and
linked against a small Boehm-GC'd runtime.

This document is **examples-first**: fourteen worked examples
(E1–E14) introduce the language by progressive elaboration. The
reference sections after the examples are intended as lookup, not
linear reading.

> **Authoring contract.** This spec is the LLM's only context for
> Sigil's surface syntax and semantics. Code generated against this
> spec should compile first try at ≥ 70 % of the validation prompt
> bank ([`spec/validation-prompts.md`](validation-prompts.md)) and
> ≥ 90 % after a single error-feedback edit. If a generated program
> fails to parse against this spec but works against the actual
> compiler, that's a spec gap — file an issue.

---

## Worked examples (E1–E14)

### E1 — Hello, world

```sigil
fn main() -> Int ![IO] {
  perform IO.println("hello, world");
  0
}
```

Every function declares an **effect row** in `![ … ]`. `IO` is a
builtin effect with multiple sub-operations (`print`, `println`,
`read_line`, `read_file`, `write_file` — see §13); these examples
use only `IO.println`. `perform` is the syntax
for invoking an effect; the result of `perform IO.println(...)` is
`Unit` (Sigil's no-information type), discarded by the `;`.

`fn main` must return `Int`. A non-zero return becomes the process
exit code; this program returns 0 (success).

### E2 — Arithmetic and the pure effect row

```sigil
fn square(n: Int) -> Int ![] {
  n * n
}

fn main() -> Int ![IO] {
  perform IO.println(int_to_string(square(7)));
  0
}
```

`![]` is the **closed empty effect row** — `square` performs no
effects. The compiler rejects any function with an `![]` row that
contains `perform` or that calls a function with a non-empty row.

`int_to_string(n: Int) -> String ![]` is a builtin (§13.2). String
literals use `"..."` with C-style backslash escapes (`\\`, `\"`,
`\n`, `\t`).

### E3 — Recursion and exhaustive `match`

```sigil
fn fib(n: Int) -> Int ![] {
  match n {
    0 => 0,
    1 => 1,
    _ => fib(n - 1) + fib(n - 2),
  }
}

fn main() -> Int ![IO] {
  perform IO.println(int_to_string(fib(10)));
  0
}
```

`match` is **exhaustive**. Omitting either the `0`, the `1`, or the
`_` arm fires `E0066: \`match\` on \`Int\` is not exhaustive` at
compile time. Patterns include integer literals, sum-type
constructors, record patterns, tuple patterns, identifier patterns
(`name` binds), and the wildcard `_`. Patterns are matched in source
order; the first matching arm wins.

### E4 — Sum types and pattern matching

```sigil
import std.option

fn safe_div(num: Int, den: Int) -> Option[Int] ![ArithError] {
  match den {
    0 => None,
    _ => Some(num / den),
  }
}

fn main() -> Int ![IO, ArithError] {
  match safe_div(10, 0) {
    Some(v) => perform IO.println(int_to_string(v)),
    None => perform IO.println("zero divisor"),
  };
  0
}
```

Sum types are declared with `type T = | Variant1(Args) |
Variant2(Args) | …` (see §6). `Option[A]` is shipped in
[`std/option.sigil`](../std/option.sigil); `Some(x)` and `None` are
its constructors. Constructor names start with an uppercase letter
by convention (the parser does not enforce this in v1, but every
stdlib type follows it).

### E5 — Higher-order functions and lambdas

```sigil
import std.list

fn add_one(n: Int) -> Int ![] { n + 1 }

fn main() -> Int ![IO] {
  let xs: List[Int] = range(1, 5);                    // [1, 2, 3, 4]
  let ys: List[Int] = map(xs, add_one);               // [2, 3, 4, 5]
  let zs: List[Int] = map(ys, fn (n: Int) -> Int ![] => n * 10);
  perform IO.println(int_to_string(length(zs)));
  0
}
```

`map`, `range`, and `length` are in [`std/list.sigil`](../std/list.sigil).
Lambdas have the syntax `fn (params) -> Ret ![Effects] => body` —
parameter types, return type, and effect row are all required, just
like top-level `fn`.

### E6 — Generic functions

```sigil
fn identity[A](x: A) -> A ![] { x }

fn main() -> Int ![IO] {
  let n: Int = identity(42);
  let s: String = identity("hello");
  perform IO.println(int_to_string(n));
  perform IO.println(s);
  0
}
```

Generic parameters are introduced in `[A, B, …]` after the function
name. Each call site instantiates fresh type variables; inference
finds the unique satisfying assignment via Hindley–Milner.

The same syntax extends to generic types:

```sigil
type Box[A] = | Wrap(A)

fn unwrap[A](b: Box[A]) -> A ![] {
  match b { Wrap(x) => x }
}
```

### E7 — Records

```sigil
type Point = { x: Int, y: Int }

fn manhattan(a: Point, b: Point) -> Int ![] {
  let ax: Int = match a { Point { x, y: _ } => x };
  let ay: Int = match a { Point { x: _, y } => y };
  let bx: Int = match b { Point { x, y: _ } => x };
  let by: Int = match b { Point { x: _, y } => y };
  abs(ax - bx) + abs(ay - by)
}

fn abs(n: Int) -> Int ![] {
  match n < 0 {
    true => 0 - n,
    false => n,
  }
}

fn main() -> Int ![IO] {
  let p: Point = Point { x: 1, y: 2 };
  let q: Point = Point { x: 4, y: 6 };
  perform IO.println(int_to_string(manhattan(p, q)));   // 7
  0
}
```

Record fields are declared `name: Type` in the type, constructed
with `Name { name: value, … }`, and destructured via `match` with
`Name { name: binding, … }` (field-pun `name` is shorthand for
`name: name`). v1 has no `.name` field-access syntax; use match
destructuring instead. Records are nominal — two records with the
same fields but different declared names do not unify.

### E8 — Effects: `Raise` for exceptions

```sigil
import std.raise
import std.result

fn parse_pos(n: Int) -> Int ![Raise[String]] {
  match n {
    0 => raise("expected positive"),
    _ => n,
  }
}

fn main() -> Int ![IO] {
  let result: Result[Int, String] = catch(fn () -> Int ![Raise[String]] => parse_pos(0));
  match result {
    Ok(v) => perform IO.println(int_to_string(v)),
    Err(m) => perform IO.println(m),
  };
  0
}
```

`std.raise` ships a generic effect:

```sigil
effect Raise[E] { fail: (E) -> Int }
```

Calling `raise(s)` performs `Raise.fail(s)`; under `catch`'s
discharging handler, the call short-circuits to an `Err` result.
`Raise[E]` is generic over the error type — `Raise[String]` raises
string errors, `Raise[Int]` raises integer error codes, etc.

`catch` is row-polymorphic: `catch[A, E](body: () -> A ![Raise[E] | e]) -> Result[A, E] ![| e]` — it discharges the `Raise[E]` effect and passes any other effects in the row through to the caller.

### E9 — Effects: `State[S]` for threaded state

```sigil
import std.state
import std.pair

fn comp() -> Int ![State[Int]] {
  let _: Int = perform State.set(10);
  let v: Int = perform State.get();
  v + 1
}

fn main() -> Int ![IO] {
  let result: (Int, Int) = run_state(5, comp);   // (11, 10)
  perform IO.println(int_to_string(fst(result))); // 11
  0
}
```

`State[S]` is parametric over the state type `S`. `run_state[A, S]
(initial, body)` discharges the `State[S]` effect by threading
`initial` through every `perform State.get/set` site in `body`'s call
tree, returning `(A, S)` — the body's result paired with the final
state. The discharger is a higher-order function defined in pure Sigil
(see [`std/state.sigil`](../std/state.sigil)). Both type parameters
are inferred from the call site (e.g. `run_state(5, comp)` instantiates
`A = Int`, `S = Int`).

### E10 — Multi-effect rows

```sigil
import std.raise

fn pipeline(s: String) -> Int ![IO, Raise[String]] {
  perform IO.println(string_concat("processing: ", s));
  match string_compare(s, "") {
    0 => raise("empty input"),
    _ => string_length(s),
  }
}

fn main() -> Int ![IO] {
  let r: Result[Int, String] = catch(fn () -> Int ![IO, Raise[String]] => pipeline("hello"));
  match r {
    Ok(n) => perform IO.println(int_to_string(n)),
    Err(m) => perform IO.println(string_concat("error: ", m)),
  };
  0
}
```

Effect rows are unordered sets of effect names. `![IO, Raise[String]]` and
`![Raise[String], IO]` are the same row. A function with row `![Raise[String]]`
may be called from any row that **contains** `Raise[String]`; `![IO, Raise[String]]`
calls `![Raise[String]]` callees freely.

`catch` is row-polymorphic — it accepts bodies with extra effects
beyond `Raise[E]` and passes them through. The `| e` row variable
in `catch`'s signature captures the residual row.

### E11 — Mutable state via the `Mem` effect

```sigil
fn main() -> Int ![IO, Mem] {
  let zero: Byte = byte_truncate(0);
  let buf: MutByteArray = mut_byte_array_new(4, zero);
  mut_byte_array_set(buf, 0, byte_truncate(72));   // 'H'
  mut_byte_array_set(buf, 1, byte_truncate(105));  // 'i'
  let h: Byte = mut_byte_array_get(buf, 0);
  let i: Byte = mut_byte_array_get(buf, 1);
  perform IO.println(int_to_string(byte_to_int(h)));
  perform IO.println(int_to_string(byte_to_int(i)));
  0
}
```

`Mem` is a marker effect (zero ops, no perform-dispatch); it gates
mutation: `mut_byte_array_set` requires `Mem` in the row. Pure
functions (`![]`) cannot mutate. See §9.

`StringBuilder` (`std/string_builder.sigil`) is the canonical
incremental-string surface under `Mem` — see E12.

### E12 — Building a JSON document with `StringBuilder`

```sigil
import std.string_builder

fn render() -> String ![Mem] {
  let sb: StringBuilder = sb_new();
  sb_append(sb, "{\"name\": \"ada\", \"count\": ");
  sb_append(sb, int_to_string(36));
  sb_append(sb, "}");
  sb_finalize(sb)
}

fn main() -> Int ![IO, Mem] {
  perform IO.println(render());                                // {"name": "ada", "count": 36}
  0
}
```

`sb_new() -> StringBuilder ![Mem]` allocates a fresh segmented
rope; `sb_append` writes into the tail segment (allocating new
4 KiB segments on overflow); `sb_finalize` packs everything into a
single `String`. Avoids the O(n²) cost of repeated
`string_concat`.

For a fuller example see [`examples/json.sigil`](../examples/json.sigil).

### E13 — Tuples and pair destructuring

```sigil
import std.pair

fn swap(p: (Int, String)) -> (String, Int) ![] {
  match p { (a, b) => (b, a) }
}

fn main() -> Int ![IO] {
  let pair: (Int, String) = (42, "hello");
  perform IO.println(int_to_string(fst(pair)));      // 42
  perform IO.println(snd(pair));                      // hello
  let swapped: (String, Int) = swap(pair);
  perform IO.println(fst(swapped));                   // hello
  0
}
```

Tuple types are written `(T1, T2, ...)` and tuple values as
`(e1, e2, ...)`. Tuples of any arity are supported. Binary tuples
can use `fst[A, B]` and `snd[A, B]` from `std.pair`; all tuples
support destructuring in match patterns with `(p1, p2, ...)`.

### E14 — Nondeterminism with `Choose`

```sigil
import std.choose
import std.list
import std.io

fn pick_pair() -> Int ![Choose] {
  let a: Int = perform Choose.choose(3);
  let b: Int = perform Choose.choose(3);
  a * 10 + b
}

fn main() -> Int ![IO] {
  let results: List[Int] = all_choices(pick_pair);
  perform IO.println(int_to_string(length(results)));   // 9
  0
}
```

`Choose` is a multi-shot effect (`resumes: many`): the handler can
invoke the continuation multiple times per perform. `all_choices`
enumerates every branch by resuming `k(0)`, `k(1)`, …, `k(n-1)` and
collecting results into a list. `first_choice` returns the first
non-failing branch as `Option[A]`.

---

## Reference

### §1 — Lexical structure

Sigil source is UTF-8. The parser reads tokens line by line; line
endings are not significant beyond delimiting line / column for
diagnostics.

**Comments.** Line comments start with `//` and run to end of
line. There are no block comments in v1.

**Identifiers.** `[A-Za-z_][A-Za-z0-9_]*`. Identifiers are
case-sensitive. Constructor / type names conventionally start with
uppercase; variable / function names with lowercase. The parser
enforces no case rule in v1, but the stdlib follows it.

**Keywords (reserved).** `fn`, `let`, `match`, `if`, `else`, `true`,
`false`, `effect`, `perform`, `handle`, `with`, `return`, `import`,
`type`, `as`, `resumes`. Reserved keywords cannot be used as
identifiers.

**Literals.**
- Integer: decimal `42`, `-7`, `0`. No hex/oct/bin literals in v1.
  Range: `[-2^62, 2^62)` (63-bit tagged Int).
- String: `"..."` with escapes `\\`, `\"`, `\n`, `\t`, `\r`.
- Char: `'a'`, `'\n'`. Width: 1 byte (ASCII / latin-1 codepoint).
  v1 has no codepoint-aware string ops.
- Byte: constructed via `byte_truncate(n: Int) -> Byte ![]` (truncates
  to low 8 bits) and validated with `byte_in_range(n: Int) -> Bool ![]`.
- Bool: `true` and `false` are the literal forms of the builtin
  `Bool` type. Pattern-match with `match b { true => ..., false => ... }`.
- Float: `3.14`, `1e10`, `2.5e-3`. IEEE 754 f64, heap-boxed.
  Requires digits before and after the decimal point (`3.0` not `3.`
  or `.3`). Exponent form uses `e`/`E` with optional `+`/`-`,
  but requires at least one digit after the marker (`1e10` is a
  float; `1e` is integer `1` followed by identifier `e`).
  Negative float literals: `-3.14` (unary minus is constant-folded).
  Float literals are not valid in pattern position.
- Unit: `()`. The singleton value of type `Unit`. Not valid in
  pattern position.

### §2 — Top-level items

A program is a sequence of top-level items, in any order:

```sigil
import std.io                                   // import
type Color = | Red | Green | Blue              // type declaration
effect Counter { tick: () -> Int }              // effect declaration
fn main() -> Int ![IO] { 0 }                   // function
```

**`fn` syntax.** `fn name[generics](params) -> RetType ![Effects] body`.
- Generic params (`[A, B, …]`) are optional; absent means no quantifier.
- Each `param` is `name: Type`.
- Return type is mandatory.
- Effect row is mandatory (use `![]` for pure).
- Body is an expression; multi-statement bodies use a block (§5).

### §3 — Type system

#### §3.1 — Built-in types

| Type | Description |
|------|-------------|
| `Int` | 63-bit signed integer. |
| `Bool` | `true` / `false`. |
| `String` | Immutable UTF-8 byte sequence. |
| `Char` | 1-byte codepoint. |
| `Byte` | 1-byte unsigned integer (0..255). |
| `Unit` | The single-value type. Literal: `()`. |
| `Array[A]` | Immutable indexed collection. |
| `MutArray[A]` | Mutable indexed collection (Mem-gated). |
| `ByteArray` | Immutable flat byte buffer. |
| `MutByteArray` | Mutable flat byte buffer (Mem-gated). |
| `Float` | Boxed IEEE 754 f64. |
| `Int64` | Boxed 64-bit signed integer. |
| `StringBuilder` | Segmented-rope string accumulator (Mem-gated). |
| `(T1, T2, …)` | Tuple types of arbitrary arity. Binary tuples have `fst`/`snd` accessors in `std.pair`. |
| `Continuation[OpRet, Ret]` | First-class single-shot or multi-shot continuation captured from a handler arm's `k`. Dynamic-extent enforcement via scope IDs. |

User-declared sum types and records form the rest of the type
universe (§6).

#### §3.2 — Type expressions

```text
type-expr := identifier
           | identifier "[" type-expr ("," type-expr)* "]"   -- generic instantiation
           | "(" type-expr ("," type-expr)* ")" "->" type-expr "![" effects "]"
                                                              -- function type
           | "(" type-expr ("," type-expr)* ")"              -- tuple type
```

Function types carry effect rows just like declarations:
`(Int) -> Int ![]` is the type of a pure unary integer function;
`(String) -> Int ![Raise[String]]` is a fallible parser.

Tuple types are written `(T1, T2)` — parentheses with comma-separated
element types. Arity 1 is not a tuple (it's just a parenthesized
type); arity 2+ creates a distinct tuple type.

#### §3.3 — Effect rows

An effect row is a comma-separated set of effect names enclosed in
`![ … ]`:

```text
![]                            -- pure
![IO]                          -- can do IO
![IO, Raise[String], Mem]      -- can do all three
```

Rows are unordered. Two rows are equivalent iff they list the same
name set (modulo type arguments for generic effects like `Raise[E]`).

**Row variables.** Functions may include a row variable `| e` in
their effect row to express row polymorphism:

```sigil
fn catch[A, E](body: () -> A ![Raise[E] | e]) -> Result[A, E] ![| e]

fn with_io[A](body: () -> A ![IO | e]) -> A ![IO | e] {
  perform IO.println("start");
  let result: A = body();
  perform IO.println("end");
  result
}
```

The row variable `e` captures whatever additional effects are not
explicitly listed. Effects performed by the body that aren't in the
explicit list are absorbed by the row variable and resolved at call
sites via row unification. Row variables work both in fn-typed
parameter positions and in a function's own declared effect row.

#### §3.4 — Inference rules (overview)

Sigil uses Hindley–Milner with explicit annotations. Every `let`
binding requires an explicit type; the inference engine then unifies
the body's type against the annotation. Generic parameters (`[A]`
on functions or types) introduce universally-quantified type
variables that instantiate fresh at each call site.

The full inference algorithm follows the standard HM presentation
(Damas–Milner with effect rows). Specific diagnostics (E0044,
E0042, etc.) point at unification failures with their location and
suggested fix.

### §4 — Expressions

#### §4.1 — Expression forms

| Form | Example |
|------|---------|
| Integer literal | `42` |
| Float literal | `3.14`, `1e10` |
| String literal | `"hello"` |
| Char literal | `'A'` |
| Bool literal | `true`, `false` |
| Unit literal | `()` |
| Identifier | `x`, `length` |
| Function call | `f(x, y)` |
| Lambda | `fn (x: Int) -> Int ![] => x + 1` |
| Binary op | `a + b`, `a == b`, `a && b` |
| Unary op | `-n`, `!b` |
| If/else | `if cond { … } else { … }` |
| Match | `match scrut { p1 => e1, p2 => e2 }` |
| Block | `{ stmt1; stmt2; tail }` |
| Record literal | `Point { x: 1, y: 2 }` |
| Sum constructor | `Some(42)`, `Cons(1, Nil)` |
| Tuple literal | `(1, "hello")` |
| Perform | `perform Effect.op(args)` |
| Handle | `handle expr with { return(v) => …, Effect.op(args, k) => … }` |

#### §4.2 — Operators

| Category | Operators | Type |
|----------|-----------|------|
| Arithmetic | `+`, `-`, `*`, `/`, `%` | `(Int, Int) -> Int ![]` (or `![ArithError]` for `/` and `%`) |
| Comparison | `==`, `!=`, `<`, `<=`, `>`, `>=` | `(Int, Int) -> Bool ![]` |
| Logic | `&&`, `\|\|`, `!` | `(Bool, Bool) -> Bool ![]` |
| String compare | `string_compare(a, b)` | `(String, String) -> Int ![]` |

`/` and `%` perform `ArithError.div_by_zero` / `mod_by_zero` on
zero divisors; the top-level `main` shim installs default handlers
that print to stderr and exit 2.

#### §4.3 — Match patterns

```text
pattern := "_"                                          -- wildcard
         | identifier                                    -- binding (matches anything; binds name)
         | integer-literal                               -- exact match
         | bool-literal
         | char-literal
         | constructor-name "(" pattern ("," pattern)* ")"   -- positional constructor
         | constructor-name "{" field-pats "}"           -- record constructor
         | constructor-name                              -- nullary constructor
         | "(" pattern ("," pattern)* ")"                -- tuple destructure
```

Patterns are matched in source order. Bindings introduced by
patterns are scoped to the arm body. Exhaustiveness is checked at
compile time (E0066).

### §5 — Statements and blocks

A block is `{ stmt1; stmt2; …; tail }`. Statements end in `;`;
the final expression (no trailing `;`) is the block's value.

```sigil
{
  let x: Int = 1;
  let y: Int = 2;
  x + y                 // value of the block
}
```

Two statement forms exist in v1:
- `let name: Type = expr;` — binds `name` to the value of `expr`.
- `expr;` — evaluates `expr` for effect; the value is discarded.

There is no shadowing: `let x = 1; let x = 2;` is a compile error.
There is no `return` statement; the block's value flows out
naturally.

### §6 — Sum types, records, and tuples

```sigil
type Option[A] = | Some(A) | None
type Result[A, E] = | Ok(A) | Err(E)
type Tree[A] = | Leaf | Node(Tree[A], A, Tree[A])
type Point = { x: Int, y: Int }
type Person = { name: String, age: Int }
```

**Sum types.** Each `|` introduces a constructor. Constructors take
zero or more positional arguments; nullary constructors omit the
parens. Constructors of generic types receive type arguments at the
use site (inferred from constructor argument types).

**Records.** Field declarations are unordered; field access and
construction are nominal (the record's name in the `type`
declaration matters for equivalence).

**Tuples.** Tuple types are built-in — no `type` declaration needed.
`(Int, String)` is a binary tuple; `(Bool, Int, String)` is a ternary
tuple. Tuple values are constructed with `(e1, e2, ...)` and
destructured via `match`:

```sigil
let pair: (Int, String) = (42, "hello");
match pair { (n, s) => perform IO.println(s) };
```

Binary tuples have `fst[A, B]` and `snd[A, B]` accessors in
`std.pair`. Larger tuples use match destructuring.

### §7 — Pattern matching

See E3, E4 for examples. The `match` expression evaluates the
scrutinee once and dispatches to the first arm whose pattern
matches:

```sigil
match expr {
  pattern1 => arm_body1,
  pattern2 => arm_body2,
  _ => fallback,
}
```

Each arm body has the same type (unified by the checker). The
match's overall type is that unified arm-body type.

### §8 — Algebraic effects and handlers

#### §8.1 — Declaring effects

```sigil
effect Raise[E] {
  fail: (E) -> Int,
}

effect State[S] resumes: many {
  get: () -> S,
  set: (S) -> S,
}

effect Logger {
  log: (String) -> Unit,
}
```

Each effect declares zero or more **operations**; each op is a typed
function declaration without a body. Effects may be generic
(`Raise[E]`, `State[S]`); type parameters follow the effect name in
`[…]` brackets.

The optional `resumes: many` annotation marks a multi-shot effect
(the op's continuation `k` may be invoked more than once per arm
activation). Default is single-shot.

In v1 only the builtin `Mem` effect has zero ops (it's a marker).

#### §8.2 — Performing effects

```sigil
perform Effect.op(args)
```

The result is the value the active handler resumes with (for
single-shot ops) or short-circuits to (for discharging arms).

#### §8.3 — Handling effects

```sigil
handle body with {
  return(v) => …,                  -- optional return arm
  Effect.op(args, k) => …,          -- op arm; k is the continuation
}
```

The body runs to completion or until a matching `perform`. For each
op arm, `args` are the perform's arguments; `k` is a first-class
single-argument continuation that, when called, resumes the body
from the perform site.

In a single-shot handler, `k` is invoked at most once per arm
activation (typically exactly once for resumption, or zero times
for discard / short-circuit).

In a multi-shot handler (`effect E resumes: many`), `k` may be
invoked multiple times per arm — but in v1 the arm body must follow
a static N-let-chain shape:

```sigil
Effect.op(arg, k) => {
  let r1: T = k(arg1);
  let r2: T = k(arg2);
  …
  let rN: T = k(argN);
  combine(r1, r2, …, rN)            -- pure tail
}
```

N is fixed at compile time; runtime-N variations use first-class
continuations (see §8.5).

**Conditional k-call.** Handler arm bodies may use `if`/`else` and
`match` to conditionally invoke `k`:

```sigil
Effect.op(arg, k) => {
  if arg > 0 {
    k(arg)
  } else {
    0    -- discard k, short-circuit
  }
}
```

#### §8.4 — Effect row inference

When a function calls another function whose row contains effect
`E`, the caller's row must contain `E` (or discharge it via a
`handle`). Row inference is structural — the checker computes the
union of effects performed by the body and unifies it against the
declared row. Mismatches fire E0042.

#### §8.5 — First-class continuations

The continuation `k` in a handler arm can be bound to a variable
of type `Continuation[OpRet, Ret]` where `OpRet` is the operation's
return type and `Ret` is the handler's return type:

```sigil
effect Step resumes: many {
  step: (Int) -> Int,
}

handle body() with {
  Step.step(n, k) => {
    let f: Continuation[Int, Int] = k;
    f(n + 1)
  },
}
```

First-class continuations enable passing `k` to helper functions
(including recursive helpers for runtime-N enumeration, as used by
`all_choices` in `std.choose`). Dynamic-extent enforcement ensures
a continuation cannot be invoked after its handler frame has exited.

### §9 — `Mem` and mutation

`Mem` is the builtin marker effect that gates all in-place
mutation. Operations:

| Op | Type |
|----|------|
| `mut_array_*` | `MutArray[A]` constructors / accessors / setters. |
| `mut_byte_array_*` | `MutByteArray` constructors / accessors / setters. |
| `sb_new` / `sb_append` / `sb_finalize` | StringBuilder. |

A function declaring `![Mem]` may mutate. A pure function (`![]`)
cannot. There is no per-region or per-value isolation in v1; `Mem`
is a single global capability.

### §10 — Modules and imports

Sigil's stdlib lives in [`std/`](../std/). User code imports a
module by writing `import std.<name>` at the top of the file:

```sigil
import std.option
import std.list
```

Imports are flat — every public item from each imported module
becomes available in the importing file's scope. There is no
re-export, alias, or namespace qualification in v1.

The `std.io` / `std.array` / `std.mut_array` / `std.byte_array` /
`std.mut_byte_array` / `std.mem` / `std.string` / `std.int64` /
`std.string_builder` modules are **documentation-only**; their
types and operations are registered as compiler builtins. Importing
them is allowed (a no-op at the resolver) for documentary clarity.

### §11 — Diagnostics

Compiler errors are emitted as JSONL on stderr by default:

```json
{"level":"error","code":"E0044","file":"x.sigil","line":3,"column":12,
 "end_line":3,"end_column":18,"message":"…","hint":"…"}
```

`--human-errors` switches to human-readable text. Each error code
(`E0001`+) has a stable catalog entry accessible via:

```shell
sigil explain E0042
```

Common codes:

| Code | Meaning |
|------|---------|
| E0010 | parser syntax error |
| E0042 | effect not in row |
| E0044 | type mismatch |
| E0066 | non-exhaustive match |
| E0113 | duplicate type declaration |

Full catalog: see [`compiler/src/errors/catalog.rs`](../compiler/src/errors/catalog.rs).

### §12 — Runtime model

- **Memory:** Boehm conservative GC. Every heap object begins with
  an 8-byte header `(tag, count, bitmap, reserved)`.
- **Tagged values:** `Int` is 63-bit at FFI boundaries (one bit
  reserved for the heap-vs-immediate tag); `Int64` and `Float` are heap-boxed.
- **Effects:** dispatched through a CPS trampoline
  (`sigil_run_loop`); each `perform` returns a `NextStep` packet
  that the trampoline routes to the matching arm fn or the
  enclosing handler frame.
- **Multi-shot machinery:** thread-local stack handles re-entry from
  within multi-shot arm bodies; canonical pattern is the static-N
  let-chain (§8.3). Runtime-N enumeration uses first-class
  continuations (§8.5).

### §13 — Stdlib reference

Each module is documented in its own `std/<name>.sigil` source
file with `// @example` blocks demonstrating idiomatic use. The
files are the authoritative API reference.

| Module | Surface |
|--------|---------|
| `std.option` | `Option[A]`, `map`, `and_then`, `unwrap_or`. |
| `std.result` | `Result[A, E]`, `map`, `map_err`, `and_then`. |
| `std.list` | `List[A]`, `length`, `map`, `filter`, `fold`, `reverse`, `append`, `range`. |
| `std.array` | `Array[A]`, `array_alloc`, `array_get`, `array_set` (returns new), `array_length`. |
| `std.mut_array` | `MutArray[A]` (Mem-gated). |
| `std.byte_array` | `ByteArray`, conversion to/from `String`. |
| `std.mut_byte_array` | `MutByteArray` (Mem-gated). |
| `std.string` | Byte-indexed string ops: `string_concat`, `string_substring`, `string_byte_at`, `string_compare`, `string_starts_with`, `string_ends_with`, `string_contains`, `string_index_of`, `string_trim`, `string_to_int_validate`, `string_to_int_parse`, `string_length`. |
| `std.float` | Boxed `Float` (IEEE 754 f64): arithmetic (`float_add`/`sub`/`mul`/`div`/`neg`), comparison (`float_eq`/`lt`/`le`/`gt`/`ge`; NaN≠NaN), math (`float_abs`/`floor`/`ceil`/`sqrt`), conversion (`float_from_int`/`float_to_int`/`float_to_string`/`string_to_float_validate`/`string_to_float_parse`). `float_to_string` always includes `.0` for whole numbers; `inf`/`NaN` unchanged. |
| `std.int64` | Boxed `Int64` with arithmetic, comparison, conversion, stringify. |
| `std.string_builder` | `StringBuilder` rope (Mem-gated). |
| `std.pair` | `fst[A, B]`, `snd[A, B]` accessors for binary tuples `(A, B)`. |
| `std.io` | `IO` effect: `print`, `println`, `read_line`, `read_file`, `write_file`. |
| `std.mem` | `Mem` marker effect. |
| `std.random` | `Random` effect + `run_pseudo_random` (process-global xorshift64) + `run_seeded_random` (deterministic xorshift64 from an `Int64` seed). **Not cryptographically secure.** |
| `std.clock` | `Clock` effect + `run_os_clock` (wall-clock nanos) + `run_frozen_clock` (fixed `Int64` timestamp for test determinism). |
| `std.raise` | `Raise[E]` effect (generic over error type) + `raise[A, E](e: E) -> A ![Raise[E]]` + `catch[A, E](body) -> Result[A, E] ![| e]` (row-polymorphic residual). |
| `std.state` | `State[S]` effect (generic over state type) + `run_state[A, S](initial, body) -> A ![]`. |
| `std.choose` | `Choose resumes: many` effect + `all_choices[A](body) -> List[A]` (enumerate all branches) + `first_choice[A](body) -> Option[A]` (find first non-failing branch). Both use first-class continuations for runtime-N enumeration. |

#### §13.2 — Builtin primitives (not in stdlib modules)

These functions are available without any `import`:

| Function | Type | Description |
|----------|------|-------------|
| `int_to_string(n)` | `(Int) -> String ![]` | Decimal string from Int. |
| `int_xor(a, b)` | `(Int, Int) -> Int ![]` | Bitwise XOR. |
| `int_shl(a, b)` | `(Int, Int) -> Int ![]` | Left shift. `b` masked to 6 bits. |
| `int_shr(a, b)` | `(Int, Int) -> Int ![]` | Arithmetic right shift. `b` masked to 6 bits. Sign-extends. |
| `int_abs(n)` | `(Int) -> Int ![]` | Absolute value. `int_abs(i64::MIN)` wraps to `i64::MIN`. |
| `byte_truncate(n)` | `(Int) -> Byte ![]` | Truncate to low 8 bits. |
| `byte_in_range(n)` | `(Int) -> Bool ![]` | Range check: `0 <= n < 256`. |
| `byte_to_int(b)` | `(Byte) -> Int ![]` | Widen byte to integer. |
| `random_pseudo_int()` | `() -> Int ![]` | Process-global xorshift64. **Not cryptographic.** |

### §14 — v1 limits

The following limits are permanent v1 design choices:

- **`for` / `while`:** no looping syntax; recursion is the only
  iteration mechanism.
- **Multi-shot N at runtime without continuations:** the static
  N-let-chain (§8.3) requires N to be known at compile time. For
  runtime-N iteration, use first-class continuations (§8.5) as
  `all_choices` does.

### §15 — Build and run

```shell
# Linux
sudo apt-get install -y libgc-dev pkg-config

# macOS
brew install bdw-gc pkg-config

# Build
cargo build --release

# Compile
./target/release/sigil my_program.sigil -o my_program

# Run
./my_program
```

The compiler produces a self-contained native binary; no runtime
installation is needed beyond the Boehm GC system library.
