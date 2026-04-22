//! Elaboration to A-normal form — plan A1 Stage 1 task 8.
//!
//! Stage 1's surface programs are already near-ANF: `main`'s body is
//! statements followed by a tail expression that is an integer literal.
//! The only real flattening is `perform IO.println(literal)` — which
//! becomes `let _tmp = perform IO.println(literal); _tmp;`, except we keep
//! it as a statement (same shape) since `IO.println`'s result type is
//! `Unit` and we have nowhere useful to bind it at Stage 1.
//!
//! This module exists so the pipeline has a place to grow. Later plans
//! flatten nested expressions (arithmetic sub-expressions, nested calls)
//! into explicit lets; for Plan A1 it is a pass-through.

use crate::typecheck::CheckedProgram;

#[derive(Clone, Debug)]
pub struct AnfProgram {
    pub checked: CheckedProgram,
}

pub fn elaborate(checked: CheckedProgram) -> AnfProgram {
    // Stage 1 elaboration is identity: hello-world already fits ANF shape.
    // TODO(plan-a2): flatten arithmetic and nested calls here.
    AnfProgram { checked }
}
