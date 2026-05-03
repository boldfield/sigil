# Sigil — Language Specification (v1)

Sigil is a compiled, statically-typed programming language designed to
be reliably authored by large language models. Programs are parsed by
a strict recursive-descent parser, type-checked by a Hindley–Milner
checker extended with effect rows, lowered to Cranelift IR, and
linked against a small Boehm-GC'd runtime.

This document is **examples-first**: twelve worked examples
(E1–E12) introduce the language by progressive elaboration. The
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

## Worked examples (E1–E12)

### E1 — Hello, world

```sigil
fn main() -> Int ![IO] {
  perform IO.println("hello, world");
  0
}
```

Every function declares an **effect row** in `![ … ]`. `IO` is a
builtin effect; `IO.println` is its only sub-operation in the v1
surface used here (full IO surface in §13). `perform` is the syntax
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
constructors, record patterns, identifier patterns (`name` binds),
and the wildcard `_`. Patterns are matched in source order; the
first matching arm wins.

### E4 — Sum types and pattern matching

```sigil
import std.option

fn safe_div(num: Int, den: Int) -> Option[Int] ![] {
  match den {
    0 => None,
    _ => Some(num / den),
  }
}

fn main() -> Int ![IO] {
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
  abs(a.x - b.x) + abs(a.y - b.y)
}

fn abs(n: Int) -> Int ![] {
  match n < 0 {
    true => 0 - n,
    false => n,
  }
}

fn main() -> Int ![IO] {
  let p: Point = { x: 1, y: 2 };
  let q: Point = { x: 4, y: 6 };
  perform IO.println(int_to_string(manhattan(p, q)));   // 7
  0
}
```

Record fields are declared `name: Type` in the type, accessed with
`.name`, and constructed with `{ name: value, … }`. Records are
nominal — two records with the same fields but different declared
names do not unify.

### E8 — Effects: `Raise` for exceptions

```sigil
import std.raise
import std.result

fn parse_pos(n: Int) -> Int ![Raise] {
  match n {
    0 => raise("expected positive"),
    _ => n,
  }
}

fn main() -> Int ![IO] {
  let result: Result[Int, String] = catch(fn () -> Int ![Raise] => parse_pos(0));
  match result {
    Ok(v) => perform IO.println(int_to_string(v)),
    Err(m) => perform IO.println(m),
  };
  0
}
```

`std.raise` ships an effect:

```sigil
effect Raise { fail: (String) -> Int }
```

Calling `raise(s)` performs `Raise.fail(s)`; under `catch`'s
discharging handler, the call short-circuits to an `Err` result.
The `Int` return is a v1 placeholder (no `Never` type yet); the
perform never actually returns under `catch`.

### E9 — Effects: `State` for threaded state

```sigil
import std.state

fn comp() -> Int ![State] {
  let _: Int = perform State.set(10);
  let v: Int = perform State.get();
  v + 1
}

fn main() -> Int ![IO] {
  let result: Int = run_state(5, comp);     // 11
  perform IO.println(int_to_string(result));
  0
}
```

`run_state(initial, body)` discharges the `State` effect by
threading `initial` through every `perform State.get/set` site in
`body`'s call tree. The discharger is a higher-order function
defined in pure Sigil (see [`std/state.sigil`](../std/state.sigil)).

In v1, **inline** `perform State.get/set` invocations work; wrapping
them in helper functions hits the documented wrapper-fn-frame
composition gap (`[DEVIATION Task 72]` in `PLAN_C_DEVIATIONS.md`).

### E10 — Multi-effect rows

```sigil
import std.raise

fn pipeline(s: String) -> Int ![IO, Raise] {
  perform IO.println(string_concat("processing: ", s));
  match string_compare(s, "") {
    0 => raise("empty input"),
    _ => string_length(s),
  }
}

fn main() -> Int ![IO] {
  let r: Result[Int, String] = catch(fn () -> Int ![IO, Raise] => pipeline("hello"));
  match r {
    Ok(n) => perform IO.println(int_to_string(n)),
    Err(m) => perform IO.println(string_concat("error: ", m)),
  };
  0
}
```

Effect rows are unordered sets of effect names. `![IO, Raise]` and
`![Raise, IO]` are the same row. A function with row `![Raise]` may
be called from any row that **contains** `Raise`; `![IO, Raise]`
calls `![Raise]` callees freely.

