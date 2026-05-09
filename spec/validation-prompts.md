# Sigil spec validation prompts

Each prompt is run against a fresh LLM session given only `spec/language.md`.
The session produces a program; `scripts/validate-spec.sh` compiles it, runs
it, and checks the output against the oracle.

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

**Oracle (notes):** Exercises recursion with `match` on `Int`
(literal pattern + wildcard), the `int_to_string` builtin, and
top-level recursive function calls.

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

**Oracle (notes):** Exercises the `%` operator, `==` comparison on
`Int`, a `let` binding of type `Bool`, and `if`/`else` as an
expression whose branches unify to `String`. The prompt hard-codes
`n = 14` so the oracle is deterministic.

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

**Oracle (notes):** Sigil has no `for`/`while` loops; the only way to
iterate is recursion with a match or if-guard on the loop counter.
The two-fn shape pins the iteration structure so the oracle is
deterministic. Exercises nested recursion and `int_to_string`.

## P07 — safe divide with explicit divisor check

**Prompt:** Write a Sigil program that assigns `n = 42` and `d = 0`,
then computes a value `q`: when `d == 0`, `q` should be `-1`;
otherwise `q` should be `n / d`. The program must not trigger the
runtime's division-by-zero trap. Return `q` as the process exit
status (no stdout output).

**Oracle (stdout):** *(empty)*

**Oracle (exit):** `255`

**Oracle (notes):** Unix exit codes are unsigned 8-bit, so `-1`
surfaces as `255` to a calling shell. Without the guard, the
runtime's ArithError handler fires and the process exits with
status `2`. A correct program uses `if`/`else` to select between
`-1` and `n / d`, avoiding the trap.

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

**Oracle (notes):** Exercises recursion-as-iteration (no loops in
v1), `match` on `Bool` for the loop guard, self-referential
recursion (`fib`) and cross-fn recursion (`print_range` → `fib`).

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

**Oracle (notes):** Exercises function types in return position,
lambda expressions that capture outer parameters, and closure
application at the call site.

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

**Oracle (notes):** Exercises function types in parameter positions,
two-level closure capture (the body-lambda captures both `f` and
`g`), and indirect calls through captured fn-typed values.

## P11 — length of a cons-list via recursive match

**Prompt:** Declare `type List = | Nil | Cons(Int, List)`. Declare
`fn length(xs: List) -> Int ![] { match xs { Nil => 0, Cons(_, rest)
=> 1 + length(rest), } }`. In `main`, build the list `Cons(10,
Cons(20, Cons(30, Nil)))`, call `length` on it, convert the result
via `int_to_string`, print via `perform IO.println`, and return `0`.

**Oracle (stdout):**
```
3
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises nominal sum-type declaration,
constructor application, pattern matching with a wildcard `_` and
a variable binding `rest`, and recursive function calls with
user-declared types.

## P12 — sum of a cons-list

**Prompt:** Reuse the `type List = | Nil | Cons(Int, List)` from P11
(or declare it again in-file). Declare `fn sum(xs: List) -> Int ![] {
match xs { Nil => 0, Cons(x, rest) => x + sum(rest), } }`. In
`main`, build `Cons(1, Cons(2, Cons(3, Cons(4, Cons(5, Nil)))))`,
call `sum` on it, convert via `int_to_string`, print via `perform
IO.println`, return `0`.

**Oracle (stdout):**
```
15
```

**Oracle (exit):** `0`

**Oracle (notes):** Same machinery as P11 but the `Cons` arm
destructures both the head and tail into variables. Exercises that
each constructor-pattern positional slot binds a distinct variable
scoped to the arm body.

## P13 — Option-returning safe lookup

**Prompt:** Declare `type List = | Nil | Cons(Int, List)` and `type
Option = | None | Some(Int)`. Declare `fn lookup(xs: List, i: Int) ->
Option ![] { match xs { Nil => None, Cons(x, rest) => if i == 0 {
Some(x) } else { lookup(rest, i - 1) }, } }`. Declare `fn
describe(o: Option) -> String ![] { match o { None => "not found",
Some(n) => int_to_string(n), } }`. In `main`, build `Cons(100,
Cons(200, Cons(300, Nil)))`, call `lookup(..., 1)` to get the
second element, call `describe` on the result, `perform IO.println`
the resulting string, return `0`.

**Oracle (stdout):**
```
200
```

**Oracle (exit):** `0`

**Oracle (notes):** Demonstrates two sum types in the same program
(each gets its own type tag) and two matches (one on `List`, one on
`Option`). Exhaustiveness must accept `None => ..., Some(n) => ...`
as complete without a wildcard. The nested `if/else` inside `Cons`'s
arm body is independent of the outer match.

## P14 — 2D-point record with match destructuring

**Prompt:** Declare `type Point = { x: Int, y: Int }` (single-
constructor record). Declare `fn sq(n: Int) -> Int ![] { n * n }` and
`fn dist_sq(p: Point, q: Point) -> Int ![]` that extracts fields via
match destructuring (e.g., `match p { Point { x: px, y: _ } => px }`)
and computes `sq(px - qx) + sq(py - qy)`. In `main`, build
`Point { x: 0, y: 0 }` and `Point { x: 3, y: 4 }`, call `dist_sq`
on them, convert via `int_to_string`, print, return `0`.

**Oracle (stdout):**
```
25
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises record type declaration, record literal
construction with `Name { field: value }` syntax, and record pattern
destructuring via `match`. Sigil v1 has no `.name` field-access
syntax; records are accessed exclusively through match destructuring.

