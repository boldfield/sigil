//! Per-call-site effect-discharge analysis — Plan E3 Phase 1.
//!
//! For every call to a top-level user fn, classify whether the callee's
//! declared effect row is fully covered by the union of effect-handlers
//! that lexically enclose the call site. The classification is purely
//! contextual at the call site; no language-surface annotation, no
//! per-monomorph reclassification (every monomorph's color in the SCC
//! sense is unchanged — Plan E3 layers per-call-site analysis on top).
//!
//! ## Classification
//!
//! Let `R_f` be the callee's declared effect row at this monomorph
//! instantiation. Let `D_c` be the union of effects discharged by every
//! lexically enclosing `handle` expression at call site `c`.
//!
//! - `R_f ⊆ D_c` → [`DischargeStatus::FullyDischarged`]. Phase 2 (if
//!   activated by Phase 1's inventory) emits direct dispatch through a
//!   thin Sync wrapper instead of the standard Cps `post_arm_k`
//!   machinery.
//! - `R_f ∩ D_c` non-empty AND `R_f ⊄ D_c` →
//!   [`DischargeStatus::PartiallyDischarged`]. Cps dispatch still
//!   required because some effects flow upward past the enclosing
//!   handles.
//! - `R_f ∩ D_c` empty → [`DischargeStatus::NotDischarged`].
//!
//! ## Open-row callees
//!
//! Sigil v1 does not monomorphize effect rows ("effect rows are NOT
//! monomorphized" — see `monomorphize.rs` module docs). A callee with
//! `FnDecl::effect_row_var: Some(_)` retains its row variable through
//! the post-mono AST; the row variable can absorb arbitrary effects
//! from the caller context at unification time.
//!
//! Conservatively, an open-row callee is treated as `NotDischarged`
//! when the concrete-effects intersection with `D_c` is empty, and
//! `PartiallyDischarged` otherwise. Open-row callees are *never*
//! classified `FullyDischarged` — the row variable could be unified
//! with effects not in `D_c` at runtime, so static full-discharge
//! cannot be proved. This matches Plan E3's scope guardrail "No new
//! effect-row annotation": the conservative answer is the only one
//! available without a language-surface change.
//!
//! ## What counts as a call site
//!
//! Only direct top-level fn calls — `Expr::Call { callee:
//! Expr::Ident(name, span), .. }` where `name` resolves to a `Fn` item
//! in the post-monomorphization program AND `span` is a key in
//! `CheckedProgram::call_site_instantiations`. The instantiations map
//! filters out shadowed locals (the typecheck `env.get` rule), so a
//! `let foo = 1; foo` shadowing a top-level `foo` does not produce a
//! spurious entry.
//!
//! Indirect calls (lambdas, fn-typed parameters, closures) are
//! excluded. They dispatch through the closure Sync ABI today; the
//! Plan E3 optimization does not apply to them.
//!
//! Constructor applications (`Cons(1, Nil)`, `Wrap { v: 7 }`) are
//! excluded — `monomorphize::rewrite_expr` rewrites their callees to
//! mangled ctor names which are not `Item::Fn` entries in the post-
//! mono program. The `fn_index` lookup naturally filters them out.
//!
//! ## Phase 1 scope
//!
//! This pass is **read-only**. It produces a side-table of
//! classifications and a `--dump-discharge` diagnostic; codegen does
//! not consume the side-table yet (Phase 2 wires the dispatch switch).
//! Per the plan body's three-phase activation rule, Phase 1 alone has
//! no observable runtime behaviour change. The diagnostic is reusable
//! infrastructure regardless of Phase 2's outcome.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{Block, EffectRef, Expr, FnDecl, Item, MatchArm, Stmt};
use crate::color::{Color, ColoredProgram};
use crate::errors::Span;

/// Three-way classification of a call site per Plan E3 Phase 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DischargeStatus {
    /// `R_f ⊆ D_c`. The callee's row is fully covered by enclosing
    /// handles. Phase 2 (if activated) emits direct Sync dispatch.
    FullyDischarged,
    /// `R_f ∩ D_c` non-empty AND `R_f ⊄ D_c`. Some effects covered,
    /// some flow upward. Standard Cps dispatch remains required.
    PartiallyDischarged,
    /// `R_f ∩ D_c` is empty. No effects covered by enclosing handles.
    /// Standard Cps dispatch.
    NotDischarged,
}

impl DischargeStatus {
    pub fn label(self) -> &'static str {
        match self {
            DischargeStatus::FullyDischarged => "FullyDischarged",
            DischargeStatus::PartiallyDischarged => "PartiallyDischarged",
            DischargeStatus::NotDischarged => "NotDischarged",
        }
    }
}

/// One entry in the per-call-site analysis: the callee identity, its
/// row, the enclosing-handle context at the call site, and the
/// classification. Held in source order per caller (insertion order of
/// the walker matches body traversal order).
#[derive(Clone, Debug)]
pub struct CallSite {
    /// Mangled fn name of the caller monomorph that contains this call
    /// site. Caller-keying allows two clones of a generic fn to produce
    /// distinct entries even though they share source spans.
    pub caller_name: String,
    /// Mangled fn name of the callee monomorph being invoked. Plain
    /// (non-mangled) for non-generic callees.
    pub callee_name: String,
    /// Span of the `Expr::Call`'s callee `Ident` (used for the
    /// diagnostic location).
    pub span: Span,
    /// Effect names in the callee's closed-row declaration. For open-
    /// row callees this contains the *concrete* effects in the row
    /// (excluding the row variable's unknown tail).
    pub callee_effects: BTreeSet<String>,
    /// `true` iff the callee declared an open effect row (carries a row
    /// variable, e.g. `![IO | e]`). Open-row callees can never be
    /// `FullyDischarged`.
    pub callee_open_row: bool,
    /// Color of the callee monomorph. The Phase 2 optimization targets
    /// `Color::Cps` callees — Native callees go through the synchronous
    /// path already and the dispatch-shape change does not apply.
    pub callee_color: Color,
    /// Snapshot of `D_c` — the union of effect names discharged by all
    /// `handle` expressions lexically enclosing this call site, in the
    /// order the handles were entered (outermost first).
    pub enclosing_discharged: BTreeSet<String>,
    /// Source spans of the enclosing handle expressions, outermost
    /// first. Used by the diagnostic to point at the specific handles
    /// that contributed to `D_c`.
    pub enclosing_handle_spans: Vec<Span>,
    pub status: DischargeStatus,
}

