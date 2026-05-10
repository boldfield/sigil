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

## P21 — tuple construction and destructure

**Prompt:** Write a Sigil program that defines `fn min_max(a: Int, b: Int) -> (Int, Int) ![]`
returning `(a, b)` if `a <= b` and `(b, a)` otherwise. In `main`, call `min_max(7, 3)`,
destructure the result via `match` into `(lo, hi)`, print `lo` then `hi` on separate lines
via `perform IO.println(int_to_string(...))`, and return `0`.

**Oracle (stdout):**
```
3
7
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises tuple types in fn return position (Plan D Task 113), tuple
value construction `(a, b)`, and tuple destructuring via `match`.

## P22 — `std.pair` accessors

**Prompt:** Write a Sigil program that imports `std.pair`. Construct the binary tuple
`(42, "answer")`, print `int_to_string(fst(p))` then `snd(p)` via `perform IO.println` on
separate lines, return `0`.

**Oracle (stdout):**
```
42
answer
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises `import std.pair` and the generic `fst[A, B]` / `snd[A, B]`
accessors on binary tuples.

## P23 — type-parameterized effect row

**Prompt:** Write a Sigil program that declares `effect Raise[E] { fail: (E) -> Int }`
(generic over the error payload type). Define `fn check_positive(n: Int) -> Int
![Raise[String]]` whose body returns `n` when `n > 0` and `perform Raise.fail("not
positive")` otherwise. In `main`, wrap `check_positive(-5)` in a
`handle ... with { Raise.fail(msg, k) => { perform IO.println(msg); 0 } }` arm. Print
the handle's value via `int_to_string`, return `0`.

**Oracle (stdout):**
```
not positive
0
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises generic effect declarations (Plan D Task 114) — the `[E]`
parameter in `effect Raise[E] { ... }` and its instantiation as `Raise[String]` at the
row site.

## P24 — per-op generic params

**Prompt:** Write a Sigil program that declares `effect Raise[E] { fail[A]: (E) -> A }`
— note the per-op generic `[A]` on `fail`, distinct from the effect-decl generic `[E]`.
Define `fn check_pos(n: Int) -> Int ![Raise[String]]` returning `n` when `n > 0` and
`perform Raise.fail("not positive")` otherwise (the per-op `A` instantiates to `Int`
here). In `main`, wrap `check_pos(0)` in a
`handle ... with { Raise.fail(msg, k) => { perform IO.println(msg); -1 } }` arm. Print
the handle's value via `int_to_string`, return `0`.

**Oracle (stdout):**
```
not positive
-1
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises per-op generic params (Plan D Task 115). The per-op `A` in
`fail[A]: (E) -> A` is bound at the op's scheme and instantiated fresh at each perform
site.

## P25 — row-polymorphic discharger

**Prompt:** Write a Sigil program that imports `std.raise` and `std.result`, then uses
`catch` (the row-polymorphic discharger). Define `fn risky() -> Int ![Raise[String] | e]`
whose body unconditionally performs `raise("boom")`. In `main`, call
`let r: Result[Int, String] = catch(fn () -> Int ![Raise[String] | e] => risky())`,
then `match r` into `Ok(_) => perform IO.println("ok")` /
`Err(msg) => perform IO.println(msg)`, return `0`.

**Oracle (stdout):**
```
boom
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises row-polymorphic fn parameters (Plan D Task 116). The `e`
in `![Raise[String] | e]` is a row variable (bound implicitly by the `| e` tail) that
lets `catch` accept bodies with any residual effect row.

## P26 — conditional k-call

**Prompt:** Write a Sigil program that declares
`effect Try resumes: many { attempt: (Int) -> Int }`. Define
`fn body() -> Int ![Try, IO]` whose body is
`let n: Int = perform Try.attempt(7); perform IO.println(int_to_string(n)); n`. In
`main`, wrap `body()` in a
`handle ... with { Try.attempt(arg, k) => if arg > 0 { k(arg) } else { 0 } }` arm.
Print the handle's value via `int_to_string`, return `0`.

**Oracle (stdout):**
```
7
7
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises conditional k-call (Plan D Task 118). The handler's
`if arg > 0 { k(arg) } else { 0 }` is the shape where one branch resumes and another
short-circuits.

## P27 — `return(v) =>` arm

