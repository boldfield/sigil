# Plan B Deviations

Deviations from `in-progress/2026-04-21-sigil-effects.md`. Each entry is logged
**before** the implementing commit (per the plan's commit discipline). Entries
remain after Plan B closes as a permanent record.

Format:

```
## <date> — [DEVIATION Task N] <one-line topic>

**Context:** ...

**Deviation:** ...

**Rationale:** ...

**Implementing commit(s):** <SHAs>
```

Untagged sweep / chore entries use `[CHORE]` instead of `[DEVIATION Task N]`.

## 2026-04-25 — [Task 4.5.5 / A3-carryover] Tagged-vs-raw Int ABI decision

**Context:** Plan A3's `QUESTIONS.md` entry `[PLAN-A3] main-return-tagging`
(resolved 2026-04-24) explicitly reopened the broader ABI question for
Plan B: should internal user-function calls pass `Int` as tagged 64-bit
values, or as raw `i64` with tagging only at the C-ABI boundary? The
effect-runtime CPS transform (Stage 6 Task 55) and trampoline machinery
need a definitive answer before they ship.

Today's codebase:
- User functions compute on **raw `i64`** (no tagging in body-local
  arithmetic).
- **Tagging happens only at `main`'s return**: codegen emits
  `ishl_imm TAG_INT_SHIFT` on the Int value and the generated C-main
  shim emits `sshr_imm TAG_INT_SHIFT` + `ireduce I32` to produce the
  process exit code.
- Heap-stored `Int` values (closure env slots, user-type field slots)
  currently flow through as whatever Cranelift `Value` the field type
  resolves to — `I64` for `Int`. This avoids GC surprises because GC
  only scans pointer-typed slots via the pointer bitmap; non-pointer
  fields are opaque payloads.

**Decision:** keep the current pattern — raw `i64` internally within
user code, tag at the C-ABI boundary only. Effect-runtime CPS work in
Stage 6 layers onto this by introducing *new* boundary moments:

1. **Continuations captured across handler boundaries** (the
   `current_k` that `sigil_perform` passes to handler arms) must
   carry tagged `Int` arguments, because a captured continuation can
   sit on the heap in a `HandlerFrame` slot that the GC scans.
2. **`NextStep` records arena-allocated by the trampoline** keep
   their args untagged (raw `i64`) for arithmetic cycles. The arena
   is reset per dispatch and never scanned by the Boehm collector;
   raw values here avoid tag/untag churn on the hot trampoline path.

This is option (c)'s "raw everywhere internally, tag at the C-ABI
boundary" applied to user-fn calls specifically, plus a narrower
tag-at-heap-observability rule for continuations and handler-scope
slots. It is compatible with the A3 resolution (main → Int locked)
because main's C-ABI boundary was already the one tagging site.

**Rationale:**
1. **Minimal code churn in this PR.** The existing pattern is already
   "raw internally". Formalising the decision without rewriting every
   user-fn call site keeps the diff focused.
2. **Tagging is a GC-discipline question, not a performance one.**
   Heap-observable slots need tags so the GC can tell Int from
   pointer. Non-heap slots don't. Arena-allocated `NextStep` records
   are non-heap (arena is reset, not scanned); continuations are
   heap (scanned), so they need tagged payloads.
3. **CPS hot path stays tight.** Arithmetic-dense CPS-color code
   (fib under `!State[Int]`) will dispatch through the trampoline
   billions of times per `fib(30)` invocation. Every tag/untag pair
   saved on the hot path matters for the Task 60 performance floor.
4. **One constant to audit.** `sigil_abi::tag::TAG_INT_SHIFT` is the
   single reference for the shift amount. Stage 6's new tagging sites
   consume it too. A future revisit would edit one place.

**Audit of `ishl_imm` / `sshr_imm` sites (2026-04-25):**

| file                     | site                 | purpose                                | status              |
|--------------------------|----------------------|----------------------------------------|---------------------|
| compiler/src/codegen.rs  | user-main return     | tag `Int` for C-ABI exit code          | updated to TAG_INT_SHIFT |
| compiler/src/codegen.rs  | C-main shim untag    | untag tagged `Int` → raw → `ireduce I32` | updated to TAG_INT_SHIFT |

No other tag-shift sites exist in the compiler or runtime today.
`from_int` / `as_int` in `sigil_runtime::value` already consume
`TAG_INT_SHIFT` as of Task 4.5.5.

**Implementing commit(s):** [HEAD]

**Cross-references:** QUESTIONS.md — the `[PLAN-A3] main-return-tagging`
entry's Forward-Implications paragraph is now closed by this decision.
Added a `[PLAN-B] tagged-vs-raw-int-abi` entry pointing back here.

## 2026-04-25 — [VERIFICATION DEBT] Tagged-vs-raw ABI contract enforcement

**Context:** The Task 4.5.8 decision (raw `i64` internally; tag at GC-
observable boundaries) creates a forward contract that today is just
documentation. Stage 6 introduces the first non-`main` boundary moments:

- `HandlerFrame` slots that hold continuation-captured `Int` arguments
  must be **tagged** (heap-observable; the GC scans them).
- Arena-allocated `NextStep` slots that hold per-step `Int` values must
  be **raw `i64`** (arena resets per dispatch; not GC-scanned).

Without an enforcement mechanism, the contract is easy to violate by
accident — a developer writing `frame.args[0] = raw_int` instead of
`frame.args[0] = tag(raw_int)` would compile and produce wrong-but-
silent GC behavior under stress.

**Closure point:** Task 56 (Stage 6 — `runtime/src/handlers.rs` and
`runtime/src/arena.rs` ship the runtime-side data structures).

**Picked mechanism:** **Newtype wrappers around `i64`.** Specifically:

```rust
// runtime/src/value.rs (or sigil-abi::tag, TBD at Task 56)
#[repr(transparent)]
pub struct TaggedInt(i64);

#[repr(transparent)]
pub struct RawInt(i64);

impl TaggedInt {
    pub fn from_raw(r: RawInt) -> Self { Self((r.0 as u64).wrapping_shl(TAG_INT_SHIFT) as i64) }
    pub fn untag(self) -> RawInt { RawInt(self.0 >> TAG_INT_SHIFT) }
}
```

`HandlerFrame` slot fields use `TaggedInt`; arena `NextStep` fields use
`RawInt`. Conversion is explicit at every boundary; the Rust compiler
rejects any direct assignment of one to the other. Conversion functions
themselves consume `TAG_INT_SHIFT` from `sigil-abi::tag` so a future ABI
revision still has a single mechanical edit point.

This was picked over the alternatives because:

- **Newtype wrappers** are statically enforced at compile time — every
  contract violation is a Rust type error, not a runtime debug-only
  assertion that production builds elide. The Stage 6 effect runtime
  will be extensively used; runtime-only checks are weaker.
- Newtypes are zero-cost (`#[repr(transparent)]`) and the conversion
  functions inline; the trampoline hot path keeps its tight codegen.
- The runtime side (where this contract matters most — `sigil_perform`,
  handler-arm calling convention, arena `alloc` / `reset`) is in Rust;
  newtypes work cleanly there.

Codegen-side stores into Cranelift IR slots are outside the Rust type
system (Cranelift `Value`s are opaque IDs). At those sites, a paired
contract comment cross-referencing this entry is the practical follow-
through; the test that the contract holds is end-to-end (the Stage 6
multi-shot stress test in `examples/multishot_stress.sigil` will fail
under stress if any boundary mis-tags).

**Acceptance criterion for Task 56:**
1. `runtime/src/handlers.rs` declares `HandlerFrame` with `TaggedInt`
   for any `Int`-typed slot.
2. `runtime/src/arena.rs` declares `NextStep` with `RawInt` for any
   `Int`-typed slot.
3. The `TaggedInt` <-> `RawInt` conversion functions consume
   `sigil_abi::tag::TAG_INT_SHIFT` (no inline literal `1`).
4. Every codegen site that emits a Cranelift store into a handler-frame
   slot or arena slot has a contract comment cross-referencing this
   entry and the Task 4.5.8 decision.
5. The Plan B Task 60 multi-shot stress test passes on both hosts.

**Implementing commit(s):** TBD — closes at Task 56.

## 2026-04-25 — [VERIFICATION DEBT] Codegen path for un-monomorphized generic params

**Context:** Plan B Task 47 grew the AST with `TypeExpr::Apply` and
parser-only support for generic parameters on `fn` and `type`
declarations. Today the typechecker rejects every `Apply` with E0124
and rejects every `effect_row_var` with E0125, so codegen never sees
generic-applied types or row-polymorphic effect rows.

When Task 48 (HM unification with row variables) lands, those E0124 /
E0125 rejections turn off — generic params become bound type variables
that flow through typecheck. Task 49 (monomorphization) then specialises
generics down to concrete instantiations. Codegen (in particular
`cranelift_ty_for_type_expr` at `compiler/src/codegen.rs:95`) is
written assuming monomorphization completed: it consults `head_name()`
and falls through to `pointer_ty` for any unrecognised name. A
post-Task-48-pre-Task-49 pipeline could in principle hand codegen an
un-monomorphized generic-parameter reference (`A` from `fn id[A](x: A)`),
which would silently lower as `pointer_ty` — wrong for `Int`-typed `A`,
and undetectable until the program crashes at the platform-call
boundary.

**Closure point:** Task 48 (Stage 5 — HM unification binds the generic
parameters; the codegen invariant becomes assertable for the first time).

**Picked mechanism:** **Explicit `assert!` at the codegen entry point**
that monomorphization has erased every `TypeExpr::Apply` and every
generic-param reference, paired with an invariant comment at
`cranelift_ty_for_type_expr` documenting the expectation.

```rust
// compiler/src/codegen.rs (added at the top of emit_object)
assert!(
    !checked.program.contains_apply_or_generic_ref(),
    "codegen invariant: monomorphization (Task 49) must complete before \
     codegen; received program still contains TypeExpr::Apply or generic-\
     parameter references — see PLAN_B_DEVIATIONS verification-debt entry",
);
```

`contains_apply_or_generic_ref()` is a small AST walker that scans every
`TypeExpr` in the checked program for `Apply` nodes and for `Named` nodes
whose name appears in any in-scope `generic_params` list. The assert
fires in release builds (cheap walk), so a pipeline ordering bug is
loud rather than silent.

This was picked over the alternative (codegen detects generic-param
references mid-emit and emits an internal-compiler-error diagnostic)
because:

- A single entry-point assert is simpler to audit than per-emission-site
  ICE machinery. Reviewers see one invariant, one check, one place to
  update if the contract evolves.
- Failure mode is loud and immediate (process abort with a clear
  message) rather than diffuse (an ICE deep in lowering is harder to
  diagnose; the user sees \"compiler bug\" without knowing why).
- Cost is one whole-program AST walk per `emit_object` invocation
  (linear in IR size; cheap relative to Cranelift codegen).
- The invariant comment at `cranelift_ty_for_type_expr` is the
  documentation half: future readers see why the catchall is safe.

**Acceptance criterion for Task 48:**
1. `emit_object` (or its successor entry point if codegen is
   restructured) opens with an `assert!` that the input IR contains no
   `TypeExpr::Apply` and no generic-parameter references.
2. The walker that backs the assert is a single function on
   `CheckedProgram` (or whatever IR shape Task 48 settles on),
   exercised by at least one unit test that constructs a program with
   a residual `Apply` and confirms the assert fires.
3. `cranelift_ty_for_type_expr` carries an invariant comment cross-
   referencing this entry and the assertion site.
4. The assertion holds on every example in `examples/` after Task 49
   (monomorphization) lands.

**Implementing commit(s):** [HEAD, REVIEW-FIXUP] (Task 48).

**Closure status (2026-04-25):** all four acceptance bullets met.
Bullet 1 satisfied by the assertion at `emit_object`'s top. Bullet
2 satisfied by four codegen-test unit tests (`walker_rejects_residual_apply_in_fn_param`,
`walker_rejects_generic_param_decl`, `walker_rejects_generic_type_decl`,
`walker_accepts_concrete_program`) that construct `Program` AST
nodes directly via the `ast` types — independent of monomorphization,
exercising the walker's accept and reject paths against synthetic
inputs. Bullet 3 satisfied by the invariant comment at
`cranelift_ty_for_type_expr`. Bullet 4 (assertion holds on every
example after Task 49) is the long-tail check that closes when
monomorphization lands — the assertion is now the canonical contract,
and Task 49's reviewer just needs to confirm it doesn't fire on any
example. **This entry is closed for Task 48.**

## 2026-04-25 — [DEVIATION Task 48 / Stage 6] `subsume_row` rejects open-caller / closed-callee with extra effects

**Context:** Task 48's call-site row check uses asymmetric `subsume_row`
(`compiler/src/typecheck.rs:1074`) instead of symmetric `unify_row`,
which preserves the caller's row variable for generalisation rather
than collapsing it. This was the fix for the PR #15 review's "open-row
caller silently collapses to closed" bug.

**Deviation:** the asymmetric check requires `callee.effects ⊆
caller.known_effects`, even when the caller declares an open row.
For a caller `![IO | e]` invoking a callee `![Raise]` (closed):

- `callee_set = {Raise}`, `caller_set = {IO}` (caller's *known* part only)
- `missing = {Raise}` → `subsume_row` pushes E0042

The caller's open tail `e` is supposed to be extensible — semantically,
`![IO | e]` accepts any callee effect by absorbing the difference into
`e`. The current check rejects this case rather than extending `e`.

**Why deferred:** Sigil v1 only admits `IO` as a real surface effect.
Multi-effect programs (the case where this bug bites) land with Stage 6
effect handlers (Task 55 onward). There is no current example or test
that exposes the wrong behavior, and the fix has a non-trivial design
question: extending `e` requires either binding the caller's row var
to a row that includes `missing` (which is the symmetric-unification
behavior we explicitly rejected for the original bug) or threading
effect *constraints* on row variables through scheme-generalisation
so they accumulate without committing the row var to a specific shape.

The Stage 6 work has the constraint machinery in scope anyway (handler
typing requires reasoning about which effects flow through which
continuations); piggy-backing the open-tail extension on that work
keeps the design coherent rather than picking a v1-only stop-gap.

**Closure point:** Stage 6 effect handlers (Task 55 — runtime, plus
the typing rules that introduce additional surface effects).

**Workaround until then:** declare the explicit effect on the caller's
known list. `fn caller[e]() -> Int ![IO, Raise | e]` accepts callees
performing `IO` *or* `Raise` (or both); only callees with effects
outside `{IO, Raise}` would still trip E0042 against this caller.

**Acceptance criterion for closure:**
1. `subsume_row` (or its successor in the constraint-graph design)
   accepts open-caller / closed-callee with callee effects not in
   the caller's known list, by extending the caller's row constraints
   rather than emitting E0042.
2. A surface-level test in `compiler/src/typecheck.rs` covers the
   case (a generic caller with `![IO | e]` calling a closed-row
   callee that performs `Raise`) and confirms it typechecks.
3. The Plan B Stage 6 effect-handler suite (TBD) exercises the path
   end-to-end with at least one program where multi-effect flow
   through an open-row caller is the intended pattern.

**Implementing commit(s):** TBD — closes at Task 55 or earlier if a
multi-effect program needs the path before then.

## 2026-04-25 — [DEVIATION Task 49] Mangled name format and ordering

**Context:** Plan body specifies "Canonical specialization names use
lexicographically-sorted type arguments with a canonical recursive form
for nested generics: `list_map__List_Option_Int__List_Option_Int`."
Two interpretation gaps:

1. *Lexicographic sort of type arguments.* Read literally, this would
   reorder a fn's type-argument list before mangling — losing the
   positional binding between a declared generic parameter (`A`, `B`) and
   its concrete instantiation. `f[A=Int, B=String]` and
   `f[A=String, B=Int]` are different instantiations; lex-sorting their
   args to `[Int, String]` and `[Int, String]` respectively would
   produce identical mangled names but completely different bodies.
2. *Canonical recursive form.* The example `list_map__List_Option_Int`
   shows nested generics rendered with single underscores within a
   type-arg, double underscores between top-level type-args. The
   plan does not formalise the separator rule.

**Deviation:**
1. Type arguments are emitted in the callee's *declared* generic-
   parameter order (positional), not lex-sorted. The lex-sort interpretation
   is treated as a plan misstatement; positional ordering is the only
   reading that preserves correctness.
2. Canonical form is formalised in `compiler/src/monomorphize.rs` module
   docs and pinned by unit tests:
   - Primitives render as themselves: `Int`, `String`, etc.
   - `User(name, [])` renders as `name`.
   - `User(name, [a1, a2, ...])` renders as `name$<canon(a1)>$<canon(a2)>$...`
     (single `$` between parts within a type-arg).
   - Top-level fn/type instantiation: `fn_name$$<canon(a1)>$$<canon(a2)>$$...`
     (double `$$` between top-level type-args).
   - Ctor of generic type: `ctor_name$$<canon(a1)>$$<canon(a2)>$$...`
     (same suffix as the owning type so the global ctor namespace stays
     unique across instantiations).

**Rationale (round 2 — separator change to `$`):** The first round of
this PR used `_`/`__` separators. Reviewer round-2 feedback (PR #16
comment 4318163326 #1) flagged that the asymmetric underscore separator
is **not** unambiguous when user fn / type names contain underscores:
`type List_Option[A]` instantiated at `Int` mangled to `List_Option_Int`,
which collided with `List[Option[Int]]`'s rendering of `List_Option_Int`.
`type_seen` would silently collapse the two distinct instantiations,
producing miscompiled programs the GC would scan incorrectly.

**Switched to `$` / `$$`** because the lexer rejects `$` as an identifier
character (same constraint Plan A2 relied on for `$lambda_N` synthetic
names from closure conversion). `List_Option$$Int` and `List$$Option$Int`
are now structurally distinct strings regardless of underscore density
in user-declared identifiers. Determinism flows from BTreeMap iteration
order over the reachability worklist; argument ordering is positional
because it's the only ordering that preserves the type-var → concrete-Ty
binding. Codegen's `mangle_user_fn` rewrites `$` to `__` for ELF /
Mach-O linker compatibility — the rewrite preserves AST-level
unambiguity since uniqueness is enforced at the AST level (`fn_seen`
/ `type_seen`) before any linker-symbol step.

**Hardening:** `Ty::Var(_)` and `Ty::Fn(_)` reaching `canon_ty` /
`ty_to_type_expr` now trip `unreachable!` rather than rendering a
placeholder string. Reviewer round-1 feedback (PR #16 comment
4318161359 #2) flagged the placeholders as silent collision vectors —
two `Ty::Var(3)` and `Ty::Var(7)` rendering identically would collide
in `fn_seen`. Closing that path requires both:
  - The new E0132 diagnostic at end-of-typecheck rejecting any pending
    instantiation whose type-arg resolves to an unbound `Ty::Var(_)`
    that isn't an outer-fn's free var.
  - The `unreachable!` arms in mangling so a future regression is loud,
    not silent.

**Acceptance criterion for closure:** if a future plan adds first-class
function-typed values (`TypeExpr::Fn` surface syntax), `canon_ty`'s
`Ty::Fn` arm needs to be replaced with a real rendering rule before
the surface ships. Until then the `unreachable!` fires immediately
on any code path that produces a `Ty::Fn` reaching mangling, surfacing
the gap loudly.

**Implementing commit(s):** Same commit as Task 49 (initial impl) +
the PR #16 review fix-up commit (separator change + E0132 +
hardened arms).

## 2026-04-25 — [DEVIATION Task 49] Effect rows preserved through monomorphization (permanent v1 design choice — informational)

**Status:** **permanent v1 design choice. Informational — no closure
path expected.** Reviewer round-2 feedback (PR #16 comment
4318163326 #4) flagged the original "open-ended / closes if v2 lands"
framing as wrong-shape: row-specialised monomorphs are explicitly
reserved for v2 in the design doc, and v1 effect dispatch is
fundamentally runtime-indirect — there's no v1 path that would
reopen this deviation. Reframed as informational so future readers
don't waste time looking for a closure trigger.

**Context:** Plan body's Task 49 entry: "Effect rows are not
monomorphized in v1 — they remain polymorphic through this phase and
are erased at codegen (effect dispatch is runtime-indirect). This is a
v1 optimization-budget choice; the design doc explicitly reserves
row-specialised monomorphs for v2."

The Task 48 codegen-entry walker (`contains_apply_or_generic_ref`)
treated `f.effect_row_var.is_some()` as a hard rejection. That made
sense at Task 48 (before monomorphization existed and any program
reaching codegen with a row variable was unmonomorphised). Task 49
introduces monomorphization which intentionally preserves
`effect_row_var` per the plan, so the existing walker rule needs
relaxing.

**Deviation:** the walker no longer rejects `f.effect_row_var.is_some()`
or `Expr::Lambda { effect_row_var: Some(_), .. }`. The rejection is
narrowed to:

- `f.generic_params.is_empty()` (still rejected)
- `td.generic_params.is_empty()` (still rejected)
- `TypeExpr::Apply { .. }` anywhere (still rejected)
- `TypeExpr::Named(name, _)` referencing a generic-param surface
  name in scope (still rejected)

Monomorphized fns retain their `effect_row_var` field as a sentinel
that the body's row was polymorphic; codegen treats `Some(_)` as a
no-op (effect dispatch is runtime-indirect; v1 codegen doesn't
emit per-row dispatch tables).

**Rationale:** The plan body explicitly carves out effect rows from
monomorphization. v1 effect dispatch is runtime-indirect — the row
variable serves no codegen purpose past type-checking. v2 row-specialised
monomorphs are reserved for the design doc.

**Closure path:** none for v1. If v2 ever introduces row-specialised
monomorphs (a non-decision today), a fresh deviation entry should be
opened then to track the new code path; this entry stays as historical
record.

**Implementing commit(s):** Same commit as Task 49.

## 2026-04-25 — [DEVIATION Task 49] Pattern-ctor rewriting via scrutinee Ty

**Context:** Plan body says "Typed IR preserved" but does not specify
how monomorphization should resolve constructor names inside `match`
arm patterns. The typecheck instantiation index keys construction sites
(call / record-lit / unit-ident); pattern-ctor sites are not
constructions and don't get their own instantiation entry.

**Deviation:** Pattern ctors are rewritten by combining two existing
indices: the scrutinee's `Ty::User(name, args)` from
`CheckedProgram::match_scrut_tys`, plus the `ctor_to_type` reverse
index built from the original program's TypeDecls. For
`Pattern::Ctor { name: "Some", .. }` inside a match whose scrutinee
typed as `Option[Int]`, the rewriter looks up "Some" → "Option" via
`ctor_to_type`, observes the scrutinee's args `[Int]`, and produces
`Some__Int`.

Sub-patterns inherit the scrut_ty for their own ctor lookups —
acceptable in v1 because nested ctor patterns either match the
scrutinee's type (same args) or refer to a different type whose own
args resolve via `ctor_to_type` of that ctor's owner-type. v1 doesn't
yet thread per-sub-pattern type-args through inference; if Plan B
Task 50+ exposes a need (e.g., GADT-flavoured cases), this gets
revisited.

**Rationale:** Avoids extending the typecheck instantiation index with
a third map keyed by pattern span. The two-index lookup already covers
all v1 patterns the test surface exercises (Option-style sums, single-
ctor records, nested ctors of the same type).

**Acceptance criterion for closure:** v1's pattern surface ships
unchanged in Tasks 50–52; deviation closes when Stage 5 review
checkpoint accepts the implementation. If Plan C / v2 introduces
patterns whose sub-patterns reference different generic-type
instantiations than the scrutinee's, monomorph gains a per-pattern
instantiation index.

**Implementing commit(s):** Same commit as Task 49.
