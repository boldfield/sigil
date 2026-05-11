# CLAUDE.md — sigil repo

Project-specific guidance for Claude Code. The user's global
preferences in `~/.claude/CLAUDE.md` still apply; this file adds repo
guidance on top.

## What sigil is

A compiled, statically-typed language designed around LLM authorship.
v1.0.0 shipped. Currently working through the v2 architectural cluster
(profile-data emission, precise GC, per-context CPS).

- Spec: `spec/language.md` (canonical)
- Published site: <https://sigillang.ai/> (sourced from `docs/`)
- v1.0.0 release: <https://github.com/boldfield/sigil/releases/tag/v1.0.0>
- License: Apache 2.0 with LLVM Runtime Library Exception. Compiler
  is `Apache-2.0`; the three crates whose code ships inside every
  Sigil-compiled binary (`sigil-runtime`, `sigil-abi`,
  `sigil-header-constants`) are `Apache-2.0 WITH LLVM-exception` so
  the exception covers user-compiled output. Don't drop the
  exception on those three crates without thinking about it — it's
  what makes Sigil-compiled binaries shippable without inheriting
  Apache 2.0 notice obligations.

## Docs sync (tag-triggered)

The published GitHub Pages site at `sigillang.ai` serves
`docs/language.md`, `docs/capabilities.md`, and `docs/for-llms.md` —
each is a Jekyll-front-matter-wrapped copy of a canonical source in
the repo:

| Canonical source       | Published copy            | Site URL          |
|------------------------|---------------------------|-------------------|
| `spec/language.md`     | `docs/language.md`        | `/language/`      |
| `spec/language.md`     | `docs/language.raw.md`    | `/language.raw.md` (no front matter — raw markdown for LLM ingestion) |
| `CAPABILITIES.md`      | `docs/capabilities.md`    | `/capabilities/`  |
| `SIGIL_FOR_LLMS.md`    | `docs/for-llms.md`        | `/for-llms/`      |

Pages-specific callouts (like the "for LLM ingestion" note on
`/language/`) live between the front matter and the
`<!-- BEGIN SYNCED CONTENT -->` sentinel. `sync-docs.sh` preserves
that region across syncs.

**Sync runs automatically on every `v*` tag push** via the `sync-docs`
job in `.github/workflows/release.yml`. The job re-runs
`scripts/sync-docs.sh` against `main`, commits any drift back to
`main` as `Sync docs/ for <tag>`, and pushes. Between tags, `docs/`
may lag the canonical source — that's expected. The published site
reflects whatever was last synced (the last tagged release).

Editing a canonical source (`spec/language.md`, `CAPABILITIES.md`,
`SIGIL_FOR_LLMS.md`) does **not** require a same-commit sync. Don't
run sync-docs locally as a pre-commit ritual; let the tag workflow
handle it. If you want the site to reflect a change immediately,
either cut a tag or run `./scripts/sync-docs.sh` + commit + push
explicitly.

`scripts/check-docs-sync.sh` is still available for local sanity
checks but is no longer CI-gated.

## CI surface (sigil-specific)

Standard rust/cargo gates (rustfmt, clippy, test) plus several
sigil-specific scripts in `scripts/`:

- `pod-verify.sh` — pod-safe subset (fmt + per-crate clippy +
  workspace check + runtime lib tests + discipline greps).
- `check-no-interior-pointers.sh` — runtime invariant (no pointers
  into heap-object payloads).
- `reproducibility.sh` — every example compiles to a byte-identical
  binary across two cold builds.
- `smoke.sh` — every example compiles + runs + matches its oracle.
- `plan-b-invariants.sh` — multi-shot continuation + selective CPS
  charter invariants.

The `pod-verify.sh` script is the fastest "did I break something
load-bearing" check during a working session. The full e2e suite
(`cargo test -p sigil-compiler --test e2e`) is the authoritative gate
but takes ~3 minutes locally.

## Runtime crate gotcha (load-bearing)

`compiler/src/link.rs::locate_runtime_lib` prefers
`target/release/libsigil_runtime.a` over `target/debug/...`. After
editing `runtime/src/*.rs`, rebuild the release lib OR local e2e
tests will silently link against the stale archive:

```sh
cargo build --release -p sigil-runtime
```

This bit twice during the return-arm-via-args lift (Stage 5):
once as a phantom regression that wasted hours, and once as a
local-vs-CI divergence that only surfaced on the CI debug build.
Default to rebuilding release after any runtime edit.

## Open work pointers

- **v2 cluster (active):** `queue/2026-05-08-sigil-v2-*` in the
  designs repo. PR #148 ships profile-data emission as the foundation
  for the other two.
- **H-tier friction follow-ups (queued for v2):**
  - Qualified call syntax (`std.list.map(...)`) to close E0147
    ambiguous-bare-name failures on H03.
  - Field-access operator (`record.field`) to close E0151 failures
    on H04.

## What lives where

- `compiler/` — typechecker + Cranelift codegen. ~33K LOC across
  `src/`; the largest file is `codegen.rs` (~30K LOC). Read the
  diagnostics catalog (`compiler/src/errors/catalog.rs`) before
  proposing a new E-code.
- `runtime/` — Boehm-GC'd runtime: handlers, arena, perform/run_loop
  trampoline, IO/Fs/Process/Env arm fns, profile data emission.
- `std/` — `.sigil`-source stdlib modules (Option, Result, List,
  State, Random, etc.).
- `spec/` — language spec + validation prompts.
- `docs/` — published GitHub Pages site (do not hand-edit the
  derived pages; edit the canonical sources and run sync-docs).
- `comp/` — LLM-authorship comparison harness (P*, C*, H* corpora).
- `examples/` — sample programs used by smoke.sh + reproducibility.sh.
