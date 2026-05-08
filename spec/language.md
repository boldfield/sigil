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

## Worked examples (E1–E17)

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
state. The discharger is defined in pure Sigil over a runtime cell
primitive (see [`std/state.sigil`](../std/state.sigil)). Both type
parameters are inferred from the call site (e.g. `run_state(5, comp)`
instantiates `A = Int`, `S = Int`).

`State[S]` composes with `Raise[E]` in either nesting order:
`catch(run_state(...))` and `run_state(catch(...))` both work. A
foreign `raise` inside a `run_state` body propagates through the
State handle as `Discharge(effect=Raise, value=Err)`, reaching the
enclosing `catch` cleanly — the State arm bodies resume `k(...)`
directly (rather than returning a state-fn closure), so the foreign
discharge passes through the existing CPS infrastructure without the
Sync-ABI gap that would otherwise mask the discharge tag.

### E10 — Multi-effect rows

```sigil
import std.raise
import std.ordering

fn pipeline(s: String) -> Int ![IO, Raise[String]] {
  perform IO.println(string_concat("processing: ", s));
  match string_compare(s, "") {
    Equal => raise("empty input"),
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

### E15 — CLI: argv + `Fs.read_dir` + `Fs.read_file`

```sigil
import std.env
import std.fs
import std.io
import std.list

fn dump_dir(path: String) -> Int ![IO, Fs] {
  match read_dir(path) {
    Ok(entries) => dump_each(entries),
    Err(NotFound) => fail_with("(directory missing)"),
    Err(_) => fail_with("(error reading directory)"),
  }
}

fn dump_each(xs: List[String]) -> Int ![IO] {
  match xs {
    Nil => 0,
    Cons(name, rest) => dump_each_step(name, rest),
  }
}

// Helper: print one entry then recurse on the rest. Sigil v1's
// match arm body must be a single expression, so the
// print-and-recurse sequence lives in a fn body (which IS a block).
fn dump_each_step(name: String, rest: List[String]) -> Int ![IO] {
  perform IO.println(name);
  dump_each(rest)
}

fn fail_with(msg: String) -> Int ![IO] {
  perform IO.println(msg);
  1
}

fn main() -> Int ![IO, Env, Fs] {
  // First arg after argv[0] is the directory to list; default to "."
  // if not provided.
  let argv: List[String] = env_args();
  let path: String = match argv {
    Nil => ".",
    Cons(_prog, Nil) => ".",
    Cons(_prog, Cons(p, _)) => p,
  };
  dump_dir(path)
}
```

This is the CLI-tool baseline: `env_args()` gives the argv list
(POSIX convention — `env_args()[0]` is the program name);
`read_dir(path)` returns `Result[List[String], FsError]` with entry
names (no path joining); pattern-match handles each FsError variant;
output prints via `IO.println`. Replace `read_dir` with
`read_file(p)` to read a file; replace with `run("cmd", argv)` to
spawn a subprocess.

### E16 — Word-frequency counter with `Map[Char, Int]`

```sigil
import std.io
import std.list
import std.map

fn count_chars(cs: List[Char], m: Map[Char, Int]) -> Map[Char, Int] ![] {
  match cs {
    Nil => m,
    Cons(c, rest) => {
      let next: Int = match map_get(m, c) {
        Some(n) => n + 1,
        None => 1,
      };
      count_chars(rest, map_insert(m, c, next))
    },
  }
}

fn print_pairs(xs: List[(Char, Int)]) -> Int ![IO] {
  match xs {
    Nil => 0,
    Cons(p, rest) => match p {
      (c, n) => {
        perform IO.println(string_concat(char_to_string(c),
          string_concat(": ", int_to_string(n))));
        print_pairs(rest)
      },
    },
  }
}

fn main() -> Int ![IO] {
  let cs: List[Char] = string_chars("banana");
  let counts: Map[Char, Int] = count_chars(cs, map_char_keys());
  print_pairs(map_to_list(counts))
}
```

`Map[Char, Int]` keys each unique codepoint to its running count.
`map_get` + `map_insert` is the canonical histogram-update pattern;
because the persistent map carries its comparator (`char_compare`,
threaded through `map_char_keys`) every lookup is O(log n) without
the caller threading an equality predicate. `map_to_list` returns
the entries sorted ascending by key, so the output is deterministic
across runs.

### E17 — Format-string log-line builder

```sigil
import std.io
import std.format

