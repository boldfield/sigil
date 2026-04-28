//! Color inference — Plan B Task 50.
//!
//! After monomorphization, each top-level fn (each "monomorph") is
//! tagged with a [`Color`]:
//!
//! - **[`Color::Native`]**: the function's effect row is `![]` or
//!   `![IO]` (closed; no row variable), it does not `perform` any
//!   non-IO effect, and it transitively calls only other native
//!   monomorphs.
//! - **[`Color::Cps`]**: anything else. The CPS transform (Task 55)
//!   will rewrite these into trampolined continuation-passing form.
//!
//! Color propagates transitively along the monomorph call graph: any
//! monomorph that calls a CPS-color monomorph is itself CPS. The
//! propagation is **SCC-aware**: within a strongly-connected
//! component (a cycle of mutually-recursive monomorphs), the SCC's
//! color is CPS if any member requires CPS, otherwise native. This
//! avoids over-pessimizing one member because of another's unrelated
//! cycle.
//!
//! Lambdas inside a fn body are part of their parent's color: they
//! have not yet been hoisted to top-level (closure conversion runs
//! after color in the pipeline). Their bodies contribute to the
//! parent's `perform` and outgoing-call analysis.
//!
//! ## `--dump-color`
//!
//! [`dump_color`] renders a stable per-monomorph diagnostic line
//! `<name> native|cps <reason>` used by the `--dump-color` CLI flag.
//! Required for diagnosing performance-floor misses in tests and for
//! adversarial review at the Stage 5 checkpoint.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{Block, Expr, FnDecl, Item, MatchArm, PerformExpr, Stmt};
use crate::errors::Span;
use crate::monomorphize::MonoProgram;
use crate::typecheck::GenericInstantiation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Native,
    Cps,
}

impl Color {
    fn label(self) -> &'static str {
        match self {
            Color::Native => "native",
            Color::Cps => "cps",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ColoredProgram {
    pub mono: MonoProgram,
    /// Color per top-level fn, in original program order.
    pub colors: Vec<(String, Color)>,
    /// Explanation per top-level fn, same order as `colors`. The
    /// reason text is stable, machine-readable enough for tests, and
    /// human-readable enough for `--dump-color`.
    pub reasons: Vec<(String, String)>,
}

impl ColoredProgram {
    /// Plan B Task 55, Phase 4e — does the user fn named `name` need
    /// CPS-form codegen treatment?
    ///
    /// At HEAD this is equivalent to "is `name` CPS-color?", because
    /// the [`Color::Cps`] classification is exactly the set of fns
    /// that need CPS calling convention + CPS-aware body lowering.
    /// Future Phase 4e commits may refine the per-fn decision (e.g.,
    /// a CPS-color fn whose body has no perform and no CPS-call
    /// could in principle be emitted as native; Plan B treats this
    /// as a v2 optimization), but at HEAD the answer matches color
    /// directly.
    ///
    /// Returns `false` for fns not in the colored program (no panic).
    /// The codegen entry walker is the source of truth for which fns
    /// reach codegen; this accessor is a query, not an assertion.
    ///
    /// **Consumer contract.** Consumers driven by per-fn ABI
    /// selection should iterate [`Self::cps_color_user_fns`] directly
    /// (e.g., `for name in colored.cps_color_user_fns()` then look
    /// up the FuncId keyed by `name`), not query `needs_cps_transform`
    /// with a name harvested from an AST walk. The latter pattern
    /// allows a typo to silently classify an unknown name as Native
    /// (since this method returns `false` for unknown fns by design).
    /// The codegen-consumes-color commit follows the iterate-the-list
    /// pattern.
    pub fn needs_cps_transform(&self, name: &str) -> bool {
        self.colors
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, c)| matches!(c, Color::Cps))
            .unwrap_or(false)
    }

    /// Plan B Task 55, Phase 4e — list CPS-color user fn names in
    /// program declaration order (matching the order in which they
    /// appear in [`Self::colors`], which is the post-monomorphization
    /// program order from `monomorphize::run`).
    ///
    /// Used by the codegen-consumes-color commit (next on this branch)
    /// to iterate the fns that need CPS calling convention. Stable
    /// ordering matters for reproducibility (Plan A1's reproducibility
    /// test compares object-file bytes between runs); the underlying
    /// `colors` Vec preserves program declaration order
    /// (`color.rs::infer_colors` iterates
    /// `mono.anf.checked.program.items`), so this accessor preserves
    /// that. Source declaration order is stronger than alphabetical
    /// — a future refactor that silently swaps to a BTreeMap-derived
    /// order (alphabetical-only) would change reproducibility-relevant
    /// byte sequences without the colorer's tests catching it.
    pub fn cps_color_user_fns(&self) -> Vec<String> {
        self.colors
            .iter()
            .filter_map(|(name, c)| {
                if matches!(c, Color::Cps) {
                    Some(name.clone())
                } else {
                    None
                }
            })
            .collect()
    }
}

/// The only effect treated as "pure" for native classification in
/// Plan B v1. Post-Task-57, IO is a normal registry-driven effect
/// at typecheck and codegen — it routes through `sigil_perform`
/// like every other effect — but the colorer keeps it special-cased
/// here as a perf-preserving choice: the top-level IO handler is
/// always installed by the `main` shim, and `lower_perform_to_value`
/// wraps `sigil_perform` synchronously, so IO-only fns can stay
/// Native-color without correctness loss. Lifting this constant
/// would force every fn doing `perform IO.println(...)` into the
/// CPS-ABI with trampoline overhead per println call. Anything else
/// in the row makes the monomorph CPS.
const NATIVE_EFFECT: &str = "IO";

/// Local (pre-propagation) classification of a single fn.
#[derive(Clone, Debug)]
enum LocalColor {
    /// Locally pure — eligible for native unless a transitive callee
    /// drags us into CPS. Carries the eventual "native" reason text.
    Native(String),
    /// Locally CPS for an intrinsic reason on this fn alone. Carries
    /// the reason text.
    Cps(String),
}

pub fn infer_colors(mono: MonoProgram) -> ColoredProgram {
    // -------- Step 1: collect fns in program order --------
    let mut fn_names: Vec<String> = Vec::new();
    let mut fn_index: BTreeMap<String, usize> = BTreeMap::new();
    let mut fn_decls: Vec<&FnDecl> = Vec::new();
    for item in &mono.anf.checked.program.items {
        if let Item::Fn(f) = item {
            let idx = fn_names.len();
            fn_names.push(f.name.clone());
            fn_index.insert(f.name.clone(), idx);
            fn_decls.push(f.as_ref());
        }
    }
    let n = fn_names.len();

    // -------- Step 2: local analysis --------
    let mut local: Vec<LocalColor> = Vec::with_capacity(n);
    for f in &fn_decls {
        local.push(local_color(f));
    }

    // -------- Step 3: build call graph (caller idx -> set of callee idxs) --------
    // Edges are driven by `CheckedProgram::call_site_instantiations`,
    // a span-keyed map populated by the typechecker for every Ident
    // (call-position or value-position) that resolved to a top-level
    // fn under the env-precedence rules at that span. Locals that
    // shadow a top-level fn name are not in the map (the typechecker's
    // `env.get` wins before `fn_schemes.get`), so this drive is
    // precise — a body that references a parameter or `let`-binding
    // that happens to share a name with a top-level fn does not get
    // a spurious edge.
    //
    // Edges still cover both call-position (`Call { callee: Ident }`)
    // and value-position references (`let f = some_fn`) because
    // typecheck records both; that preserves the soundness claim that
    // a parent binding a CPS fn as a value is itself CPS.
    //
    // Lambda bodies are walked: closure conversion runs after color,
    // so nested lambdas are still part of their parent fn's edge set.
    let calls = &mono.anf.checked.call_site_instantiations;
    let mut edges: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];
    for (i, f) in fn_decls.iter().enumerate() {
        let mut out: BTreeSet<usize> = BTreeSet::new();
        collect_calls_in_block(&f.body, &fn_index, calls, &mut out);
        edges[i] = out;
    }

    // -------- Step 4: Tarjan SCC --------
    // Iterative; reverse-topological order (sinks first); within-SCC
    // node order is ascending.
    let (sccs, scc_of) = tarjan_scc(n, &edges);

    // -------- Step 5: propagate CPS color across SCCs --------
    let mut scc_color: Vec<Color> = vec![Color::Native; sccs.len()];
    let mut node_reason: Vec<String> = vec![String::new(); n];

    for (scc_idx, scc) in sccs.iter().enumerate() {
        // (a) First intrinsic-CPS member by program order.
        let mut intrinsic_member: Option<usize> = None;
        for &node in scc {
            if matches!(local[node], LocalColor::Cps(_)) {
                intrinsic_member = Some(node);
                break;
            }
        }

        // (b) Per-node bridge-callee map. Always computed because
        // the per-node reason loop below uses it to assign bridge-
        // form reasons to non-intrinsic SCC members regardless of
        // whether the SCC has an intrinsic CPS member. The walk is
        // O(edges_out_of_scc); pure leaf SCCs naturally hit the
        // empty case without an explicit skip.
        let mut bridge_callee_of: BTreeMap<usize, usize> = BTreeMap::new();
        for &node in scc {
            for &callee in &edges[node] {
                let callee_scc = scc_of[callee];
                if callee_scc == scc_idx {
                    continue;
                }
                if scc_color[callee_scc] == Color::Cps {
                    bridge_callee_of.entry(node).or_insert(callee);
                }
            }
        }
        // First bridge member, by program order.
        let mut bridge_member: Option<usize> = None;
        for &node in scc {
            if bridge_callee_of.contains_key(&node) {
                bridge_member = Some(node);
                break;
            }
        }

        // (c) SCC color: CPS if any intrinsic OR any bridge.
        let scc_is_cps = intrinsic_member.is_some() || bridge_member.is_some();
        if !scc_is_cps {
            scc_color[scc_idx] = Color::Native;
            for &node in scc {
                if let LocalColor::Native(r) = &local[node] {
                    node_reason[node] = r.clone();
                }
            }
            continue;
        }
        scc_color[scc_idx] = Color::Cps;

        // (d) Per-node reason: each node's *own* most-proximate cause.
        for &node in scc {
            // Intrinsic locally-CPS members keep their specific reason.
            if let LocalColor::Cps(r) = &local[node] {
                node_reason[node] = r.clone();
                continue;
            }
            // Bridge members reference their actual outgoing callee.
            if let Some(&callee) = bridge_callee_of.get(&node) {
                node_reason[node] = format!(
                    "cps: transitively calls `{}` which is cps",
                    fn_names[callee]
                );
                continue;
            }
            // Non-intrinsic, non-bridge member: pulled into CPS by SCC
            // membership only. Distinguish "propagated through an
            // intrinsic peer" from "propagated through a bridge peer"
            // so the `--dump-color` user can follow the causal chain.
            if let Some(intrinsic) = intrinsic_member {
                node_reason[node] = format!(
                    "cps: in SCC with intrinsically-cps member `{}`",
                    fn_names[intrinsic]
                );
            } else if let Some(bridge) = bridge_member {
                node_reason[node] = format!(
                    "cps: in SCC bridging to cps callee via `{}`",
                    fn_names[bridge]
                );
            } else {
                // Cannot happen: scc_is_cps requires at least one of
                // intrinsic_member / bridge_member to be Some.
                unreachable!("color: SCC marked CPS but has neither intrinsic nor bridge member");
            }
        }
    }

    // -------- Step 6: assemble outputs --------
    let mut colors: Vec<(String, Color)> = Vec::with_capacity(n);
    let mut reasons: Vec<(String, String)> = Vec::with_capacity(n);
    for (i, name) in fn_names.iter().enumerate() {
        let scc_idx = scc_of[i];
        colors.push((name.clone(), scc_color[scc_idx]));
        reasons.push((name.clone(), node_reason[i].clone()));
    }

    ColoredProgram {
        mono,
        colors,
        reasons,
    }
}

