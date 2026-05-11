//! Sigil compiler library. The binary in `main.rs` is a thin CLI wrapper
//! over these modules. Exposing everything as a library simplifies testing
//! and keeps the CLI free of compiler logic.
//!
//! Stage 1 pipeline modules:
//!   lexer -> parser -> resolve -> typecheck -> elaborate -> color -> cps
//!   -> closure_convert -> codegen -> link.
//!
//! Each module carries a doc header describing its Stage-1 scope (often
//! "near-identity for hello-world") and a TODO for Plan B's real work.

pub mod ast;
pub mod cli;
pub mod closure_convert;
pub mod codegen;
pub mod color;
pub mod elaborate;
pub mod errors;
pub mod imports;
pub mod layout;
pub mod lexer;
pub mod link;
pub mod monomorphize;
pub mod parser;
pub mod pipeline;
pub mod resolve;
pub mod stdlib_embed;
pub mod stdlib_index;
pub mod symtab;
pub mod typecheck;