fn log_line(level: String, request_id: Int, message: String) -> String ![] {
  format3("[{}] req={} msg={}", AString(level), AInt(request_id), AString(message))
}

fn main() -> Int ![IO] {
  perform IO.println(log_line("INFO", 42, "ok"));
  perform IO.println(log_line("WARN", 43, "slow query"));
  0
}
```

`format(template, args)` walks `template` and substitutes each `{}`
placeholder with the next `FormatArg` from `args`. `{{` and `}}`
escape to literal braces; mismatched arity is forgiving (unfilled
`{}` emits the literal marker `{?}`, extra args drop). The arity
helpers `format1`...`format8` build the args list mechanically; the
per-type wrappers (`format_int`, `format_string`, ...) save the
constructor ceremony for the common single-arg case. See §13.

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
- Char: `'a'`, `'\n'`, `'\u{1F600}'`. Heap-boxed (`TAG_CHAR=0x0C`,
  16 bytes per allocation) Unicode scalar value in
  `0x000000..=0x10FFFF` excluding surrogates `0xD800..=0xDFFF`.
  Literal escapes: `\n`, `\t`, `\r`, `\\`, `\'`, `\"`, `\0`,
  and `\u{HEX}` accepting 1–6 hex digits. Bare-codepoint UTF-8
  literals (`'é'`, `'中'`, `'😀'`) are decoded from source. The
  lexer rejects multi-codepoint bodies, `\u{...}` values >
  `0x10FFFF`, and surrogate-range values at parse time.
  Operator overloading (`==`, `<`) is **not** provided — use the
  named functions `char_eq` / `char_lt` etc. (§3.1.1). Pattern-
  matching against literal Chars in `match c { 'a' => ... }`
  IS supported.
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

**`fn main` constraints.** `fn main` must take no parameters and
return `Int`. Its effect row may only contain effects discharged by
the top-level shim:

- `IO` — `print`, `println`, `read_line` arms (`std.io`)
- `ArithError` — div-by-zero / mod-by-zero default handlers
- `Mem` — marker effect (no shim handler; allowed for type-level
  gating)
- `Env` — `args`, `var`, `vars` (`std.env`)
- `Fs` — `read_file`, `write_file`, `read_dir`, `exists`, `is_file`,
  `is_dir`, `file_size`, `mkdir`, `remove_file`, `remove_dir`
  (`std.fs`)
- `Process` — `run` (`std.process`)

