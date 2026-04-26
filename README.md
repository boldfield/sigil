# Sigil

A compiled, statically-typed programming language designed to be reliably
authored by large language models — not humans.

Sigil is under active construction. Plan A2 is complete: the compiler
handles arithmetic, conditionals, multi-argument functions, recursion,
closures, and higher-order programs. Plan A3 (sum types + pattern
matching), Plan B (polymorphism + algebraic effects), and Plan C (stdlib +
demos + spec + polish) follow.

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

Currently working (Plan A2 — arithmetic, closures, higher-order):

```
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

Lands in Plan B (effects + handlers):

```
effect Raise {
  fail: (String) -> Never,
}

fn parse_digit(c: Char) -> Int ![Raise] {
  if c >= '0' && c <= '9' {
    char_to_int(c) - char_to_int('0')
  } else {
    perform Raise.fail("not a digit")
  }
}

fn safe_parse(c: Char) -> Result[Int, String] ![] {
  handle parse_digit(c) with {
    return(n) => Ok(n),
    fail(msg, _k) => Err(msg),
  }
}
```

Four more things to notice: `parse_digit`'s signature declares
`![Raise]` — the type tells callers it can fail. `safe_parse` is `![]`
(pure) because the `handle` block discharges `Raise` entirely. Effect
operations use `perform E.op(...)`, syntactically distinct from
ordinary function calls. `handle … with { return(v) => …,
op(args, k) => … }` is a first-class expression; `k` is the
continuation, a first-class value.

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

## Verification limits (in-flight)

Sigil is **under active construction**. Some language features compile
and run, but with semantic gaps that have not yet been closed. Programs
that depend on the gapped behavior may produce results that diverge
from standard algebraic-effects semantics without any compile-time
signal. The list below documents every known gap as of the current
HEAD; each entry names the plan-tracker phase that closes it.

- **Discard-continuation handlers (Raise-style early-exit) do not yet
  propagate through function-call boundaries.** Behavior currently
  matches the Phase 4c synchronous shape, where the arm's return value
  flows to the perform site, not the handle expression. Programs
  depending on this — e.g., `handle helper() with { Raise.fail(k) =>
  default }` where `helper` performs `Raise.fail` and the surrounding
  code expects the handle to return `default` — work as expected
  **only when the perform is in tail position of the handle body** (no
  function-call boundary between the handle and the perform). When the
  perform reaches the arm via a function call, the discarded
  continuation does not abort the helper fn: the arm's value is
  returned to the helper's perform site as if `k` had been invoked
  with that value, the helper continues executing, and the handle's
  overall result reflects the helper's tail expression rather than
  the arm's value. The fix is the colorer's handler-discharge
  refinement, which reclassifies helper fns whose performs reach a
  discharging handler as CPS-color so their performs return
  `NextStep::Call` to the enclosing trampoline rather than
  synchronously blocking on `sigil_run_loop`. **Closure point: Plan B
  Task 55 Phase 4e** (tracked in `PLAN_B_PROGRESS.md`).
- **Non-tail continuation use is rejected at codegen time.** Arm bodies
  whose tail expression is not a single `k(arg)` call (e.g., `k(x) +
  1`, `let r = k(x); r * 2`) currently produce a clean codegen-time
  diagnostic pointing at Phase 4e. Programs that fit the
  tail-position pattern compile and run correctly; programs that need
  arm-body computation around a continuation invocation do not yet
  compile.
- **Multi-shot continuations are not yet implemented.** `effect E
  resumes: many { ... }` parses and typechecks but multi-shot arm
  bodies that invoke `k` more than once produce undefined behavior at
  runtime under the current Phase 4d shape. The runtime data
  structures (`HandlerFrame.arms[i].closure_ptr`, `(k_closure, k_fn)`
  pairs) are pointer-shaped to support multi-shot; codegen does not
  yet build the persistent re-invokable continuation closures.
  **Closure point: Plan B Task 55 Phase 4e**.
- **Multi-effect handlers are rejected at codegen time.** Handlers
  whose arms target more than one effect produce a clean codegen-time
  diagnostic. Single-effect handlers (one effect per `handle`) work.
  **Closure point: Plan B Task 55 Phase 4f**.
- **Return arms are rejected at codegen time.** `handle expr with {
  return(v) => ..., Op(...) => ... }` parses and typechecks but the
  `return` arm is rejected at codegen entry. Handlers without a
  return arm work. **Closure point: Plan B Task 55 Phase 4g**.
- **Stage 9 spec validation is gated on Phase 4e.** The fresh-Claude
  validation script (`scripts/validate-spec.sh`, planned for Stage 9
  in `boldfield/designs:docs/plans/2026-04-21-sigil-finish.md`)
  cannot run until the discard-`k` correctness gap closes. The 20-prompt
  validation bank includes patterns (`Raise[String]` safe parser,
  `State[Int]` counter, multi-shot `Choose`) that depend on standard
  algebraic semantics across function-call boundaries.

This list is a living section: each entry tracks an in-flight gap that
will close in a specific tracked phase, and is updated alongside the
implementing PR. The authoritative source for current Plan B phase
state is [`PLAN_B_PROGRESS.md`](PLAN_B_PROGRESS.md); architectural
decisions are in [`PLAN_B_DEVIATIONS.md`](PLAN_B_DEVIATIONS.md).

## Status

- **Plan A1** — Stage 0 scaffolding + Stage 1 hello-world vertical slice: **done**.
- **Plan A2** — Stages 2–3, arithmetic + conditionals + closures: **done**.
- **Plan A3** — Stage 4, sum types + pattern matching: pending.
- **Plan B** — Stages 5–6, HM parametric polymorphism + algebraic effects with multi-shot handlers: pending.
- **Plan C** — Stages 7–10, stdlib + three demo programs + language specification + polish: pending.