## P15 — map a function over a cons-list

**Prompt:** Reuse `type List = | Nil | Cons(Int, List)` from P11.
Declare `fn map_inc(xs: List) -> List ![] { match xs { Nil => Nil,
Cons(x, rest) => Cons(x + 1, map_inc(rest)), } }`. Declare `fn
sum(xs: List) -> Int ![] { match xs { Nil => 0, Cons(x, rest) => x
+ sum(rest), } }`. In `main`, build `Cons(10, Cons(20, Cons(30,
Nil)))`, pass through `map_inc`, call `sum` on the result (which
should be `11 + 21 + 31 = 63`), `int_to_string`, print, return `0`.

**Oracle (stdout):**
```
63
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises that a recursive function can both
destructure a sum type and allocate a fresh constructor of the same
type at every recursive step. The incrementing function is hard-coded
because this prompt uses a monomorphic `List` — the generic
`map` in `std.list` uses `List[A]`.

## P16 — generic identity function applied at Int and String

**Prompt:** Declare `fn id[A](x: A) -> A ![] { x }`. In `main`, bind
`let n: Int = id(42);` and `let s: String = id("sigil");`, then
`perform IO.println(int_to_string(n))` and `perform IO.println(s)`.
Return `0`.

**Oracle (stdout):**
```
42
sigil
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises generics end-to-end: single-parameter
generic fn declaration (`fn id[A]`), HM let-polymorphism with two
distinct call-site instantiations (`id(42)` as `Int`, `id("sigil")`
as `String`), and monomorphization producing exactly two specialized
clones.

## P17 — compose two unary functions across types

**Prompt:** Declare `fn compose[A, B, C](f: (B) -> C ![], g: (A) ->
B ![]) -> (A) -> C ![] ![]` whose body is the lambda `fn (x: A) ->
C ![] => f(g(x))`. Declare a thin wrapper `fn its(n: Int) ->
String ![] { int_to_string(n) }` (builtins cannot be passed as
fn-values directly; wrap them in a user fn). In `main`, bind `let
inc_then_format: (Int) -> String ![] = compose(its, fn (n: Int) ->
Int ![] => n + 1);`, then `perform IO.println(inc_then_format(41))`.
Return `0`.

**Oracle (stdout):**
```
42
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises three-type-parameter generics (`A`,
`B`, `C`), inferred instantiation `(A=Int, B=Int, C=String)`, and
monomorphization to a single specialized clone. The `its` wrapper
is needed because builtins like `int_to_string` cannot be used as
fn-values directly — wrapping in a user fn makes it materializable
as a closure.

## P18 — Raise[String]-based safe parser for a small grammar

**Prompt:** Write a Sigil program that declares `effect Raise { fail:
(String) -> Int }`, defines `fn parse_token(token: Int) -> Int
![Raise, IO]` whose body uses `match token { 0 => perform Raise.fail(
"token zero is not allowed"), _ => token * 10 }` (treating `0` as the
"invalid grammar" case for the toy parser, and any other `Int` as a
successfully parsed token whose value is `token * 10`). In `main`,
wrap a call to `parse_token(0)` in a `handle ... with { Raise.fail(
msg, k) => { perform IO.println(msg); -1 } }` arm so that the
recovery path prints the failure message via `perform IO.println`
and the handle expression evaluates to `-1`. Then `perform IO.println(
int_to_string(handled_value))` to print the recovered sentinel, and
return `0`.

**Oracle (stdout):**
```
token zero is not allowed
-1
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises user-declared effects, a `String`-payload
operation, multi-statement arm bodies with discard-`k` semantics
(the arm value -1 flows to the handle expression rather than through
the continuation), and nested handler frames (the arm body's
`perform IO.println(msg)` reaches the top-level IO handler from
inside the user handler). Note: `std.raise` provides a generic
`Raise[E]` with `catch`; this prompt uses a hand-declared
non-generic `Raise` to exercise the raw `handle` surface.