Other effects (`Random`, `Clock`, `Raise[E]`, `State[S]`, `Choose`,
user-defined) must be handled inside `main`'s body via `handle ...
with { ... }` or stdlib helpers like `run_pseudo_random` /
`run_state` / `catch`. A `main`-row entry referencing an effect
without a top-level handler frame is rejected at typecheck (E0041).

### §3 — Type system

#### §3.1 — Built-in types

| Type | Description |
|------|-------------|
| `Int` | 63-bit signed integer. |
| `Bool` | `true` / `false`. |
| `String` | Immutable UTF-8 byte sequence. |
| `Char` | Boxed Unicode codepoint (`TAG_CHAR=0x0C`, 21-bit codepoint payload). |
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

##### §3.1.1 — `Char` and codepoint string operations

`Char` is Sigil's first-class Unicode codepoint type — boxed,
single Unicode scalar value in `0x000000..=0x10FFFF` excluding
surrogates `0xD800..=0xDFFF`. Literal syntax (§1, expanded):

| Form | Example | Notes |
|------|---------|-------|
| Bare ASCII | `'a'`, `'5'`, `' '` | Single ASCII codepoint |
| Bare multi-byte UTF-8 | `'é'`, `'中'`, `'😀'` | Source decoded as UTF-8 |
| Escape | `'\n'`, `'\t'`, `'\r'`, `'\\'`, `'\''`, `'\"'`, `'\0'` | Standard C escapes |
| Unicode escape | `'\u{41}'`, `'\u{1F600}'` | 1–6 hex digits; out-of-range / surrogate rejected at parse |

The lexer rejects multi-codepoint bodies (`'ab'`, `'a\u{301}'`)
with a "Char literal must be a single codepoint" diagnostic. Char
is exactly one codepoint, never a grapheme cluster.

**Operations on `Char`** (all `![]`-pure, registered in
`std.char`):

- Equality / ordering: `char_eq` / `char_lt` / `char_le` /
  `char_gt` / `char_ge`. Compare codepoint-numerically. There is
  **no `==` / `<` operator overload** for `Char` — use the
  named functions.
- Conversion: `char_to_int` (always succeeds), `int_to_char`
  (returns `Option[Char]`; `None` for out-of-range or surrogate
  inputs), `char_to_string` (UTF-8 encode into a fresh String).
- ASCII classifiers (return `false` for any non-ASCII codepoint):
  `is_ascii`, `is_ascii_digit`, `is_ascii_alpha`,
  `is_ascii_alphanumeric`, `is_ascii_whitespace`.
- ASCII case folding (non-ASCII passes through unchanged):
  `to_lower_ascii`, `to_upper_ascii`. The `*_ascii` suffix is
  intentional — v2 may add `is_unicode_*` /
  `to_lower_unicode` additively without renaming.

**Codepoint-aware string operations** (in `std.string`,
documented in `std.char`):

- `string_chars : (String) -> List[Char] ![]` — eager UTF-8
  decode. Invalid byte sequences emit `U+FFFD` (replacement
  char) and resync to the next valid leading byte; lossy by
  design.
- `string_char_at : (String, Int) -> Option[Char] ![]` —
  **codepoint** index (not byte). `None` if out of bounds.
  O(n) decode walk.
- `string_from_chars : (List[Char]) -> String ![]` — UTF-8
  encode each codepoint, concatenate.

The byte-indexed (`string_byte_at`, `string_substring`,
`string_index_of`) and codepoint-indexed surfaces coexist —
choose based on whether the program reasons in terms of bytes
or codepoints.

##### Worked example — count digits

```sigil
import std.list
import std.char

fn count_digits(s: String) -> Int ![] {
  __count_digits(string_chars(s))
}