In v1, `catch[A](body: () -> A ![Raise]) -> Result[A, String] ![]`
is closed over the body's row — it accepts only `![Raise]` bodies.
The row-polymorphic `catch` (which would accept `![IO, Raise]`
bodies and pass `IO` through) lands in v2 (`[DEVIATION Task 71]`).

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
  Range: `[-2^62, 2^62)` (Plan A2's 63-bit tagged Int).
- String: `"..."` with escapes `\\`, `\"`, `\n`, `\t`, `\r`.
- Char: `'a'`, `'\n'`. Width: 1 byte (ASCII / latin-1 codepoint).
  v1 has no codepoint-aware string ops.
- Byte: constructed via `byte_truncate(n: Int) -> Byte ![]` or
  `byte_from_int(n: Int) -> Option[Byte] ![]` (the latter is the
  range-checking constructor; deferred to a follow-up per
  `[DEVIATION Task 66.5]` namespace work).
- Bool: `true` and `false` are the literal forms of the builtin
  `Bool` type. Pattern-match with `match b { true => ..., false => ... }`.

There is no `Float` literal in v1.

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
| `Unit` | The single-value type returned by mutation ops. No literal in v1. |
| `Array[A]` | Immutable indexed collection. |
| `MutArray[A]` | Mutable indexed collection (Mem-gated). |
| `ByteArray` | Immutable flat byte buffer. |
| `MutByteArray` | Mutable flat byte buffer (Mem-gated). |
| `Int64` | Boxed 64-bit signed integer (Task 69). |
| `StringBuilder` | Segmented-rope string accumulator (Mem-gated). |

User-declared sum types and records form the rest of the type
universe (§6).

#### §3.2 — Type expressions

```text
type-expr := identifier
           | identifier "[" type-expr ("," type-expr)* "]"   -- generic instantiation
           | "(" type-expr ("," type-expr)* ")" "->" type-expr "![" effects "]"
                                                              -- function type
```

Function types carry effect rows just like declarations:
`(Int) -> Int ![]` is the type of a pure unary integer function;
`(String) -> Int ![Raise]` is a fallible parser.

#### §3.3 — Effect rows

An effect row is a comma-separated set of effect names enclosed in
`![ … ]`:

```text
![]                 -- pure
![IO]               -- can do IO
![IO, Raise, Mem]   -- can do all three
```

Rows are unordered. Two rows are equivalent iff they list the same
name set. Row variables (for row-polymorphic functions) are not
yet supported in v1 (`[DEVIATION Task 71]`).

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
| String literal | `"hello"` |
| Char literal | `'A'` |
| Bool literal | `true`, `false` |
| Identifier | `x`, `length` |
| Function call | `f(x, y)` |
| Lambda | `fn (x: Int) -> Int ![] => x + 1` |
| Binary op | `a + b`, `a == b`, `a && b` |
| Unary op | `-n`, `!b` |
| If/else | `if cond { … } else { … }` |
| Match | `match scrut { p1 => e1, p2 => e2 }` |
| Block | `{ stmt1; stmt2; tail }` |
| Record literal | `{ x: 1, y: 2 }` |
| Field access | `point.x` |
| Sum constructor | `Some(42)`, `Cons(1, Nil)` |
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
that print to stderr and exit 2 (matching Plan A2 behavior).

#### §4.3 — Match patterns

```text
pattern := "_"                                          -- wildcard
         | identifier                                    -- binding (matches anything; binds name)
         | integer-literal                               -- exact match
         | bool-literal
         | constructor-name "(" pattern ("," pattern)* ")"   -- sum constructor
         | constructor-name                              -- nullary constructor
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

### §6 — Sum types and records

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
effect Raise {
  fail: (String) -> Int,
}

effect State resumes: many {
  get: () -> Int,
  set: (Int) -> Int,
}
```

Each effect declares zero or more **operations**; each op is a typed
function declaration without a body. The optional `resumes: many`
annotation marks a multi-shot effect (the op's continuation `k` may
be invoked more than once per arm activation). Default is single-
shot.

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

N is fixed at compile time; runtime-N variations need first-class
continuations (v2 work).

#### §8.4 — Effect row inference

When a function calls another function whose row contains effect
`E`, the caller's row must contain `E` (or discharge it via a
`handle`). Row inference is structural — the checker computes the
union of effects performed by the body and unifies it against the
declared row. Mismatches fire E0042.

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
re-export, alias, or namespace qualification in v1; collisions
between stdlib modules are deferred (`[DEVIATION Task 66.5]`).

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
  reserved for the heap-vs-immediate tag); `Int64` is heap-boxed.
- **Effects:** dispatched through a CPS trampoline
  (`sigil_run_loop`); each `perform` returns a `NextStep` packet
  that the trampoline routes to the matching arm fn or the
  enclosing handler frame.
