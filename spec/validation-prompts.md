# Sigil spec validation prompts

Each prompt is run against a fresh LLM session given only `spec/language.md`.
The session produces a program; `scripts/validate-spec.sh` (added in Plan C)
compiles it, runs it, and checks the output against the oracle.

Plan A1 seeds three prompts whose feature surface is covered by the hello-world
vertical slice. Plans A2, A3, B, and C add the remaining 17 prompts as their
feature surface enables them; the full bank reaches 20 before Plan C can
declare the spec validated.

Each prompt carries:
- **Prompt:** exactly what the fresh LLM session receives (after `spec/language.md`).
- **Oracle (stdout):** required stdout bytes, whitespace-sensitive.
- **Oracle (exit):** required process exit code.

If a prompt gains additional grading criteria (e.g., stderr must be empty), add
them as `Oracle (stderr):` / `Oracle (notes):` lines in the same block.

## P01 — hello world

**Prompt:** Write a Sigil program that prints exactly the text `hello, world`
followed by a newline, then exits with status 0.

**Oracle (stdout):**
```
hello, world
```

**Oracle (exit):** `0`

## P02 — string concatenation through IO

**Prompt:** Write a Sigil program that prints the string `hello, world` on a
single line by constructing the string from two parts (`"hello, "` and
`"world"`) via the stdlib's `string_concat` primitive, then exits with
status 0.

**Oracle (stdout):**
```
hello, world
```

**Oracle (exit):** `0`

**Oracle (notes):** If the compiler does not yet expose `string_concat`
(Plan A1 may not — the primitive is required from Plan A2 onward), this
prompt is graded only against "program compiles"; the run portion is
deferred until the feature lands.

## P03 — multi-line output

**Prompt:** Write a Sigil program that calls `perform IO.println` twice, once
with `"first"` and once with `"second"`, then exits with status 0.

**Oracle (stdout):**
```
first
second
```

**Oracle (exit):** `0`

## P04 — sum-to-n via recursion

**Prompt:** Write a Sigil program that defines a recursive function
named `sum_to` taking a single `Int` parameter `n` and returning
`0 + 1 + 2 + ... + n`. Use pattern matching on `n` with base case
`0 => 0` and recursive case `_ => n + sum_to(n - 1)`. In `main`, call
`sum_to(10)`, convert the result to a `String` using the
`int_to_string` builtin, print it on a single line via `perform
IO.println`, then return `0` as the process exit status.

**Oracle (stdout):**
```
55
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises Stage-3 features — a recursive
user-defined fn with a single `Int` parameter, `match` on an `Int`
scrutinee with a literal pattern and a wildcard, the `int_to_string`
builtin (Plan A2 task 34), and the closure-calling-convention
direct-call path for a top-level recursive fn.

## P05 — parity check via mod and if/else

**Prompt:** Write a Sigil program that assigns the integer `14` to a
variable `n`, uses the `%` operator and `==` to check whether `n` is
even (i.e. `n % 2 == 0`), prints `even` on a single line if it is and
`odd` otherwise via `perform IO.println`, then returns `0` as the
process exit status.

**Oracle (stdout):**
```
even
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises Stage-2 features — the `%` operator, the
`==` comparison on `Int`, a `let` binding of type `Bool`, and
`if`/`else` as an expression whose branches unify to `String`. The
prompt deliberately hard-codes `n = 14` so the oracle is
deterministic; generalising to runtime-varied input arrives with Plan
B's effect handlers.

## P06 — multiplication table via nested recursion

**Prompt:** Write a Sigil program that prints a 3x3 multiplication
table. Define two recursive functions: `print_row(row: Int, col: Int)`
walks the inner axis by printing a single line with `perform
IO.println(int_to_string(row * col))` then recursing as `print_row(row,
col + 1)` until `col > 3`; `print_rows(row: Int)` walks the outer axis
by calling `print_row(row, 1)` then recursing as `print_rows(row + 1)`
until `row > 3`. In `main`, call `print_rows(1)`, then return `0`.

**Oracle (stdout):**
```
1
2
3
2
4
6
3
6
9
```

**Oracle (exit):** `0`

**Oracle (notes):** Plan A2 has no `for`/`while` loops; the only way
to iterate is recursion, and the only way to short-circuit is a match
or if-guard on the loop counter. The prompt's two-fn shape pins that
shape so the oracle stdout is deterministic. Exercises nested
recursion over the closure-calling-convention direct-call path,
plus the `int_to_string` builtin (Plan A2 task 34).