fn __count_digits(cs: List[Char]) -> Int ![] {
  match cs {
    Nil => 0,
    Cons(c, rest) => match is_ascii_digit(c) {
      true => 1 + __count_digits(rest),
      false => __count_digits(rest),
    },
  }
}
```

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
| String compare | `string_compare(a, b)` (from `std.ordering`) | `(String, String) -> Ordering ![]` |

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

##### Reserved effect names

The following effect names are reserved by the standard library;
declaring `effect <name> { … }` for any of them is a compile-time
error (E0136 — duplicate effect declaration):

`ArithError`, `IO`, `Mem`, `Env`, `Fs`, `Process`, `Random`,
`Clock`, `Raise[E]`, `State[S]`, `Choose`.

The first six (`ArithError`, `IO`, `Mem`, `Env`, `Fs`, `Process`)
are *builtin* effects — synthesized at typecheck pre-pass with
fixed effect IDs (`ArithError = 0`, `IO = 1`, `Mem = 2`, `Env = 3`,
`Fs = 4`, `Process = 5`). They appear in every program's effect-
id table whether the program uses them or not. The remaining names
are user-stdlib effects defined in `std/<name>.sigil`; redeclaring
them collides at typecheck unless the user code shadows the
import.

User effects with novel names (`Cfg`, `Network`, `Audit`, etc.)
remain free.

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

#### §12.1 — Tail-call optimization

Every direct user-fn call in **tail position** with a Cranelift
signature exactly matching the surrounding fn's signature is
lowered to Cranelift's `return_call` instruction — a native
tail-jump that deallocates the current stack frame before
transferring control to the callee. Programs may rely on this
for **unbounded recursion** in the shapes listed below; tail
calls whose signatures don't match (cross-arity, cross-return-
type) fall back to a non-tail call (one stack frame per call) and
are depth-bounded by the host thread's stack size.

A call is in tail position when it appears as:

- The last expression of a function body (the body's tail
  expression after any preceding statements).
- The tail expression of a `Block` whose surrounding context is
  itself in tail position.
- An arm body of a `match` whose surrounding context is in tail
  position. `if/else` desugars to `match`, so an `if`-arm in tail
  position is a tail-position match arm.
- The body of a `let x = …; tail` whose surrounding context is in
  tail position (i.e., the `tail` slot of a let-block).

A call is **not** in tail position when it appears as:

- An operand of `+`, `-`, `*`, etc. (the surrounding operator
  consumes the call's value).
- A non-tail statement of a block (`let _ = recurse(…); …`).
- The scrutinee of a `match` (the scrutinee feeds pattern tests,
  not the match's value).
- Inside a `handle … with { … }` body (the body executes under a
  synchronous trampoline driver; tail-jumping out would skip the
  handler machinery).
- Inside a `perform` argument (a perform's args are non-tail by
  construction).

Tail-call optimization covers:

- **Self-recursion** (`f` calling `f`).
- **Mutual recursion** (`f → g → f → …`) — provided the recursive
  fns share an exact signature (param types, return type, calling
  convention). Sigil's typechecker enforces matching return types
  for tail-position calls; cross-arity tail calls fall back to a
  non-tail call (one stack frame per call), since Cranelift's
  `return_call` rejects signature mismatches at verifier time.
- **Cps-colored fns whose body has the chained-let-yield + tail-
  match shape** (e.g., `let _ = perform Eff.op(); match { ...
  recurse }`). Such fns lower as `UserFnAbi::Cps`, but the
  chained-let-yield Final-step's tail expression goes through
  tail-position lowering. A recursive Cps→Cps call in a tail
  match-arm body emits `return_(NextStep::Call(callee, args))`
  directly. The OUTER trampoline iterates without nesting
  `sigil_run_loop` per call — stack-bounded to the same
  unbounded depth as pure-Sync recursion. The recursive call
  forwards the surrounding chained-let-yield's incoming
  `(post_arm_k_closure, post_arm_k_fn)` pair as the inner call's
  trailing pair, preserving continuation chains across nested
  handlers; non-identity outer continuations route through the
  captured chain rather than being silently dropped.
- **Indirect calls** (closure dispatch through `code_ptr`) when the
  callee is a fn-typed value (let-binding, fn parameter, or
  expression returning a closure) whose signature matches the
  surrounding fn's. These lower to Cranelift's
  `return_call_indirect`. Mutual indirect tail-recursion through
  fn-typed bindings (e.g., `a` and `b` each indirectly tail-call
  the other through a fn-typed local) is depth-unbounded.

Tail-call optimization does **not** apply to:

- Sync→Cps cross-ABI calls in tail position. The surrounding Sync
  fn returns the user's value type, not `*mut NextStep`; tail-
  jumping to a Cps callee would lose the trampoline drive that
  unwraps the NextStep into the user value. Such call sites use
  the synchronous `sigil_run_loop` wrapper (one nested run_loop
  per call), which is correct but stack-bounded.

Regression tests in `compiler/tests/e2e.rs` pin the guarantee at
depth 10,000,000 for all covered shapes (Sync self, Sync mutual,
let-block tail, if-arm tail, match-arm tail with literal-pattern
arms, `Mem`-effect-row body, Cps-colored chained-let-yield with
tail recursion, Cps→Cps under nested non-identity-k handler, and
indirect-call mutual tail-recursion through fn-typed bindings).
See `done/2026-05-07-01-sigil-tco-verify.md` for the
diagnostic-first plan and the `[DEVIATION Task TCO-4 ...]`
entries in `PLAN_C_DEVIATIONS.md` for the architectural walk
through all three TCO mechanisms (Sync `return_call`, Cps→Cps
`NextStep::Call` return, and indirect-call `return_call_indirect`).

### §13 — Stdlib reference

Each module is documented in its own `std/<name>.sigil` source
file with `// @example` blocks demonstrating idiomatic use. The
files are the authoritative API reference.