**Prompt:** Write a Sigil program that declares `type Option = | None | Some(Int)` and
`effect Maybe { miss: () -> Int }`. Define
`fn lookup(present: Bool) -> Int ![Maybe]` whose body is
`match present { true => 42, false => perform Maybe.miss() }`. In `main`, wrap
`lookup(true)` in a `handle` with both `return(v) => Some(v)` and
`Maybe.miss(k) => None`. Match the handled value:
`None => perform IO.println("none")` / `Some(n) => perform IO.println(int_to_string(n))`,
return `0`.

**Oracle (stdout):**
```
42
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises the `return(v) =>` discharge arm. The handle's normal-flow
value gets transformed (wrapped in `Some`) before the handle expression evaluates.

## P28 — multi-arm handler with `std.state`

**Prompt:** Write a Sigil program that imports `std.state` and declares
`effect Counter { incr: () -> Int, decr: () -> Int, read: () -> Int }`. Define
`fn body() -> Int ![Counter, State[Int]]` that performs `incr` × 2, `decr`, then
`read`. Define `fn run() -> Int ![State[Int]]` that handles `body()` with three arms:
each `Counter.*` arm reads the cell via `perform State.get()`, writes back via
`perform State.set(...)`, then resumes `k`. In `main`, call
`let final_pair: (Int, Int) = run_state(0, run)`, destructure into
`(returned, _state)`, print `returned` via `int_to_string`, return `0`.

**Oracle (stdout):**
```
1
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises multi-arm handlers (one handle, three arms on the same
effect) composed on top of `std.state.run_state`. Final state: incr × 2 + decr × 1 = 1.

## P29 — nested handlers on distinct effects

**Prompt:** Write a Sigil program that declares `effect Greet { say: (String) -> Int }`
and `effect Quiet { suppress: () -> Int }`. Define
`fn body() -> Int ![Greet, Quiet, IO]` that performs `Greet.say("hello")`,
`Quiet.suppress()`, then returns `0`. In `main`, nest handlers: outer
`handle (...) with { Quiet.suppress(k) => k(0) }` around inner
`handle body() with { Greet.say(msg, k) => { perform IO.println(msg); k(0) } }`. After
the outer handle returns, print `"done"`, return `0`.

**Oracle (stdout):**
```
hello
done
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises nested handlers on distinct effects. The inner handle
discharges `Greet`; the outer discharges `Quiet`.

## P30 — `MutArray` construction and indexed read/write

**Prompt:** Write a Sigil program that imports `std.mut_array`. Allocate a 3-element
`MutArray[Int]` initialized to `0` via `mut_array_new(3, 0)`. Write `10`, `20`, `30` at
indices `0`, `1`, `2` via `mut_array_set`. Read each index back via `mut_array_get` and
print on separate lines via `perform IO.println(int_to_string(...))`. Return `0`.
`main`'s row should be `![Mem, IO]`.

**Oracle (stdout):**
```
10
20
30
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises the `Mem` effect via `std.mut_array`. Verifies allocation,
indexed write, indexed read.

## P31 — `MutArray` in-place sum

**Prompt:** Write a Sigil program that imports `std.mut_array`. Allocate a 5-element
`MutArray[Int]` and `mut_array_set` indices 0..4 to values `1`, `2`, `3`, `4`, `5`.
Allocate a 1-element `MutArray[Int]` accumulator initialized to `0`. Write a recursive
helper `fn walk(arr: MutArray[Int], acc: MutArray[Int], i: Int, n: Int) -> Int ![Mem]`
that walks each index, reads the accumulator, adds `mut_array_get(arr, i)`, writes back
the new total, and recurses. After the walk, read the accumulator and print via
`int_to_string` + `perform IO.println`. Return `0`. `main`'s row is `![Mem, IO]`.

**Oracle (stdout):**
```
15
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises mutation under `![Mem]`, recursion-as-iteration over
indices, and a mutable accumulator. 1+2+3+4+5 = 15.

## P32 — `StringBuilder`

**Prompt:** Write a Sigil program that imports `std.string_builder`. Use `sb_new`,
`sb_append`, and `sb_finalize` to build `"hello world"` by appending `"hello"`, `" "`,
`"world"` sequentially. `perform IO.println` the final string, return `0`. `main`'s
row is `![Mem, IO]`.

**Oracle (stdout):**
```
hello world
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises `StringBuilder` rope under the `Mem` effect.

