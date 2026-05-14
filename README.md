# Sigil

A compiled, statically-typed programming language designed to be reliably
authored by large language models — not humans.

Sigil is in late v1. Plans A1, A2, A3, B, B', C, and D are complete; the
v1 surface includes mandatory effect rows, first-class continuations
(with dynamic-extent escape barrier), multi-shot handlers,
type-parameterized effects (`Raise[E]`, `State[S]`), per-op generic
params (`fail[A]: (E) -> A`), row-polymorphic dischargers, tuples,
mutable collections, conditional/branched k-call in arm bodies, and
wrapper-fn-frame composition. The stdlib covers effects (`Raise`,
`State`, `Choose`, `Mem`, `IO`, `Env`, `Random`, `Clock`, `Fs`,
`Process`), data (`Option`, `Result`, `List`, `Array`, `MutArray`,
`Map`, `Set`), and primitives (`Float`, `Int64`, `Char`, `ByteArray`,
`MutByteArray`, `String`, `StringBuilder`). Examples in
[`examples/`](examples) cover the interpreter, JSON pretty-printer, and
Sudoku demos. Remaining work is the v2 architectural cluster (precise
GC + Cranelift stackmaps, runtime profile-data emission, per-context
CPS color refinement) — none required for v1.

## Why sigil exists

Every general-purpose programming language was designed for human authors.
Python, Rust, TypeScript, Go — their syntax, error messages, and standard
libraries all carry decades of accommodations for human memory, typing
speed, tool preferences, and political history.

LLMs don't need those accommodations. They have different failure modes.
LLMs hallucinate toward training-data priors, producing code that *looks*
statistically plausible but is subtly wrong: a function whose signature
lies about what it does, a shadowed name that refers to the wrong value,
a missing branch in a match, an exception that isn't caught. Catching
those via a type checker or test suite catches them *after* they're
written. Sigil's bet: make the wrong patterns **fail to parse** in the
first place — push invariants down into the grammar, so the LLM's
next-token distribution is pruned to correct outputs by the language
itself, not by a downstream verifier.

## Design philosophy: fight the priors

Sigil deliberately chooses **redundancy and syntactic unfamiliarity**
over ergonomics.

- **Honest signatures.** Every function's type declares what it returns,
  what effects it can perform, and what it can fail at. Silence is
  impossible: a function that performs IO must write `![IO]` in its
  signature; a pure function must write `![]`. An LLM physically cannot
  generate a function whose signature lies about what it does — the
  type checker rejects it.
- **Effect rows, not function colors.** Effects are row types on the
  function arrow. `map` propagates whatever effects its argument
  performs. No `async`/`sync` split, no viral `await`, no "what color is
  your function" problem. One mechanism unifies exceptions, async, state,
  and nondeterminism. *(Plan B.)*
- **No shadowing, ever.** `let x = 1; let x = 2;` is a compile error,
  not a quiet rebinding. An LLM can't accidentally reuse a name.
- **Explicit types on every binding.** `let x: Int = …` is mandatory
  even though Hindley–Milner inference would work. The redundancy is
  deliberate — the annotation anchors the next-token distribution.
  Inference is for the type checker; the annotation is for the
  generator.
- **Exhaustive pattern matching.** `match` that doesn't cover every case
  is a compile error with a counterexample. The LLM can't forget the
  `None` branch.
- **One way to do each thing.** No exceptions vs. `Result` vs. `Option`
  vs. `null`. No for-loop vs. while-loop vs. `.map()` vs. comprehension.
  Fewer templates to confuse means more consistent output.

