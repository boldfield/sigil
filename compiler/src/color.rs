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
use crate::monomorphize::MonoProgram;

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

/// The only effect treated as "pure" for native classification in
/// Plan B v1. Plan B Stage 6 keeps this as a special case for the
/// top-level IO handler shim. Anything else in the row makes the
/// monomorph CPS.
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
    // Edges only point at known top-level fns; calls to lambdas /
    // closure-record values / first-class fn-typed args are deliberately
    // ignored because they can't be statically resolved here without
    // closure conversion. Plan B v1's color model is conservative on
    // those cases via the local analysis (lambdas inside a fn body are
    // walked for performs and effects already).
    let mut edges: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); n];
    for (i, f) in fn_decls.iter().enumerate() {
        let mut out: BTreeSet<usize> = BTreeSet::new();
        collect_calls_in_block(&f.body, &fn_index, &mut out);
        edges[i] = out;
    }

    // -------- Step 4: Tarjan SCC --------
    // Returns SCCs in reverse-topological order (sinks first), and a
    // node->scc index map.
    let (sccs, scc_of) = tarjan_scc(n, &edges);

    // -------- Step 5: propagate CPS color across SCCs --------
    let mut scc_color: Vec<Color> = vec![Color::Native; sccs.len()];
    // Per-fn reason after propagation.
    let mut node_reason: Vec<String> = vec![String::new(); n];

    for (scc_idx, scc) in sccs.iter().enumerate() {
        // (a) intrinsic-CPS check
        let mut intrinsic_member: Option<(usize, &str)> = None;
        for &node in scc {
            if let LocalColor::Cps(reason) = &local[node] {
                intrinsic_member = Some((node, reason.as_str()));
                break; // first by program order
            }
        }

        // (b) transitive-CPS check (only meaningful if no intrinsic member)
        let mut transitive_callee: Option<(usize, usize)> = None;
        if intrinsic_member.is_none() {
            'outer: for &node in scc {
                for &callee in &edges[node] {
                    let callee_scc = scc_of[callee];
                    if callee_scc == scc_idx {
                        continue;
                    }
                    if scc_color[callee_scc] == Color::Cps {
                        transitive_callee = Some((node, callee));
                        break 'outer;
                    }
                }
            }
        }

        // (c) decide SCC color and reason text
        match (intrinsic_member, transitive_callee) {
            (Some((member, reason)), _) => {
                scc_color[scc_idx] = Color::Cps;
                let reason_owned = reason.to_string();
                for &node in scc {
                    if node == member {
                        node_reason[node] = reason_owned.clone();
                    } else {
                        node_reason[node] =
                            format!("cps: in SCC with cps member `{}`", fn_names[member]);
                    }
                }
            }
            (None, Some((caller, callee))) => {
                scc_color[scc_idx] = Color::Cps;
                let caller_reason = format!(
                    "cps: transitively calls `{}` which is cps",
                    fn_names[callee]
                );
                for &node in scc {
                    if node == caller {
                        node_reason[node] = caller_reason.clone();
                    } else {
                        node_reason[node] =
                            format!("cps: in SCC with cps member `{}`", fn_names[caller]);
                    }
                }
            }
            (None, None) => {
                scc_color[scc_idx] = Color::Native;
                for &node in scc {
                    if let LocalColor::Native(r) = &local[node] {
                        node_reason[node] = r.clone();
                    }
                }
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

/// Walk `b` and its nested expressions for direct calls whose callee
/// is a known top-level fn name; insert each callee's index into `out`.
fn collect_calls_in_block(
    b: &Block,
    fn_index: &BTreeMap<String, usize>,
    out: &mut BTreeSet<usize>,
) {
    for s in &b.stmts {
        match s {
            Stmt::Let(l) => collect_calls_in_expr(&l.value, fn_index, out),
            Stmt::Expr(e) => collect_calls_in_expr(e, fn_index, out),
            Stmt::Perform(p) => {
                for a in &p.args {
                    collect_calls_in_expr(a, fn_index, out);
                }
            }
        }
    }
    if let Some(t) = &b.tail {
        collect_calls_in_expr(t, fn_index, out);
    }
}

fn collect_calls_in_expr(e: &Expr, fn_index: &BTreeMap<String, usize>, out: &mut BTreeSet<usize>) {
    match e {
        Expr::IntLit(_, _)
        | Expr::StringLit(_, _)
        | Expr::BoolLit(_, _)
        | Expr::CharLit(_, _)
        | Expr::ClosureEnvLoad { .. } => {}
        Expr::Ident(name, _) => {
            // Bare identifier in expression position can be the
            // callee of a `Call` — that case is handled below — or a
            // value reference (e.g., `let f = some_fn`). Plan A2's
            // closure model treats top-level fn names as values via
            // `lower_call`'s direct-Ident branch; outside of that
            // branch the Ident still resolves to the same top-level
            // fn, so we conservatively count any reference as an
            // outgoing edge. This keeps the call graph sound when
            // future passes add fn-as-value propagation.
            if let Some(&idx) = fn_index.get(name) {
                out.insert(idx);
            }
        }
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident(name, _) = callee.as_ref() {
                if let Some(&idx) = fn_index.get(name) {
                    out.insert(idx);
                }
            } else {
                collect_calls_in_expr(callee, fn_index, out);
            }
            for a in args {
                collect_calls_in_expr(a, fn_index, out);
            }
        }
        Expr::Perform(p) => {
            for a in &p.args {
                collect_calls_in_expr(a, fn_index, out);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            collect_calls_in_expr(lhs, fn_index, out);
            collect_calls_in_expr(rhs, fn_index, out);
        }
        Expr::Unary { operand, .. } => collect_calls_in_expr(operand, fn_index, out),
        Expr::If {
            cond,
            then_block,
            else_block,
            ..
        } => {
            collect_calls_in_expr(cond, fn_index, out);
            collect_calls_in_block(then_block, fn_index, out);
            collect_calls_in_block(else_block, fn_index, out);
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            collect_calls_in_expr(scrutinee, fn_index, out);
            for MatchArm { body, .. } in arms {
                collect_calls_in_expr(body, fn_index, out);
            }
        }
        Expr::Block(b) => collect_calls_in_block(b, fn_index, out),
        Expr::Lambda { body, .. } => collect_calls_in_expr(body, fn_index, out),
        Expr::ClosureRecord { env_exprs, .. } => {
            for ex in env_exprs {
                collect_calls_in_expr(ex, fn_index, out);
            }
        }
        Expr::RecordLit { fields, .. } => {
            for f in fields {
                collect_calls_in_expr(&f.value, fn_index, out);
            }
        }
    }
}

/// Tarjan's SCC. Returns `(sccs, scc_of)` where `sccs[i]` is a
/// vector of node indices in SCC `i` (sorted ascending for
/// determinism within each SCC) and `scc_of[node]` is the SCC index
/// of `node`. Tarjan's algorithm naturally emits SCCs in
/// reverse-topological order — sinks first — which is exactly what
/// the propagation pass needs.
fn tarjan_scc(n: usize, edges: &[BTreeSet<usize>]) -> (Vec<Vec<usize>>, Vec<usize>) {
    struct State<'a> {
        edges: &'a [BTreeSet<usize>],
        index_counter: usize,
        index_of: Vec<Option<usize>>,
        lowlink: Vec<usize>,
        on_stack: Vec<bool>,
        stack: Vec<usize>,
        sccs: Vec<Vec<usize>>,
        scc_of: Vec<usize>,
    }
    fn strongconnect(st: &mut State<'_>, v: usize) {
        st.index_of[v] = Some(st.index_counter);
        st.lowlink[v] = st.index_counter;
        st.index_counter += 1;
        st.stack.push(v);
        st.on_stack[v] = true;
        // Snapshot v's edges to drop the immutable borrow before
        // recursive descent (which mutates `st`). Sorted-set iteration
        // gives deterministic SCC numbering.
        let neighbors: Vec<usize> = st.edges[v].iter().copied().collect();
        for w in neighbors {
            if st.index_of[w].is_none() {
                strongconnect(st, w);
                st.lowlink[v] = st.lowlink[v].min(st.lowlink[w]);
            } else if st.on_stack[w] {
                // `on_stack` was set together with `index_of`, so an
                // on-stack node is always indexed. Pattern-match for
                // structural exhaustiveness rather than `.expect()`,
                // which the compiler's clippy gate forbids in non-test
                // code.
                if let Some(w_index) = st.index_of[w] {
                    st.lowlink[v] = st.lowlink[v].min(w_index);
                } else {
                    unreachable!("Tarjan invariant: on_stack node has an index");
                }
            }
        }
        let v_index = match st.index_of[v] {
            Some(i) => i,
            None => unreachable!("Tarjan invariant: just-set index"),
        };
        if st.lowlink[v] == v_index {
            let mut comp: Vec<usize> = Vec::new();
            loop {
                let w = match st.stack.pop() {
                    Some(w) => w,
                    None => unreachable!("Tarjan invariant: SCC root must be on stack"),
                };
                st.on_stack[w] = false;
                comp.push(w);
                if w == v {
                    break;
                }
            }
            comp.sort();
            let scc_idx = st.sccs.len();
            for &node in &comp {
                st.scc_of[node] = scc_idx;
            }
            st.sccs.push(comp);
        }
    }

    let mut st = State {
        edges,
        index_counter: 0,
        index_of: vec![None; n],
        lowlink: vec![0; n],
        on_stack: vec![false; n],
        stack: Vec::new(),
        sccs: Vec::new(),
        scc_of: vec![0; n],
    };
    for v in 0..n {
        if st.index_of[v].is_none() {
            strongconnect(&mut st, v);
        }
    }
    (st.sccs, st.scc_of)
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

    use crate::ast::{Block as AstBlock, FnDecl, Item, Param, Program, TypeExpr};
    use crate::elaborate::AnfProgram;
    use crate::errors::Span;
    use crate::monomorphize::MonoProgram;
    use crate::typecheck::CheckedProgram;
    use std::collections::BTreeMap;

    fn span() -> Span {
        Span::new("test.sigil", 1, 1, 1, 1)
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

    fn synth_program(items: Vec<Item>) -> MonoProgram {
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
            call_site_instantiations: BTreeMap::new(),
            ctor_site_instantiations: BTreeMap::new(),
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
        let ping_reason = reason_of(&cp, "ping");
        assert!(
            ping_reason.contains("in SCC with cps member `pong`"),
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
    fn unused_param_warning_silenced() {
        // Sanity: synth_fn with a parameter list compiles without error.
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
}