#[derive(Clone, Debug)]
pub struct DischargeAnalysis {
    /// Call sites in caller-then-source order. Caller order matches
    /// `mono.anf.checked.program.items` (post-monomorphization
    /// declaration order); within a caller, sites appear in body
    /// traversal order.
    pub sites: Vec<CallSite>,
}

impl DischargeAnalysis {
    pub fn count(&self, status: DischargeStatus) -> usize {
        self.sites.iter().filter(|s| s.status == status).count()
    }

    /// Phase 2 activation upper bound: call sites that are
    /// `FullyDischarged` AND the callee is `Color::Cps`. Native-color
    /// callees are excluded because they dispatch through the
    /// synchronous path already — wrapper emission would be no-op.
    /// Phase 1's HARD-STOP review uses this count.
    pub fn fully_discharged_cps_callees(&self) -> usize {
        self.sites
            .iter()
            .filter(|s| {
                s.status == DischargeStatus::FullyDischarged && s.callee_color == Color::Cps
            })
            .count()
    }
}

/// Drive the analysis over a colored program. Walks every monomorph
/// fn's body, threading the enclosing-handle stack and producing one
/// [`CallSite`] entry per qualifying call expression.
pub fn analyze(cp: &ColoredProgram) -> DischargeAnalysis {
    // Build the post-mono fn index — name → &FnDecl. Used to resolve
    // call-site callee names back to the callee monomorph's declared
    // effect row (the "R_f at this monomorph instantiation" the plan
    // body refers to).
    let mut fn_by_name: BTreeMap<&str, &FnDecl> = BTreeMap::new();
    for item in &cp.mono.anf.checked.program.items {
        if let Item::Fn(f) = item {
            fn_by_name.insert(f.name.as_str(), f.as_ref());
        }
    }

    // Map fn name → color so the per-site classification can carry the
    // callee's color for the inventory cross-reference. Build once from
    // the colored program's `colors` Vec.
    let color_by_name: BTreeMap<&str, Color> =
        cp.colors.iter().map(|(n, c)| (n.as_str(), *c)).collect();

    let calls = &cp.mono.anf.checked.call_site_instantiations;

    let mut sites: Vec<CallSite> = Vec::new();
    for item in &cp.mono.anf.checked.program.items {
        if let Item::Fn(f) = item {
            let mut ctx = Ctx {
                caller_name: &f.name,
                fn_by_name: &fn_by_name,
                color_by_name: &color_by_name,
                calls,
                handle_stack: Vec::new(),
                sites: &mut sites,
            };
            ctx.visit_block(&f.body);
        }
    }

    DischargeAnalysis { sites }
}

/// Walker state. Holds the caller identity, the post-mono fn lookup,
/// and the stack of enclosing-handle discharge sets (outermost first).
struct Ctx<'a> {
    caller_name: &'a str,
    fn_by_name: &'a BTreeMap<&'a str, &'a FnDecl>,
    color_by_name: &'a BTreeMap<&'a str, Color>,
    calls: &'a BTreeMap<Span, crate::typecheck::GenericInstantiation>,
    /// Stack of (handle_span, effects_discharged_by_this_handle), with
    /// outermost handle at index 0. A new entry is pushed when entering
    /// the body of an `Expr::Handle`; popped on exit.
    handle_stack: Vec<(Span, BTreeSet<String>)>,
    sites: &'a mut Vec<CallSite>,
}

impl<'a> Ctx<'a> {
    fn visit_block(&mut self, b: &Block) {
        for s in &b.stmts {
            match s {
                Stmt::Let(l) => self.visit_expr(&l.value),
                Stmt::Expr(e) => self.visit_expr(e),
                Stmt::Perform(p) => {
                    for a in &p.args {
                        self.visit_expr(a);
                    }
                }
            }
        }
        if let Some(t) = &b.tail {
            self.visit_expr(t);
        }
    }

