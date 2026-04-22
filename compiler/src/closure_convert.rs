//! Closure conversion — plan A1 Stage 1 task 11.
//!
//! Hello-world's `main` captures nothing; Stage-1 closure conversion is an
//! identity pass that assigns every function an empty capture set. The
//! module exists so Plan B can grow real capture analysis without
//! restructuring the pipeline.

use crate::cps::CpsProgram;

#[derive(Clone, Debug)]
pub struct ClosureConvertedProgram {
    pub cps: CpsProgram,
    /// Empty capture sets in Stage 1.
    pub captures: Vec<(String, Vec<String>)>,
}

pub fn convert(cps: CpsProgram) -> ClosureConvertedProgram {
    use crate::ast::Item;
    let mut captures = Vec::new();
    for item in &cps.colored.mono.anf.checked.program.items {
        if let Item::Fn(f) = item {
            captures.push((f.name.clone(), Vec::new()));
        }
    }
    ClosureConvertedProgram { cps, captures }
}
