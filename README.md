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

## Status

Plan A1 (current): Stage 0 scaffolding + Stage 1 hello-world vertical slice.
Plans A2, A3, B, C follow once each checkpoint is reviewed.
