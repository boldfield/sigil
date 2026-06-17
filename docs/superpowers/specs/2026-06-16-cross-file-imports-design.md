# Cross-file user-module imports — design

Status: draft (feature_spec for the agentask board)
Date: 2026-06-16

## Problem / motivation

Sigil v1 forbids cross-file user-code imports. A program is a single
`.sigil` file; only the standard library may span modules. The parser
rejects any non-`std` import with **E0031** ("user-code imports are not
supported in v1"). This caps every user program at one file.

That cap is now the binding constraint on real programs. The immediate
forcing case is a `jq` clone: its lexer, parser, and evaluator want to
be separate modules, but as one file they cannot be decomposed for
parallel authorship and they grow past the size at which an LLM author
can edit them reliably. More broadly, single-file is the wrong default
for the language's primary audience — LLMs and agent fleets — which
author best when work is split into small, independently-addressable
units.

This feature lifts the cap: user programs may be split across multiple
`.sigil` files that import each other, using the **same** `import` /
`use` syntax already used for the standard library.

## Design principle: LLM ergonomics is the priority

Sigil is designed around LLM authorship, so the import model is chosen
to eliminate the two ways LLMs most reliably fail at imports, not to
minimize implementation effort:

1. **Location-dependent path reasoning.** LLMs lose track of directory
   structure across long or multi-file generations. Paths resolved
   relative to the *importing* file (`../utils`, `./parser/lexer`) force
   a "where am I" computation that drifts — and drifts worst in an agent
   fleet where different agents author different files blind to each
   other's location.
2. **Ambiguity.** A flat shared namespace where a bare name (`map`)
   could come from several modules is a guess the model gets wrong.

Every decision below falls out of removing those two failure modes.

## Goals

- A user program may be split across multiple `.sigil` files that the
  author writes, importing one another and shared modules.
- User modules use the **same** `import a.b.c` / `use a.b.c.{x, y}`
  syntax as the standard library. No second mechanism, no new concepts.
- Module paths are **root-anchored and location-independent**: a given
  `import` means the same file regardless of which file writes it.
- The **filesystem layout is the module tree** — no manifest, no
  module-declaration step. If the file exists at the resolved path, the
  import works.
- **Qualified calls** (`module.fn(...)`) work for user modules and are
  the collision-safe form; bare names via `use` remain available and
  require qualification only when ambiguous.
- Diagnostics close the LLM edit loop: a missing module names the exact
  expected file path; a bad symbol lists what the module exports; a
  cycle names the cycle path.

## Behavior

### Resolution rule

The **root** is the directory containing the entry file handed to the
compiler (the file defining `main`). Every user import resolves against
that fixed root, dotted-segments to path-segments:

- `import app.parser` → `<root>/app/parser.sigil`
- `import app.parser.lexer` → `<root>/app/parser/lexer.sigil`
- `use app.parser.{parse}` → binds `parse` from `<root>/app/parser.sigil`

The rule is identical from every file in the program — the entry file
and any nested module resolve `import app.parser` to the same file. The
importing file's own location never enters the computation.

The `std.` prefix is unchanged: it continues to resolve from the
stdlib embedded in the compiler binary, and always wins for that prefix.

### Filesystem is the module tree

There is no manifest and no declaration step. A module exists iff its
`.sigil` file exists at the resolved path. Authors (human or LLM) add a
module by creating a file; nothing registers it.

### Qualified vs. bare names

- `app.parser.parse(...)` — qualified, always unambiguous, self-
  documenting (origin is in the call site). The recommended form.
- `parse(...)` after `use app.parser.{parse};` — bare, ergonomic, and
  valid as long as the name is unambiguous across that file's imports.
- When a bare name is ambiguous across imported modules, the compiler
  requires qualification rather than silently choosing. This reuses the
  qualified-call-syntax work already queued for H03; the two ship
  together because they are the same ergonomic problem.

### Diagnostics

- **Missing module** (E0032): names the exact expected file path
  ("no module `app.parser` — expected `<root>/app/parser.sigil`").
- **Unknown imported symbol**: lists the module's exported names.
- **Cycle** (E0033): names the full cycle path across the user modules.

### Generics across modules

A generic function defined in one user module and instantiated from
another monomorphizes into the single compilation unit, exactly as
cross-module stdlib generics do today. No separate or precompiled
module artifacts.

## Third-party libraries (explicit non-goal of a package manager)

Third-party libraries require **zero additional language machinery.**
They are **vendored source** that lives in the project tree and imports
through the identical root-anchored rule. There is deliberately **no**
package manager, registry, version resolver, lockfile, or network fetch
in this feature.

Rationale, from the same LLM-ergonomics oracle: a package-manager import
path would be a *second* import mechanism (doubling the failure
surface), would introduce **version strings** (which LLMs hallucinate),
and would make what-actually-linked depend on a solver the model cannot
see (the ambiguity failure mode). Sigil also has no package ecosystem to
manage yet, so building one now is premature. Vendoring keeps programs
self-contained, byte-reproducible, and offline — properties the project
already values.

**Convention:** vendored third-party source lives under a `deps.`-
prefixed root (`import deps.json5.parse` → `<root>/deps/json5/parse.sigil`).
Provenance-in-the-name tells an LLM author at the call site that the code
is external, the same way qualified calls put origin in the call. The
`deps.` prefix is a recommended layout, **not** a special language
construct — it resolves by the ordinary rule.

**Future seam (out of scope here):** package *acquisition* is separated
from package *import*. The language only ever resolves a module path to a
file. A later, non-LLM-authored tool (`sigil fetch` or similar) may
resolve versions, pull source, and write it into `deps/` with a
lockfile. When that arrives, the language's import model does not change.

## Non-goals

- **Package manager / registry / versioning / lockfiles / network
  fetch.** Third-party = vendored source (above).
- **Visibility modifiers** (`pub` / `priv`). Everything in a module is
  importable. Fewer decisions for the author to get wrong; deferrable.
- **Search paths / configurable roots / `sigil.toml`.** The root is
  implicitly the entry file's directory.
- **Importer-relative paths.** Explicitly rejected — the central LLM
  failure mode this design removes.
- **Separate / incremental compilation into precompiled module
  artifacts.** The single-compilation-unit model is retained.
- **New aliasing / re-export surface** beyond what `use` already
  provides for std.

## Acceptance criteria (the stopping condition)

All testable, all gating:

1. A multi-file program — an entry file plus at least two user modules,
   including a **transitive** import (A imports B imports C) and a
   **generic used across modules** — compiles, links, runs, and matches
   its oracle.
2. **Location independence:** the same `import app.x` written in the
   entry file and in a nested module resolves to the same file and
   compiles in both.
3. A **name collision** across two imported user modules surfaces a
   clear diagnostic and is resolved by qualifying the call.
4. A **cycle** across user modules reports E0033 with the cycle path.
5. A **missing module** reports E0032 naming the expected file path.
6. An **unknown imported symbol** error lists the module's exports.
7. The **`deps.` vendoring convention** works as an ordinary user
   import (one end-to-end test).
8. **No regression:** all existing std-only programs and the full
   e2e / smoke / reproducibility suites still pass.

## Constraints and gotchas

- **Source loading.** Today the stdlib is embedded via `include_dir!`
  (`compiler/src/stdlib_embed.rs:11`) and `imports::resolve` already
  takes a pluggable source closure (`compiler/src/imports.rs:84`). User
  modules slot in by extending that closure: try embedded `std/` for the
  `std.` prefix, else read from the filesystem rooted at the entry-file
  directory. The import machinery itself (recursive load, dedup, cycle
  detection) is generic over the source and is reused unchanged
  (`compiler/src/imports.rs:182`).
- **The real work is path-based keying.** User modules are currently
  keyed by basename only — `canonical_module_label` maps `x.sigil → x`
  (`compiler/src/typecheck.rs:2979`), so two files with the same basename
  in different directories would collide. This must become root-relative
  path keying so module identity is unique and matches the resolution
  rule. This key (`canonical_fn_key`) is consumed pervasively (function
  resolution, the `use`-binding prepass at `typecheck.rs:2178`,
  monomorphization, diagnostics); this is the bulk of the effort and the
  highest-risk area.
- **Gates to lift (5 sites):** parser `import` (`parser.rs:343`), parser
  `use` (`parser.rs:468`), `path_to_module` (`imports.rs:149`),
  `module_file_for_path` (`typecheck.rs:2945`), and the E0031 message.
  E0031 is reworded/retired; E0032 and E0033 are extended to user
  modules.
- **Single compilation unit.** All modules flatten into one Cranelift
  object as today; symbol mangling must stay unique across user modules
  (path-based keys make this natural).
- **Test-harness gap.** The e2e harness currently compiles a single
  file (`compile_file_and_run`). Multi-file programs need a fixture
  mechanism (write an entry file plus sibling modules into a temp tree,
  compile the entry). Building that fixture support is itself a task.
- The release-runtime-lib rebuild gotcha does not apply — no runtime
  edits are expected.

## Test expectations

- **e2e** (`compiler/tests/e2e.rs`): one test per acceptance criterion,
  using a new multi-file fixture helper. Cover basic import, transitive
  import, generic-across-modules, location independence, collision +
  qualification, cycle → E0033, missing module → E0032, unknown symbol,
  and the `deps.` convention.
- **Unit** (`compiler/src/imports.rs`): filesystem source resolution and
  path-based keying, alongside the existing dedup / cycle tests.
- **smoke** (`scripts/smoke.sh`): a small multi-file example with a
  hardcoded oracle (the `jq` clone is the natural first consumer once
  this lands).
- **No regression** across the existing suite and `reproducibility.sh`.

## Rough milestones (for decomposition)

- **M1 — Lift gates + filesystem source.** Remove the 5 std-only gates;
  extend the source closure to resolve user modules from the entry-file
  root; reuse dedup/cycle. Multi-file imports load and flatten.
- **M2 — Path-based keying (the meat).** Root-relative module identity;
  qualified + bare resolution through the new keys; ambiguity requires
  qualification (bundled with H03). The high-risk milestone.
- **M3 — Generics + symbol uniqueness across modules.** Prove and test
  cross-module monomorphization and mangling.
- **M4 — Diagnostics + tests + spec.** E0032/E0033 extended with paths
  and cycle reporting; multi-file e2e fixture support and the full test
  matrix; spec §10 (module system) updated; E0031 retired.

## Board note

This is compiler work — the subtle, less-Haiku-friendly kind. M1, M3,
and M4 slice into cleaner Haiku-sized tasks; **M2 carries most of the
risk** and its tasks will be the high-reject-rate ones at the review
gate. Decompose M2 finer than feels necessary and expect heavier Opus
review there.
