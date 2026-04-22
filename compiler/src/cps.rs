//! CPS transform — plan A1 Stage 1 task 10.
//!
//! Stage 1 is near-identity. No user handlers exist yet; the only effect
//! operation `IO.println` is a runtime intrinsic that the Stage-1 codegen
//! lowers to a direct `sigil_println` call.
//!
//! TODO(plan-b): when effect handlers ship, remove the `IO` runtime-
//! intrinsic special-case in codegen and handle `perform` through the
//! general CPS transform produced here.

use crate::color::ColoredProgram;

#[derive(Clone, Debug)]
pub struct CpsProgram {
    pub colored: ColoredProgram,
}

pub fn transform(colored: ColoredProgram) -> CpsProgram {
    // TODO(plan-b): remove IO special-case once effect runtime is general.
    CpsProgram { colored }
}
