//! Color inference — plan A1 Stage 1 task 9.
//!
//! Stage 1 tags every function as `Native`; there are no effect-crossing
//! calls yet. Plan B replaces this with real analysis: a function is CPS
//! iff its effect row contains any non-top-level handler, or it calls a
//! CPS function.

use crate::monomorphize::MonoProgram;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Native,
    Cps,
}

#[derive(Clone, Debug)]
pub struct ColoredProgram {
    pub mono: MonoProgram,
    /// Color per function name. Plan A1 always `Native`; Plan B grows.
    pub colors: Vec<(String, Color)>,
}

pub fn infer_colors(mono: MonoProgram) -> ColoredProgram {
    use crate::ast::Item;
    let mut colors = Vec::new();
    for item in &mono.anf.checked.program.items {
        if let Item::Fn(f) = item {
            colors.push((f.name.clone(), Color::Native));
        }
    }
    ColoredProgram { mono, colors }
}