Full rationale: [`boldfield/designs:docs/plans/2026-04-21-sigil-design.md`](https://github.com/boldfield/designs/blob/main/docs/plans/2026-04-21-sigil-design.md).

## Testability is a consequence

Because effects are declared in types and dispatched through handlers,
**every effect is a testing seam**. Tests provide alternative handlers
to stub IO, fake databases, inject deterministic time — no
dependency-injection framework, no mocking library, no monkey-patching.
Multi-shot handlers additionally enable replay-style testing: run one
computation N times with N different input streams via a single handler
invocation.

This isn't a feature bolted onto the effect system; it's a direct
structural consequence of it, and one of the strongest arguments for
sigil's existence.

## What it looks like

Pure recursion (Plan A2):

```sigil
fn fib(n: Int) -> Int ![] {
  match n {
    0 => 0,
    1 => 1,
    _ => fib(n - 1) + fib(n - 2),
  }
}

fn main() -> Int ![] {
  fib(10)
}
```

Things to notice: every function signature carries an explicit effect
row (`![]` for pure). The `Int` type annotation on `n` is mandatory.
The `match` is exhaustive — adding a case without `_` would be a
compile error. No shadowing; no implicit conversions; no surprises.

Algebraic effects with handlers (Plan B + Plan C stdlib):

```sigil
import std.raise
import std.result
use std.raise.{Raise, catch, raise};

fn parse_pos(n: Int) -> Int ![Raise] {
  match n {
    0 => raise("expected positive"),
    _ => n,
  }
}

fn main() -> Int ![IO] {
  let r: Result[Int, String] = catch(fn () -> Int ![Raise] => parse_pos(3));
  match r {
    Ok(v) => perform IO.println(int_to_string(v)),
    Err(m) => perform IO.println(m),
  };
  0
}
```

Things to notice (Plan F1): the `import` line names the module
without binding any of its symbols; a separate `use mod.{name1,
name2};` line opts specific names into the bare namespace of this
file. Names not opt'd in can still be called qualified
(`std.raise.raise(...)`). There is no prelude — `Option`, `Result`,
`Some`, `None`, `Ok`, `Err` come from `std.option` / `std.result`
and require an explicit `import` + `use`. Primitive type names
(`Int`, `String`, `Bool`, `Char`, `Float`, `Unit`, etc.) are the
only globally-available names.

Things to notice: `parse_pos`'s signature declares `![Raise]` — the
type tells callers it can fail. `catch[A]` discharges `Raise` and
returns `Result[A, String] ![]`. Effect operations use
`perform E.op(...)`, syntactically distinct from ordinary calls.
`handle … with { return(v) => …, op(args, k) => … }` is a
first-class expression — `k` is the continuation, a first-class
value (single-shot in v1; multi-shot supported for the static-N
let-chain shape per [`PLAN_C_DEVIATIONS.md`](PLAN_C_DEVIATIONS.md)
Tasks 71–73).

Stateful computation with `State[S]` + multi-effect rows:

```sigil
import std.state
use std.state.{State, run_state};

fn counter() -> Int ![State[Int]] {
  let _: Int = perform State.set(10);
  let v: Int = perform State.get();
  v + 1
}

fn main() -> Int ![IO] {
  let pair: (Int, Int) = run_state(5, counter);
  match pair { (result, _final_state) =>
    perform IO.println(int_to_string(result));   // prints 11
  };
  0
}
```

`run_state[A, S](initial, body) -> (A, S)` is a higher-order
discharger that threads the state through the body's
`perform State.get/set` sites and returns the body's value paired
with the final state. Other effects in the row (here `IO`) are
unaffected. State composes with `Raise[E]` in either nesting order
(`catch(run_state(...))` or `run_state(catch(...))`); the cell-backed
encoding propagates foreign discharges cleanly through the existing
CPS infrastructure.

See [`examples/interpreter.sigil`](examples/interpreter.sigil) for a
tree-walking interpreter using `Raise` + `catch`, and
[`examples/json.sigil`](examples/json.sigil) for a JSON pretty-printer
+ recursive-descent parser using `State[Int]` cursor + `Raise[String]`
short-circuit, discharged via `run_state` + `catch`.

## Imports and `use`

> **Rule.** Names from imports need a path. To use a name bare,
> write `use module.path.{name}`.

`import std.list` makes the `std.list` module addressable but binds
no symbols. To call `map(xs, f)` bare, add `use std.list.{map};`.
Without the `use` line, the call site must qualify:
`std.list.map(xs, f)`. Module aliasing is supported on `import`:
`import std.option as O;` makes `O.map(opt, f)` a synonym for
`std.option.map(opt, f)`.

Two `use` lines in the same file that collide on a local name fire
E0147; the fix is to alias one (`use std.option.{map as option_map}`)
or remove one. There is no prelude — primitive type names (`Int`,
`String`, `Bool`, `Char`, `Float`, `Unit`, opaque `Array`, `MutArray`,
`ByteArray`, `MutByteArray`, `Int64`, `StringBuilder`) are the only
globally-available names. Every other symbol in a file must come
from an `import` + `use` line, or be qualified at the call site.
Wildcard `use mod.*;` is not supported (E0034): it would re-introduce
the cross-module bare-name ambiguity that this design is built to
close.

## What sigil deliberately is not

- **Not faster than C or Rust.** Cranelift is lighter than LLVM; Boehm
  GC is slower than a precise GC. Sigil prioritizes honest signatures
  over throughput.
- **Not smaller.** Mandatory type annotations and effect rows make sigil
  verbose. This is a feature.
- **Not more human-ergonomic than Python.** Deliberately. Every
  ergonomic shortcut is a place where an LLM's prior can substitute
  something subtly wrong.
- **Not novel in any single feature.** Algebraic effects, HM, row
  polymorphism, exhaustive matching — all prior art. The bet is on the
  combination, unburdened by human-ergonomic tradeoffs.

## Supported hosts

Both hosts must build and test from a clean checkout.

- `x86_64-unknown-linux-gnu` (headless build environment)
- `aarch64-apple-darwin` (development machine)

Native compilation only; the compiler emits for the host it runs on.

## Quickstart

```shell
# Linux
sudo apt-get update && sudo apt-get install -y libgc-dev pkg-config

# macOS
brew install bdw-gc pkg-config
export PKG_CONFIG_PATH="$(brew --prefix)/opt/bdw-gc/lib/pkgconfig:$PKG_CONFIG_PATH"

# Build + test
cargo test --workspace

# Compile and run hello-world
cargo run --bin sigil -- examples/hello.sigil -o /tmp/hello
/tmp/hello
```

## Diagnostics

Default compiler error output is JSON Lines on stderr, one event per line:

```json
{"level":"error","code":"E0010","file":"x.sigil","line":1,"column":1,"end_line":1,"end_column":2,"message":"...","hint":null}
```

`--human-errors` switches to human-readable text. `sigil explain <code>`
prints the long-form explanation and canonical fix for any diagnostic code.

## Local verification on memory-constrained hosts

Plan A1 established that `cargo test --workspace` and `cargo build --release`
OOM on memory-constrained hosts (the reference case is a headless Talos
Linux pod). On such hosts, do not run the workspace test suite locally —
use `scripts/pod-verify.sh` for local verification instead:

```shell
./scripts/pod-verify.sh
```

The script wraps the pod-safe subset: `cargo check --workspace`,
`cargo fmt --check`, per-crate `cargo clippy`, `cargo test -p sigil-runtime
--lib`, `scripts/check-no-interior-pointers.sh`, and the discipline greps.
It explicitly does *not* run `cargo test --workspace`,
`cargo build --release`, `scripts/reproducibility.sh`, or
`scripts/smoke.sh` — those are CI's responsibility.

**CI is authoritative** for the full test suite and for multi-host
verification. A task is not considered complete until CI is green on
both `x86_64-unknown-linux-gnu` and `aarch64-apple-darwin`; local
pod-verify green is a necessary but not sufficient signal.

On large-memory development machines (the reference case is an
aarch64-apple-darwin laptop with ≥16 GiB), `cargo test --workspace`
works fine — see the quickstart above.

**Measured peak RSS per workload:** see
[`docs/memory-profile.md`](docs/memory-profile.md). Short version:
compiling any single Plan-A2 sigil program peaks at ~63 MiB;
`cargo test --workspace` peaks at ~1.17 GiB; a parallel
`cargo build --release` peaks at ~1.56 GiB. The constraint lives in
the Rust toolchain, not sigil itself. Reproduce with
`scripts/peak-rss.sh`.

## Verification limits (current Plan C)

Sigil is **under active construction**. The Plan B effect-handler
correctness gates closed in PRs #26–#30; the remaining gaps are
expressivity-class limits captured in
[`PLAN_C_DEVIATIONS.md`](PLAN_C_DEVIATIONS.md) per stdlib task. The
load-bearing remaining gap is a v2-deferred architectural cluster
covering **first-class continuations** that would unlock the rest of
Task 73's `Choose` dischargers and the Sudoku demo:

| Gap | Behavior today | Closure point |
|-----|----------------|---------------|
| First-class continuations (`k` as a value, captured into a closure, passed to a helper) | Rejected with "first-class continuations are deferred to v2" diagnostic in `compiler/src/codegen.rs::arm_body_walk`. Single-shot and static-N let-chain multi-shot handler arms work; arbitrary-arity / fold-callback / nested-match k-call shapes do not. Blocks `std/choose.sigil`'s `all_choices` / `first_choice` dischargers and the Sudoku demo. | v2 future architectural slice |
| Multi-shot State (`run_state` with re-entered `k`) | The current cell-backed `run_state` is correct under single-shot resume only; a custom multi-shot State handler that re-enters `k` would see cell mutations from the first invocation aliased into the second. The `resumes: many` annotation on `effect State` permits user-defined multi-shot State handlers, but the canonical `run_state` never invokes `k` twice — the practical surface in v1 is single-shot. | v2 (snapshotting layer atop the cell encoding) |

Each row's "Closure point" links to the corresponding `[DEVIATION
Task NN]` entry in `PLAN_C_DEVIATIONS.md` for the technical
detail. The remaining v2 architectural lift is **first-class
continuations** — `k` as a value, captured into a closure, passed
to a helper. That's the path that unlocks `std.choose`'s
dischargers and the Sudoku demo (Task 81); scoping is a
future-work decision. Wrapper-fn-frame composition,
type-parameterized effect rows (`![Raise[E]]`, `![State[S]]`),
tuples (`(A, B)`), and the State+Raise composition gap have all
landed in Plan C / D / State-Cell follow-ups.

Authoritative sources:
- [`PLAN_C_PROGRESS.md`](PLAN_C_PROGRESS.md) — current task status.
- [`PLAN_C_DEVIATIONS.md`](PLAN_C_DEVIATIONS.md) — per-task deviation
  entries with v1 constraints and v2 closure paths.
- [`PLAN_B_PROGRESS.md`](PLAN_B_PROGRESS.md) +
  [`PLAN_B_DEVIATIONS.md`](PLAN_B_DEVIATIONS.md) — historical Plan B
  effect-handler correctness gates and the architectural decisions
  underlying them.

## Status

- **Plan A1** — Stage 0 scaffolding + Stage 1 hello-world: **done**.
- **Plan A2** — Stages 2–3, arithmetic + conditionals + closures: **done**.
- **Plan A3** — Stage 4, sum types + pattern matching: **done**.
- **Plan B** — Stages 5–6, HM parametric polymorphism + algebraic
  effects with multi-shot handlers (static-N): **done** (effect-handler
  correctness gates closed in PRs #26–#30; multi-shot composition fix
  shipped in Plan B' Stage 6.7+6.8).
- **Plan C** — Stages 7–10, stdlib + three demo programs + language
  specification + polish: **in progress** (~75%). Stdlib core
  shipped (Tasks 62–76 except 67/69 part 2 deferred and Task 73's
  `Choose` dischargers v2-deferred per
  [`PLAN_C_DEVIATIONS.md`](PLAN_C_DEVIATIONS.md)); interpreter +
  JSON pretty-printer demos shipped; Sudoku demo + spec
  validation gate pending.

## License

Apache 2.0 with the LLVM Runtime Library Exception. The compiler
(`sigil-compiler`) is `Apache-2.0`; the three crates whose compiled
artifacts ship inside every Sigil-compiled binary (`sigil-runtime`,
`sigil-abi`, `sigil-header-constants`) are
`Apache-2.0 WITH LLVM-exception`.

The exception says: code produced by the Sigil compiler is NOT
subject to the runtime's license terms. You can compile a Sigil
program and ship the resulting binary under any license you choose,
including closed-source commercial terms, with no obligation to
preserve the runtime's copyright notices or comply with its
attribution requirements.

See [`LICENSE`](LICENSE) for the full text.
