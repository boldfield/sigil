//! Compile-pipeline orchestration — plan A1 Stage 1 task 3.
//!
//! Threads the Stage-1 subset through lexer → parser → resolve →
//! typecheck → elaborate → monomorphize → color → cps → closure_convert
//! → codegen → link. Errors flow through `CompilerError` with JSON Lines
//! or human output per the `ErrorFormat` the caller picks.

use std::io::Write;
use std::path::PathBuf;

use crate::closure_convert;
use crate::codegen;
use crate::color;
use crate::cps;
use crate::elaborate;
use crate::errors::{CompilerError, DiagnosticEmitter, ErrorFormat};
use crate::lexer;
use crate::link;
use crate::monomorphize;
use crate::parser;
use crate::resolve;
use crate::typecheck;

/// Compile an input file to an executable at `output`. Returns `Ok` with
/// the number of non-error diagnostics emitted, or `Err` with the error
/// count. Diagnostics themselves are emitted to stderr during the call.
pub fn compile(input: &str, output: &str, format: ErrorFormat) -> Result<usize, usize> {
    let src = match std::fs::read_to_string(input) {
        Ok(s) => s,
        Err(e) => {
            let stderr = std::io::stderr();
            let mut out = stderr.lock();
            let _ = writeln!(out, "sigil: cannot read `{input}`: {e}");
            return Err(1);
        }
    };

    let mut all_errs: Vec<CompilerError> = Vec::new();

    let (tokens, lex_errs) = lexer::lex(input, &src);
    all_errs.extend(lex_errs);

    let (prog, parse_errs) = parser::parse(input, &tokens);
    all_errs.extend(parse_errs);

    let (resolved, resolve_errs) = resolve::resolve(prog);
    all_errs.extend(resolve_errs);

    let (checked, tc_errs) = typecheck::typecheck(resolved.program);
    all_errs.extend(tc_errs);

    // Early exit on any errors before we try codegen — invariants downstream
    // (e.g. "main exists", "IO.println arg is a String") are only valid on
    // clean type-checks.
    if all_errs.iter().any(|e| matches!(e.severity, crate::errors::Severity::Error)) {
        emit_errors(&all_errs, format);
        return Err(all_errs.len());
    }

    let anf = elaborate::elaborate(checked);
    let mono = monomorphize::monomorphize(anf);
    let colored = color::infer_colors(mono);
    let cps_ir = cps::transform(colored);
    let cc = closure_convert::convert(cps_ir);

    // Emit object file to a temp location alongside the output.
    let obj_path = PathBuf::from(format!("{output}.o"));
    if let Err(e) = codegen::emit_object(&cc, &obj_path) {
        eprintln!("sigil: codegen failed: {e}");
        return Err(1);
    }
    if let Err(e) = link::link(&obj_path, std::path::Path::new(output)) {
        eprintln!("sigil: link failed: {e}");
        return Err(1);
    }
    let _ = std::fs::remove_file(&obj_path);

    emit_errors(&all_errs, format);
    Ok(all_errs.len())
}

fn emit_errors(errs: &[CompilerError], format: ErrorFormat) {
    let stderr = std::io::stderr();
    let mut em = DiagnosticEmitter::new(stderr.lock(), format);
    for e in errs {
        let _ = em.emit(e);
    }
}