- **Multi-shot machinery:** Plan B' Stage 6.7 outer-post-arm-k
  thread-local stack handles re-entry from within multi-shot arm
  bodies; canonical pattern is the static-N let-chain (§8.3).

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
| `std.string` | Byte-indexed string ops: `string_concat`, `_substring`, `_byte_at`, `_compare`, `_starts_with`, `_ends_with`, `_contains`, `_index_of`, `_trim`, `_to_int_validate`, `_to_int_parse`, `_length`. |
| `std.int64` | Boxed `Int64` with arithmetic, comparison, conversion, stringify. |
| `std.string_builder` | `StringBuilder` rope (Mem-gated). |
| `std.io` | `IO` effect: `print`, `println`, `read_line`, `read_file`, `write_file`. |
| `std.mem` | `Mem` marker effect. |
| `std.random` | `Random` effect + `run_pseudo_random` (xorshift64; **not** cryptographic). |
| `std.clock` | `Clock` effect + `run_os_clock` (wall-clock nanos). |
| `std.raise` | `Raise` effect + `raise(s)` + `catch[A]`. |
| `std.state` | `State` effect + `run_state(initial, body)`. |
| `std.choose` | `Choose resumes: many` effect declaration; dischargers (`all_choices`, `first_choice`) deferred to v2. |

### §14 — v1 limits (deferred to v2)

The Plan D architectural cluster (Tasks 111–118) closed every limit
in this section that originally pointed at a v2 lift in Plan C
deviations. Survivors below are limits with no Plan D / Plan C
closure path — they ship as permanent v1 design choices or queue
for a future plan-tier slice.

- **`Float` type:** no v1 floating-point. No closure path scheduled
  in any current plan.
- **`Unit` literal:** Unit values can only be obtained as the
  return of a `Mem` mutation op or `IO.println`. Permanent v1
  surface decision (no `()` literal).
- **`for` / `while`:** no looping syntax; recursion is the only
  iteration mechanism. Permanent v1 surface decision.
- **Captured-k inside lambdas across generic-fn boundaries
  (E0145):** `Plan D Task 117`'s `Ty::Continuation` escape barrier
  rejects captured-k inside any lambda when the program contains a
  generic fn (monomorphization walks every reachable fn once any
  generic exists, and continuation values cannot cross generic-
  instantiation boundaries). Mechanical fix path: move the handler
  into a non-generic wrapper around the generic body, or rewrite
  the arm body to call `k(arg)` directly without intermediate
  lambda capture. Plan-C-completion or future-PR territory.

The following limits ship as **closed in v1** by Plan D and remain
documented here as a closure log for the v1 → v2 transition:

- **First-class continuations:** Closed by Plan D Task 117 (PR #60
  substrate `4b3f0b4` + follow-up type-position `Continuation[op_-
  ret, ret]` surface). `Ty::Continuation` + escape barrier +
  ScopeId enforce dynamic extent; both single-shot (`let f = k;
  f(42)`) and multi-shot 2-let (`let f = k; let r1 = f(true); let
  r2 = f(false); r1 + r2`) work end-to-end.
- **Conditional / branched k-call:** Closed by Plan D Task 118
  (PR #81 squash `3904a12`). `lower_arm_body_to_next_step`
  recursively descends arm-body tail through `Expr::Block` /
  `Expr::If` / `Expr::Match`, emitting one `*NextStep` ptr per leaf.
- **Wrapper-fn-frame discharge composition:** Closed by Plan D
  Task 112 (a + b + c). Tail-perform Cps wrapper composition
  (PR #83), chained-let-yield Cps wrapper composition (PR #85),
  Case D wrapper-in-chain + Slice B outer arm (PR #86).
- **Type-parameterized effect rows:** Closed by Plan D Task 114
  (PR #54). `EffectRef`/`EffectInst` split with structural row
  unification + E0143 row-arg arity check.
- **Tuple types / `Pair[A, B]`:** Closed by Plan D Task 113
  (PR #53). `(T1, T2, ...)` types, `(e1, e2, ...)` values,
  `Pattern::Tuple` element-wise unification + destructure,
  `std/pair.sigil` with `fst[A,B]` / `snd[A,B]`.
- **Row-polymorphic fn-typed params:** Closed by Plan D Task 116
  (PR #56). Row vars in inner `FnTypeExpr`s resolve through the
  enclosing fn's `effect_row_var`; E0137 narrowed to fire only on
  unbound row vars.

Each limit's Plan-D-shipped closure links to its corresponding
`[DEVIATION Task NN]` entry in
[`PLAN_D_DEVIATIONS.md`](../PLAN_D_DEVIATIONS.md). The remaining
non-Plan-D survivors link to entries in
[`PLAN_C_DEVIATIONS.md`](../PLAN_C_DEVIATIONS.md) with their
respective closure scheduling.

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