## P33 — `MutByteArray` with byte conversion

**Prompt:** Write a Sigil program that imports `std.mut_byte_array`. Allocate a 3-byte
`MutByteArray` initialized to `byte_truncate(0)` via `mut_byte_array_new`. Write the
bytes for ASCII `'A'` (65), `'B'` (66), `'C'` (67) at indices 0, 1, 2 via
`mut_byte_array_set` (use `byte_truncate(n)` to convert `Int → Byte`). Read each byte
back via `mut_byte_array_get`, widen via `byte_to_int`, and print via
`int_to_string` + `perform IO.println`. Return `0`. `main`'s row is `![Mem, IO]`.

**Oracle (stdout):**
```
65
66
67
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises `MutByteArray` + Byte/Int conversion builtins
(`byte_truncate`, `byte_to_int`).

## P34 — `ByteArray` checksum

**Prompt:** Write a Sigil program that imports `std.byte_array`. Construct a `ByteArray`
holding the bytes for `"ABC"` via `string_to_bytes("ABC")`. Walk each byte with a
recursive helper that calls `byte_array_get` + `byte_to_int` and accumulates the sum.
Print the total via `int_to_string` + `perform IO.println`, return `0`.

**Oracle (stdout):**
```
198
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises immutable `ByteArray` (no `Mem` needed) + byte iteration
+ `byte_to_int`. 65 + 66 + 67 = 198. (`byte_array_alloc` only takes a uniform fill;
`string_to_bytes` is the path to a `ByteArray` containing specific bytes.)

## P35 — `string_from_bytes` happy path

**Prompt:** Write a Sigil program that imports `std.byte_array`. Build a `ByteArray`
from `string_to_bytes("hi")`. Call `string_from_bytes_validate(ba)` (returns `Int`:
`-1` on success, otherwise the byte offset of the first invalid byte). On success,
print `string_from_bytes_alloc(ba)`; otherwise print `"invalid utf-8"`. Return `0`.

**Oracle (stdout):**
```
hi
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises ByteArray → String conversion via the validate + alloc
primitive pair (no `Result`-returning `string_from_bytes` wrapper exists in v1; user
code composes the validate + alloc primitives).

## P36 — `std.list.map` + `fold`

**Prompt:** Write a Sigil program that imports `std.list`. Build `[1, 2, 3, 4, 5]` via
`range(1, 6)`. Apply `map(xs, fn (n: Int) -> Int ![] => n * n)` to square each element,
then apply `fold(squared, 0, fn (acc: Int, n: Int) -> Int ![] => acc + n)` to sum.
Print the result via `int_to_string` + `perform IO.println`, return `0`.

**Oracle (stdout):**
```
55
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises `std.list` higher-order fns. 1+4+9+16+25=55.

## P37 — `std.list.filter` for evens

**Prompt:** Write a Sigil program that imports `std.list`. `filter`'s pred type is
`(A) -> Bool ![]` — pure. To filter for even integers (which uses `%`, a possibly-
ArithError-producing op), define a wrapper `fn is_even(n: Int) -> Bool ![]` that
discharges `ArithError` via `handle n % 2 == 0 with { ArithError.div_by_zero(k) =>
false, ArithError.mod_by_zero(k) => false }`. Build `range(1, 7)`, filter with
`is_even`, walk the result list and print each element on its own line via
`int_to_string` + `perform IO.println`. Return `0`.

**Oracle (stdout):**
```
2
4
6
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises `filter` plus the discharging-handler pattern for
isolating an effect-having computation behind a pure interface.

## P38 — `std.list.list_sort_int`

**Prompt:** Write a Sigil program that imports `std.list`. Build the list
`[3, 1, 4, 1, 5, 9, 2, 6]` (via nested `Cons` / `Nil` constructors). Sort with
`list_sort_int`. Walk the sorted result and print each element on its own line via
`int_to_string` + `perform IO.println`, return `0`.

**Oracle (stdout):**
```
1
1
2
3
4
5
6
9
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises the per-type sort wrapper `list_sort_int` (no comparator
passed by user — wrapper threads `int_compare`).

## P39 — `std.option.unwrap_or`

**Prompt:** Write a Sigil program that imports `std.option`. Define
`a: Option[Int] = Some(42)` and `b: Option[Int] = None`. Apply `unwrap_or(o, -1)` to
each; print both results on separate lines via `int_to_string` + `perform IO.println`.
Return `0`.

