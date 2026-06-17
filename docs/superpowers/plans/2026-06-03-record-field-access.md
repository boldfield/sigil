# record.field Field-Access Operator Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let an author read a named record field with `binding.field`
(and chains `a.b.c`), replacing the E0151 "use match destructure" error —
an LLM-ergonomics win (every model expects `entry.name`).

**Architecture:** Mostly a typecheck change. The parser already
accumulates `a.b.c` into one `Expr::Ident("a.b.c")` (same as qualified
names). At the existing E0151 site in `check_expr`, when no module prefix
matches, attempt **field-access resolution**: the chain head must be a
single-variant record binding; walk the field chain. On success, **build
the equivalent one-arm `match` expression, type-check it inline, and
record it in a span-keyed map**; a rewrite pass then replaces the
`Ident` node with that `match` before monomorphize/codegen. No new AST
variant, no codegen change — the existing record-pattern machinery does
the work.

**Tech Stack:** Rust (`compiler/src/typecheck.rs`, `errors/catalog.rs`),
Sigil e2e tests (`compiler/tests/e2e.rs`, helper `compile_and_run`),
spec markdown.

**Spec:** `docs/superpowers/specs/2026-06-03-record-field-access-design.md`.

---

## Integration map (confirmed in-repo — use these exact names)

- **Resolver:** `fn check_expr(&mut self, e: &Expr, row: &[EffectInst], row_tail: Option<u32>) -> Option<Ty>` at `typecheck.rs:6247`. The `Expr::Ident(name, span)` arm contains the E0151 block at **`typecheck.rs:6447-6480`** (`if name.contains('.') { … if !any_prefix_known { push_error("E0151", …); return None; } }`). The field-access logic replaces the body of the `if !any_prefix_known { … }` block.
- **`Tc` struct** (`typecheck.rs:3753`) fields available on `self`:
  - `env: BTreeMap<String, Ty>` — local bindings. `self.env.get(name).cloned()`.
  - `types: BTreeMap<String, TypeDecl>` — type decls by name.
  - `current_generic_subst: BTreeMap<String, Ty>` — active generic subst.
  - `subst: Subst` — HM substitution; apply via `self.subst.apply_ty(&ty)`.
  - `current_fn_file: Option<String>`, `file_module_paths`.
  - `push_error(code: &str, span: Span, msg: String)`.
- **`Ty` enum** (`typecheck.rs:127`): record type is `Ty::User(String /*type name*/, Vec<Ty> /*type args*/)`. Also `Int/String/Unit/Bool/Char/Byte/Fn/Var/Tuple/Continuation`.
- **Record decl** (`ast.rs`): `TypeDecl{ name, generic_params: Vec<GenericParam>, variants: Vec<Variant>, … }` (`ast.rs:333`); `Variant{ name, fields: VariantFields, … }` (`ast.rs:346`); `VariantFields::{Unit, Positional(Vec<TypeExpr>), Record(Vec<RecordFieldDecl>)}` (`ast.rs:355`); `RecordFieldDecl{ name: String, ty: TypeExpr, span }` (`ast.rs:367`). A **single-variant record** is `td.variants.len() == 1 && matches!(td.variants[0].fields, VariantFields::Record(_))`. The variant's *constructor name* for the pattern is `td.variants[0].name`.
- **Field type resolution:** `fn resolve_field_ty(&self, t: &TypeExpr, subst: &BTreeMap<String, Ty>) -> Option<Ty>` (`typecheck.rs:4245`). Build `subst` by zipping `td.generic_params` (names via `gp.name`) with the `Ty::User` args (see `typecheck.rs:8322-8330` for the exact zip pattern).
- **Match/Pattern AST** (`ast.rs`): `Expr::Match{ scrutinee: Box<Expr>, arms: Vec<MatchArm>, span }` (`ast.rs:472`); `MatchArm{ pattern: Pattern, body: Expr, span }` (`ast.rs:686`); `Pattern::Ctor{ name: String, fields: CtorPatternFields, span }` (`ast.rs:723`); `CtorPatternFields::Record(Vec<CtorPatternField>)` (`ast.rs:733`); `CtorPatternField{ name: String, pattern: Pattern, span }` (`ast.rs:744`); `Pattern::Var(String, Span)`, `Pattern::Wildcard(Span)`.
- **Rewrite pass:** `fn rewrite_resolved_idents(program: &mut Program, resolved_idents: &BTreeMap<Span,String>, bare_name_origins, stdlib_files)` (`typecheck.rs:2318`), called from `pub fn typecheck(...)` at `typecheck.rs:2127` gated on `!has_errors`. The inner walker `fn rewrite_expr(e: &mut Expr, …)` (`typecheck.rs:2422`) recurses all sub-exprs and renames `Expr::Ident`. We extend this walker to **replace** an `Expr::Ident` whose span is in a new desugar map.
- **Qualified resolution that must keep working:** `resolve_qualified_or_use_scheme` (`typecheck.rs:4081`) runs *before* the E0151 block, so `std.list.map` / `IO.println` / `Option.Some` never reach field-access. No change there.
- **E0151 catalog entry:** `errors/catalog.rs:1182-1220`.
- **Existing E0151 tests:** only in `parser.rs` (`ident_dot_ident_parses_as_dotted_ident` @2793, `import_dot_does_not_fire_e0151` @2812, `perform_effect_dot_does_not_fire_e0151` @2828) — these assert the *parser* doesn't fire E0151 and **do not need changes**. No typecheck/e2e tests assert E0151 today.