## P07 — safe divide with explicit divisor check

**Prompt:** Write a Sigil program that assigns `n = 42` and `d = 0`,
then computes a value `q`: when `d == 0`, `q` should be `-1`;
otherwise `q` should be `n / d`. The program must not trigger the
runtime's division-by-zero trap. Return `q` as the process exit
status (no stdout output).

**Oracle (stdout):** *(empty)*

**Oracle (exit):** `255`

**Oracle (notes):** Unix exit codes are unsigned 8-bit, so `-1`
surfaces as `255` to a calling shell. The prompt tests the Stage-2
"explicit guard around `/`" pattern: without the guard the runtime
trap (E0401) fires and the process exits with status `2`. A correct
program threads a `Bool` through `if`/`else` to select between `-1`
and `n / d`, dodging the trap entirely. Plan B replaces this pattern
with a `Raise[ArithError]` effect the caller can handle; until then,
`if` guards are the only tool.

## P08 — print fib(n) for n = 10..15

**Prompt:** Write a Sigil program that defines a recursive function
`fib(n: Int) -> Int ![]` with `match n { 0 => 0, 1 => 1, _ => fib(n -
1) + fib(n - 2) }`. Add a second recursive helper `print_range(n: Int,
end: Int) -> Int ![IO]` that, while `n <= end`, prints
`int_to_string(fib(n))` via `perform IO.println`, then tail-calls
itself with `n + 1`; when `n > end` it returns `0`. In `main`, call
`print_range(10, 15)` and return its value as the process exit status.

**Oracle (stdout):**
```
55
89
144
233
377
610
```

**Oracle (exit):** `0`

**Oracle (notes):** Plan A2 has no `for`/`while` loops; iteration must
go through a recursive helper. `match` on `Bool` (reached via `n >
end` desugared through elaborate's `if` → `match` rewrite) gates the
recursion's base case. Exercises the closure-calling-convention
direct-call path for both self-referential recursion (`fib`) and
cross-fn recursion (`print_range` → `fib`), plus the `int_to_string`
builtin (Plan A2 task 34).

## P09 — partial application via a returned lambda

**Prompt:** Write a Sigil program that defines a function
`make_adder(x: Int) -> (Int) -> Int ![]` whose body is the lambda
`fn (y: Int) -> Int ![] => x + y` (which captures `x` from
`make_adder`'s parameter list). In `main`, bind `let add3: (Int) ->
Int ![] = make_adder(3);` then apply it as `add3(4)`, convert the
result via `int_to_string`, print via `perform IO.println`, and
return `0` as the process exit status.

**Oracle (stdout):**
```
7
```

**Oracle (exit):** `0`

**Oracle (notes):** Requires `TypeExpr::Fn` surface syntax — the
ability to declare `(Int) -> Int ![]` as a user-fn return type and as
a `let`-binding's declared type — which Plan A2 defers to Plan A3.
Until Plan A3 lands, this prompt is graded only against "program
compiles"; the run portion of the oracle is deferred. The semantic
target is that closure conversion preserves the captured `x` through
a synthetic `$lambda_0` whose env is `[x: Int]`, and codegen's
`sigil_alloc` + `call_indirect` path (Plan A3 fills in the
`unreachable!` arm deferred from Task 32) wires the application site
to the heap-allocated closure record.

## P10 — compose two lambdas

**Prompt:** Write a Sigil program that defines `compose(f: (Int) ->
Int ![], g: (Int) -> Int ![]) -> (Int) -> Int ![]` whose body is the
lambda `fn (x: Int) -> Int ![] => f(g(x))`. In `main`, call
`compose(fn (x: Int) -> Int ![] => x + 1, fn (x: Int) -> Int ![] => x
* 2)` and apply the resulting closure to `5`, convert via
`int_to_string`, print via `perform IO.println`, return `0`.

**Oracle (stdout):**
```
11
```

**Oracle (exit):** `0`

**Oracle (notes):** Requires `TypeExpr::Fn` surface syntax on
parameter, return, and `let`-binding positions. Deferred to Plan A3
on the same terms as P09. The semantic target exercises two-level
closure capture (`compose`'s body-lambda captures both `f` and `g`)
and a call-of-a-call at the application site. A valid A2-only
approximation using nested IIFEs is not accepted because it doesn't
define `compose` as a first-class higher-order function.
