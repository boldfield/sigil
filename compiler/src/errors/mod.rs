pub mod catalog;
pub mod diagnostic;

pub use catalog::{lookup, ErrorCode, ErrorEntry, CATALOG};
pub use diagnostic::{CompilerError, DiagnosticEmitter, ErrorFormat, Severity, Span};
