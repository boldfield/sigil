# Sigil v2 — status rollup

_Last updated: 2026-05-30._

"v2" is not a release — it's a cluster of three independent design docs
in `boldfield/designs` (`2026-05-08-sigil-v2-*`), tracked locally by the
`PLAN_E*` progress files. v1.0.0 shipped; these are the post-v1
architecture projects. This file is the one-screen ground truth, because
the per-plan trackers and `docs/superpowers/plans/` lag reality — **trust
`git log` / `gh pr list` over any `PLAN_*.md` status line.**

## The three plans

| Plan | What it is | State |
|------|------------|-------|
| **E1 — profile-data emission** | Runtime CPU + allocation sampling → pprof/folded, consumable by flamegraph/speedscope/perfetto. Zero-overhead when the env-var gates are off. | **Done.** PR #148 + #179 (dyld symbolization). Spec §16. |
| **E2 — precise GC + Cranelift stackmaps** | Replace Boehm conservative scanning with precise roots: Cranelift emits stackmaps at safepoints; the runtime walks them for exact live GC refs. Precise typed heap marking via the header bitmap. | **Done.** See below. Design doc in `designs/done/`. |
| **E3 — per-context CPS** | Decide CPS-vs-native per *call context* instead of coloring a whole function one way. | **Open frontier.** Only Phase 1 merged (#175). |

## E2 — complete

All 19 task PRs merged to `main`: #151, #156, #157, #159, #162–#167,
#169–#171, #178, #181–#186. Tracked in `PLAN_E2_PROGRESS.md`.

- **Phase 1 — stackmaps:** Cranelift 0.131 emits `UserStackMap`s at
  safepoints; codegen flags heap-pointer values (`lower_alloc_call` /
  `lower_heap_pointer_load`); v1 wire format written to the
  `__SIGIL,__stackmaps` section; runtime reader + `SIGIL_GC_CROSS_CHECK`
  harness assert precise-roots ⊆ conservative-scan.
- **Phase 2 — precise heap marking:** non-atomic objects allocate via
  `GC_malloc_explicitly_typed`, driven by a compile-time shape table
  (PR #178 replaced the runtime descriptor cache). False-retention
  reproducer is the ship-gate test.
- **Phase 3 — precise stack roots:** Sigil thread stacks scanned via the
  stackmap walker (`GC_set_push_other_roots` + `GC_do_blocking` /
  `GC_call_with_gc_active` + captured-FP mechanism); conservative stack
  scan dropped for Sigil threads. Alloc-trampoline-elision follow-up
  (#181–#186) recovers ~20% of the per-alloc overhead, default-on.

**Throughput verdict:** Phase 3's mark-phase savings hypothesis is
**disproven** — `boehm_gc_time_ms = 0` on every workload even under
forced-collection injection. Precise GC is load-bearing for
**correctness** (false-retention closure), not throughput. Full per-alloc
cost is ~14 ns (ubuntu) / ~18 ns (macos).

**Conservative scanning still survives**, by design, for `count == 0`
payloads (arrays, string-builder segments — element count exceeds the
6-bit header `count` field) and runtime-internal threads. The eventual
typed-walker via `TAG_EXTERNAL_DESCRIPTOR` would close the array case;
not scheduled.

## E3 — the open frontier

Per-context CPS is the one genuinely unfinished v2 project. Today
effect/CPS coloring is **whole-function**: if any path needs the CPS
trampoline, the whole function pays. Per-context CPS lets a function run
native in contexts that don't activate an effect and CPS only where it
does. Phase 1 (PR #175: discharge analyzer + activation-gate finding)
landed the analysis; the codegen that acts on it is not built. Most of
the remaining v2 design risk lives here.

## Cross-cutting / not-E2

- **Outstanding correctness bug — codegen ICE.** `codegen.rs:~23501`
  `unreachable!("codegen: unknown ident")` fires on H04 for both
  `claude-opus-4-7` and `claude-sonnet-4-6` (per
  `comp/log/dashboard-20260529T235820`). It's a typecheck soundness gap:
  an `Expr::Ident` reaches codegen without being in env, a registered
  ctor, or rewritten — the resolver should have rejected it with E0046
  first. Not covered by any plan. A crash-with-backtrace is the worst
  LLM-authorship outcome; fix before further friction work.
- **Ergonomic follow-ups (scheduled for the v2 timeframe, NOT gated on
  the GC/CPS architecture):** field-access operator `record.field`
  (closes E0151), qualified call syntax `std.list.map(...)` (closes
  E0147 on H03), type-arg threading for `mut`-array element types
  (closes E0044 / `mut-array-element-type`). These are independent
  features, unscheduled.
- **Stack traces on `panic`:** still unimplemented. Their stackmap
  prerequisite shipped with E2, so it's now a buildable follow-up
  (needs a symbolizing unwinder), not blocked. Spec §13 limitations
  table updated to reflect this.

## Baseline first-pass rates

From `comp/log/dashboard-20260529T235820-baseline.md` (corpus `e4c6683`):

| Model | First-pass | Final-pass |
|-------|-----------|-----------|
| `claude-haiku-4-5` | 84.8% | 93.2% |
| `claude-sonnet-4-6` | 90.0% | 94.8% |
| `claude-opus-4-7` | 98.4% | 99.6% |

Headroom is concentrated on the weaker models and the H-tier (H01, H04).