    fn visit_expr(&mut self, e: &Expr) {
        match e {
            Expr::IntLit(_, _)
            | Expr::FloatLit(_, _)
            | Expr::StringLit(_, _)
            | Expr::BoolLit(_, _)
            | Expr::CharLit(_, _)
            | Expr::UnitLit(_)
            | Expr::Ident(_, _)
            | Expr::ClosureEnvLoad { .. } => {}
            Expr::Call { callee, args, .. } => {
                // Direct top-level-fn call shape: callee is an Ident
                // whose span is keyed in `call_site_instantiations`
                // AND whose post-mono name resolves to a fn in
                // `fn_by_name`. Indirect calls (Lambda callee, Call
                // callee, etc.) and ctor applications (rewritten by
                // monomorphize but with names not in `fn_by_name`)
                // fall through to descent without classification.
                if let Expr::Ident(name, span) = callee.as_ref() {
                    if self.calls.contains_key(span) {
                        if let Some(callee_decl) = self.fn_by_name.get(name.as_str()) {
                            self.record_call(name, span, callee_decl);
                        }
                    }
                }
                self.visit_expr(callee);
                for a in args {
                    self.visit_expr(a);
                }
            }
            Expr::Perform(p) => {
                for a in &p.args {
                    self.visit_expr(a);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.visit_expr(lhs);
                self.visit_expr(rhs);
            }
            Expr::Unary { operand, .. } => self.visit_expr(operand),
            Expr::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                self.visit_expr(cond);
                self.visit_block(then_block);
                self.visit_block(else_block);
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.visit_expr(scrutinee);
                for MatchArm { body, .. } in arms {
                    self.visit_expr(body);
                }
            }
            Expr::Block(b) => self.visit_block(b),
            Expr::Lambda { body, .. } => self.visit_expr(body),
            Expr::ClosureRecord { env_exprs, .. } => {
                for ex in env_exprs {
                    self.visit_expr(ex);
                }
            }
            Expr::RecordLit { fields, .. } => {
                for f in fields {
                    self.visit_expr(&f.value);
                }
            }
            Expr::Handle {
                body,
                return_arm,
                op_arms,
                span,
            } => {
                // Push the discharge set contributed by this handle's
                // op_arms onto the stack. The set is the union of
                // distinct effect names appearing in the arms — a
                // handle that lists `State.get` and `State.set`
                // discharges `{State}`; one that lists `State.get` and
                // `Raise.raise` discharges `{State, Raise}`. The
                // *operation* granularity is collapsed to *effect*
                // granularity because Sigil's effect rows are per-
                // effect (`![State, IO]`, not `![State.get, IO]`) and
                // typecheck rejects partial arm coverage of an
                // effect's ops at the handle expression (E0unhandled).
                let mut arm_effects: BTreeSet<String> = BTreeSet::new();
                for arm in op_arms {
                    arm_effects.insert(arm.effect.clone());
                }
                // The handle's body executes under this discharge
                // context. Arm bodies execute *after* the discharged
                // perform has resumed via `k(...)` — they are inside
                // the handle's lexical scope but the perform that
                // triggered the arm has already discharged, so for
                // call sites inside arm bodies the same handle is
                // still in scope (arm bodies can in turn perform
                // unrelated effects, which propagate to outer
                // handles).
                self.handle_stack.push((span.clone(), arm_effects));
                self.visit_expr(body);
                if let Some(ra) = return_arm {
                    self.visit_expr(&ra.body);
                }
                for arm in op_arms {
                    self.visit_expr(&arm.body);
                }
                self.handle_stack.pop();
            }
            Expr::Tuple { elems, .. } => {
                for ex in elems {
                    self.visit_expr(ex);
                }
            }
        }
    }

    fn record_call(&mut self, callee_name: &str, span: &Span, callee_decl: &FnDecl) {
        let (callee_effects, callee_open_row) = collect_callee_effects(callee_decl);
        let (enclosing_discharged, enclosing_handle_spans) = self.snapshot_handle_stack();
        let status = classify(&callee_effects, callee_open_row, &enclosing_discharged);
        // Invariant: every name in `fn_by_name` must have a matching
        // entry in `color_by_name` — both maps are built from the
        // same `mono.anf.checked.program.items` list (Item::Fn
        // entries). A missing entry would indicate the color pass
        // filtered an item the analyzer reached, which would be a
        // real bug rather than a "default to Native" situation.
        //
        // `unreachable!` (not `unwrap_or`) so the bug surfaces loudly
        // if a future refactor diverges the two maps. PR #175
        // reviewer flagged the silent fallback explicitly.
        // `Option::expect` is in `clippy.toml`'s disallowed list (the
        // codebase routes user-facing failures through `CompilerError`
        // and uses `unreachable!` for invariant violations like this
        // one — see `color.rs::tarjan_scc` for the same pattern).
        let callee_color = match self.color_by_name.get(callee_name) {
            Some(c) => *c,
            None => unreachable!(
                "discharge: callee `{}` present in fn_by_name but missing from color_by_name",
                callee_name
            ),
        };
        self.sites.push(CallSite {
            caller_name: self.caller_name.to_string(),
            callee_name: callee_name.to_string(),
            span: span.clone(),
            callee_effects,
            callee_open_row,
            callee_color,
            enclosing_discharged,
            enclosing_handle_spans,
            status,
        });
    }

    fn snapshot_handle_stack(&self) -> (BTreeSet<String>, Vec<Span>) {
        let mut union: BTreeSet<String> = BTreeSet::new();
        let mut spans: Vec<Span> = Vec::with_capacity(self.handle_stack.len());
        for (s, eff) in &self.handle_stack {
            union.extend(eff.iter().cloned());
            spans.push(s.clone());
        }
        (union, spans)
    }
}

/// Extract the effect-name set from a callee fn's declared row, and
/// whether the row carries a row variable (open row). Effects are
/// keyed by *name only* — `Raise[E]` and `Raise[F]` collapse to
/// `{Raise}` because Sigil's handler syntax dispatches by effect name
/// (not by effect+type-args), and the v1 monomorphizer does not
/// row-specialize.
fn collect_callee_effects(f: &FnDecl) -> (BTreeSet<String>, bool) {
    let names: BTreeSet<String> = f
        .effects
        .iter()
        .map(|e: &EffectRef| e.name.clone())
        .collect();
    (names, f.effect_row_var.is_some())
}

/// Apply Plan E3's three-way classification rule to a single call
/// site. Pure function for testability.
fn classify(
    callee_effects: &BTreeSet<String>,
    callee_open_row: bool,
    enclosing_discharged: &BTreeSet<String>,
) -> DischargeStatus {
    // Empty concrete row + no row var: callee performs nothing. The
    // plan body classifies this as `NotDischarged` (vacuously — no
    // Cps dispatch happens anyway, the callee is Native-color). The
    // diagnostic still emits the entry so the dump is exhaustive over
    // top-level-fn call sites.
    if callee_effects.is_empty() && !callee_open_row {
        return DischargeStatus::NotDischarged;
    }

    let intersect: BTreeSet<&String> = callee_effects.intersection(enclosing_discharged).collect();
    let intersect_empty = intersect.is_empty();
    let subset_closed = callee_effects.is_subset(enclosing_discharged);

    if callee_open_row {
        // Open row: row variable can absorb arbitrary effects at
        // unification time, so full discharge cannot be proved. Drop
        // to PartiallyDischarged if any concrete effect is covered;
        // otherwise NotDischarged.
        if intersect_empty {
            DischargeStatus::NotDischarged
        } else {
            DischargeStatus::PartiallyDischarged
        }
    } else if subset_closed {
        DischargeStatus::FullyDischarged
    } else if intersect_empty {
        DischargeStatus::NotDischarged
    } else {
        DischargeStatus::PartiallyDischarged
    }
}