---

## File structure

| File | Responsibility |
|---|---|
| `compiler/src/typecheck.rs` | New `Tc.field_access_desugar` map; `try_resolve_field_access` helper (the walk + synthetic-match build + inline check + record); wire it into the E0151 block; thread the map into the rewrite pass and replace the node in `rewrite_expr`. |
| `compiler/src/errors/catalog.rs` | Reword the `E0151` entry from "no field-access operator" to the not-a-record / no-such-field diagnostics. |
| `compiler/tests/e2e.rs` | New `field_access_*` tests (flat, nested, generic, errors) + the H04-style ergonomic demo. |
| `spec/language.md` | §6 records: document the operator; add an idioms/§13 note. |

---

## Task 1: Mechanism + flat single-field read (end-to-end)

This is the load-bearing task: it proves the whole pipeline (resolve →
build synthetic match → inline check → record → rewrite-replace → mono →
codegen → run) on the simplest case. Later tasks extend the resolver
only.

**Files:**
- Modify: `compiler/src/typecheck.rs`
- Test: `compiler/tests/e2e.rs`

- [ ] **Step 1: Write the failing e2e test**

Append to the end of `compiler/tests/e2e.rs`:

```rust
// ===== record.field field access =======================================

#[test]
fn field_access_flat_record() {
    // `p.name` reads the field; was E0151 before this feature.
    let source = "import std.io\n\
                  use std.io.{IO};\n\
                  type Person = { name: String, age: Int }\n\
                  fn greet(p: Person) -> String ![] { p.name }\n\
                  fn main() -> Int ![IO] {\n\
                    let p: Person = Person { name: \"Ada\", age: 36 };\n\
                    perform IO.println(greet(p));\n\
                    0\n\
                  }\n";
    let (stdout, stderr, code) = compile_and_run(source, "field_access_flat");
    assert_eq!(code, 0, "expected clean exit; stderr={stderr}");
    assert_eq!(stdout.trim_end(), "Ada");
}
```

- [ ] **Step 2: Run, verify it fails with E0151**

Run: `cargo test -p sigil-compiler --test e2e -- field_access_flat_record`
Expected: FAIL — compile error E0151 ("no field-access operator") on `p.name`.

- [ ] **Step 3: Add the desugar map to `Tc` and its initializer**

In `typecheck.rs`, add a field to the `Tc` struct (near `resolved_idents`, ~line 3753 block):

```rust
    /// Span of a dotted `Expr::Ident` resolved as record field access →
    /// the equivalent (already type-checked) `match` expression that
    /// reads the field. `rewrite_field_access` replaces the Ident node
    /// with this match after type-checking, before monomorphize.
    field_access_desugar: BTreeMap<Span, Expr>,
```

Find where `Tc` is constructed (the `Tc { … }` literal in `typecheck` / a `Tc::new`-like site — search for `resolved_idents: BTreeMap::new()`) and add:

```rust
            field_access_desugar: BTreeMap::new(),
```

- [ ] **Step 4: Implement `try_resolve_field_access` (flat case only for now)**

Add this method on `impl Tc` (near `check_expr`). For Task 1 it handles a
single field on a non-generic record; chains and generics come in Tasks
2–3. It returns `Some(field_ty)` on success (and records the synthetic
match), or `None` if this dotted name is NOT field access (head not a
local binding) so the caller falls through to the existing error path.
For field-access-shaped-but-invalid cases (head is a record but field
missing, or head is a non-record binding) it pushes a specific error and
returns `Some(Ty::Var(fresh))`-style poison… — simplest: push the error
and return `None` is acceptable too, but then the caller must NOT also
fire E0151. To keep control explicit, return an enum:

```rust
enum FieldAccessOutcome {
    /// Resolved; expression type is `ty`, synthetic match recorded.
    Resolved(Ty),
    /// Looks like field access but is invalid; a specific error was
    /// already pushed. Caller returns None without firing E0151.
    Errored,
    /// Not field access (head isn't a local binding); caller proceeds
    /// to its existing fallthrough (E0046/E0151).
    NotFieldAccess,
}

impl Tc {
    fn try_resolve_field_access(
        &mut self,
        name: &str,
        span: &Span,
        row: &[EffectInst],
        row_tail: Option<u32>,
    ) -> FieldAccessOutcome {
        let segments: Vec<&str> = name.split('.').collect();
        // Guard: must be head.field (Task 1 handles exactly one field;
        // Task 2 generalises to chains).
        if segments.len() < 2 {
            return FieldAccessOutcome::NotFieldAccess;
        }
        let head = segments[0];
        // Head must be a local binding (param/let/match-binding).
        let head_ty = match self.env.get(head).cloned() {
            Some(t) => self.subst.apply_ty(&t),
            None => return FieldAccessOutcome::NotFieldAccess,
        };
        // Walk one field for Task 1 (generalised in Task 2).
        let field = segments[1];
        // Head must be a single-variant record type.
        let (type_name, type_args) = match &head_ty {
            Ty::User(tn, args) => (tn.clone(), args.clone()),
            _ => {
                self.push_error(
                    "E0151",
                    span.clone(),
                    format!(
                        "cannot read field `{field}`: `{head}` has type `{}`, which is \
                         not a record. Use `match` to destructure a sum type.",
                        ty_display(&head_ty)
                    ),
                );
                return FieldAccessOutcome::Errored;
            }
        };
        let td = match self.types.get(&type_name) {
            Some(td) if td.variants.len() == 1
                && matches!(td.variants[0].fields, VariantFields::Record(_)) => td.clone(),
            _ => {
                self.push_error(
                    "E0151",
                    span.clone(),
                    format!(
                        "cannot read field `{field}`: `{head}` has type `{type_name}`, which \
                         is not a single-variant record. Use `match` to destructure it."
                    ),
                );
                return FieldAccessOutcome::Errored;
            }
        };
        let variant = &td.variants[0];
        let fields = match &variant.fields {
            VariantFields::Record(fs) => fs.clone(),
            _ => unreachable!("guarded above"),
        };
        if !fields.iter().any(|f| f.name == field) {
            let names: Vec<String> = fields.iter().map(|f| f.name.clone()).collect();
            self.push_error(
                "E0151",
                span.clone(),
                format!(
                    "no field `{field}` on record `{type_name}` (fields: {}).",
                    names.join(", ")
                ),
            );
            return FieldAccessOutcome::Errored;
        }
        // Build the synthetic one-arm match:
        //   match head { Variant { field: __fa_field, others: _ } => __fa_field }
        let binder = format!("__fa_{field}");
        let pat_fields: Vec<CtorPatternField> = fields
            .iter()
            .map(|f| CtorPatternField {
                name: f.name.clone(),
                pattern: if f.name == field {
                    Pattern::Var(binder.clone(), span.clone())
                } else {
                    Pattern::Wildcard(span.clone())
                },
                span: span.clone(),
            })
            .collect();
        let synthetic = Expr::Match {
            scrutinee: Box::new(Expr::Ident(head.to_string(), span.clone())),
            arms: vec![MatchArm {
                pattern: Pattern::Ctor {
                    name: variant.name.clone(),
                    fields: CtorPatternFields::Record(pat_fields),
                    span: span.clone(),
                },
                body: Expr::Ident(binder.clone(), span.clone()),
                span: span.clone(),
            }],
            span: span.clone(),
        };
        // Type-check the synthetic match through the normal path so it
        // validates and populates span-keyed annotations codegen needs.
        let ty = match self.check_expr(&synthetic, row, row_tail) {
            Some(t) => t,
            None => return FieldAccessOutcome::Errored,
        };
        // Record for the rewrite pass to swap the Ident → this match.
        self.field_access_desugar.insert(span.clone(), synthetic);
        FieldAccessOutcome::Resolved(ty)
    }
}
```

