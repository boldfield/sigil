---
layout: default
title: Sigil
---

# Sigil

**A statically-typed compiled language designed around LLM authorship.**

Sigil is an academic exercise in compiler design with a load-bearing
hypothesis: explicit types, mandatory effect rows, exhaustive matching,
and no operator overloading aren't just good engineering — they make a
language that LLMs can author correctly. The compile-time surface
catches the wrong patterns before they reach runtime.

## The empirical case

Across **1,626 fresh-session LLM-authoring trials** with
`claude-opus-4-7`, `claude-sonnet-4-6`, and `claude-haiku-4-5`:

- **~85% first-shot correctness** (compile + run + match oracle on the
  first sampled program)
- **~99% after one edit-loop iteration** (the model sees its own
  compile error and tries again)
- **>99% of failures caught at compile time** — not in production

This is the load-bearing claim. Python is friendlier to write but every
bug ships to runtime. Go catches more at type-check but its type system
is intentionally thin. Sigil's typecheck + effect row + exhaustivity
together catch the LLM's mistakes where they belong: before the program
runs.

See the [LLM authorship case]({{ '/for-llms/' | relative_url }}) for
the per-corpus numbers and the [capabilities document]({{ '/capabilities/' | relative_url }})
for the cut-and-dry data.

## What's in v1

- **Cranelift codegen + Boehm GC.** Native binaries on
  `x86_64-unknown-linux-gnu` and `aarch64-apple-darwin`.
- **Algebraic effects** with one-shot AND multi-shot handlers
  (Plotkin-style). Generator / non-determinism / Sudoku-style
  backtracking all work end-to-end.
- **Stdlib** covering List, Option, Result, String, Array, MutArray,
  ByteArray, IO, Fs, Process, Env, Mem, ArithError, Ordering.
- **Diagnostics** for every common LLM failure mode, with E-code-tagged
  hints (`E0042 unknown effect`, `E0046 unknown stdlib fn with import
  suggestion`, `E0112 unknown type`, `E0117 tuple arity`, `E0151 no
  field access`, …).

## A tiny example

```sigil
import std.list
import std.io

effect Gen[A] {
  yield: (A) -> Int,
}

fn iterate(xs: List[Int]) -> Int ![Gen[Int]] {
  match xs {
    Nil => 0,
    Cons(x, rest) => {
      let _: Int = perform Gen.yield(x);
      iterate(rest)
    },
  }
}

fn main() -> Int ![IO] {
  let xs: List[Int] = Cons(1, Cons(2, Cons(3, Nil)));
  let result: List[Int] = handle iterate(xs) with {
    Gen.yield(x, k) => {
      let rest: List[Int] = k(0);
      Cons(x, rest)
    },
    return(_v) => Nil,
  };
  // Print each element so the multi-shot composition is observable.
  match result {
    Nil => 0,
    Cons(h, _t) => {
      perform IO.println(int_to_string(h));
      // ... recurse, omitted for brevity.
      0
    },
  }
}
```

The `Gen[A]` effect captures the continuation in the handler arm.
`k(0)` resumes the body with `0` as the perform's return value; the
arm wraps each yielded value into `Cons(x, rest)`. Multi-shot
composition produces the full list, in order, deterministically.

## Why "no shadowing" matters

```sigil
let x: Int = 1;
let x: Int = 2;  // E0020: redefinition of `x` — Sigil forbids shadowing
```

LLMs hallucinate toward training-data priors. Shadowing is a top
prior. Pre-Sigil, this code compiles in every major language; the
second `x` overwrites the first and any reader has to track which
binding is in scope at each line. Sigil rejects it at parse-resolve.
The LLM that wrote two `let x`s gets a one-line compile error that
points at the second binding. Edit-loop fixes it in one turn.

## Quick start

```shell
# Clone and build (Rust toolchain + Boehm GC + lld).
git clone https://github.com/boldfield/sigil.git
cd sigil
cargo build --release

# Compile and run a program.
./target/release/sigil examples/hello.sigil -o hello
./hello
```

The full toolchain setup and platform notes are in the
[language spec]({{ '/language/' | relative_url }}).

## Sigil isn't trying to be the language humans want most

It's trying to be the language LLMs get right.

Browse the [spec]({{ '/language/' | relative_url }}), read the
[empirical case]({{ '/for-llms/' | relative_url }}), or look at the
[source on GitHub](https://github.com/boldfield/sigil).