| Module | Surface |
|--------|---------|
| `std.option` | `Option[A]`, `map`, `and_then`, `unwrap_or`. |
| `std.result` | `Result[A, E]`, `map`, `map_err`, `and_then`. |
| `std.list` | `List[A]`, `length`, `map`, `filter`, `fold`, `reverse`, `append`, `range`, `list_sort` (stable functional merge sort, comparator-driven, `(T, T) -> Ordering`), per-type wrappers `list_sort_int`, `list_sort_string`, `list_sort_char`, `list_sort_float`. |
| `std.ordering` | `Ordering = \| Less \| Equal \| Greater` plus per-type comparators `int_compare`, `string_compare`, `char_compare`, `bool_compare`, `float_compare`, `int64_compare`. `string_compare` is the canonical string comparator (returns `Ordering`) — the legacy Int-returning builtin was retired in this addendum. `float_compare` uses total-order NaN semantics: `NaN == NaN`, `NaN < non-NaN`, `non-NaN > NaN`. |
| `std.map` | Persistent ordered `Map[K, V]` (AA tree, O(log n) lookup / insert / remove). `map_empty(cmp)`, `map_size`, `map_is_empty`, `map_get`, `map_contains`, `map_insert`, `map_remove`, `map_keys`, `map_values`, `map_to_list`, `map_from_list`, `map_fold`, `map_map`, `map_filter`. Convenience constructors `map_int_keys`, `map_string_keys`, `map_char_keys` thread the matching `std.ordering` comparator. Iteration order is sorted ascending by key. |
| `std.set` | Persistent ordered `Set[T]` layered over `Map[T, Unit]` (same AA-tree O(log n) lookup / insert / remove). `set_empty(cmp)`, `set_size`, `set_is_empty`, `set_contains`, `set_insert`, `set_remove`, `set_to_list`, `set_from_list`, `set_fold`, `set_filter`. Set-theoretic operations (`set_union`, `set_intersect`, `set_difference`, `set_subset`, `set_eq`) use the **left operand's comparator** — when `a` and `b` were built with semantically-different comparators, the result is well-defined (carries `a`'s ordering) but may surprise. Convenience constructors `set_int`, `set_string`, `set_char`. Iteration order is sorted ascending. Persistent semantics match `Map`: every op returns a fresh `Set[T]`; inputs are unchanged. |
| `std.array` | `Array[A]`, `array_alloc`, `array_get`, `array_set` (returns new), `array_length`. |
| `std.mut_array` | `MutArray[A]` (Mem-gated). |
| `std.byte_array` | `ByteArray`, conversion to/from `String`. |
| `std.mut_byte_array` | `MutByteArray` (Mem-gated). |
| `std.string` | Byte-indexed string ops: `string_concat`, `string_substring`, `string_byte_at`, `string_starts_with`, `string_ends_with`, `string_contains`, `string_index_of`, `string_trim`, `string_to_int_validate`, `string_to_int_parse`, `string_length`. Lexicographic comparison is `string_compare` from `std.ordering` (returns `Ordering`). Codepoint-indexed: `string_chars`, `string_char_at`, `string_from_chars`. |
| `std.char` | Boxed `Char` (`TAG_CHAR`): equality / ordering (`char_eq`/`lt`/`le`/`gt`/`ge`), conversion (`char_to_int`, `int_to_char` → `Option[Char]`, `char_to_string`), ASCII classifiers (`is_ascii`, `is_ascii_digit`, `is_ascii_alpha`, `is_ascii_alphanumeric`, `is_ascii_whitespace`), ASCII case (`to_lower_ascii`, `to_upper_ascii`). See §3.1.1. |
| `std.float` | Boxed `Float` (IEEE 754 f64): arithmetic (`float_add`/`sub`/`mul`/`div`/`neg`), comparison (`float_eq`/`lt`/`le`/`gt`/`ge`; NaN≠NaN), math (`float_abs`/`floor`/`ceil`/`sqrt`), conversion (`float_from_int`/`float_to_int`/`float_to_string`/`string_to_float_validate`/`string_to_float_parse`). `float_to_string` always includes `.0` for whole numbers; `inf`/`NaN` unchanged. |
| `std.int64` | Boxed `Int64` with arithmetic, comparison, conversion, stringify. |
| `std.string_builder` | `StringBuilder` rope (Mem-gated). |
| `std.format` | Format-string output. `FormatArg = \| AInt \| AInt64 \| AFloat \| AString \| ABool \| AChar`. General entry `format(template, args: List[FormatArg]) -> String ![]`; arity helpers `format1`..`format8` build the args list mechanically; per-type wrappers (`format_int`, `format_int64`, `format_string`, `format_float`, `format_bool`, `format_char`) save the `AInt(...)` ceremony for single-arg cases. Template syntax: `{}` is a positional placeholder, `{{` / `}}` escape literal braces. Mismatched arity is forgiving (unfilled `{}` emits the literal marker `{?}`, extra args drop). The walker is mutually tail-recursive (TCO'd per §12.1), so stack depth is O(1) regardless of template length. The accumulator is a concat chain (`format` is `![]`, so `StringBuilder` is unavailable — `sb_*` ops gate on `Mem`); each concat allocates a fresh String, so total work is O(L²) in output length L. Suitable for short-to-medium strings (log lines, error messages, ≤ a few KB); for large outputs (rendering a 100 KB document via repeated `format`) prefer `StringBuilder` directly under `![Mem]`. No format specifiers, named args, or positional indices in v1 — see §14.1. |
| `std.pair` | `fst[A, B]`, `snd[A, B]` accessors for binary tuples `(A, B)`. |
| `std.io` | `IO` effect: `print`, `println`, `read_line`. (File ops moved to `std.fs`.) |
| `std.env` | `Env` effect: `env_args() -> List[String]`, `env_var(name) -> Option[String]`, `env_vars() -> List[(String, String)]`. The effect-prefixed naming matches `random_int` / `clock_now` and avoids shadowing the very common parameter name `args`. |
| `std.fs` | `Fs` effect + `FsError` sum type. Predicates: `exists`, `is_file`, `is_dir` → `Bool`. Fallible ops: `read_file`, `write_file`, `read_dir`, `mkdir`, `remove_file`, `remove_dir`, `file_size` → `Result[T, FsError]`. `FsError = \| NotFound \| PermissionDenied \| AlreadyExists \| NotADirectory \| IsADirectory \| InvalidUtf8 \| Other(String)`. |
| `std.process` | `Process` effect + `ProcessError` sum type. `run(cmd, args: Array[String]) -> Result[(Int, String, String), ProcessError]` — direct exec (no shell), captures stdout / stderr after wait. `run_list(cmd, args: List[String])` — same surface with the more idiomatic `List[String]` argv shape; converts internally and forwards to `run`. `ProcessError = \| NotFound \| PermissionDenied \| Other(String)`. |
| `std.mem` | `Mem` marker effect. |
| `std.random` | `Random` effect + `run_pseudo_random` (process-global xorshift64) + `run_seeded_random` (deterministic xorshift64 from an `Int64` seed). **Not cryptographically secure.** |
| `std.clock` | `Clock` effect + `run_os_clock` (wall-clock nanos) + `run_frozen_clock` (fixed `Int64` timestamp for test determinism). |
| `std.raise` | `Raise[E]` effect (generic over error type) + `raise[A, E](e: E) -> A ![Raise[E]]` + `catch[A, E](body) -> Result[A, E] ![| e]` (row-polymorphic residual). |
| `std.state` | `State[S]` effect (generic over state type) + `run_state[A, S](initial, body) -> (A, S) ![]`. Backed by a runtime mutable cell (`Ref[S]`) — `run_state` allocates a cell on entry, threads State arms' `get` / `set` resumes through it, and reads the final state out at exit. The cell-backed encoding composes cleanly with `Raise[E]` in either nesting order; the prior Plotkin lambda-encoding had a Sync-ABI gap that surfaced as SIGSEGV on `catch(run_state(... raise ...))`. `Ref[T]` is internal scaffolding: the typechecker rejects calls to `sigil_ref_alloc` / `sigil_ref_deref` / `sigil_ref_set` from outside `std/state.sigil` (E0148). |
| `std.choose` | `Choose resumes: many` effect + `all_choices[A](body) -> List[A]` (enumerate all branches) + `first_choice[A](body) -> Option[A]` (find first non-failing branch). Both use first-class continuations for runtime-N enumeration. |
| `std.panic` | Doc-only header for the `panic` / `assert` builtins (see §13.2.1). Importing it is a no-op — both names are available without `import`. |

#### §13.1 — Comparator-mixing in `Set` operations

The binary set-theoretic operations on `std.set` — `set_union`,
`set_intersect`, `set_difference`, `set_subset`, `set_eq` — use
the **left operand's comparator** for the result, but the
**right operand's comparator** for membership tests inside the
predicate (via `set_contains(b, x)`). When `a` and `b` were
built with the same comparator, the asymmetry is invisible. When
the comparators differ on the same `T`, results are still
well-defined but depend on which side performs which role.

Concrete example: case-sensitive vs case-insensitive string sets.

```sigil
import std.set
import std.ordering
import std.string

// `string_compare_ci(x, y)` would be a case-insensitive variant
// (not shipped in v1; user-defined). For the purposes of the
// example, treat it as comparing "foo" and "Foo" as equal.

fn main() -> Int ![IO] {
  let cs: Set[String] = set_string();                 // case-sensitive
  let ci: Set[String] = set_empty(string_compare_ci); // case-insensitive
  let a: Set[String] = set_insert(set_insert(cs, "foo"), "Foo");
  let b: Set[String] = set_insert(ci, "foo");

  // set_intersect(a, b): keeps every a-element that b "contains".
  // Under b's case-insensitive comparator, b contains both "foo"
  // and "Foo". Result: {"foo", "Foo"} ordered by a's case-sensitive
  // comparator. Size 2.
  perform IO.println(int_to_string(set_size(set_intersect(a, b))));

  // set_subset(a, b): every a-element matches in b under b's
  // comparator. "foo" → match. "Foo" → match (case-insensitive).
  // Result: true, even though `a` has "more" elements under its
  // own comparator.
  perform IO.println(match set_subset(a, b) { true => "yes", false => "no" });
  0
}
```

The two-step semantics is consistent: the result's downstream
lookups use `a`'s comparator (the result's stored comparator),
but the construction-time membership decision used `b`'s. For
LLM-authored code, the safe rule is **always pass sets built
with the same comparator**; mix only when you have a specific
reason and have walked through the asymmetry above.

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