/// Render the analysis for `--dump-discharge`. One line per call site
/// in (caller-program-order, source-order) — deterministic across
/// compiler runs.
///
/// Format:
///
/// ```text
/// <file>:<line>:<col>: <caller> -> <callee> ![<row>] -> <status> [<reason>]
/// ```
///
/// Examples:
///
/// ```text
/// examples/state.sigil:42:14: count_elements -> step ![State] -> FullyDischarged (handles at examples/state.sigil:60:5 discharge State)
/// std/raise.sigil:7:5: do_work -> helper ![Raise, IO] -> PartiallyDischarged (handle at examples/foo.sigil:3:5 discharges {Raise}; IO flows up)
/// std/option.sigil:12:9: map_some -> id ![] -> NotDischarged (callee row is empty)
/// ```
pub fn dump_discharge(da: &DischargeAnalysis) -> String {
    let mut out = String::new();
    for s in &da.sites {
        out.push_str(&format_call_site(s));
        out.push('\n');
    }
    // Trailing summary line — single source of truth for the HARD-STOP
    // inventory the user reviews after Phase 1 lands. The textual
    // summary stays stable across compiler runs (deterministic counts)
    // so adversarial-review harnesses can diff it.
    let n_full = da.count(DischargeStatus::FullyDischarged);
    let n_partial = da.count(DischargeStatus::PartiallyDischarged);
    let n_none = da.count(DischargeStatus::NotDischarged);
    let n_full_cps = da.fully_discharged_cps_callees();
    out.push_str(&format!(
        "# summary: {} call site(s); {} FullyDischarged ({} with cps-color callee), {} PartiallyDischarged, {} NotDischarged\n",
        da.sites.len(),
        n_full,
        n_full_cps,
        n_partial,
        n_none,
    ));
    out
}

fn format_call_site(s: &CallSite) -> String {
    let row = render_row(&s.callee_effects, s.callee_open_row);
    let reason = render_reason(s);
    format!(
        "{}:{}:{}: {} -> {} {} -> {} {}",
        s.span.file,
        s.span.line,
        s.span.column,
        s.caller_name,
        s.callee_name,
        row,
        s.status.label(),
        reason,
    )
}