**Oracle (stdout):**
```
42
-1
```

**Oracle (exit):** `0`

## P40 — `std.result` match

**Prompt:** Write a Sigil program that imports `std.result` and `std.ordering`. Define
`fn parse(s: String) -> Result[Int, String] ![]` that returns `Ok(7)` when
`string_compare(s, "seven")` is `Equal` and `Err("unknown")` otherwise. In `main`, call
`parse("seven")` and `parse("foo")` separately, `match` each into either
`Ok(n) => perform IO.println(int_to_string(n))` or
`Err(msg) => perform IO.println(msg)`. Return `0`.

**Oracle (stdout):**
```
7
unknown
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises `Result[A, E]` from `std.result` + string equality via
`std.ordering.string_compare` returning `Ordering`.

## P41 — `std.string` ops

**Prompt:** Write a Sigil program that imports `std.string`. Define
`fn parse_or_invalid(s: String) -> String ![]` that calls `string_to_int_validate(s)`
(returns `Int` error code: `0` = success, `1` = empty, `2` = non-decimal,
`3` = overflow). On success (code `== 0`) returns `int_to_string(string_to_int_parse(s))`,
otherwise returns `"invalid"`. In `main`, print `parse_or_invalid(string_concat("4",
"2"))`, then print `"yes"` if `string_starts_with("hello, world", "hello")` and
`"no"` otherwise. Return `0`.

**Oracle (stdout):**
```
42
yes
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises `string_to_int_validate` (error-code Int, not Option),
`string_to_int_parse`, `string_concat`, `string_starts_with`.

## P42 — `std.char` ASCII classifier

**Prompt:** Write a Sigil program that imports `std.char`. Define
`fn classify(c: Char) -> String ![]` returning `"digit"` if `is_ascii_digit(c)` is
`true` and `"not digit"` otherwise. In `main`, print `classify('5')` then
`classify('a')` on separate lines. Return `0`.

**Oracle (stdout):**
```
digit
not digit
```

**Oracle (exit):** `0`

## P43 — `std.format.format_int`

**Prompt:** Write a Sigil program that imports `std.format`. Use `format_int` to render
the template `"answer: {}"` with the integer `42`. Print the result via
`perform IO.println`, return `0`.

**Oracle (stdout):**
```
answer: 42
```

**Oracle (exit):** `0`

## P44 — `std.raise.catch`

**Prompt:** Write a Sigil program that imports `std.raise` and `std.result`. Define
`fn body() -> Int ![Raise[String]]` that performs `raise("oops")`. In `main`, call
`let r: Result[Int, String] = catch(fn () -> Int ![Raise[String]] => body())`. Match
`r` into `Ok(_) => perform IO.println("ok")` or
`Err(msg) => perform IO.println(msg)`, return `0`.

**Oracle (stdout):**
```
oops
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises the canonical stdlib `catch` discharger.

## P45 — `std.state.run_state`

**Prompt:** Write a Sigil program that imports `std.state`. Define
`fn body() -> Int ![State[Int]]` that performs
`let n: Int = perform State.get(); perform State.set(n + 100); perform State.get()`. In
`main`, call `let final_pair: (Int, Int) = run_state(7, body)`, destructure via
`match final_pair { (returned, final_state) => ... }`, and print `returned` then
`final_state` on separate lines via `int_to_string`. Return `0`.

**Oracle (stdout):**
```
107
107
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises the canonical `run_state` discharger from `std.state`
(cell-backed encoding). Returned and final_state are both 107: the body returns the
result of the final `State.get()` (107), and the cell holds 107 after the
`State.set(7 + 100)`.

## P46 — `std.choose.all_choices`

**Prompt:** Write a Sigil program that imports `std.choose`. Define
`fn body() -> Int ![Choose]` that performs `let n: Int = perform Choose.choose(3);
n * 10`. In `main`, call `let results: List[Int] = all_choices(body)`. Walk the
result list with a recursive helper and print each element on its own line via
`int_to_string` + `perform IO.println`. Return `0`.