#### §13.2.1 — Diagnostics: `panic` and `assert`

These builtins close the recurring "bail out with a clear message"
gap without polluting effect rows with `Raise[String]`. Both are
available without any `import`.

| Function | Type | Description |
|----------|------|-------------|
| `panic[A](msg)` | `(String) -> A ![]` | Aborts the program. Writes `msg` to stderr followed by `\n`; exits with status 1. The per-call generic `A` instantiates fresh at each call site (same idiom as `Raise.fail[A]`), so `panic("oops")` typechecks anywhere any expression typechecks. |
| `assert(cond, msg)` | `(Bool, String) -> Unit ![]` | Sugar over `if cond { unit } else { panic(msg) }`. `assert(true, _)` is a no-op; `assert(false, msg)` calls `panic(msg)`. |

`panic` is a **hard abort**. It is not catchable: there is no
`catch_panic`, no `try`, no way to observe the abort from sigil
code. `Raise[E]` (see §13 / `std.raise`) is the catchable error
mechanism — use it when the caller may want to recover; use
`panic` when the program's invariants have been violated and
continuing would be unsafe or nonsensical.

`panic` carries effect row `![]` — aborting is not an effect
users handle.

`assert` is shipped as a top-level builtin because LLMs reach for
it specifically; the `if !cond { panic(msg) }` form one inversion
away has the same semantics, but `assert` is the prior every LLM
has.