Notes for the implementer:
- `ty_display` — use the typechecker's existing `Ty`-to-string helper. Search for how `Ty` is formatted in other diagnostics (e.g. an `impl Display for Ty` or a `display_ty`/`ty_to_string` fn) and use that exact function instead of the placeholder `ty_display`.
- `Span` is `Clone`. `EffectInst` is the row element type already used in `check_expr`'s signature.
- The `td.clone()` avoids borrow conflicts with the later `self.push_error` / `self.check_expr` (which need `&mut self`). If `TypeDecl` is large, instead clone only `variant.name` + `fields` before dropping the `self.types` borrow.

- [ ] **Step 5: Wire it into the E0151 block**

In `check_expr`'s `Expr::Ident` arm, replace the body of the
`if !any_prefix_known { … }` block (currently `push_error("E0151", …);
return None;` at ~6465-6479) with:

```rust
                    if !any_prefix_known {
                        match self.try_resolve_field_access(name, span, row, row_tail) {
                            FieldAccessOutcome::Resolved(ty) => return Some(ty),
                            FieldAccessOutcome::Errored => return None,
                            FieldAccessOutcome::NotFieldAccess => {
                                // Not a record field read — restore the
                                // original teaching diagnostic.
                                let head = segments[0];
                                self.push_error(
                                    "E0151",
                                    span.clone(),
                                    format!(
                                        "`{name}` is not a known qualified name, and `{head}` \
                                         is not a record binding in scope."
                                    ),
                                );
                                return None;
                            }
                        }
                    }
```

(Keep the surrounding `if name.contains('.')` and the `segments`/`modules`/`any_prefix_known` computation above it unchanged.)

- [ ] **Step 6: Thread the desugar map into the rewrite pass and replace the node**

In `rewrite_resolved_idents` (`typecheck.rs:2318`) add a parameter
`field_access_desugar: &BTreeMap<Span, Expr>` and pass it down to
`rewrite_expr`. At the call site in `typecheck` (~line 2127) pass
`&tc.field_access_desugar`.

In `rewrite_expr` (`typecheck.rs:2422`), at the TOP of the function
(before the existing `match e`), add the whole-node replacement:

```rust
    // Field-access desugar: replace a dotted `Expr::Ident` resolved as
    // record field access with its synthetic `match`. Done before the
    // rename match below; the synthetic match's own idents are locals /
    // synthetic binders that need no further rewriting.
    if let Expr::Ident(_, span) = e {
        if let Some(desugared) = field_access_desugar.get(span) {
            *e = desugared.clone();
            return;
        }
    }
```

Add `field_access_desugar` to every recursive `rewrite_expr` call inside
the walker (the function recurses into sub-expressions — thread the new
arg through each call). If that's many call sites, prefer making
`field_access_desugar` available via the same passing style the other
maps use.

- [ ] **Step 7: Run, verify it passes**

Run: `cargo test -p sigil-compiler --test e2e -- field_access_flat_record`
Expected: PASS (`Ada`).

If it fails at compile time of the *Sigil* program with a panic in mono/codegen
(rather than a clean error), the synthetic match's spans likely aren't
in the annotation maps codegen reads — verify Step 4 calls
`self.check_expr(&synthetic, …)` (not a manual type derivation) so the
synthetic is fully integrated. Report specifics rather than guessing.

- [ ] **Step 8: Run the broader resolver tests for regressions**

Run: `cargo test -p sigil-compiler --lib typecheck`
Expected: PASS — qualified names (`std.list.map`, `IO.println`) and the
existing parser E0151 tests are unaffected (field-access only fires after
qualified resolution misses).

- [ ] **Step 9: Commit**

```bash
git add compiler/src/typecheck.rs compiler/tests/e2e.rs
git commit -m "feat(typecheck): record.field access via match-desugar (flat single field)"
```

---

## Task 2: Chained field access (`a.b.c`)

**Files:**
- Modify: `compiler/src/typecheck.rs`
- Test: `compiler/tests/e2e.rs`

- [ ] **Step 1: Write the failing test**

Append to `compiler/tests/e2e.rs`:

```rust
#[test]
fn field_access_chained() {
    // a.b.c reads through nested single-variant records.
    let source = "import std.io\n\
                  use std.io.{IO};\n\
                  type Inner = { value: String }\n\
                  type Outer = { inner: Inner, tag: Int }\n\
                  fn deep(o: Outer) -> String ![] { o.inner.value }\n\
                  fn main() -> Int ![IO] {\n\
                    let i: Inner = Inner { value: \"deep\" };\n\
                    let o: Outer = Outer { inner: i, tag: 7 };\n\
                    perform IO.println(deep(o));\n\
                    0\n\
                  }\n";
    let (stdout, stderr, code) = compile_and_run(source, "field_access_chained");
    assert_eq!(code, 0, "expected clean exit; stderr={stderr}");
    assert_eq!(stdout.trim_end(), "deep");
}
```

