//! Monomorphization — Plan A1 stub.
//!
//! Hello-world has no generics. Later plans populate this module with
//! a reachability-bounded whole-program specialisation pass. See the
//! design doc's "Monomorphization is reachability-bounded" section.

use crate::elaborate::AnfProgram;

#[derive(Clone, Debug)]
pub struct MonoProgram {
    pub anf: AnfProgram,
}

pub fn monomorphize(anf: AnfProgram) -> MonoProgram {
    MonoProgram { anf }
}