/// Local analysis on a single fn: classify based on its declared row,
/// any explicit row variable, and any non-IO `perform` operation in
/// its body (or any nested lambda body). Independent of the call
/// graph; transitive CPS is layered on top in the propagation phase.
fn local_color(f: &FnDecl) -> LocalColor {
    // (1) Open row → CPS. An explicit row variable can absorb arbitrary
    // effects at unification time, so we cannot prove the function is
    // pure post-monomorph. Plan B Stage 5 keeps row vars in the IR; the
    // Stage 6 effect runtime is the only mechanism that could discharge
    // them. Treat them as CPS to stay safe.
    if f.effect_row_var.is_some() {
        return LocalColor::Cps("cps: open effect row".to_string());
    }

    // (2) Any non-IO effect in the closed row → CPS. `![IO]` and `![]`
    // are the two acceptable native shapes per Plan B's color spec.
    for e in &f.effects {
        if e != NATIVE_EFFECT {
            return LocalColor::Cps(format!("cps: row contains effect `{e}`"));
        }
    }

    // (3) Walk the body for any `perform` of a non-IO effect. Even when
    // the row is `![IO]`, a body that performs a non-IO effect would
    // have been rejected by typecheck (E0042); but we still want to
    // walk so the analysis is robust against synthetic monomorphs in
    // tests and against future changes that loosen the typecheck rule.
    if let Some(non_io) = find_non_io_perform_in_block(&f.body) {
        return LocalColor::Cps(format!("cps: performs `{}.{}`", non_io.0, non_io.1));
    }

    // (4) Default native — pending the propagation pass.
    LocalColor::Native(if f.effects.is_empty() {
        "native: pure row".to_string()
    } else {
        "native: row is `![IO]`".to_string()
    })
}

/// Walk a block's stmts and tail, descending into every nested
/// expression, for the first `perform <effect>.<op>` whose effect is
/// not `IO`. Returns `(effect, op)` of the first match found.
fn find_non_io_perform_in_block(b: &Block) -> Option<(String, String)> {
    for s in &b.stmts {
        match s {
            Stmt::Let(l) => {
                if let Some(p) = find_non_io_perform_in_expr(&l.value) {
                    return Some(p);
                }
            }
            Stmt::Expr(e) => {
                if let Some(p) = find_non_io_perform_in_expr(e) {
                    return Some(p);
                }
            }
            Stmt::Perform(p) => {
                if p.effect != NATIVE_EFFECT {
                    return Some((p.effect.clone(), p.op.clone()));
                }
                for a in &p.args {
                    if let Some(q) = find_non_io_perform_in_expr(a) {
                        return Some(q);
                    }
                }
            }
        }
    }
    if let Some(t) = &b.tail {
        if let Some(p) = find_non_io_perform_in_expr(t) {
            return Some(p);
        }
    }
    None
}

fn find_non_io_perform_in_expr(e: &Expr) -> Option<(String, String)> {
    match e {
        Expr::IntLit(_, _)
        | Expr::StringLit(_, _)
        | Expr::BoolLit(_, _)
        | Expr::CharLit(_, _)
        | Expr::Ident(_, _)
        | Expr::ClosureEnvLoad { .. } => None,
        Expr::Call { callee, args, .. } => {
            if let Some(p) = find_non_io_perform_in_expr(callee) {
                return Some(p);
            }
            for a in args {
                if let Some(p) = find_non_io_perform_in_expr(a) {
                    return Some(p);
                }
            }
            None
        }
        Expr::Perform(p) => find_non_io_perform_in_perform(p),
        Expr::Binary { lhs, rhs, .. } => {
            find_non_io_perform_in_expr(lhs).or_else(|| find_non_io_perform_in_expr(rhs))
        }
        Expr::Unary { operand, .. } => find_non_io_perform_in_expr(operand),
        Expr::If {
            cond,
            then_block,
            else_block,
            ..
        } => find_non_io_perform_in_expr(cond)
            .or_else(|| find_non_io_perform_in_block(then_block))
            .or_else(|| find_non_io_perform_in_block(else_block)),
        Expr::Match {
            scrutinee, arms, ..
        } => {
            if let Some(p) = find_non_io_perform_in_expr(scrutinee) {
                return Some(p);
            }
            for MatchArm { body, .. } in arms {
                if let Some(p) = find_non_io_perform_in_expr(body) {
                    return Some(p);
                }
            }
            None
        }
        Expr::Block(b) => find_non_io_perform_in_block(b),
        Expr::Lambda { body, .. } => find_non_io_perform_in_expr(body),
        Expr::ClosureRecord { env_exprs, .. } => {
            for ex in env_exprs {
                if let Some(p) = find_non_io_perform_in_expr(ex) {
                    return Some(p);
                }
            }
            None
        }
        Expr::RecordLit { fields, .. } => {
            for f in fields {
                if let Some(p) = find_non_io_perform_in_expr(&f.value) {
                    return Some(p);
                }
            }
            None
        }
        // Plan B task 53 — handler expressions don't taint the
        // surrounding fn's color: a `handle <body> with { ... }`
        // *discharges* effects from the body's row at the type-
        // checker level (Task 54), so for color purposes the body's
        // performs are scoped to the handler. We still recurse into
        // the arm bodies to surface their own intrinsic-CPS triggers
        // (an arm body that itself performs an unhandled non-IO
        // effect would taint the enclosing fn).
        //
        // **Phase coverage** (Task 55 in flight): this rule is
        // correct for the codegen-entry guard's currently-supported
        // subset — synchronous, single-shot, `IntLit`-only arm
        // bodies, no `k` usage. Phase 4d (k-using arms +
        // continuation reification) makes the rule load-bearing in
        // a stronger sense: an arm that reifies `k` represents the
        // remainder of computation after the perform, and the
        // colorer must agree with codegen on which monomorphs sit
        // at the native↔CPS boundary.
        //
        // See `PLAN_B_DEVIATIONS.md` entries:
        //   - `[DEVIATION Task 55] Native callers drive sigil_run_loop
        //     synchronously` (PB4) — names Phase 4d as the closure
        //     point for the colorer's handler-discharge refinement.
        //     Today's synchronous `lower_perform_non_io_to_value`
        //     path works precisely because this stub treats handle
        //     bodies as inert from the parent fn's color
        //     perspective; lifting the synchronous-blocking shape
        //     in Phase 4d requires this stub to refine at the same
        //     time.
        //
        // Replacing this stub with the proper handler-context color
        // rule (PR #18 reviewer's Stage 6 ask) is the closing
        // dependency of Phase 4d. Until then: walk arm bodies (and
        // the optional return arm), skip the wrapped body itself.
        Expr::Handle {
            return_arm,
            op_arms,
            ..
        } => {
            if let Some(ra) = return_arm {
                if let Some(p) = find_non_io_perform_in_expr(&ra.body) {
                    return Some(p);
                }
            }
            for arm in op_arms {
                if let Some(p) = find_non_io_perform_in_expr(&arm.body) {
                    return Some(p);
                }
            }
            None
        }
    }
}

fn find_non_io_perform_in_perform(p: &PerformExpr) -> Option<(String, String)> {
    if p.effect != NATIVE_EFFECT {
        return Some((p.effect.clone(), p.op.clone()));
    }
    for a in &p.args {
        if let Some(q) = find_non_io_perform_in_expr(a) {
            return Some(q);
        }
    }
    None
}