fn render_row(effects: &BTreeSet<String>, open: bool) -> String {
    // BTreeSet iteration is already in ascending key order, so no
    // explicit sort is needed (PR #175 reviewer's nit). If `effects`
    // ever switches to a HashSet, restore the explicit sort here.
    let body = effects
        .iter()
        .map(|s| s.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    match (effects.is_empty(), open) {
        (true, false) => "![]".to_string(),
        (true, true) => "![| e]".to_string(),
        (false, false) => format!("![{body}]"),
        (false, true) => format!("![{body} | e]"),
    }
}

fn render_reason(s: &CallSite) -> String {
    // Build a parenthesized human reason. Two structural cases:
    // (a) handle spans contributed; (b) no handles in scope.
    let mut bits: Vec<String> = Vec::new();
    if !s.enclosing_handle_spans.is_empty() {
        let span_strs: Vec<String> = s
            .enclosing_handle_spans
            .iter()
            .map(|sp| format!("{}:{}:{}", sp.file, sp.line, sp.column))
            .collect();
        let discharged_list: Vec<&str> =
            s.enclosing_discharged.iter().map(|s| s.as_str()).collect();
        let discharged_body = discharged_list.join(", ");
        bits.push(format!(
            "handles at {} discharge {{{}}}",
            span_strs.join(", "),
            discharged_body
        ));
    } else {
        bits.push("no enclosing handles".to_string());
    }
    if s.callee_open_row {
        bits.push("callee row is open (row var)".to_string());
    }
    if s.callee_effects.is_empty() && !s.callee_open_row {
        bits.push("callee row is empty".to_string());
    }
    match s.callee_color {
        Color::Native => bits.push("callee is native".to_string()),
        Color::Cps => bits.push("callee is cps".to_string()),
    }
    format!("({})", bits.join("; "))
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::color::infer_colors;
    use crate::elaborate;
    use crate::lexer;
    use crate::monomorphize::monomorphize;
    use crate::parser;
    use crate::resolve;
    use crate::typecheck;

    fn analyze_src(src: &str) -> DischargeAnalysis {
        let file = "test.sigil";
        let (tokens, lex_errs) = lexer::lex(file, src);
        assert!(lex_errs.is_empty(), "lex: {lex_errs:?}");
        let (prog, parse_errs) = parser::parse(file, &tokens);
        assert!(parse_errs.is_empty(), "parse: {parse_errs:?}");
        // PR #175 reviewer fidelity nit: real pipeline runs
        // imports::resolve between parse and resolve::resolve. Test
        // sources are self-contained today (no `import` lines), but
        // wiring it through here keeps the test pipeline identical
        // to the real one so future tests that add stdlib imports
        // don't silently regress.
        let (prog, import_errs) = crate::imports::resolve(prog);
        assert!(import_errs.is_empty(), "imports: {import_errs:?}");
        let (resolved, resolve_errs) = resolve::resolve(prog);
        assert!(resolve_errs.is_empty(), "resolve: {resolve_errs:?}");
        let (checked, tc_errs) = typecheck::typecheck(resolved.program);
        let hard: Vec<_> = tc_errs
            .iter()
            .filter(|e| matches!(e.severity, crate::errors::Severity::Error))
            .collect();
        assert!(hard.is_empty(), "typecheck: {hard:?}");
        let anf = elaborate::elaborate(checked);
        let mono = monomorphize(anf);
        let cp = infer_colors(mono);
        analyze(&cp)
    }

    fn first_site_in<'a>(da: &'a DischargeAnalysis, caller: &str, callee: &str) -> &'a CallSite {
        da.sites
            .iter()
            .find(|s| s.caller_name == caller && s.callee_name == callee)
            .unwrap_or_else(|| {
                panic!(
                    "no site {}->{} in: {:#?}",
                    caller,
                    callee,
                    da.sites
                        .iter()
                        .map(|s| (s.caller_name.as_str(), s.callee_name.as_str()))
                        .collect::<Vec<_>>(),
                )
            })
    }

    #[test]
    fn pure_callee_classifies_as_not_discharged_vacuously() {
        // Plan body Task 1: "foo: ![] (pure) → NotDischarged
        // (vacuously; no Cps anyway)."
        let src = r#"
            fn helper(n: Int) -> Int ![] { n + 1 }
            fn main() -> Int ![] { helper(7) }
        "#;
        let da = analyze_src(src);
        let s = first_site_in(&da, "main", "helper");
        assert_eq!(s.status, DischargeStatus::NotDischarged);
        assert!(s.callee_effects.is_empty());
        assert!(!s.callee_open_row);
        assert_eq!(s.callee_color, Color::Native);
    }

    #[test]
    fn raise_callee_inside_raise_handle_classifies_as_fully_discharged() {
        // Plan body Task 1: "foo: ![Raise] inside handle Raise →
        // FullyDischarged."
        let src = r#"
            import std.io
            use std.io.{IO};
            effect Raise { raise: () -> Int }
            fn helper() -> Int ![Raise] {
                perform Raise.raise()
            }
            fn main() -> Int ![IO] {
                let v: Int = handle helper() with {
                    Raise.raise(k) => 0,
                };
                perform IO.println("done");
                v
            }
        "#;
        let da = analyze_src(src);
        let s = first_site_in(&da, "main", "helper");
        assert_eq!(s.status, DischargeStatus::FullyDischarged);
        assert!(s.callee_effects.contains("Raise"));
        assert_eq!(s.callee_color, Color::Cps);
        assert!(s.enclosing_discharged.contains("Raise"));
    }

    #[test]
    fn raise_callee_outside_any_handle_classifies_as_not_discharged() {
        // Plan body Task 1: "foo: ![Raise] not inside handle Raise →
        // NotDischarged." Sigil's typecheck rejects `fn main` with
        // any effect outside the top-level-shim-discharged set
        // (E0041), so `outer` is the relevant caller and `main` runs
        // the handle to keep typecheck quiet.
        let src = r#"
            effect Raise { raise: () -> Int }
            fn helper() -> Int ![Raise] { perform Raise.raise() }
            fn outer() -> Int ![Raise] { helper() }
            fn main() -> Int ![] {
                handle outer() with {
                    Raise.raise(k) => 0,
                }
            }
        "#;
        let da = analyze_src(src);
        // `outer -> helper` is inside `outer`'s body. `outer` has no
        // `handle` around the call site, so D_c is empty.
        let s = first_site_in(&da, "outer", "helper");
        assert_eq!(s.status, DischargeStatus::NotDischarged);
        assert!(s.enclosing_handle_spans.is_empty());
    }

    #[test]
    fn raise_and_io_callee_inside_raise_only_classifies_as_partial() {
        // Plan body Task 1: "foo: ![Raise, IO] inside handle Raise (IO
        // not discharged) → PartiallyDischarged."
        let src = r#"
            import std.io
            use std.io.{IO};
            effect Raise { raise: () -> Int }
            fn helper() -> Int ![Raise, IO] {
                perform IO.println("x");
                perform Raise.raise()
            }
            fn main() -> Int ![IO] {
                handle helper() with {
                    Raise.raise(k) => 0,
                }
            }
        "#;
        let da = analyze_src(src);
        let s = first_site_in(&da, "main", "helper");
        assert_eq!(s.status, DischargeStatus::PartiallyDischarged);
        assert!(s.callee_effects.contains("Raise"));
        assert!(s.callee_effects.contains("IO"));
        // Handle discharges only Raise; IO flows up.
        assert!(s.enclosing_discharged.contains("Raise"));
        assert!(!s.enclosing_discharged.contains("IO"));
    }

    #[test]
    fn nested_handle_inner_discharge_seen_at_inner_call_site() {
        // The plan body's "nested handles" expectation: the inner
        // call sees the inner handle's discharge AS WELL AS the outer
        // handle's. Inner site classifies as FullyDischarged when the
        // union covers the callee row.
        let src = r#"
            import std.io
            use std.io.{IO};
            effect Raise { raise: () -> Int }
            effect State { get: () -> Int, set: (Int) -> Int }
            fn inner() -> Int ![Raise, State] {
                let _: Int = perform State.get();
                perform Raise.raise()
            }
            fn main() -> Int ![IO] {
                handle
                    handle inner() with {
                        Raise.raise(k) => 0,
                    }
                with {
                    State.get(k) => k(0),
                    State.set(arg, k) => k(arg),
                }
            }
        "#;
        let da = analyze_src(src);
        let s = first_site_in(&da, "main", "inner");
        // Both Raise (inner handle) AND State (outer handle) are in
        // scope at the call site, so the union covers `![Raise, State]`.
        assert_eq!(s.status, DischargeStatus::FullyDischarged);
        assert!(s.enclosing_discharged.contains("Raise"));
        assert!(s.enclosing_discharged.contains("State"));
        // Both handles' spans recorded, outermost first.
        assert_eq!(s.enclosing_handle_spans.len(), 2);
    }

    #[test]
    fn recursive_fn_with_mixed_discharge_per_call_site() {
        // Plan body Task 1: "Recursive fn with mixed-discharge call
        // sites — both classifications produced correctly per call
        // site." Here `loop_helper` recurses to itself from two
        // contexts: one inside the Raise handle (FullyDischarged) and
        // one outside (NotDischarged). `main` discharges both
        // entrypoints' Raise rows so the program typechecks.
        let src = r#"
            effect Raise { raise: () -> Int }
            fn loop_helper(n: Int) -> Int ![Raise] {
                match n {
                    0 => perform Raise.raise(),
                    _ => loop_helper(n - 1),
                }
            }
            fn use_inside() -> Int ![] {
                handle loop_helper(5) with {
                    Raise.raise(k) => 99,
                }
            }
            fn use_outside() -> Int ![Raise] {
                loop_helper(3)
            }
            fn main() -> Int ![] {
                let a: Int = use_inside();
                let b: Int = handle use_outside() with {
                    Raise.raise(k) => 0,
                };
                a + b
            }
        "#;
        let da = analyze_src(src);
        // The recursive call site inside `loop_helper`'s body — there
        // are no enclosing handles within `loop_helper` itself, so it
        // classifies as NotDischarged regardless of how the fn was
        // called externally.
        let recursive_site = first_site_in(&da, "loop_helper", "loop_helper");
        assert_eq!(recursive_site.status, DischargeStatus::NotDischarged);
        // External call site inside a handle: FullyDischarged.
        let inside_site = first_site_in(&da, "use_inside", "loop_helper");
        assert_eq!(inside_site.status, DischargeStatus::FullyDischarged);
        // External call site outside any handle: NotDischarged.
        let outside_site = first_site_in(&da, "use_outside", "loop_helper");
        assert_eq!(outside_site.status, DischargeStatus::NotDischarged);
    }

    #[test]
    fn open_row_callee_never_fully_discharged() {
        // Open-row callees are conservatively never FullyDischarged
        // even when every concrete effect in the row is in scope.
        // Tests the explicit row-var guard in `classify`.
        let empty_concrete: BTreeSet<String> = BTreeSet::new();
        let d_c: BTreeSet<String> = ["IO".to_string()].into_iter().collect();
        assert_eq!(
            classify(&empty_concrete, true, &d_c),
            DischargeStatus::NotDischarged
        );
        let raise: BTreeSet<String> = ["Raise".to_string()].into_iter().collect();
        assert_eq!(
            classify(&raise, true, &d_c),
            // Raise ∩ {IO} is empty → NotDischarged even with open row.
            DischargeStatus::NotDischarged
        );
        let io: BTreeSet<String> = ["IO".to_string()].into_iter().collect();
        assert_eq!(
            classify(&io, true, &d_c),
            // IO ∩ {IO} non-empty → PartiallyDischarged because row var
            // can absorb effects we don't know about.
            DischargeStatus::PartiallyDischarged
        );
    }

    #[test]
    fn dump_discharge_is_deterministic_and_includes_summary() {
        let src = r#"
            effect Raise { raise: () -> Int }
            fn helper() -> Int ![Raise] { perform Raise.raise() }
            fn caller() -> Int ![Raise] { helper() }
            fn main() -> Int ![] {
                handle caller() with {
                    Raise.raise(k) => 0,
                }
            }
        "#;
        let da = analyze_src(src);
        let dump = dump_discharge(&da);
        // Summary line is present. Two top-level fn calls exist:
        // `caller -> helper` (no handle in scope inside `caller`) and
        // `main -> caller` (under a Raise handle).
        assert!(dump.contains("# summary:"), "dump missing summary: {dump}");
        assert!(
            dump.contains("2 call site(s)"),
            "dump wrong site count: {dump}"
        );
        assert!(
            dump.contains("1 FullyDischarged"),
            "dump wrong FullyDischarged count: {dump}"
        );
        assert!(
            dump.contains("1 NotDischarged"),
            "dump wrong NotDischarged count: {dump}"
        );
        // Run twice — output must be byte-identical.
        let da2 = analyze_src(src);
        let dump2 = dump_discharge(&da2);
        assert_eq!(dump, dump2);
    }

    #[test]
    fn classify_pure_function_returns_not_discharged() {
        // Defensive unit test for the `classify` pure fn — empty
        // closed row falls into the early-return NotDischarged branch
        // even when D_c is non-empty.
        let empty: BTreeSet<String> = BTreeSet::new();
        let d_c: BTreeSet<String> = ["IO".to_string()].into_iter().collect();
        assert_eq!(
            classify(&empty, false, &d_c),
            DischargeStatus::NotDischarged
        );
    }

    // ===== Real-example inventory checks (Plan E3 Phase 1 gating data)
    //
    // The plan body's Task 2 specifies "golden-file test against
    // examples/state.sigil and examples/choose_demo.sigil outputs."
    // True byte-stable golden files would break every time the
    // example sources are reformatted; instead the tests below assert
    // *count properties* of the analysis output — the Phase-2
    // activation review needs the FullyDischarged-Cps inventory
    // count, and the count is exactly what these tests pin. Lib-level
    // (not e2e) so they run in pod-verify without invoking the sigil
    // binary on a .sigil file (the Cranelift OOM hazard CLAUDE.md
    // calls out).

    /// Resolve `rel` against the workspace root and return its
    /// absolute path. `None` if the path doesn't exist on disk —
    /// callers should treat missing files as "skip" rather than
    /// "fail" so the test surface works when the compiler crate is
    /// consumed outside the cargo workspace (e.g., a future pre-
    /// publish wheel that ships only `compiler/`).
    fn example_path(rel: &str) -> Option<std::path::PathBuf> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("compiler manifest has workspace parent")
            .join(rel);
        if p.exists() {
            Some(p)
        } else {
            None
        }
    }

    fn analyze_example_file(rel: &str) -> DischargeAnalysis {
        let path = example_path(rel).unwrap_or_else(|| {
            panic!(
                "analyze_example_file: `{}` not found relative to workspace root",
                rel
            )
        });
        let src = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!("failed to read {}: {e}", path.display());
        });
        let file_label = path.display().to_string();
        let (tokens, lex_errs) = lexer::lex(&file_label, &src);
        assert!(lex_errs.is_empty(), "lex {rel}: {lex_errs:?}");
        let (prog, parse_errs) = parser::parse(&file_label, &tokens);
        assert!(parse_errs.is_empty(), "parse {rel}: {parse_errs:?}");
        // Mirror pipeline order — imports::resolve sits between parse
        // and resolve::resolve so the stdlib sources land in the
        // program before name resolution runs.
        let (prog, import_errs) = crate::imports::resolve(prog);
        assert!(import_errs.is_empty(), "imports {rel}: {import_errs:?}");
        let (resolved, resolve_errs) = resolve::resolve(prog);
        assert!(resolve_errs.is_empty(), "resolve {rel}: {resolve_errs:?}");
        let (checked, tc_errs) = typecheck::typecheck(resolved.program);
        let hard: Vec<_> = tc_errs
            .iter()
            .filter(|e| matches!(e.severity, crate::errors::Severity::Error))
            .collect();
        assert!(hard.is_empty(), "typecheck {rel}: {hard:?}");
        let anf = elaborate::elaborate(checked);
        let mono = monomorphize(anf);
        let cp = infer_colors(mono);
        analyze(&cp)
    }

    #[test]
    fn state_sigil_inventory_is_analyzable_and_deterministic() {
        // examples/state.sigil's user-program calls go through the
        // stdlib `run_state` wrapper, so most of the discharge action
        // happens inside `std/state.sigil`'s `handle` (not in user
        // code). The user-visible call site is `main -> run_state`.
        // Pin that the analyzer runs to completion + produces a
        // stable dump rather than asserting a specific
        // FullyDischarged count; the Phase 1 review surfaces the
        // actual inventory to the user.
        if example_path("examples/state.sigil").is_none() {
            eprintln!("skipping state_sigil_inventory test: examples/state.sigil not present");
            return;
        }
        let da = analyze_example_file("examples/state.sigil");
        let dump = dump_discharge(&da);
        assert!(
            dump.contains("# summary:"),
            "dump missing summary line: {dump}",
        );
        // Determinism guard.
        let da2 = analyze_example_file("examples/state.sigil");
        assert_eq!(dump, dump_discharge(&da2));
    }

    #[test]
    fn catch_example_has_one_fully_discharged_cps_site() {
        // PR #175 review N1: hard-asserting regression guard against
        // a single stable example. The Phase 1 inventory aggregator
        // (above) asserts only `total_sites > 0`, which lets a
        // future analyzer refactor silently undercount or overcount
        // without surfacing in CI. This test pins one concrete
        // count so a regression that mis-classifies the canonical
        // `Raise.fail` discard-`k` shape from `examples/catch.sigil`
        // breaks the build.
        //
        // `main -> risky` inside `handle risky(7) with { Raise.fail
        // (_msg, k) => 42 }`:
        //   - risky's row is `![Raise[String]]` (closed, single
        //     effect).
        //   - The enclosing handle's op_arms list a Raise.fail arm
        //     → D_c = {Raise}.
        //   - R_f ⊆ D_c → FullyDischarged.
        //   - risky's body is `let result = raise(...); result + input`
        //     — chained-let-yield-then-pure-tail shape; color.rs
        //     classifies risky as Cps.
        //
        // The single FullyDischarged + Cps site is therefore the
        // canonical positive case for Plan E3's intended target.
        if example_path("examples/catch.sigil").is_none() {
            eprintln!("skipping catch_example regression test: examples/catch.sigil not present");
            return;
        }
        let da = analyze_example_file("examples/catch.sigil");
        assert_eq!(
            da.fully_discharged_cps_callees(),
            1,
            "examples/catch.sigil must produce exactly 1 FullyDischarged Cps-color call site; dump=\n{}",
            dump_discharge(&da),
        );
    }

    #[test]
    fn div_recover_example_has_one_fully_discharged_cps_site() {
        // Companion to the catch.sigil regression guard above. The
        // div_recover example uses the typecheck-elaborated
        // `ArithError` perform — same FullyDischarged shape but
        // under a different effect surface. Pinning both gives a
        // regression guard against effect-specific classifier bugs.
        if example_path("examples/div_recover.sigil").is_none() {
            eprintln!(
                "skipping div_recover_example regression test: examples/div_recover.sigil not present"
            );
            return;
        }
        let da = analyze_example_file("examples/div_recover.sigil");
        assert_eq!(
            da.fully_discharged_cps_callees(),
            1,
            "examples/div_recover.sigil must produce exactly 1 FullyDischarged Cps-color call site; dump=\n{}",
            dump_discharge(&da),
        );
    }

    /// Aggregate the Phase-1 activation-gate inventory across the
    /// effect-heavy `examples/*.sigil` set. Printed to stderr so the
    /// `--nocapture` invocation surfaces the totals; pin only that
    /// every example analyzes to completion and the total non-test
    /// site count is non-zero (analyzer reaches user code).
    ///
    /// The Phase 1 review checkpoint reads the eprintln output to
    /// gate Phase 2. The HARD-STOP threshold is < ~10
    /// FullyDischarged-Cps sites across the inventory.
    /// Confirms the analyzer reaches into stdlib fns when they're
    /// transitively pulled into the mono items by a user program's
    /// imports. Without this guarantee, the activation inventory
    /// would systematically undercount discharge in `std/raise.sigil`'s
    /// `catch`, `std/state.sigil`'s `run_state`, etc. — and the Phase
    /// 1 HARD-STOP decision (read off the `Phase 2 activation upper
    /// bound` line) would be unreliable.
    #[test]
    fn analyzer_reaches_stdlib_fns_pulled_in_via_imports() {
        let path = match example_path("examples/json.sigil") {
            Some(p) => p,
            None => {
                eprintln!(
                    "skipping analyzer_reaches_stdlib_fns test: examples/json.sigil not present"
                );
                return;
            }
        };
        let src = std::fs::read_to_string(&path).expect("read examples/json.sigil");
        let file_label = path.display().to_string();
        // Assert no hard errors at every pipeline stage (re-review
        // 4294874724 #2). Earlier shape silently discarded all
        // diagnostics; if json.sigil ever introduces a hard error,
        // the test would feed a broken program into mono and the
        // stdlib-fn-presence heuristic could pass spuriously.
        let (tokens, lex_errs) = lexer::lex(&file_label, &src);
        assert!(lex_errs.is_empty(), "lex json.sigil: {lex_errs:?}");
        let (prog, parse_errs) = parser::parse(&file_label, &tokens);
        assert!(parse_errs.is_empty(), "parse json.sigil: {parse_errs:?}");
        let (prog, import_errs) = crate::imports::resolve(prog);
        assert!(
            import_errs.is_empty(),
            "imports json.sigil: {import_errs:?}"
        );
        let (resolved, resolve_errs) = resolve::resolve(prog);
        assert!(
            resolve_errs.is_empty(),
            "resolve json.sigil: {resolve_errs:?}"
        );
        let (checked, tc_errs) = typecheck::typecheck(resolved.program);
        let hard: Vec<_> = tc_errs
            .iter()
            .filter(|e| matches!(e.severity, crate::errors::Severity::Error))
            .collect();
        assert!(hard.is_empty(), "typecheck json.sigil: {hard:?}");
        let anf = elaborate::elaborate(checked);
        let mono = monomorphize(anf);
        let stdlib_fns: Vec<&str> = mono
            .anf
            .checked
            .program
            .items
            .iter()
            .filter_map(|i| {
                if let crate::ast::Item::Fn(f) = i {
                    Some(f.name.as_str())
                } else {
                    None
                }
            })
            .filter(|n| {
                // Heuristic: stdlib-derived fns include canonical
                // names like `catch`, `run_state`, `sb_*`, `iter_*`
                // when they survive mono. Just check at least one is
                // present.
                matches!(
                    *n,
                    "catch" | "run_state" | "sb_new" | "sb_append" | "sb_finalize"
                ) || n.starts_with("catch$$")
                    || n.starts_with("run_state$$")
            })
            .collect();
        eprintln!(
            "stdlib fns found in json.sigil mono items: {:?}",
            stdlib_fns
        );
        assert!(
            !stdlib_fns.is_empty(),
            "expected at least one stdlib fn (catch, run_state, sb_*) \
             to appear in json.sigil's mono items"
        );
    }

    #[test]
    fn phase_1_activation_inventory_across_examples() {
        // Effect-heavy examples whose user-program contains direct
        // top-level-fn call sites in handle-discharge contexts.
        // `examples/hello.sigil` and arithmetic-only examples are
        // omitted because their call shapes are dominated by builtins
        // (`int_to_string`, `IO.println`) that the analyzer correctly
        // skips (not user-source fns).
        let examples = [
            "examples/state.sigil",
            "examples/choose_demo.sigil",
            "examples/json.sigil",
            "examples/sudoku.sigil",
            "examples/interpreter.sigil",
            "examples/nested_effects.sigil",
            "examples/multishot_perf.sigil",
            "examples/tree_stress_repeat.sigil",
            "examples/catch.sigil",
            "examples/option_demo.sigil",
            "examples/div_recover.sigil",
        ];

        let mut total_sites = 0usize;
        let mut total_full = 0usize;
        let mut total_full_cps = 0usize;
        let mut total_partial = 0usize;
        let mut total_none = 0usize;
        let mut per_file: Vec<(String, usize, usize, usize, usize, usize)> = Vec::new();

        for rel in examples {
            // Use the same `example_path()` helper the per-example
            // tests use — one workspace-relative-path resolver,
            // single skip-if-missing pattern across the file.
            if example_path(rel).is_none() {
                eprintln!("inventory: skipping {} (not present)", rel);
                continue;
            }
            let da = analyze_example_file(rel);
            let sites = da.sites.len();
            let full = da.count(DischargeStatus::FullyDischarged);
            let full_cps = da.fully_discharged_cps_callees();
            let partial = da.count(DischargeStatus::PartiallyDischarged);
            let none = da.count(DischargeStatus::NotDischarged);
            total_sites += sites;
            total_full += full;
            total_full_cps += full_cps;
            total_partial += partial;
            total_none += none;
            per_file.push((rel.to_string(), sites, full, full_cps, partial, none));
        }

        eprintln!("\n===== Phase 1 activation-gate inventory =====");
        eprintln!(
            "{:<48} {:>6} {:>6} {:>10} {:>9} {:>5}",
            "example", "sites", "Full", "Full+Cps", "Partial", "None"
        );
        for (file, sites, full, full_cps, partial, none) in &per_file {
            eprintln!(
                "{:<48} {:>6} {:>6} {:>10} {:>9} {:>5}",
                file, sites, full, full_cps, partial, none
            );
        }
        eprintln!(
            "{:<48} {:>6} {:>6} {:>10} {:>9} {:>5}",
            "TOTAL", total_sites, total_full, total_full_cps, total_partial, total_none
        );
        eprintln!(
            "Phase 2 activation upper bound (FullyDischarged + Cps callee): {}",
            total_full_cps
        );
        eprintln!("HARD-STOP threshold: < ~10 → land Phase 1 only");
        eprintln!("==============================================\n");

        // Soft invariant: every example analyzed produced at least one
        // call site (otherwise the analyzer is failing on real
        // programs).
        assert!(
            total_sites > 0,
            "inventory found zero call sites across {} examples — analyzer is mis-firing",
            per_file.len(),
        );
    }

    #[test]
    fn choose_demo_sigil_inventory_is_parseable_and_analyzable() {
        // examples/choose_demo.sigil exercises the multi-shot `Choose`
        // effect under a `handle` with re-entry into the continuation.
        // Pin that the analyzer runs to completion and reports a
        // deterministic site count; the specific FullyDischarged
        // count is a property the Phase 1 review surfaces to the user
        // rather than a hard assertion (the example's structure can
        // evolve).
        if example_path("examples/choose_demo.sigil").is_none() {
            eprintln!(
                "skipping choose_demo_sigil_inventory test: examples/choose_demo.sigil not present"
            );
            return;
        }
        let da = analyze_example_file("examples/choose_demo.sigil");
        let dump = dump_discharge(&da);
        assert!(
            dump.contains("# summary:"),
            "dump missing summary line: {dump}",
        );
        // Run twice; output must be byte-identical (determinism
        // guard — Plan A1's reproducibility test compares object-file
        // bytes between runs and pins the analyzer's stability).
        let da2 = analyze_example_file("examples/choose_demo.sigil");
        let dump2 = dump_discharge(&da2);
        assert_eq!(dump, dump2);
    }
}
