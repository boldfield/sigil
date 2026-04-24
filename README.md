# Sigil

A compiled, statically-typed programming language optimized for reliable authorship by large language models.

Sigil is under active construction; Plan A1 (this repo's current state) delivers the scaffolding and a hello-world vertical slice of the compiler.

## Design philosophy

Sigil targets only LLM authors. It deliberately chooses redundancy and syntactic
unfamiliarity over human ergonomics, on the hypothesis that LLMs hallucinate
toward training-data priors and the best defense is to make wrong-looking code
syntactically wrong. The full rationale lives in `docs/design.md` (populated in
later plans) and the authoritative design lives at
`boldfield/designs:docs/plans/2026-04-21-sigil-design.md`.

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
Linux pod, ~8–12 GiB). On such hosts, do not run the workspace test
suite locally — use `scripts/pod-verify.sh` for local verification
instead:

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

## Status

Plan A1: Stage 0 scaffolding + Stage 1 hello-world vertical slice — done.
Plan A2 (current): arithmetic, conditionals, closures (Stages 2–3).
Plans A3, B, C follow once each checkpoint is reviewed.
