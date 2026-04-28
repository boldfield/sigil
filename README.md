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
signal. Each row in the table below names the plan-tracker phase that
closes the gap; the discard-continuation entry has additional prose
because the failure mode is silent (programs compile and run but
produce algebraic-incorrect results) and is the load-bearing one for
Stage 9 spec validation.

| Gap | Behavior today | Closure point |
|-----|----------------|---------------|
| Discard-continuation handlers across function-call boundaries | **Closed at PR #26** — the colorer-driven dispatch reclassifies helper fns whose performs reach a discharging handler as CPS-color; their performs return `NextStep::Call` to the enclosing trampoline rather than synchronously blocking on `sigil_run_loop`. | Phase 4e (PR #26) |
| Non-tail continuation use (`let r = k(x); pure_tail`) | **Closed at PR #27 Slice B** for the let-then-pure-tail shape; the arm body's post-`k` rest is lambda-lifted into a synthetic post-arm-k fn dispatched via the trailing-pair convention. Other non-tail `k` shapes (3+ invocations, Binary-of-`k`-calls, computed conditional `k`-use) require future captures-bearing extensions and remain rejected. | Phase 4e captures+ (PR #27) |
| Multi-shot continuations (`effect E resumes: many`) | **Closed at PR #27 Slice C** for the explicit two-let arm body shape `{ let r1 = k(arg1); let r2 = k(arg2); pure_tail }`. The k_closure (helper synth-cont's TAG_CLOSURE record) is heap-reified and the trampoline dispatches into it twice with different args. N-let chains (3+ invocations) and Binary-of-`k`-calls remain in future captures-bearing extension territory. | Phase 4e captures+ (PR #27) |
| Captures from a surrounding lambda's closure record | **Closed at PR #27 Slice D** — when an `Expr::Handle` lives inside a `Lambda`, closure_convert rewrites outer-scope references inside the arm body to `Expr::ClosureEnvLoad`; codegen sources the captured value via `lower_closure_env_load` against the lifted lambda's closure_ptr at handle codegen time. The post-arm-k synth fn body (Slice B/C path) does NOT yet support these captures — that surface is deferred to a future captures-bearing extension that mirrors Slice D's pattern at the post-arm-k closure-record allocation site. | Phase 4e captures+ Slice D (PR #27) for arm bodies; future extension for post-arm-k synth fns |
| Multi-effect handlers (arms targeting different effects) | **Closed at PR #28** — codegen groups arms by effect via a `BTreeMap<String, _>` (stable iteration order pinned to effect-id-lex-order) and emits one `HandlerFrame` per distinct effect. Frames are pushed at handle entry in BTreeMap order, popped in reverse at handle exit; a `cfg!(debug_assertions)`-gated discipline check (`TRAP_HANDLE_DISCIPLINE_VIOLATION = 0x42`) verifies the last pop returns the first-pushed frame snapshot. Per-frame `MAX_HANDLER_ARMS = 14` cap rejected at compile time via the codegen walker (clean diagnostic, not a runtime abort). See `[DEVIATION Task 55] Phase 4f` in `PLAN_B_DEVIATIONS.md` for the architectural rationale (Option A push-N-frames over Option B extend-HandlerFrame; reversibility-led; Phase 4f-2 escape valve). | Phase 4f (PR #28) |
| Return arms (`handle … with { return(v) => …, … }`) | Codegen-time rejection | Phase 4g |
| Stage 9 spec validation | **Phase 4e correctness gate closes at PR #27 squash-merge.** This PR closes the algebraic-effects codegen correctness gates that were the load-bearing prerequisite. **Full Stage 9 unblock additionally requires** Tasks 57–61 (Stage 6 closeout, including Task 61's P18–P20 prompt authoring) and Plan C Stage 6.5 scaffolding (`scripts/validate-spec.sh`). Today only P01–P17 exist in `spec/validation-prompts.md`; the prompt-bank's algebraic-effects entries (existing P06 `Raise`-based parser; future P19 `State` counter and P20 multi-shot `Choose`) become measurable when those prompts land in Task 61 and the validation script ships. | Phase 4e captures+ (PR #27) closes the codegen gate; Tasks 57–61 + Plan C Stage 6.5 close the rest |

**Phase 4e cadence pivot — closed.** The original Phase 4e
deviation entry committed to a single-PR comprehensive scope
covering all four remaining lifts (discard-`k` correctness, non-
tail `k`, multi-shot `k`, surrounding-lambda captures). PR #26
(`plan-b-task-55-phase-4e`) shipped the architectural foundation
plus the helper-side lambda-lifting first slices, closing hard
condition #2 (both discard-`k` test inversions). PR #27
(`plan-b-task-55-phase-4e-captures`) shipped the residual three
lifts as four slices (Slice A foundation refactor; Slice B
non-tail `k`; Slice C multi-shot `k`; Slice D surrounding-lambda
captures) plus a closeout commit. Both PRs preserved the
lambda-lifting + trailing-pair architecture from sections 5 and
6 of the comprehensive deviation entry; see the
[cadence-pivot addendum and the captures+ entry in `PLAN_B_DEVIATIONS.md`](PLAN_B_DEVIATIONS.md)
for the full record.

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
