# Sigil

A compiled, statically-typed programming language designed to be reliably
authored by large language models — not humans.

Sigil is under active construction. **Plans A1, A2, A3, and B are
complete**; **Plan C** (stdlib + demos + spec + polish) is currently in
progress, with the stdlib core (`Option`, `Result`, `List`, `Array`,
`MutArray`, `ByteArray`, `MutByteArray`, `String`, `Int64`,
`StringBuilder`, `IO`, `Mem`, `Random`, `Clock`, `Raise`, `State`,
`Choose`) shipped and the interpreter + JSON pretty-printer demos in
[`examples/`](examples). The remaining major work items are
specification authoring (Stage 9) and a v2 architectural cluster
covering first-class continuations + conditional-k arm bodies +
wrapper-fn-frame composition, which together would unlock
arbitrary-arity `Choose` dischargers and the Sudoku demo.

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

Things to notice: `parse_pos`'s signature declares `![Raise]` — the
type tells callers it can fail. `catch[A]` discharges `Raise` and
returns `Result[A, String] ![]`. Effect operations use
`perform E.op(...)`, syntactically distinct from ordinary calls.
`handle … with { return(v) => …, op(args, k) => … }` is a
first-class expression — `k` is the continuation, a first-class
value (single-shot in v1; multi-shot supported for the static-N
let-chain shape per [`PLAN_C_DEVIATIONS.md`](PLAN_C_DEVIATIONS.md)
Tasks 71–73).

Stateful computation with `State` + multi-effect rows:

```sigil
import std.state

fn counter() -> Int ![State] {
  let _: Int = perform State.set(10);
  let v: Int = perform State.get();
  v + 1
}

fn main() -> Int ![IO] {
  let result: Int = run_state(5, counter);
  perform IO.println(int_to_string(result));   // prints 11
  0
}
```

`run_state(initial, body)` is a higher-order discharger that threads
the state through the body's `perform State.get/set` sites. Other
effects in the row (here `IO`) are unaffected.

See [`examples/interpreter.sigil`](examples/interpreter.sigil) for a
tree-walking interpreter using `Raise` + `catch`, and
[`examples/json.sigil`](examples/json.sigil) for a JSON pretty-printer
using the `StringBuilder` rope under `Mem`.

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
| Wrapper-fn-frame composition for discharge-with-lambda | `examples/state.sigil`'s inline-perform shape works; wrapping `perform State.get/set` in a helper fn breaks the discharge-with-lambda continuation chain (the wrapper's frame doesn't re-thread state). Pinned by `#[ignore]`'d e2e test `std_state_run_state_via_wrappers_pending_v2_wrapper_fn_frame_fix`. | v2 (same architectural slice) |
| Type-parameterized effect rows (`![Raise[E]]`, `![State[S]]`) | Parser rejects type-parameterized effect references in rows; v1 ships concrete-typed effects (`Raise` over `String`, `State` over `Int`). | v2 (parser surface lift) |
| Tuple type / `Pair[A, B]` stdlib | No tuples; `run_state` returns just `A` (not `(A, S)`). User code threads final state via the body return value. | v2 stdlib expansion |

Each row's "Closure point" links to the corresponding `[DEVIATION
Task NN]` entry in `PLAN_C_DEVIATIONS.md` for the technical
detail. The cluster of v2 architectural lifts (first-class
continuations, wrapper-fn-frame composition for discharge-with-
lambda, conditional-k handler-arm tails) is the path that unlocks
`std.choose`'s dischargers and the Sudoku demo (Task 81); scoping
is a future-work decision.

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