- [ ] **Step 2: Run, verify it fails**

Run: `cargo test -p sigil-compiler --test e2e -- field_access_chained`
Expected: FAIL — Task 1's helper bails (`segments.len() < 2` passes, but it
only reads `segments[1]` and ignores `.value`; `o.inner` would resolve to
an `Inner` record but the trailing `.value` is dropped → type mismatch or
wrong output). Confirm the failure mode, then generalise.

- [ ] **Step 3: Generalise `try_resolve_field_access` to walk the whole chain**

Replace the single-field body in `try_resolve_field_access` (the part
after computing `head` / `head_ty`) with a loop over
`segments[1..]` that threads a *current scrutinee expression* and a
*current type*, building a nested match per field:

```rust
        // Walk every field in the chain, building nested matches.
        let mut cur_expr = Expr::Ident(head.to_string(), span.clone());
        let mut cur_ty = head_ty;
        for field in &segments[1..] {
            let field = *field;
            let (type_name, type_args) = match &cur_ty {
                Ty::User(tn, args) => (tn.clone(), args.clone()),
                _ => {
                    self.push_error(
                        "E0151",
                        span.clone(),
                        format!(
                            "cannot read field `{field}`: the value before `.{field}` has \
                             type `{}`, which is not a record.",
                            ty_display(&cur_ty)
                        ),
                    );
                    return FieldAccessOutcome::Errored;
                }
            };
            let td = match self.types.get(&type_name) {
                Some(td) if td.variants.len() == 1
                    && matches!(td.variants[0].fields, VariantFields::Record(_)) => td.clone(),
                _ => {
                    self.push_error(
                        "E0151",
                        span.clone(),
                        format!(
                            "cannot read field `{field}`: `{type_name}` is not a \
                             single-variant record. Use `match` to destructure it."
                        ),
                    );
                    return FieldAccessOutcome::Errored;
                }
            };
            let variant = &td.variants[0];
            let decl_fields = match &variant.fields {
                VariantFields::Record(fs) => fs.clone(),
                _ => unreachable!("guarded"),
            };
            let decl = match decl_fields.iter().find(|f| f.name == field) {
                Some(d) => d.clone(),
                None => {
                    let names: Vec<String> =
                        decl_fields.iter().map(|f| f.name.clone()).collect();
                    self.push_error(
                        "E0151",
                        span.clone(),
                        format!(
                            "no field `{field}` on record `{type_name}` (fields: {}).",
                            names.join(", ")
                        ),
                    );
                    return FieldAccessOutcome::Errored;
                }
            };
            // Field type, with the record's generics substituted (Task 3
            // exercises a non-trivial subst; for non-generic records this
            // subst is empty).
            let subst = Self::subst_for(&td, &type_args);
            let field_ty = match self.resolve_field_ty(&decl.ty, &subst) {
                Some(t) => t,
                None => {
                    self.push_error(
                        "E0151",
                        span.clone(),
                        format!("could not resolve the type of field `{field}` on `{type_name}`."),
                    );
                    return FieldAccessOutcome::Errored;
                }
            };
            // Build `match cur_expr { Variant { field: __b, others: _ } => __b }`.
            let binder = format!("__fa_{field}");
            let pat_fields: Vec<CtorPatternField> = decl_fields
                .iter()
                .map(|f| CtorPatternField {
                    name: f.name.clone(),
                    pattern: if f.name == field {
                        Pattern::Var(binder.clone(), span.clone())
                    } else {
                        Pattern::Wildcard(span.clone())
                    },
                    span: span.clone(),
                })
                .collect();
            cur_expr = Expr::Match {
                scrutinee: Box::new(cur_expr),
                arms: vec![MatchArm {
                    pattern: Pattern::Ctor {
                        name: variant.name.clone(),
                        fields: CtorPatternFields::Record(pat_fields),
                        span: span.clone(),
                    },
                    body: Expr::Ident(binder.clone(), span.clone()),
                    span: span.clone(),
                }],
                span: span.clone(),
            };
            cur_ty = field_ty;
        }
        let synthetic = cur_expr;
        let ty = match self.check_expr(&synthetic, row, row_tail) {
            Some(t) => t,
            None => return FieldAccessOutcome::Errored,
        };
        self.field_access_desugar.insert(span.clone(), synthetic);
        FieldAccessOutcome::Resolved(ty)
```