### §14 — v1 limits

The following limits are permanent v1 design choices:

- **`for` / `while`:** no looping syntax; recursion is the only
  iteration mechanism.
- **Multi-shot N at runtime without continuations:** the static
  N-let-chain (§8.3) requires N to be known at compile time. For
  runtime-N iteration, use first-class continuations (§8.5) as
  `all_choices` does.

#### §14.1 — Deferred to follow-up plans

| Capability | Closure path |
|------------|--------------|
| Codepoint-aware `string_split` / `string_replace` | Future `string-codepoint-helpers` plan (depends on stdlib namespace qualification, not on `Char`). |
| Unicode-aware `is_unicode_*` / `to_lower_unicode` / `to_upper_unicode` / case folding / normalization | Future `std/unicode.sigil` plan (ships general-category + case-folding tables as embedded data + dispatchers). The v1 `*_ascii` suffix lets the Unicode set ship additively without renaming. |
| Strict-UTF-8 `string_chars_strict : (String) -> Result[List[Char], Utf8Error]` | v1 ships only the lossy `string_chars`; v2 may add the strict variant additively. |
| Network effects (`Net`: TCP, TLS, DNS, sockets) | Future plan once the LLM-first thesis closes and Sigil expands beyond demo programs. Not in v1 scope. |
| Timer effects (sleep, monotonic time, deadlines) | Future plan. `Clock.now` (wall-clock) ships in v1 via `std.clock`; deadline / sleep semantics defer. |
| Process stdin piping | Future v2 follow-up of the `Process` effect. v1's `Process.run` runs with stdin closed. |
| Process stdout / stderr streaming | Future v2 follow-up. v1 captures full stdout / stderr after the child exits via `Command::output()`. |
| Effect ops returning user-defined sum types directly (e.g., `Fs.read_file: (String) -> Result[String, FsError]` as a perform-direct surface) | Path 1 architecture from the CLI-effects plan; deferred. v1 ships path 4 (raw-shape effect ops + stdlib Sigil wrappers — `match read_file(p) { Ok(s) => ..., Err(NotFound) => ... }` as a stdlib fn call). Closure path = future `BuiltinEffectArmSynth` codegen-arm-fn architecture. See `[DEVIATION Task EE]` in `PLAN_C_DEVIATIONS.md`. |
| Filesystem path manipulation (`join`, `basename`, `dirname`, `normalize`) | Future `std/path.sigil` plan. The CLI-effects plan ships only the `Fs` effect's primitive ops. |
| Recursive `mkdir -p` and recursive `rm -rf` | Future stdlib helpers layered on top of `mkdir` / `remove_dir` / `read_dir`. v1 `Fs.mkdir` / `remove_dir` are single-level. |
| Symlink-aware ops (`is_symlink`, `read_link`, `create_symlink`) | Future v2 work. v1 follows symlinks transparently; no symlink-specific surface. |
| `MutMap`, range queries on `Map` (`map_range`, prefix scans), set operations (`map_union`, `map_intersect`, `map_difference`), `map_for_each`, `map_eq` | Future map-extensions plan. v1 ships only the persistent immutable `Map[K, V]` plus the closed-row pure-helper surface (`map_get` / `map_insert` / `map_remove` / `map_keys` / `map_to_list` / `map_fold` / `map_map` / `map_filter` etc.). |
| `MutSet`, range queries on `Set` (`set_range`, prefix scans), min / max queries (`set_min`, `set_max`), `set_for_each` | Future set-extensions plan. v1 ships only the persistent immutable `Set[T]` plus the closed-row pure-helper surface (`set_contains` / `set_insert` / `set_remove` / `set_to_list` / `set_fold` / `set_filter` / set-theoretic ops). |
| `mut_array_sort` (in-place sort over `MutArray[A]`) | Future plan. v1 ships only the pure functional `list_sort`; an in-place sort would force `![Mem]` onto every call site, which the LLM-default surface chose to avoid. |
| Format specifiers (`{:.2}`, `{:>10}`, `{:#x}`) — width, precision, alignment, fill, base prefix | Future format-specifiers plan. v1 ships only positional `{}` (each placeholder consumes the next `FormatArg`); width / precision / alignment / fill / base would extend the placeholder grammar and the per-`FormatArg`-variant render path. |
| Named args (`{name}`) and positional indices (`{0}`, `{1}`) | Future format-specifiers plan. v1's `{}` is strictly positional — each placeholder consumes the next `FormatArg`. |
| Compiler-level f-string syntax (`f"x = {x}"`) | Future plan. v1 ships only the runtime `format` family in `std.format`; a compile-time f-string surface would lower to `format` calls but requires lexer + parser changes. |
| Stack traces on `panic` | Future plan. v1's `panic` prints only the user-supplied `msg` and exits — caller-context information has to be encoded into `msg` itself (or built via `format(...)` + `panic(...)`). Precise stack traces require stackmap v1 content (currently `STACKMAP_VERSION_PLACEHOLDER` per §12); deferred to v2 alongside the precise-GC stackmap rework. |

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