**Oracle (stdout):**
```
0
10
20
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises `all_choices` from `std.choose`. With `Choose.choose(3)`
enumerating `0`, `1`, `2`, the list contains `[0, 10, 20]`.

## P47 — `std.choose.first_choice`

**Prompt:** Write a Sigil program that imports `std.choose` and `std.option`. Define
`fn body() -> Int ![Choose]` that performs
`let n: Int = perform Choose.choose(5); if n == 3 { n } else { perform Choose.fail() }`.
In `main`, call `let r: Option[Int] = first_choice(body)`, `match r` into
`Some(n) => perform IO.println(int_to_string(n))` or
`None => perform IO.println("none")`, return `0`.

**Oracle (stdout):**
```
3
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises `first_choice` — short-circuits on the first non-failing
branch (here, `n == 3`).

## P48 — `std.array` immutable

**Prompt:** Write a Sigil program that imports `std.array`. Use `array_alloc(3, 0)` to
build an `Array[Int]` of length 3 filled with zeros. Call `array_set(arr, 1, 99)` to
produce a NEW array with index 1 set to `99` (immutable surface — `array_set` returns
a fresh array). Print `array_get(new_arr, 0)`, `array_get(new_arr, 1)`,
`array_get(new_arr, 2)` on separate lines via `int_to_string`. Return `0`.

**Oracle (stdout):**
```
0
99
0
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises immutable `Array[A]` — no `Mem` needed; `array_set` is
functional.

## P49 — `std.map` insert + lookup

**Prompt:** Write a Sigil program that imports `std.map` and `std.option`. Build an
empty `Map[String, Int]` via `map_string_keys()`. Insert `("alice", 1)` then
`("bob", 2)` via `map_insert` (returns a new map). Look up `"bob"` via `map_get`.
Match the result: `Some(n) => perform IO.println(int_to_string(n))` /
`None => perform IO.println("missing")`, return `0`.

**Oracle (stdout):**
```
2
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises `std.map` persistent ordered map.

## P50 — `std.env.env_var`

**Prompt:** Write a Sigil program that imports `std.env` and `std.option`. Look up the
environment variable `"SIGIL_TEST_VAR_THAT_SHOULD_NOT_BE_SET"` via `env_var(...)`
(returns `Option[String]`). Match into `Some(v) => perform IO.println(v)` /
`None => perform IO.println("unset")`, return `0`. `main`'s row is `![Env, IO]`.

**Oracle (stdout):**
```
unset
```

**Oracle (exit):** `0`

**Oracle (notes):** The validation harness should run this prompt with the named env
var unset; the `None` branch fires.

## P51 — `std.random.run_seeded_random` deterministic

**Prompt:** Write a Sigil program that imports `std.random` and `std.int64`. Construct
the seed `int64_from_int(42)`. Inside `run_seeded_random(seed, fn () -> Int ![Random]
=> random_int())` return the produced value as the process exit status. `main`'s row
is `![Mem]`.

**Oracle (stdout):** *(empty)*

**Oracle (exit):** `170`

**Oracle (notes):** Exit code is the seeded xorshift64 first draw mod 256. Verified
deterministic across runs.

## P52 — `std.clock.run_frozen_clock`