/// Walk `b` and its nested expressions for top-level-fn references
/// (call-position or value-position). An `Expr::Ident(name, span)`
/// becomes an outgoing edge iff `calls.contains_key(span)` — i.e.,
/// typecheck recorded it as a top-level-fn reference under the
/// env-precedence rules at that span. Locals shadowing a top-level
/// name are not in the map and produce no edge.
fn collect_calls_in_block(
    b: &Block,
    fn_index: &BTreeMap<String, usize>,
    calls: &BTreeMap<Span, GenericInstantiation>,
    out: &mut BTreeSet<usize>,
) {
    for s in &b.stmts {
        match s {
            Stmt::Let(l) => collect_calls_in_expr(&l.value, fn_index, calls, out),
            Stmt::Expr(e) => collect_calls_in_expr(e, fn_index, calls, out),
            Stmt::Perform(p) => {
                for a in &p.args {
                    collect_calls_in_expr(a, fn_index, calls, out);
                }
            }
        }
    }
    if let Some(t) = &b.tail {
        collect_calls_in_expr(t, fn_index, calls, out);
    }
}

fn collect_calls_in_expr(
    e: &Expr,
    fn_index: &BTreeMap<String, usize>,
    calls: &BTreeMap<Span, GenericInstantiation>,
    out: &mut BTreeSet<usize>,
) {
    match e {
        Expr::IntLit(_, _)
        | Expr::StringLit(_, _)
        | Expr::BoolLit(_, _)
        | Expr::CharLit(_, _)
        | Expr::ClosureEnvLoad { .. } => {}
        Expr::Ident(name, span) => {
            if calls.contains_key(span) {
                if let Some(&idx) = fn_index.get(name) {
                    out.insert(idx);
                }
            }
        }
        Expr::Call { callee, args, .. } => {
            // The callee Ident is handled by recursing — its span
            // resolves through `calls`. Non-Ident callees fall
            // through to recursive descent (no static target).
            collect_calls_in_expr(callee, fn_index, calls, out);
            for a in args {
                collect_calls_in_expr(a, fn_index, calls, out);
            }
        }
        Expr::Perform(p) => {
            for a in &p.args {
                collect_calls_in_expr(a, fn_index, calls, out);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_calls_in_expr(lhs, fn_index, calls, out);
            collect_calls_in_expr(rhs, fn_index, calls, out);
        }
        Expr::Unary { operand, .. } => collect_calls_in_expr(operand, fn_index, calls, out),
        Expr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            collect_calls_in_expr(cond, fn_index, calls, out);
            collect_calls_in_block(then_block, fn_index, calls, out);
            collect_calls_in_block(else_block, fn_index, calls, out);
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            collect_calls_in_expr(scrutinee, fn_index, calls, out);
            for MatchArm { body, .. } in arms {
                collect_calls_in_expr(body, fn_index, calls, out);
            }
        }
        Expr::Block(b) => collect_calls_in_block(b, fn_index, calls, out),
        Expr::Lambda { body, .. } => collect_calls_in_expr(body, fn_index, calls, out),
        Expr::ClosureRecord { env_exprs, .. } => {
            for ex in env_exprs {
                collect_calls_in_expr(ex, fn_index, calls, out);
            }
        }
        Expr::RecordLit { fields, .. } => {
            for f in fields {
                collect_calls_in_expr(&f.value, fn_index, calls, out);
            }
        }
        // Plan B task 53 — handler expressions: each arm body's
        // calls contribute to the enclosing fn's call graph just like
        // any other compound expression. The wrapped body's calls
        // also contribute (they execute under the handler).
        Expr::Handle {
            body,
            return_arm,
            op_arms,
            ..
        } => {
            collect_calls_in_expr(body, fn_index, calls, out);
            if let Some(ra) = return_arm {
                collect_calls_in_expr(&ra.body, fn_index, calls, out);
            }
            for arm in op_arms {
                collect_calls_in_expr(&arm.body, fn_index, calls, out);
            }
        }
    }
}

/// Tarjan's SCC, iterative. Returns `(sccs, scc_of)` where `sccs[i]`
/// is a vector of node indices in SCC `i` (sorted ascending for
/// determinism within each SCC) and `scc_of[node]` is the SCC index
/// of `node`. SCCs are emitted in reverse-topological order — sinks
/// first — which is exactly what the propagation pass needs.
///
/// Why iterative: the recursive form's stack frames are bounded by
/// the longest call chain in the monomorph graph. Plan B's typeclass
/// dictionary specialization will grow that bound; conversion now
/// removes the stack-overflow risk before user-visible programs hit
/// it. Tested against the recursive form's behavior on every existing
/// test case before swap.
fn tarjan_scc(n: usize, edges: &[BTreeSet<usize>]) -> (Vec<Vec<usize>>, Vec<usize>) {
    // Per-node bookkeeping.
    let mut index_of: Vec<Option<usize>> = vec![None; n];
    let mut lowlink: Vec<usize> = vec![0; n];
    let mut on_stack: Vec<bool> = vec![false; n];
    // Tarjan's value stack — nodes on the path from a root to the
    // current frontier. Distinct from the recursion-replacement
    // `work` stack below.
    let mut tj_stack: Vec<usize> = Vec::new();
    let mut sccs: Vec<Vec<usize>> = Vec::new();
    let mut scc_of: Vec<usize> = vec![0; n];
    let mut index_counter: usize = 0;

    // Each work-stack frame represents one in-flight `strongconnect`.
    // `neighbors` is the materialized neighbor list (sorted ascending
    // because it came from a BTreeSet); `next` is the index into
    // `neighbors` of the next neighbor to process. When `next ==
    // neighbors.len()`, the frame finalizes (SCC root check, pop).
    struct Frame {
        v: usize,
        neighbors: Vec<usize>,
        next: usize,
    }
    let mut work: Vec<Frame> = Vec::new();

    let push_frame = |v: usize,
                      index_counter: &mut usize,
                      index_of: &mut [Option<usize>],
                      lowlink: &mut [usize],
                      on_stack: &mut [bool],
                      tj_stack: &mut Vec<usize>,
                      work: &mut Vec<Frame>| {
        index_of[v] = Some(*index_counter);
        lowlink[v] = *index_counter;
        *index_counter += 1;
        tj_stack.push(v);
        on_stack[v] = true;
        let neighbors: Vec<usize> = edges[v].iter().copied().collect();
        work.push(Frame {
            v,
            neighbors,
            next: 0,
        });
    };

    for start in 0..n {
        if index_of[start].is_some() {
            continue;
        }
        push_frame(
            start,
            &mut index_counter,
            &mut index_of,
            &mut lowlink,
            &mut on_stack,
            &mut tj_stack,
            &mut work,
        );

        while let Some(top) = work.last_mut() {
            let v = top.v;
            if top.next < top.neighbors.len() {
                let w = top.neighbors[top.next];
                top.next += 1;
                if index_of[w].is_none() {
                    // Descend into w as a fresh frame. Lowlink min
                    // with w's lowlink will fold in when w finishes.
                    push_frame(
                        w,
                        &mut index_counter,
                        &mut index_of,
                        &mut lowlink,
                        &mut on_stack,
                        &mut tj_stack,
                        &mut work,
                    );
                } else if on_stack[w] {
                    // Back-edge / cross-edge to an on-stack node.
                    // Tarjan's original paper: use w's *index*, not
                    // its lowlink, to ensure SCC root identification.
                    let w_index = match index_of[w] {
                        Some(i) => i,
                        None => unreachable!("Tarjan invariant: on_stack implies indexed"),
                    };
                    if w_index < lowlink[v] {
                        lowlink[v] = w_index;
                    }
                }
                continue;
            }

            // No more neighbors — finalize this frame. First check
            // SCC root status, then pop and propagate `lowlink[v]`
            // into the parent frame's `lowlink[parent]`.
            let v_index = match index_of[v] {
                Some(i) => i,
                None => unreachable!("Tarjan invariant: just-set on entry"),
            };
            if lowlink[v] == v_index {
                let mut comp: Vec<usize> = Vec::new();
                loop {
                    let w = match tj_stack.pop() {
                        Some(w) => w,
                        None => unreachable!("Tarjan invariant: SCC root must be on stack"),
                    };
                    on_stack[w] = false;
                    comp.push(w);
                    if w == v {
                        break;
                    }
                }
                comp.sort();
                let scc_idx = sccs.len();
                for &node in &comp {
                    scc_of[node] = scc_idx;
                }
                sccs.push(comp);
            }

            let v_lowlink = lowlink[v];
            work.pop();
            if let Some(parent) = work.last_mut() {
                if v_lowlink < lowlink[parent.v] {
                    lowlink[parent.v] = v_lowlink;
                }
            }
        }
    }

    (sccs, scc_of)
}