Add the helper (associated fn — no `self` borrow) that builds the
generic substitution for a record decl applied to type args:

```rust
    fn subst_for(td: &TypeDecl, type_args: &[Ty]) -> BTreeMap<String, Ty> {
        let mut subst = BTreeMap::new();
        for (gp, arg) in td.generic_params.iter().zip(type_args.iter()) {
            subst.insert(gp.name.clone(), arg.clone());
        }
        subst
    }
```

(Use `gp.name` — confirm the field name on `GenericParam` by reading its
definition in `ast.rs`; the zip pattern mirrors `typecheck.rs:8322-8330`.)

- [ ] **Step 4: Run, verify both pass**

Run: `cargo test -p sigil-compiler --test e2e -- field_access_chained field_access_flat_record`
Expected: both PASS.

- [ ] **Step 5: Commit**

```bash
git add compiler/src/typecheck.rs compiler/tests/e2e.rs
git commit -m "feat(typecheck): chained record.field access (a.b.c)"
```

---

## Task 3: Generic records

Confirms a field whose type involves the record's generic parameters
resolves under substitution (e.g. a `Box[T] = { value: T }`).

**Files:**
- Test: `compiler/tests/e2e.rs` (the `subst_for` + `resolve_field_ty` path from Task 2 should already handle this; this task adds the proving test and fixes only if it surfaces a bug).

- [ ] **Step 1: Write the failing/proving test**

Append to `compiler/tests/e2e.rs`:

```rust
#[test]
fn field_access_generic_record() {
    // Field type uses the record's generic param T.
    let source = "import std.io\n\
                  import std.int\n\
                  use std.io.{IO};\n\
                  use std.int.{int_to_string};\n\
                  type Box[T] = { value: T, label: String }\n\
                  fn unbox(b: Box[Int]) -> Int ![] { b.value }\n\
                  fn main() -> Int ![IO] {\n\
                    let b: Box[Int] = Box { value: 42, label: \"answer\" };\n\
                    perform IO.println(int_to_string(unbox(b)));\n\
                    0\n\
                  }\n";
    let (stdout, stderr, code) = compile_and_run(source, "field_access_generic");
    assert_eq!(code, 0, "expected clean exit; stderr={stderr}");
    assert_eq!(stdout.trim_end(), "42");
}
```

- [ ] **Step 2: Run**

Run: `cargo test -p sigil-compiler --test e2e -- field_access_generic_record`
Expected: PASS if Task 2's `subst_for` + `resolve_field_ty` handle the
generic correctly. If the field type comes back as the unresolved generic
name (wrong type / a type error on `int_to_string(b.value)`), the
`subst_for` zip or the `Ty::User` args threading is off — fix
`try_resolve_field_access` so `type_args` are the *applied* args from the
binding's `Ty::User(_, args)` and `subst_for` maps `generic_params → args`.

- [ ] **Step 3: Commit**

```bash
git add compiler/tests/e2e.rs compiler/src/typecheck.rs
git commit -m "test(typecheck): generic record field access resolves under subst"
```

---

## Task 4: Error cases + E0151 catalog reword

**Files:**
- Modify: `compiler/src/errors/catalog.rs`
- Test: `compiler/tests/e2e.rs`

- [ ] **Step 1: Write the failing tests (negative: must fail to compile with a clear message)**

Append to `compiler/tests/e2e.rs`:

```rust
#[test]
fn field_access_unknown_field_errors() {
    let source = "type Person = { name: String, age: Int }\n\
                  fn bad(p: Person) -> Int ![] { p.height }\n\
                  fn main() -> Int ![] { 0 }\n";
    let (_stdout, stderr, code) = compile_and_run(source, "field_access_unknown_field");
    assert_ne!(code, 0, "must fail: no field `height`");
    assert!(
        stderr.contains("no field `height`") && stderr.contains("Person"),
        "expected a no-such-field message naming the record; stderr={stderr}"
    );
}

#[test]
fn field_access_on_non_record_errors() {
    let source = "fn bad(n: Int) -> Int ![] { n.value }\n\
                  fn main() -> Int ![] { 0 }\n";
    let (_stdout, stderr, code) = compile_and_run(source, "field_access_non_record");
    assert_ne!(code, 0, "must fail: Int is not a record");
    assert!(
        stderr.contains("not a record"),
        "expected a not-a-record message; stderr={stderr}"
    );
}

#[test]
fn field_access_on_sum_type_errors() {
    // A multi-variant sum type has no statically-known field set —
    // field access must error and point the author at `match`.
    let source = "type Shape = | Circle(Int) | Square(Int)\n\
                  fn bad(s: Shape) -> Int ![] { s.radius }\n\
                  fn main() -> Int ![] { 0 }\n";
    let (_stdout, stderr, code) = compile_and_run(source, "field_access_sum_type");
    assert_ne!(code, 0, "must fail: Shape is a multi-variant sum type");
    assert!(
        stderr.contains("not a single-variant record") && stderr.contains("match"),
        "expected a sum-type message pointing at match; stderr={stderr}"
    );
}
```