## P19 — State[Int]-based counter threaded through a list walk

**Prompt:** Declare `effect State { get: () -> Int, set: (Int) ->
Int }` and a cons-list type `type IntList = | Nil | Cons(Int,
IntList)`. Define `fn count_elements(xs: IntList) -> Int ![State, IO]`
that walks the list and increments the State counter once per `Cons`
cell via `let cur: Int = perform State.get(); let _: Int = perform
State.set(cur + 1); count_elements(rest)`, returning the final
threaded count via `perform State.get()` at the `Nil` base case.
Define a higher-order helper `fn run_state(initial: Int, comp:
() -> Int ![State, IO]) -> Int ![IO]` that discharges State by
maintaining the threaded counter through handler arms returning
lambdas-of-state (the canonical Koka/Effekt shape:
`State.get(k) => fn(s) { k(s)(s) }`, `State.set(s', k) =>
fn(_) { k(())(s') }`, `return(v) => fn(_) { v }`, applied to
`initial`). In `main`, build a 5-element `IntList` (e.g., `Cons(10,
Cons(20, Cons(30, Cons(40, Cons(50, Nil)))))`), `let final_count:
Int = run_state(0, fn () -> Int ![State, IO] => count_elements(
list))`, then `perform IO.println(int_to_string(final_count))`,
return `0`.

**Oracle (stdout):**
```
5
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises the full higher-order handler pattern:
fn-typed parameters (`comp: () -> Int ![State, IO]`), fn-as-value
passing of a top-level user fn, arm-body lambdas that capture `k`,
recursive Call-of-Call dispatch (`k(s)(s)`), and the lambda-chain
state-threading discipline. Note: `std.state` provides a generic
`State[S]` with `run_state`; this prompt uses a hand-declared
non-generic `State` to exercise the raw handler machinery.

## P20 — multi-shot Choose finds all (a, b) pairs with a + b == 7

**Prompt:** Declare `effect Choose resumes: many { pick: (Int, Int)
-> Int }` whose `pick(low, high)` op nondeterministically yields an
`Int` in the inclusive range `[low, high]`. Define `fn pairs() ->
Int ![Choose, IO]` that picks `let a: Int = perform Choose.pick(1,
6); let b: Int = perform Choose.pick(1, 6);` and tests whether `a +
b == 7`, printing the matching pair via `perform IO.println(int_to_-
string(a * 10 + b))` (encoding `(a, b)` as a 2-digit decimal so the
six expected pairs print as exactly `16`, `25`, `34`, `43`, `52`,
`61`). Returns `0`. In `main`, wrap a call to `pairs()` in a
`handle ... with { Choose.pick(low, high, k) => ... }` arm whose
body iterates `k(v)` for `v ∈ [low, high]` (six resumes per pick
site) and combines the results so the multi-shot enumeration
exhausts all 36 `(a, b)` combinations, only six of which satisfy
the `a + b == 7` predicate. Return `0`.

**Oracle (stdout):**
```
16
25
34
43
52
61
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises multi-shot effects (`resumes: many`),
multi-perform bodies (chained let-binds with performs), and N-resume
arm bodies (the static let-chain pattern). The arm iterates
`k(low)`, `k(low+1)`, …, `k(high)` using a recursive helper with
the let-chain shape. Note: `std.choose` provides `Choose` with
`all_choices` / `first_choice` dischargers that use first-class
continuations for runtime-N enumeration; this prompt uses a
hand-rolled handler to exercise the raw multi-shot surface.