/// Render a colored program for `--dump-color`. One line per
/// monomorph in original program order:
///
/// ```text
/// <name> native|cps <reason>
/// ```
///
/// Stable across compiler runs (program order is the AST item order;
/// reasons are deterministic).
pub fn dump_color(cp: &ColoredProgram) -> String {
    let mut out = String::new();
    for ((name, color), (_, reason)) in cp.colors.iter().zip(cp.reasons.iter()) {
        out.push_str(name);
        out.push(' ');
        out.push_str(color.label());
        out.push(' ');
        out.push_str(reason);
        out.push('\n');
    }
    out
}

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;
    use crate::elaborate;
    use crate::lexer;
    use crate::monomorphize::monomorphize;
    use crate::parser;
    use crate::resolve;
    use crate::typecheck;

    fn color_from_src(src: &str) -> ColoredProgram {
        let file = "test.sigil";
        let (tokens, lex_errs) = lexer::lex(file, src);
        assert!(lex_errs.is_empty(), "lex: {lex_errs:?}");
        let (prog, parse_errs) = parser::parse(file, &tokens);
        assert!(parse_errs.is_empty(), "parse: {parse_errs:?}");
        let (resolved, resolve_errs) = resolve::resolve(prog);
        assert!(resolve_errs.is_empty(), "resolve: {resolve_errs:?}");
        let (checked, tc_errs) = typecheck::typecheck(resolved.program);
        let hard_errs: Vec<_> = tc_errs
            .iter()
            .filter(|e| matches!(e.severity, crate::errors::Severity::Error))
            .collect();
        assert!(hard_errs.is_empty(), "typecheck: {hard_errs:?}");
        let anf = elaborate::elaborate(checked);
        let mono = monomorphize(anf);
        infer_colors(mono)
    }

    fn color_of(cp: &ColoredProgram, name: &str) -> Color {
        cp.colors
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, c)| *c)
            .unwrap_or_else(|| panic!("no fn `{name}` in colors"))
    }

    fn reason_of<'a>(cp: &'a ColoredProgram, name: &str) -> &'a str {
        cp.reasons
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, r)| r.as_str())
            .unwrap_or_else(|| panic!("no fn `{name}` in reasons"))
    }

    #[test]
    fn empty_row_is_native() {
        let src = r#"
            fn main() -> Int ![] { 42 }
        "#;
        let cp = color_from_src(src);
        assert_eq!(color_of(&cp, "main"), Color::Native);
        assert_eq!(reason_of(&cp, "main"), "native: pure row");
    }

    #[test]
    fn io_only_row_is_native() {
        let src = r#"
            import std.io
            fn main() -> Int ![IO] {
                perform IO.println("hi");
                0
            }
        "#;
        let cp = color_from_src(src);
        assert_eq!(color_of(&cp, "main"), Color::Native);
        assert_eq!(reason_of(&cp, "main"), "native: row is `![IO]`");
    }

    #[test]
    fn caller_callee_both_native() {
        let src = r#"
            fn helper(n: Int) -> Int ![] { n + 1 }
            fn main() -> Int ![] { helper(41) }
        "#;
        let cp = color_from_src(src);
        assert_eq!(color_of(&cp, "main"), Color::Native);
        assert_eq!(color_of(&cp, "helper"), Color::Native);
    }

    #[test]
    fn dump_color_is_stable_per_program_order() {
        let src = r#"
            fn helper(n: Int) -> Int ![] { n + 1 }
            fn main() -> Int ![] { helper(41) }
        "#;
        let cp = color_from_src(src);
        let dump = dump_color(&cp);
        // Program order: helper first, then main. The format is
        // `<name> <color> <reason>\n`; reason is `native: pure row` for
        // both, which produces the literal "native native: pure row"
        // double-token. Plan B explicitly specifies this format —
        // see the Task 50 long-form description.
        assert_eq!(
            dump,
            "helper native native: pure row\nmain native native: pure row\n",
        );
    }

    // ---------------- synthetic-program tests for CPS classifications.
    //
    // These cases cannot pass typecheck today (Stage 5 only knows the
    // `IO` effect), so we construct `MonoProgram`s directly to
    // exercise `infer_colors`.

    use crate::ast::{Block as AstBlock, FnDecl, HandleOpArm, Item, Param, Program, TypeExpr};
    use crate::elaborate::AnfProgram;
    use crate::errors::Span;
    use crate::monomorphize::MonoProgram;
    use crate::typecheck::CheckedProgram;
    use std::collections::BTreeMap;

    fn span() -> Span {
        Span::new("test.sigil", 1, 1, 1, 1)
    }

    /// Test-only span generator producing a fresh unique span per
    /// call. Used by tests that need to disambiguate individual
    /// `Expr::Ident` occurrences via their span — specifically the
    /// shadowing-precision tests which model env-precedence by
    /// excluding specific Ident spans from the synthetic
    /// `call_site_instantiations` map. The constant `span()` helper
    /// above suffices for all other synth tests because they don't
    /// care which Ident has which span; only this family needs
    /// uniqueness.
    fn unique_span() -> Span {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(1);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        Span::new("test.sigil", n, 1, n, 2)
    }

    fn empty_block() -> AstBlock {
        AstBlock {
            stmts: Vec::new(),
            tail: Some(Expr::IntLit(0, span())),
            span: span(),
        }
    }

    /// Build a no-arg `() -> Int <effects> { 0 }` function. Synthetic
    /// — bypasses the front end so we can exercise non-IO effects
    /// that current typecheck rejects.
    fn synth_fn(name: &str, effects: Vec<&str>, body: AstBlock) -> Item {
        Item::Fn(Box::new(FnDecl {
            name: name.to_string(),
            name_span: span(),
            generic_params: Vec::new(),
            params: Vec::new(),
            return_type: TypeExpr::Named("Int".to_string(), span()),
            effects: effects.into_iter().map(|s| s.to_string()).collect(),
            effect_row_var: None,
            body,
            span: span(),
        }))
    }

    fn synth_fn_with_open_row(name: &str) -> Item {
        Item::Fn(Box::new(FnDecl {
            name: name.to_string(),
            name_span: span(),
            generic_params: Vec::new(),
            params: Vec::new(),
            return_type: TypeExpr::Named("Int".to_string(), span()),
            effects: vec!["IO".to_string()],
            effect_row_var: Some(crate::ast::RowVar {
                name: "e".to_string(),
                span: span(),
            }),
            body: empty_block(),
            span: span(),
        }))
    }

    /// Walk items collecting every `Expr::Ident(name, span)` whose
    /// name matches a top-level fn — this is what the typechecker
    /// would have recorded in `call_site_instantiations`. Lets
    /// synthetic-program tests exercise edge insertion through the
    /// span-keyed call map without going through typecheck.
    fn build_synthetic_calls_map(items: &[Item]) -> BTreeMap<Span, GenericInstantiation> {
        let mut fn_names: BTreeSet<String> = BTreeSet::new();
        for item in items {
            if let Item::Fn(f) = item {
                fn_names.insert(f.name.clone());
            }
        }
        let mut out: BTreeMap<Span, GenericInstantiation> = BTreeMap::new();
        for item in items {
            if let Item::Fn(f) = item {
                walk_block_for_fn_idents(&f.body, &fn_names, &mut out);
            }
        }
        out
    }

    fn walk_block_for_fn_idents(
        b: &AstBlock,
        fn_names: &BTreeSet<String>,
        out: &mut BTreeMap<Span, GenericInstantiation>,
    ) {
        for s in &b.stmts {
            match s {
                Stmt::Let(l) => walk_expr_for_fn_idents(&l.value, fn_names, out),
                Stmt::Expr(e) => walk_expr_for_fn_idents(e, fn_names, out),
                Stmt::Perform(p) => {
                    for a in &p.args {
                        walk_expr_for_fn_idents(a, fn_names, out);
                    }
                }
            }
        }
        if let Some(t) = &b.tail {
            walk_expr_for_fn_idents(t, fn_names, out);
        }
    }

    fn walk_expr_for_fn_idents(
        e: &Expr,
        fn_names: &BTreeSet<String>,
        out: &mut BTreeMap<Span, GenericInstantiation>,
    ) {
        match e {
            Expr::Ident(name, span) => {
                if fn_names.contains(name) {
                    out.insert(
                        span.clone(),
                        GenericInstantiation {
                            name: name.clone(),
                            type_args: Vec::new(),
                        },
                    );
                }
            }
            Expr::Call { callee, args, .. } => {
                walk_expr_for_fn_idents(callee, fn_names, out);
                for a in args {
                    walk_expr_for_fn_idents(a, fn_names, out);
                }
            }
            Expr::Perform(p) => {
                for a in &p.args {
                    walk_expr_for_fn_idents(a, fn_names, out);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                walk_expr_for_fn_idents(lhs, fn_names, out);
                walk_expr_for_fn_idents(rhs, fn_names, out);
            }
            Expr::Unary { operand, .. } => walk_expr_for_fn_idents(operand, fn_names, out),
            Expr::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                walk_expr_for_fn_idents(cond, fn_names, out);
                walk_block_for_fn_idents(then_block, fn_names, out);
                walk_block_for_fn_idents(else_block, fn_names, out);
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                walk_expr_for_fn_idents(scrutinee, fn_names, out);
                for MatchArm { body, .. } in arms {
                    walk_expr_for_fn_idents(body, fn_names, out);
                }
            }
            Expr::Block(b) => walk_block_for_fn_idents(b, fn_names, out),
            Expr::Lambda { body, .. } => walk_expr_for_fn_idents(body, fn_names, out),
            Expr::ClosureRecord { env_exprs, .. } => {
                for ex in env_exprs {
                    walk_expr_for_fn_idents(ex, fn_names, out);
                }
            }
            Expr::RecordLit { fields, .. } => {
                for f in fields {
                    walk_expr_for_fn_idents(&f.value, fn_names, out);
                }
            }
            Expr::IntLit(_, _)
            | Expr::StringLit(_, _)
            | Expr::BoolLit(_, _)
            | Expr::CharLit(_, _)
            | Expr::ClosureEnvLoad { .. } => {}
            Expr::Handle {
                body,
                return_arm,
                op_arms,
                ..
            } => {
                walk_expr_for_fn_idents(body, fn_names, out);
                if let Some(ra) = return_arm {
                    walk_expr_for_fn_idents(&ra.body, fn_names, out);
                }
                for arm in op_arms {
                    walk_expr_for_fn_idents(&arm.body, fn_names, out);
                }
            }
        }
    }

    fn synth_program(items: Vec<Item>) -> MonoProgram {
        let calls = build_synthetic_calls_map(&items);
        synth_program_with_calls(items, calls)
    }

    /// Build a synthetic `MonoProgram` with a caller-supplied
    /// `call_site_instantiations` map. The auto-built map produced
    /// by `build_synthetic_calls_map` matches every Ident whose name
    /// is a top-level fn — that's a name-only heuristic, not
    /// env-precedence-aware. Tests that need to model env precedence
    /// (a parameter or `let` binding shadowing a top-level fn name)
    /// supply their own map: include the spans typecheck *would*
    /// have recorded under env-precedence rules, exclude the rest.
    fn synth_program_with_calls(
        items: Vec<Item>,
        calls: BTreeMap<Span, GenericInstantiation>,
    ) -> MonoProgram {
        let program = Program {
            items,
            file: "test.sigil".to_string(),
        };
        let checked = CheckedProgram {
            program,
            string_literals: Vec::new(),
            lambda_captures: Vec::new(),
            types: BTreeMap::new(),
            match_scrut_tys: BTreeMap::new(),
            fn_schemes: BTreeMap::new(),
            call_site_instantiations: calls,
            ctor_site_instantiations: BTreeMap::new(),
            effects: BTreeMap::new(),
            effect_ids: BTreeMap::new(),
            op_ids: BTreeMap::new(),
            handle_arm_captures: BTreeMap::new(),
            handle_return_arm_captures: BTreeMap::new(),
        };
        let anf = AnfProgram { checked };
        MonoProgram { anf }
    }

    #[test]
    fn open_effect_row_is_cps() {
        let prog = synth_program(vec![synth_fn_with_open_row("opener")]);
        let cp = infer_colors(prog);
        assert_eq!(color_of(&cp, "opener"), Color::Cps);
        assert_eq!(reason_of(&cp, "opener"), "cps: open effect row");
    }

    #[test]
    fn non_io_effect_in_row_is_cps() {
        let prog = synth_program(vec![synth_fn("raises", vec!["IO", "Raise"], empty_block())]);
        let cp = infer_colors(prog);
        assert_eq!(color_of(&cp, "raises"), Color::Cps);
        assert_eq!(reason_of(&cp, "raises"), "cps: row contains effect `Raise`");
    }

    #[test]
    fn perform_non_io_in_body_is_cps() {
        // fn raiser() -> Int ![Raise] { perform Raise.fail("oops"); 0 }
        let body = AstBlock {
            stmts: vec![Stmt::Perform(PerformExpr {
                effect: "Raise".to_string(),
                op: "fail".to_string(),
                args: vec![Expr::StringLit("oops".to_string(), span())],
                span: span(),
            })],
            tail: Some(Expr::IntLit(0, span())),
            span: span(),
        };
        let prog = synth_program(vec![synth_fn("raiser", vec!["Raise"], body)]);
        let cp = infer_colors(prog);
        assert_eq!(color_of(&cp, "raiser"), Color::Cps);
        // Row check fires before body walk, so reason is the row reason.
        assert_eq!(reason_of(&cp, "raiser"), "cps: row contains effect `Raise`");
    }

    #[test]
    fn cps_taint_propagates_to_caller() {
        // helper() -> Int ![Raise] { 0 }
        // main() -> Int ![] { helper() }   <-- typecheck would reject
        // this row mismatch, but we are testing color propagation on a
        // synthetic post-mono program.
        let helper = synth_fn("helper", vec!["Raise"], empty_block());
        let main_body = AstBlock {
            stmts: Vec::new(),
            tail: Some(Expr::Call {
                callee: Box::new(Expr::Ident("helper".to_string(), span())),
                args: Vec::new(),
                span: span(),
            }),
            span: span(),
        };
        let main = synth_fn("main", vec![], main_body);
        let prog = synth_program(vec![helper, main]);
        let cp = infer_colors(prog);
        assert_eq!(color_of(&cp, "helper"), Color::Cps);
        assert_eq!(color_of(&cp, "main"), Color::Cps);
        let main_reason = reason_of(&cp, "main");
        assert!(
            main_reason.contains("transitively calls `helper`"),
            "got: {main_reason}"
        );
    }

    // ---------------- Plan B Task 55, Phase 4e regression guards
    //
    // These tests pin the colorer's behavior for the discharge-via-call-
    // graph scenario that Phase 4e's codegen-consumes-color work depends
    // on. The colorer is **already correct** for these cases — it
    // classifies a fn whose handle body calls a CPS-color helper as CPS
    // via the existing call-graph SCC bridge propagation. The Phase 4e
    // deviation entry's "colorer's handler-discharge refinement" framing
    // was misleading: the actual refinement is in codegen (consume color
    // and emit CPS-color user fns with the CPS calling convention), not
    // in color.rs. These tests exist so a future refactor of the colorer
    // that drops the call-graph edges out of `Expr::Handle` bodies — or
    // that special-cases handle-discharged effects to avoid recording
    // them as edges — is caught early. Without these tests, such a
    // regression would silently make Phase 4e codegen emit native
    // calling conventions for fns whose performs reach a discharging
    // handler via cross-function-call boundaries, reproducing the
    // discard-k correctness gap the `#[ignore]`'d e2e
    // `discard_k_handler_does_not_abort_helper_phase_4e_pending`
    // currently pins.

    /// Build a minimal `HandleOpArm` for `Effect.op(k) => IntLit(value)`
    /// with no user op-args. Used by the discharge-via-call-graph
    /// regression tests below; the arm body is a literal, the
    /// continuation `k` is named but unused. Span is the shared
    /// constant `span()`; tests that need unique spans for shadowing
    /// precision use `unique_span()`.
    fn synth_op_arm_int_literal(effect: &str, op: &str, value: i64) -> HandleOpArm {
        HandleOpArm {
            effect: effect.to_string(),
            effect_span: span(),
            op: op.to_string(),
            op_span: span(),
            params: Vec::new(),
            k_name: "k".to_string(),
            k_span: span(),
            body: Expr::IntLit(value, span()),
            span: span(),
        }
    }

    /// Plan B Task 55 Phase 4e — pins the colorer's classification for
    /// the same source as the e2e regression test
    /// `statement_form_non_io_perform_inside_handle_compiles_and_runs`
    /// (compiler/tests/e2e.rs around line 1054). The e2e test currently
    /// asserts stdout `42` — the Phase 4d MVP synchronous-broken
    /// behaviour where helper's stmt-form perform of E.op() returns the
    /// arm value 99 (discarded by Stmt) and helper continues to its
    /// tail `42`. Algebraic semantics gives `99` (the discard-k arm
    /// fires; helper aborts; the handle's overall is the arm value).
    /// Phase 4e's codegen-consumes-color commit must invert the e2e
    /// test from `42` → `99` alongside un-`#[ignore]`'ing
    /// `discard_k_handler_does_not_abort_helper_phase_4e_pending`.
    ///
    /// This colorer test pins the *classification* (main is CPS via
    /// bridge to helper) so the e2e test's correctness gap can be
    /// attributed to codegen, not to a colorer regression. If a future
    /// refactor breaks this classification (e.g., dropping the
    /// call-graph edges out of `Expr::Handle::body`), this test fires
    /// before the e2e test does — the failure points at color.rs not
    /// at codegen.
    #[test]
    fn statement_form_non_io_perform_inside_handle_classifies_main_cps_via_bridge() {
        let src = "effect E { op: () -> Int }\n\
                   fn helper() -> Int ![E] {\n  \
                     perform E.op();\n  \
                     42\n\
                   }\n\
                   fn main() -> Int ![IO] {\n  \
                     let n: Int = handle helper() with { E.op(k) => 99 };\n  \
                     perform IO.println(int_to_string(n));\n  \
                     0\n\
                   }\n";
        let cp = color_from_src(src);
        assert_eq!(color_of(&cp, "helper"), Color::Cps);
        assert_eq!(reason_of(&cp, "helper"), "cps: row contains effect `E`");
        assert_eq!(color_of(&cp, "main"), Color::Cps);
        let main_reason = reason_of(&cp, "main");
        assert!(
            main_reason.contains("transitively calls `helper`"),
            "got: {main_reason}"
        );
    }

    #[test]
    fn handle_body_calling_cps_helper_makes_caller_cps_via_bridge() {
        // helper() -> Int ![Raise] { 0 }     -- intrinsically CPS
        // main()   -> Int ![]      { handle helper() with { Raise.fail(k) => 42 } }
        //
        // Phase 4e discharge-via-call-graph: main's call to helper is
        // recorded via the synthetic calls map (the `walk_expr_for_fn_
        // idents` recursion through `Expr::Handle::body` collects the
        // Ident span). The colorer's `collect_calls_in_expr` for
        // `Expr::Handle` mirrors this — it walks the body for call
        // edges. Helper's intrinsic CPS color (row contains `Raise`)
        // taints main via SCC bridge. Reason text asserts the expected
        // `transitively calls helper` form.
        let helper = synth_fn("helper", vec!["Raise"], empty_block());
        let main_body = AstBlock {
            stmts: Vec::new(),
            tail: Some(Expr::Handle {
                body: Box::new(Expr::Call {
                    callee: Box::new(Expr::Ident("helper".to_string(), span())),
                    args: Vec::new(),
                    span: span(),
                }),
                return_arm: None,
                op_arms: vec![synth_op_arm_int_literal("Raise", "fail", 42)],
                span: span(),
            }),
            span: span(),
        };
        let main = synth_fn("main", vec![], main_body);
        let prog = synth_program(vec![helper, main]);
        let cp = infer_colors(prog);
        assert_eq!(color_of(&cp, "helper"), Color::Cps);
        assert_eq!(color_of(&cp, "main"), Color::Cps);
        let main_reason = reason_of(&cp, "main");
        assert!(
            main_reason.contains("transitively calls `helper`"),
            "got: {main_reason}"
        );
    }

    #[test]
    fn handle_body_with_only_direct_perform_keeps_caller_native_under_existing_local_rule() {
        // main() -> Int ![] { handle perform Raise.fail() with { Raise.fail(k) => 42 } }
        //
        // Pins the `find_non_io_perform_in_expr` skip-the-handle-body
        // rule (line 414 in this file at HEAD). main's body has a
        // direct `perform Raise.fail()` syntactically inside the
        // handle's body; the local-walk explicitly does NOT descend
        // into the handle body, so main's local color stays Native.
        // No call-graph edges (main calls no other fn), no
        // intrinsic-CPS triggers. Phase 4e's codegen-consumes-color
        // work relies on this rule for the "perform-in-tail-position-
        // of-handle-body" shape that Phase 4d MVP already handles
        // correctly under the synchronous run_loop pattern; if this
        // test starts failing, codegen will start emitting CPS
        // calling convention for fns that don't need it, regressing
        // performance even when correctness is preserved.
        let main_body = AstBlock {
            stmts: Vec::new(),
            tail: Some(Expr::Handle {
                body: Box::new(Expr::Perform(crate::ast::PerformExpr {
                    effect: "Raise".to_string(),
                    op: "fail".to_string(),
                    args: Vec::new(),
                    span: span(),
                })),
                return_arm: None,
                op_arms: vec![synth_op_arm_int_literal("Raise", "fail", 42)],
                span: span(),
            }),
            span: span(),
        };
        let main = synth_fn("main", vec![], main_body);
        let prog = synth_program(vec![main]);
        let cp = infer_colors(prog);
        assert_eq!(color_of(&cp, "main"), Color::Native);
        assert_eq!(reason_of(&cp, "main"), "native: pure row");
    }

    #[test]
    fn handle_arm_body_performing_undischarged_effect_taints_caller_intrinsically() {
        // main() -> Int ![] { handle 0 with { Raise.fail(k) => perform Other.boom() } }
        //
        // Pins the existing rule that `find_non_io_perform_in_expr`
        // **does** walk arm bodies. The arm body's perform of an
        // effect not in the handle's discharged set leaks into main's
        // intrinsic-CPS classification. Synthetic test only — typecheck
        // would E0042 this in real code (the row doesn't list `Other`),
        // but the colorer is the source of truth post-mono and must
        // remain robust. Phase 4e doesn't change this rule.
        let main_body = AstBlock {
            stmts: Vec::new(),
            tail: Some(Expr::Handle {
                body: Box::new(Expr::IntLit(0, span())),
                return_arm: None,
                op_arms: vec![HandleOpArm {
                    effect: "Raise".to_string(),
                    effect_span: span(),
                    op: "fail".to_string(),
                    op_span: span(),
                    params: Vec::new(),
                    k_name: "k".to_string(),
                    k_span: span(),
                    body: Expr::Perform(crate::ast::PerformExpr {
                        effect: "Other".to_string(),
                        op: "boom".to_string(),
                        args: Vec::new(),
                        span: span(),
                    }),
                    span: span(),
                }],
                span: span(),
            }),
            span: span(),
        };
        let main = synth_fn("main", vec![], main_body);
        let prog = synth_program(vec![main]);
        let cp = infer_colors(prog);
        assert_eq!(color_of(&cp, "main"), Color::Cps);
        let main_reason = reason_of(&cp, "main");
        assert!(
            main_reason.contains("performs `Other.boom`"),
            "got: {main_reason}"
        );
    }

    #[test]
    fn mutual_recursion_is_a_single_scc_native() {
        // ping calls pong, pong calls ping; both pure.
        let ping_body = AstBlock {
            stmts: Vec::new(),
            tail: Some(Expr::Call {
                callee: Box::new(Expr::Ident("pong".to_string(), span())),
                args: Vec::new(),
                span: span(),
            }),
            span: span(),
        };
        let pong_body = AstBlock {
            stmts: Vec::new(),
            tail: Some(Expr::Call {
                callee: Box::new(Expr::Ident("ping".to_string(), span())),
                args: Vec::new(),
                span: span(),
            }),
            span: span(),
        };
        let ping = synth_fn("ping", vec![], ping_body);
        let pong = synth_fn("pong", vec![], pong_body);
        let prog = synth_program(vec![ping, pong]);
        let cp = infer_colors(prog);
        assert_eq!(color_of(&cp, "ping"), Color::Native);
        assert_eq!(color_of(&cp, "pong"), Color::Native);
    }

    #[test]
    fn mutual_recursion_with_one_cps_member_taints_whole_scc() {
        // ping calls pong, pong calls ping AND has a non-IO row.
        let ping_body = AstBlock {
            stmts: Vec::new(),
            tail: Some(Expr::Call {
                callee: Box::new(Expr::Ident("pong".to_string(), span())),
                args: Vec::new(),
                span: span(),
            }),
            span: span(),
        };
        let pong_body = AstBlock {
            stmts: Vec::new(),
            tail: Some(Expr::Call {
                callee: Box::new(Expr::Ident("ping".to_string(), span())),
                args: Vec::new(),
                span: span(),
            }),
            span: span(),
        };
        let ping = synth_fn("ping", vec![], ping_body);
        let pong = synth_fn("pong", vec!["Raise"], pong_body);
        let prog = synth_program(vec![ping, pong]);
        let cp = infer_colors(prog);
        assert_eq!(color_of(&cp, "ping"), Color::Cps);
        assert_eq!(color_of(&cp, "pong"), Color::Cps);
        // ping's only outgoing edge is intra-SCC (to pong), so ping
        // is not a bridge — it's pulled in via SCC membership. The
        // reason names pong as the intrinsic peer that caused the
        // taint, with the explicit "intrinsically-cps" wording.
        let ping_reason = reason_of(&cp, "ping");
        assert_eq!(
            ping_reason, "cps: in SCC with intrinsically-cps member `pong`",
            "got: {ping_reason}"
        );
        assert_eq!(reason_of(&cp, "pong"), "cps: row contains effect `Raise`");
    }

    #[test]
    fn unrelated_cycle_does_not_pessimize_other_fns() {
        // a <-> b cycle, both CPS; c calls neither, c is native.
        let a_body = AstBlock {
            stmts: Vec::new(),
            tail: Some(Expr::Call {
                callee: Box::new(Expr::Ident("b".to_string(), span())),
                args: Vec::new(),
                span: span(),
            }),
            span: span(),
        };
        let b_body = AstBlock {
            stmts: Vec::new(),
            tail: Some(Expr::Call {
                callee: Box::new(Expr::Ident("a".to_string(), span())),
                args: Vec::new(),
                span: span(),
            }),
            span: span(),
        };
        let a = synth_fn("a", vec!["Raise"], a_body);
        let b = synth_fn("b", vec![], b_body);
        let c = synth_fn("c", vec![], empty_block());
        let prog = synth_program(vec![a, b, c]);
        let cp = infer_colors(prog);
        assert_eq!(color_of(&cp, "a"), Color::Cps);
        assert_eq!(color_of(&cp, "b"), Color::Cps);
        assert_eq!(color_of(&cp, "c"), Color::Native);
    }

    #[test]
    fn perform_in_native_row_still_caught_by_body_walk() {
        // Synthetic: declared row `![IO]` but body performs `Raise.fail`.
        // Real typecheck would reject this, but the body walk is the
        // belt-and-braces classifier — we want to confirm it fires.
        let body = AstBlock {
            stmts: vec![Stmt::Perform(PerformExpr {
                effect: "Raise".to_string(),
                op: "fail".to_string(),
                args: vec![Expr::StringLit("oops".to_string(), span())],
                span: span(),
            })],
            tail: Some(Expr::IntLit(0, span())),
            span: span(),
        };
        let prog = synth_program(vec![synth_fn("naughty", vec!["IO"], body)]);
        let cp = infer_colors(prog);
        assert_eq!(color_of(&cp, "naughty"), Color::Cps);
        assert_eq!(reason_of(&cp, "naughty"), "cps: performs `Raise.fail`");
    }

    #[test]
    fn dump_color_renders_one_line_per_fn_in_program_order() {
        let a = synth_fn("alpha", vec!["IO"], empty_block());
        let b = synth_fn("beta", vec!["Raise"], empty_block());
        let prog = synth_program(vec![a, b]);
        let cp = infer_colors(prog);
        let dump = dump_color(&cp);
        assert_eq!(
            dump,
            "alpha native native: row is `![IO]`\nbeta cps cps: row contains effect `Raise`\n"
        );
    }

    #[test]
    fn lambda_body_perform_taints_parent() {
        // fn parent() -> Int ![] { let f = fn () -> Int ![] => { perform Raise.fail("x"); 0 }; 0 }
        // The lambda body's perform is found by `find_non_io_perform_in_block`,
        // tainting the parent.
        let lambda_body = Expr::Block(Box::new(AstBlock {
            stmts: vec![Stmt::Perform(PerformExpr {
                effect: "Raise".to_string(),
                op: "fail".to_string(),
                args: vec![Expr::StringLit("x".to_string(), span())],
                span: span(),
            })],
            tail: Some(Expr::IntLit(0, span())),
            span: span(),
        }));
        let lambda = Expr::Lambda {
            params: Vec::new(),
            return_type: TypeExpr::Named("Int".to_string(), span()),
            effects: Vec::new(),
            effect_row_var: None,
            body: Box::new(lambda_body),
            span: span(),
        };
        let body = AstBlock {
            stmts: vec![Stmt::Let(crate::ast::LetStmt {
                name: "f".to_string(),
                ty: TypeExpr::Named("Int".to_string(), span()),
                value: lambda,
                span: span(),
            })],
            tail: Some(Expr::IntLit(0, span())),
            span: span(),
        };
        let prog = synth_program(vec![synth_fn("parent", vec![], body)]);
        let cp = infer_colors(prog);
        assert_eq!(color_of(&cp, "parent"), Color::Cps);
        assert_eq!(reason_of(&cp, "parent"), "cps: performs `Raise.fail`");
    }

    #[test]
    fn synth_fn_with_param_classifies_native() {
        // Sanity check: `synth_fn` extended with a parameter list
        // still classifies native when its row is pure and its body
        // has no perform sites. Pins the test scaffolding, not a
        // language invariant.
        let body = empty_block();
        let mut item = synth_fn("p", vec![], body);
        if let Item::Fn(ref mut f) = item {
            f.params.push(Param {
                name: "n".to_string(),
                ty: TypeExpr::Named("Int".to_string(), span()),
                span: span(),
            });
        }
        let prog = synth_program(vec![item]);
        let cp = infer_colors(prog);
        assert_eq!(color_of(&cp, "p"), Color::Native);
    }

    // ---------------- Review-driven additions: SCC algorithm coverage,
    //                  shadowing precision, edge soundness.

    /// Single-node SCC with self-loop, native body. `fib` calling
    /// itself directly is a single-node SCC; its lowlink doesn't
    /// drop below its index, so it stays a singleton SCC. Color
    /// inference must propagate correctly within: pure row + pure
    /// callee = native.
    #[test]
    fn self_loop_single_node_scc_native() {
        let body = AstBlock {
            stmts: Vec::new(),
            tail: Some(Expr::Call {
                callee: Box::new(Expr::Ident("fib".to_string(), span())),
                args: Vec::new(),
                span: span(),
            }),
            span: span(),
        };
        let prog = synth_program(vec![synth_fn("fib", vec![], body)]);
        let cp = infer_colors(prog);
        assert_eq!(color_of(&cp, "fib"), Color::Native);
        assert_eq!(reason_of(&cp, "fib"), "native: pure row");
    }

    /// Single-node SCC with self-loop, locally-CPS body. Reason
    /// should be the intrinsic CPS reason (own row), not the
    /// SCC-membership fallback.
    #[test]
    fn self_loop_single_node_scc_cps() {
        let body = AstBlock {
            stmts: Vec::new(),
            tail: Some(Expr::Call {
                callee: Box::new(Expr::Ident("loopy".to_string(), span())),
                args: Vec::new(),
                span: span(),
            }),
            span: span(),
        };
        let prog = synth_program(vec![synth_fn("loopy", vec!["Raise"], body)]);
        let cp = infer_colors(prog);
        assert_eq!(color_of(&cp, "loopy"), Color::Cps);
        // Intrinsic-CPS reason wins over any SCC fallback.
        assert_eq!(reason_of(&cp, "loopy"), "cps: row contains effect `Raise`");
    }

    /// `let f = some_cps_fn` in an otherwise-Native parent must
    /// still taint the parent. Pins the load-bearing soundness
    /// claim from the PR description: bare-Ident value-position
    /// references count as outgoing edges, not just call-position
    /// callees.
    #[test]
    fn let_bound_fn_value_taints_parent_via_outgoing_edge() {
        // fn cps_fn() -> Int ![Raise] { 0 }      <- intrinsic CPS
        // fn parent() -> Int ![] {
        //   let f = cps_fn;                       <- value-position fn ref
        //   0
        // }
        let parent_body = AstBlock {
            stmts: vec![Stmt::Let(crate::ast::LetStmt {
                name: "f".to_string(),
                ty: TypeExpr::Named("Int".to_string(), span()),
                value: Expr::Ident("cps_fn".to_string(), span()),
                span: span(),
            })],
            tail: Some(Expr::IntLit(0, span())),
            span: span(),
        };
        let cps_fn = synth_fn("cps_fn", vec!["Raise"], empty_block());
        let parent = synth_fn("parent", vec![], parent_body);
        let prog = synth_program(vec![cps_fn, parent]);
        let cp = infer_colors(prog);
        assert_eq!(color_of(&cp, "cps_fn"), Color::Cps);
        assert_eq!(color_of(&cp, "parent"), Color::Cps);
        // `parent`'s reason is the bridge form — it has an outgoing
        // edge to `cps_fn` (a different SCC, already Cps when this
        // SCC is processed because Tarjan emits sinks first).
        assert_eq!(
            reason_of(&cp, "parent"),
            "cps: transitively calls `cps_fn` which is cps"
        );
    }

    /// 3-SCC chain: `c` is intrinsic CPS, `b` calls `c`, `a` calls
    /// `b`. Both `a` and `b` should propagate to CPS. Pins
    /// reverse-topological propagation across depth-N hops, not just
    /// the single-hop case.
    #[test]
    fn cps_propagates_through_three_scc_chain() {
        let a_body = AstBlock {
            stmts: Vec::new(),
            tail: Some(Expr::Call {
                callee: Box::new(Expr::Ident("b".to_string(), span())),
                args: Vec::new(),
                span: span(),
            }),
            span: span(),
        };
        let b_body = AstBlock {
            stmts: Vec::new(),
            tail: Some(Expr::Call {
                callee: Box::new(Expr::Ident("c".to_string(), span())),
                args: Vec::new(),
                span: span(),
            }),
            span: span(),
        };
        let a = synth_fn("a", vec![], a_body);
        let b = synth_fn("b", vec![], b_body);
        let c = synth_fn("c", vec!["Raise"], empty_block());
        let prog = synth_program(vec![a, b, c]);
        let cp = infer_colors(prog);
        assert_eq!(color_of(&cp, "c"), Color::Cps);
        assert_eq!(color_of(&cp, "b"), Color::Cps);
        assert_eq!(color_of(&cp, "a"), Color::Cps);
        assert_eq!(reason_of(&cp, "c"), "cps: row contains effect `Raise`");
        assert_eq!(
            reason_of(&cp, "b"),
            "cps: transitively calls `c` which is cps"
        );
        assert_eq!(
            reason_of(&cp, "a"),
            "cps: transitively calls `b` which is cps"
        );
    }

    /// Transitive-only SCC reason branch: a mutually-recursive SCC
    /// {pure_a, pure_b} where neither is intrinsically CPS, but
    /// pure_a has an outgoing edge to a separate intrinsically-CPS
    /// fn `cps_x`. The whole SCC becomes CPS via the bridge. The
    /// non-bridge member (pure_b) gets the
    /// `in SCC bridging to cps callee via <bridge>` reason.
    #[test]
    fn scc_taint_via_transitive_only_branch() {
        // pure_a calls pure_b AND cps_x; pure_b calls pure_a only.
        let pure_a_body = AstBlock {
            stmts: vec![Stmt::Expr(Expr::Call {
                callee: Box::new(Expr::Ident("cps_x".to_string(), span())),
                args: Vec::new(),
                span: span(),
            })],
            tail: Some(Expr::Call {
                callee: Box::new(Expr::Ident("pure_b".to_string(), span())),
                args: Vec::new(),
                span: span(),
            }),
            span: span(),
        };
        let pure_b_body = AstBlock {
            stmts: Vec::new(),
            tail: Some(Expr::Call {
                callee: Box::new(Expr::Ident("pure_a".to_string(), span())),
                args: Vec::new(),
                span: span(),
            }),
            span: span(),
        };
        let pure_a = synth_fn("pure_a", vec![], pure_a_body);
        let pure_b = synth_fn("pure_b", vec![], pure_b_body);
        let cps_x = synth_fn("cps_x", vec!["Raise"], empty_block());
        let prog = synth_program(vec![pure_a, pure_b, cps_x]);
        let cp = infer_colors(prog);
        assert_eq!(color_of(&cp, "cps_x"), Color::Cps);
        assert_eq!(color_of(&cp, "pure_a"), Color::Cps);
        assert_eq!(color_of(&cp, "pure_b"), Color::Cps);
        assert_eq!(reason_of(&cp, "cps_x"), "cps: row contains effect `Raise`");
        // pure_a is the bridge — has direct outgoing edge to cps_x.
        assert_eq!(
            reason_of(&cp, "pure_a"),
            "cps: transitively calls `cps_x` which is cps"
        );
        // pure_b is in the SCC but has no direct outgoing edge to a
        // CPS SCC; it gets the bridging-via-peer reason naming
        // pure_a, NOT the intrinsically-cps phrasing (no member is
        // intrinsically CPS in this SCC).
        assert_eq!(
            reason_of(&cp, "pure_b"),
            "cps: in SCC bridging to cps callee via `pure_a`"
        );
    }

    /// Shadowing precision: a parameter or `let` binding sharing a
    /// name with a top-level fn must NOT produce a spurious outgoing
    /// edge to that fn. The previous heuristic-based edge logic
    /// (every Ident matching a fn name) over-approximated and
    /// pessimized this case to CPS. Driving from
    /// `call_site_instantiations` makes it precise — typecheck's
    /// env-precedence rules win, and the local reference is not
    /// recorded.
    ///
    /// This test is **discriminating**: it constructs a synthetic
    /// program where:
    /// 1. `dangerous` is intrinsically CPS (non-IO effect row).
    /// 2. `caller(dangerous: Int)` has a body that references both
    ///    the parameter `dangerous` (Ident in expression position)
    ///    AND makes a real call to a separate top-level fn `unrelated`.
    /// 3. The `call_site_instantiations` map is supplied **manually**
    ///    with only `unrelated`'s call-site span — modeling what a
    ///    real env-precedence-aware typecheck pass would record.
    ///
    /// Under the precise edge logic, `caller`'s only outgoing edge
    /// is to `unrelated` (Native), so `caller` stays Native. Under
    /// the old name-only heuristic, `caller` would also acquire a
    /// spurious edge to `dangerous` and falsely classify CPS.
    /// Asserting `Color::Native` for `caller` while `dangerous` is
    /// CPS *discriminates* the two regimes — which the prior
    /// front-end-driven version of this test could not do under
    /// Stage 5's `IO`-only typecheck.
    #[test]
    fn parameter_shadowing_top_level_fn_does_not_taint_caller() {
        // unique_span() per Ident so the call map can target an
        // exact occurrence; the param-ref `dangerous` Ident's span
        // is deliberately *omitted* from the calls map.
        let dangerous_fn = synth_fn("dangerous", vec!["Raise"], empty_block());
        let unrelated_fn = synth_fn("unrelated", vec![], empty_block());

        // caller body: `unrelated(); dangerous + 1`
        // Both Idents have unique spans. We capture the span of the
        // unrelated-call Ident so it can be recorded in the calls
        // map (a real env-precedence pass would record it). The
        // shadow Ident's span is deliberately not recorded.
        let unrelated_callee_span = unique_span();
        let caller_body = AstBlock {
            stmts: vec![Stmt::Expr(Expr::Call {
                callee: Box::new(Expr::Ident(
                    "unrelated".to_string(),
                    unrelated_callee_span.clone(),
                )),
                args: Vec::new(),
                span: unique_span(),
            })],
            tail: Some(Expr::Binary {
                op: crate::ast::BinOp::Add,
                lhs: Box::new(Expr::Ident("dangerous".to_string(), unique_span())),
                rhs: Box::new(Expr::IntLit(1, unique_span())),
                span: unique_span(),
            }),
            span: unique_span(),
        };
        let mut caller = synth_fn("caller", vec![], caller_body);
        if let Item::Fn(ref mut f) = caller {
            f.params.push(Param {
                name: "dangerous".to_string(),
                ty: TypeExpr::Named("Int".to_string(), unique_span()),
                span: unique_span(),
            });
        }

        // Build the calls map manually: only the real call to
        // `unrelated` is recorded. The shadow Ident is excluded —
        // this is exactly what an env-precedence-aware typecheck
        // would emit (param wins, fn_schemes lookup never runs).
        let mut calls: BTreeMap<Span, GenericInstantiation> = BTreeMap::new();
        calls.insert(
            unrelated_callee_span,
            GenericInstantiation {
                name: "unrelated".to_string(),
                type_args: Vec::new(),
            },
        );

        let prog = synth_program_with_calls(vec![dangerous_fn, unrelated_fn, caller], calls);
        let cp = infer_colors(prog);

        // `dangerous` is intrinsically CPS via its row.
        assert_eq!(color_of(&cp, "dangerous"), Color::Cps);
        // `unrelated` is Native (pure row, leaf).
        assert_eq!(color_of(&cp, "unrelated"), Color::Native);
        // `caller`'s only outgoing edge is to `unrelated` (Native);
        // the shadow Ident produces no edge under the precise calls-
        // map drive. Caller stays Native.
        //
        // Discrimination: under the old name-only heuristic, caller
        // would also have an edge to `dangerous` (CPS) and the SCC
        // pass would classify caller as CPS. Asserting Native here
        // genuinely tests the precision fix — not a tautology under
        // either edge regime.
        assert_eq!(color_of(&cp, "caller"), Color::Native);
        assert_eq!(reason_of(&cp, "caller"), "native: pure row");
    }

    // ---------------- Plan B Task 55, Phase 4e — accessor methods on
    // ColoredProgram (relocated from `cps::tests` when the
    // `CpsProgram` wrapper was deleted as a transitional artifact).
    //
    // The Phase 4e roadmap originally landed `needs_cps_transform`
    // and `cps_color_user_fns` as methods on a `CpsProgram` wrapper
    // (`a756bd3`). After confirming Option B as the architectural
    // direction (inline lowering in codegen, not a separate IR
    // pass), the wrapper carried no CPS-form-specific metadata
    // and was deleted; the accessors moved here. See the
    // `[DEVIATION Task 55] Phase 4e — comprehensive` entry's
    // section 1 update at that commit for the architectural
    // rationale.

    #[test]
    fn needs_cps_transform_native_main_returns_false() {
        let src = r#"
            fn main() -> Int ![] { 42 }
        "#;
        let cp = color_from_src(src);
        assert!(!cp.needs_cps_transform("main"));
    }

    #[test]
    fn needs_cps_transform_unknown_fn_returns_false() {
        let src = r#"
            fn main() -> Int ![] { 42 }
        "#;
        let cp = color_from_src(src);
        // Querying a fn that doesn't exist returns false (not a panic).
        // The codegen entry walker is the source of truth for which fns
        // reach codegen; this accessor is a query, not an assertion.
        assert!(!cp.needs_cps_transform("nonexistent_fn"));
    }

    #[test]
    fn needs_cps_transform_classifies_cps_color_helper_correctly() {
        // statement_form_non_io_perform_inside_handle source — the
        // existing passing e2e test with main calling helper inside
        // a handle body. helper has row ![E] (intrinsic CPS); main
        // is CPS via SCC bridge. Verify the accessor returns true
        // for both.
        let src = "effect E { op: () -> Int }\n\
                   fn helper() -> Int ![E] {\n  \
                     perform E.op();\n  \
                     42\n\
                   }\n\
                   fn main() -> Int ![IO] {\n  \
                     let n: Int = handle helper() with { E.op(k) => 99 };\n  \
                     perform IO.println(int_to_string(n));\n  \
                     0\n\
                   }\n";
        let cp = color_from_src(src);
        assert!(cp.needs_cps_transform("helper"));
        assert!(cp.needs_cps_transform("main"));
    }

    #[test]
    fn cps_color_user_fns_lists_program_order_cps_only() {
        let src = "effect E { op: () -> Int }\n\
                   fn helper() -> Int ![E] {\n  \
                     perform E.op();\n  \
                     42\n\
                   }\n\
                   fn pure_helper(n: Int) -> Int ![] { n + 1 }\n\
                   fn main() -> Int ![IO] {\n  \
                     let n: Int = handle helper() with { E.op(k) => pure_helper(99) };\n  \
                     perform IO.println(int_to_string(n));\n  \
                     0\n\
                   }\n";
        let cp = color_from_src(src);
        let cps_fns = cp.cps_color_user_fns();
        // helper is intrinsic CPS; main is CPS via bridge to helper;
        // pure_helper has empty row and no perform → Native. Order
        // follows program order (helper, pure_helper, main).
        assert_eq!(cps_fns, vec!["helper".to_string(), "main".to_string()]);
    }

    #[test]
    fn cps_color_user_fns_pins_multi_level_scc_bridge_ordering() {
        // a → b → c, where c is intrinsically CPS, and verify
        // cps_color_user_fns lists all three in program declaration
        // order. Pins the transitive-closure invariant for ordering,
        // which is load-bearing if the codegen-consumes-color commit
        // relies on the order. The 2-fn test above
        // (`cps_color_user_fns_lists_program_order_cps_only`)
        // exercises a single-hop bridge; this exercises three hops
        // and confirms the program-declaration-order property holds
        // through transitive classification (not just directly-
        // intrinsic-CPS members).
        let src = "effect E { op: () -> Int }\n\
                   fn c() -> Int ![E] {\n  \
                     perform E.op()\n\
                   }\n\
                   fn b() -> Int ![E] {\n  \
                     c()\n\
                   }\n\
                   fn a() -> Int ![E] {\n  \
                     b()\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let cp = color_from_src(src);
        let cps_fns = cp.cps_color_user_fns();
        // c is intrinsic CPS (row contains E + body performs E.op);
        // b and a become CPS via SCC bridge (a → b → c). main is
        // Native (empty row, no perform, no calls). Order follows
        // source declaration: c, b, a.
        assert_eq!(
            cps_fns,
            vec!["c".to_string(), "b".to_string(), "a".to_string()]
        );
    }

    #[test]
    fn cps_color_user_fns_pins_mutual_recursion_scc_with_cps_bridge() {
        // a and b mutually recurse; both call c; c performs E.op
        // (intrinsic CPS). The mutual recursion forms a single SCC
        // {a, b}; the SCC bridges to c's singleton SCC (which is
        // CPS). All three end up CPS — a and b via SCC-bridge-to-cps,
        // c intrinsically. Pins the SCC-collapse + multi-member
        // ordering invariant: cps_color_user_fns() should list all
        // SCC members in source declaration order, not just one
        // representative member.
        let src = "effect E { op: () -> Int }\n\
                   fn c() -> Int ![E] {\n  \
                     perform E.op()\n\
                   }\n\
                   fn a() -> Int ![E] {\n  \
                     let x: Int = c();\n  \
                     b()\n\
                   }\n\
                   fn b() -> Int ![E] {\n  \
                     let y: Int = c();\n  \
                     a()\n\
                   }\n\
                   fn main() -> Int ![] { 0 }\n";
        let cp = color_from_src(src);
        let cps_fns = cp.cps_color_user_fns();
        // c is intrinsic CPS. a and b are mutually recursive — they
        // form a single SCC {a, b} which bridges to c's CPS
        // classification. All three are CPS. Source declaration
        // order: c, a, b. main is Native (excluded).
        assert_eq!(
            cps_fns,
            vec!["c".to_string(), "a".to_string(), "b".to_string()]
        );
    }
}
