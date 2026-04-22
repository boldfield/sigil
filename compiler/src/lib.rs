//! Sigil compiler library. The binary in `main.rs` is a thin CLI wrapper
//! over these modules. Exposing everything as a library simplifies testing
//! and keeps the CLI free of compiler logic.

pub mod errors;