**Prompt:** Write a Sigil program that imports `std.clock` and `std.int64`. Construct
`ts: Int64 = int64_from_int(1234567890)`. Call
`let t: Int = run_frozen_clock(ts, fn () -> Int ![Clock] => perform Clock.now())`
(NOTE: `run_frozen_clock`'s body row is `![Clock]` only — no IO inside the lambda).
After the call returns, `perform IO.println(int_to_string(t))`, return `0`.

**Oracle (stdout):**
```
1234567890
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises deterministic clock for testability. `Clock.now()`
returns `Int` (nanos since epoch as i64); `run_frozen_clock` takes an `Int64`
timestamp.

## P53 — float arithmetic

**Prompt:** Write a Sigil program that imports `std.float`. Compute
`float_add(1.5, float_mul(2.0, 1.25))`. Print via `float_to_string` +
`perform IO.println`, return `0`.

**Oracle (stdout):**
```
4.0
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises Float literal + `std.float` ops. Operands chosen so the
IEEE 754 result is exact (`1.5 + 2.0 * 1.25 = 4.0`).

## P54 — `Int64` arithmetic near i64 max

**Prompt:** Write a Sigil program that imports `std.int64`. Construct
`a: Int64 = int64_from_int(9223372036854775000)` (near i64 max) and
`b: Int64 = int64_from_int(100)`. Compute `int64_add(a, b)`. Print via
`int64_to_string` + `perform IO.println`, return `0`.

**Oracle (stdout):**
```
9223372036854775100
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises Int64 arithmetic via `int64_from_int` + `int64_add` +
`int64_to_string`.

## P55 — Bool operators

**Prompt:** Write a Sigil program that defines `let a: Bool = true; let b: Bool =
false; let c: Bool = (a && !b) || b;` and prints `match c { true => "true", false =>
"false" }` via `perform IO.println`, returns `0`.

**Oracle (stdout):**
```
true
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises `&&`, `||`, `!` operators on `Bool`. `a && !b` =
`true && true` = `true`; `true || false` = `true`.

## P56 — `ArithError` discharge

**Prompt:** Write a Sigil program that wraps `10 / 0` in a
`handle ... with { ArithError.div_by_zero(k) => -1, ArithError.mod_by_zero(k) => -1 }`
arm (BOTH ops must be covered — handler arms must exhaust the effect's declared
operations). The handle's value should be `-1`; print via `int_to_string`, return `0`.

**Oracle (stdout):**
```
-1
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises `ArithError` (the builtin effect for `/` and `%` on zero
divisors). Verifies user-declared handler can discharge ArithError instead of letting
the default shim fire.

## P57 — wrap-on-overflow

**Prompt:** Write a Sigil program that defines `let big: Int = 9223372036854775807;`
(i64 max) and computes `let wrapped: Int = big + 1;`. Print `wrapped` via
`int_to_string` + `perform IO.println`. Return `0`.

**Oracle (stdout):**
```
-9223372036854775808
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises spec §1's wrap-on-overflow guarantee. i64 max + 1 wraps
to i64 min.

## P58 — 3-arity tuple destructure

**Prompt:** Write a Sigil program that defines a tuple
`let p: (Int, String, Bool) = (42, "hello", true);` and matches `p` into
`(n, s, b) => ...`. Print `int_to_string(n)`, `s`, then `match b { true => "yes",
false => "no" }` on three separate lines via `perform IO.println`. Return `0`.

**Oracle (stdout):**
```
42
hello
yes
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises 3-arity tuple destructure.

## P59 — nested constructor patterns

**Prompt:** Write a Sigil program that declares
`type Maybe = | Nothing | Just(Int)` and `type IList = | Nil | Cons(Maybe, IList)`.
Build `Cons(Just(10), Cons(Nothing, Cons(Just(30), Nil)))`. Walk the list with a
recursive `fn sum(xs: IList) -> Int ![]` whose body matches:

```
match xs {
  Nil => 0,
  Cons(Just(n), rest) => n + sum(rest),
  Cons(Nothing, rest) => sum(rest),
}
```

Print the sum (40), return `0`.

**Oracle (stdout):**
```
40
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises nested constructor patterns — `Cons(Just(n), rest)` is
two levels deep. 10 + 30 = 40.

## P60 — char literal patterns

**Prompt:** Write a Sigil program that defines `fn classify(c: Char) -> String ![]`
whose body is `match c { 'a' => "letter a", 'b' => "letter b", _ => "other" }`. In
`main`, call `classify('b')` and print the result via `perform IO.println`, return
`0`.

**Oracle (stdout):**
```
letter b
```

**Oracle (exit):** `0`

## P61 — `assert` builtin

**Prompt:** Write a Sigil program that calls `assert(2 + 2 == 4, "math broken")` (a
no-op when the predicate is true), then prints `"ok"` via `perform IO.println`,
returns `0`. The `assert` builtin is available without import.

**Oracle (stdout):**
```
ok
```

**Oracle (exit):** `0`

## P62 — multi-import composition

**Prompt:** Write a Sigil program with three imports on consecutive lines:
`import std.list`, `import std.option`, `import std.string`. Build a `List[Int]`
`[1, 2, 3]` (`Cons(1, Cons(2, Cons(3, Nil)))`), take its `length` (= 3). Then call
`string_to_int_validate("4")` (returns `Int` error code; `0` = success). On success,
parse with `string_to_int_parse("4")` (= 4). Print `int_to_string` of the sum
(3 + 4 = 7), return `0`.

**Oracle (stdout):**
```
7
```

**Oracle (exit):** `0`

**Oracle (notes):** Verifies that multiple `import std.X` lines compose correctly.