- [ ] **Step 2: Run, verify the messages already match (Tasks 1–2 push these)**

Run: `cargo test -p sigil-compiler --test e2e -- field_access_unknown_field_errors field_access_on_non_record_errors`
Expected: PASS — the `push_error` messages from `try_resolve_field_access`
already produce "no field `height` on record `Person` …" and
"`n` has type `Int`, which is not a record". If the wording differs from
the asserts, reconcile (prefer adjusting the message to read naturally
and updating the assert substring to match — keep both honest).

- [ ] **Step 3: Reword the E0151 catalog entry**

In `compiler/src/errors/catalog.rs` (the `E0151` entry at ~1182-1220),
replace `short` / `long` / `fix_example` so the code now documents
field-access *resolution failures* rather than "no field-access operator":

```rust
    ErrorEntry {
        code: "E0151",
        short: "record field access could not be resolved",
        long: "Sigil reads a record field with `binding.field` (the chain \
               head must be a value of a single-variant record type). This \
               diagnostic fires when a dotted name is not a known qualified \
               path AND is not a valid field access: the head is not a \
               record binding in scope, the head's type is not a \
               single-variant record (e.g. a primitive, a tuple, or a \
               multi-variant sum type — destructure those with `match`), or \
               the named field does not exist on the record.",
        fix_example: "type Person = { name: String, age: Int }\n\
                      fn greet(p: Person) -> String ![] {\n  \
                      p.name            // field access\n}\n\
                      // For a multi-variant sum type, destructure instead:\n\
                      // match shape { Circle { r } => r, Square { s } => s }",
    },
```

(Match the surrounding entries' exact field names — `code`, `short`,
`long`, `fix_example` — and the trailing-comma / string-continuation
style used in that file.)

- [ ] **Step 4: Run the catalog/doc consistency checks**

Run: `cargo test -p sigil-compiler --lib errors`
Expected: PASS (any catalog round-trip / formatting tests still green).

- [ ] **Step 5: Commit**

```bash
git add compiler/src/errors/catalog.rs compiler/tests/e2e.rs
git commit -m "feat(typecheck): precise field-access errors; reword E0151 catalog entry"
```

---

## Task 5: Spec docs + the H04 ergonomic-win demo

**Files:**
- Modify: `spec/language.md`
- Test: `compiler/tests/e2e.rs` (the demo, as an asserting e2e test)

- [ ] **Step 1: Update §6 (records) in `spec/language.md`**

Find the §6 text that says records are read by match destructuring and
"v1 has no `.name` field-access syntax" (search `field-access` /
`.name`). Replace the "no field-access" sentence with documentation of
the operator. Use this wording (place it where the no-field-access
sentence was):

```markdown
Record fields are read with `binding.field` (field access), where the
head of the chain is a value of a single-variant record type; chains
read through nested records (`node.inner.value`). Field access is
read-only — there is no field-update syntax. `match` destructuring
remains available and is the only way to read fields of a multi-variant
sum type (the variant, and hence the field set, isn't known statically).
Field access on a non-identifier head (e.g. `make(x).field`) is not yet
supported — bind it to a `let` first.
```

- [ ] **Step 2: Add an idioms/§13 note**

In the idioms appendix (or §13 quick-reference area — search for the
appendix added in earlier work), add a one-line entry that `record.field`
reads a field and chains, so the LLM-facing surface advertises it.
Example line to add under the surface-syntax reminders:

```markdown
- Read a record field with `r.field` (and chains `r.a.b`); `match`
  destructuring is still used for sum-type variants.
```

- [ ] **Step 3: Add the H04-style ergonomic demo as an e2e test**

Append to `compiler/tests/e2e.rs` — this is the concrete payoff: the
field reads that previously needed accessor fns / `match`, now written
directly:

```rust
#[test]
fn field_access_h04_style_ergonomic_demo() {
    // Before this feature, reading `entry.name`/`entry.score` required
    // a match or an accessor fn per field (the H04 corpus boilerplate).
    // Now it's direct.
    let source = "import std.io\n\
                  import std.int\n\
                  use std.io.{IO};\n\
                  use std.int.{int_to_string};\n\
                  type Entry = { name: String, score: Int }\n\
                  fn describe(e: Entry) -> String ![] {\n\
                    string_concat(e.name, string_concat(\": \", int_to_string(e.score)))\n\
                  }\n\
                  fn main() -> Int ![IO] {\n\
                    let e: Entry = Entry { name: \"Ada\", score: 90 };\n\
                    perform IO.println(describe(e));\n\
                    0\n\
                  }\n";
    let (stdout, stderr, code) = compile_and_run(source, "field_access_h04_demo");
    assert_eq!(code, 0, "expected clean exit; stderr={stderr}");
    assert_eq!(stdout.trim_end(), "Ada: 90");
}
```

(`string_concat` is a bare intrinsic — no import needed.)

- [ ] **Step 4: Run the demo + spec sanity**

Run: `cargo test -p sigil-compiler --test e2e -- field_access_h04_style_ergonomic_demo`
Expected: PASS (`Ada: 90`).
Run: `grep -n "binding.field\|r.field" spec/language.md` — confirm the docs landed.

- [ ] **Step 5: Commit**

```bash
git add spec/language.md compiler/tests/e2e.rs
git commit -m "docs(spec): document record.field operator + H04 ergonomic demo"
```

---

## Task 6: Full verification

- [ ] **Step 1: Full e2e suite — no regressions**

Run: `cargo test -p sigil-compiler --test e2e 2>&1 | tail -3`
Expected: all pass except the known-flaky wall-clock perf gates
(`fib_perf`, `fib_cps_perf`, `perf_gate_zero`, `tree_example`,
`multishot_perf` — verify each in isolation with `--test-threads=1`). All
new `field_access_*` tests pass.

- [ ] **Step 2: Full lib tests (typecheck + resolver regressions)**

Run: `cargo test -p sigil-compiler --lib 2>&1 | tail -3`
Expected: PASS — qualified-name resolution, exhaustiveness, and the
parser E0151 tests all still green.

- [ ] **Step 3: smoke + reproducibility (existing record-using examples unaffected)**

Run: `./scripts/smoke.sh 2>&1 | tail -2`
Run: `./scripts/reproducibility.sh 2>&1 | tail -2`
Expected: both OK.

- [ ] **Step 4: pod-verify (fmt + clippy + discipline)**

Run: `./scripts/pod-verify.sh`
Expected: `pod-verify: OK`. (Run `cargo fmt -p sigil-compiler` first if
the new Rust trips rustfmt — multi-line constructions of the synthetic
match may need formatting.)

- [ ] **Step 5: Confirm std.map / std.set still compile (they are records)**

Run: a quick program importing `std.map` + using `map_get` etc. via
`./scripts/smoke.sh` already covers example compiles; additionally run
the existing map/set-using e2e tests:
`cargo test -p sigil-compiler --test e2e -- map set 2>&1 | tail -3`
Expected: PASS — internal record types (`Map`/`Set`) are unaffected
(their code uses `match`, not `.field`).

- [ ] **Step 6: Push branch**

```bash
git push origin record-field-access
```

---

## Notes for the implementer

- **The load-bearing risk is Task 1, Step 6/7:** replacing the `Ident`
  node with a synthetic `match` in the post-typecheck rewrite, and that
  synthetic surviving monomorphize/codegen. Because the synthetic was
  type-checked inline (Step 4 calls `self.check_expr(&synthetic, …)`), its
  spans are in the typecheck annotation maps. If mono/codegen still
  choke, the fallback is to make the field-access desugar happen at the
  same point the synthetic is *checked* (keep it in the rewrite map) and
  verify the rewrite runs before monomorphize (it does — line 2127 is
  inside `typecheck`, which returns the program that mono then consumes).
- **Borrow-checker:** `try_resolve_field_access` needs `&mut self` for
  `push_error` / `check_expr` while reading `self.types` / `self.env`.
  Clone what you read (`td`, `decl_fields`, `head_ty`) before the
  `&mut self` calls, as the code above does.
- **`ty_display` / `gp.name`:** confirm the real names against the source
  (a `Ty` Display helper and `GenericParam`'s field) before relying on
  them.
- **Don't touch** the three parser E0151 tests — they assert the parser
  doesn't fire E0151 and remain correct.
- The test programs' expected stdout strings are authoritative.
