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
use crate::discharge;
use crate::elaborate;
use crate::errors::{CompilerError, DiagnosticEmitter, ErrorFormat};
use crate::imports;
use crate::lexer;
use crate::link;
use crate::monomorphize;
use crate::parser;
use crate::resolve;
use crate::symtab;
use crate::typecheck;

/// Compile an input file to an executable at `output`. Returns `Ok` with
/// the number of non-error diagnostics emitted, or `Err` with the error
/// count. Diagnostics themselves are emitted to stderr during the call.
///
/// When `emit_symbol_table` is true, also writes `<output>.symtab` next
/// to the executable: a sorted, tab-separated mapping from text-section
/// offsets to demangled function names, consumed by the runtime profiler.
pub fn compile(
    input: &str,
    output: &str,
    format: ErrorFormat,
    emit_symbol_table: bool,
) -> Result<usize, usize> {
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

    let (prog, import_errs) = imports::resolve(prog);
    all_errs.extend(import_errs);

    let (resolved, resolve_errs) = resolve::resolve(prog);
    all_errs.extend(resolve_errs);

    // Short-circuit before typecheck if any prior phase (lex / parse /
    // imports / resolve) errored. Typecheck's invariants assume a
    // resolved program — running it on AST that resolve already
    // rejected can trip debug_asserts (e.g. `env_insert`'s function-
    // wide uniqueness check) and panic in debug builds. Surfacing the
    // upstream errors first is both more useful and avoids the crash.
    if all_errs
        .iter()
        .any(|e| matches!(e.severity, crate::errors::Severity::Error))
    {
        emit_errors(&all_errs, format);
        return Err(all_errs.len());
    }

    let (checked, tc_errs) = typecheck::typecheck(resolved.program);
    all_errs.extend(tc_errs);

    // Early exit on any errors before we try codegen — invariants downstream
    // (e.g. "main exists", "IO.println arg is a String") are only valid on
    // clean type-checks.
    if all_errs
        .iter()
        .any(|e| matches!(e.severity, crate::errors::Severity::Error))
    {
        emit_errors(&all_errs, format);
        return Err(all_errs.len());
    }

    let anf = elaborate::elaborate(checked);
    let mono = monomorphize::monomorphize(anf);
    let colored = color::infer_colors(mono);
    let cc = closure_convert::convert(colored);

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

    if emit_symbol_table {
        let sidecar = PathBuf::from(format!("{output}.symtab"));
        if let Err(e) = symtab::write_for_binary(std::path::Path::new(output), &sidecar) {
            eprintln!("sigil: --emit-symbol-table: {e}");
            return Err(1);
        }
    }

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

/// Failure modes for the read-only diagnostic pipelines ([`dump_color`],
/// [`dump_discharge`]). The driver in `main.rs` maps every variant to a
/// non-zero exit code; downstream tooling can branch on the variant for
/// clearer diagnostics.
///
/// Pre-PR-#175 this was named `DumpColorError` and only served
/// `dump_color`. PR #175 added `dump_discharge` and extracted the
/// shared front-end helper [`run_frontend_to_color`]; renamed to
/// `PipelineError` so the type matches its post-extraction role.
#[derive(Debug)]
pub enum PipelineError {
    /// `std::fs::read_to_string` failed (missing file, permission
    /// denied, etc.). The underlying error is already printed to
    /// stderr by the caller before returning.
    ReadFailed,
    /// At least one front-end error fired (lex / parse / resolve /
    /// typecheck). The carried `usize` is the total error count for
    /// telemetry; diagnostics are already on stderr.
    FrontEndErrors(usize),
}

/// Backwards-compat alias for callers that still reference the
/// pre-rename type. New code uses [`PipelineError`] directly.
#[deprecated(note = "renamed to PipelineError; will be removed once external callers update")]
pub type DumpColorError = PipelineError;

/// Run the front-end pipeline (lex → parse → imports → resolve →
/// typecheck → elaborate → monomorphize → color) and return the
/// [`color::ColoredProgram`]. Shared between [`dump_color`] and
/// [`dump_discharge`] (and any future read-only diagnostic that wants
/// the same input shape). Front-end errors emit on stderr and
/// short-circuit with [`PipelineError`].
///
/// This helper does NOT cover codegen — [`compile`] keeps its own
/// inline pipeline because it also threads the output path through
/// link + symtab emission. Extracting a shared helper for `compile`
/// would require a larger refactor outside Phase 1 scope.
fn run_frontend_to_color(
    input: &str,
    format: ErrorFormat,
) -> Result<color::ColoredProgram, PipelineError> {
    let src = match std::fs::read_to_string(input) {
        Ok(s) => s,
        Err(e) => {
            let stderr = std::io::stderr();
            let mut out = stderr.lock();
            let _ = writeln!(out, "sigil: cannot read `{input}`: {e}");
            return Err(PipelineError::ReadFailed);
        }
    };

    let mut all_errs: Vec<CompilerError> = Vec::new();

    let (tokens, lex_errs) = lexer::lex(input, &src);
    all_errs.extend(lex_errs);

    let (prog, parse_errs) = parser::parse(input, &tokens);
    all_errs.extend(parse_errs);

    let (prog, import_errs) = imports::resolve(prog);
    all_errs.extend(import_errs);

    let (resolved, resolve_errs) = resolve::resolve(prog);
    all_errs.extend(resolve_errs);

    // Same guard as the main pipeline path — don't run typecheck on
    // AST that resolve has already rejected; the function-wide
    // uniqueness debug_assert in `env_insert` would panic.
    if all_errs
        .iter()
        .any(|e| matches!(e.severity, crate::errors::Severity::Error))
    {
        let n = all_errs.len();
        emit_errors(&all_errs, format);
        return Err(PipelineError::FrontEndErrors(n));
    }

    let (checked, tc_errs) = typecheck::typecheck(resolved.program);
    all_errs.extend(tc_errs);

    if all_errs
        .iter()
        .any(|e| matches!(e.severity, crate::errors::Severity::Error))
    {
        let n = all_errs.len();
        emit_errors(&all_errs, format);
        return Err(PipelineError::FrontEndErrors(n));
    }

    let anf = elaborate::elaborate(checked);
    let mono = monomorphize::monomorphize(anf);
    let colored = color::infer_colors(mono);
    emit_errors(&all_errs, format);
    Ok(colored)
}

/// Plan B Task 50 — `--dump-color`. Runs the front end through color
/// inference and returns the rendered dump as a `String`. Front-end
/// errors emit as usual on stderr and short-circuit with a typed
/// [`PipelineError`].
pub fn dump_color(input: &str, format: ErrorFormat) -> Result<String, PipelineError> {
    let colored = run_frontend_to_color(input, format)?;
    Ok(color::dump_color(&colored))
}

/// Plan E3 Phase 1 — `--dump-discharge`. Runs the front end through
/// color inference and Plan E3's per-call-site discharge analysis;
/// returns the rendered dump as a `String`. Same failure modes as
/// [`dump_color`].
pub fn dump_discharge(input: &str, format: ErrorFormat) -> Result<String, PipelineError> {
    let colored = run_frontend_to_color(input, format)?;
    let analysis = discharge::analyze(&colored);
    Ok(discharge::dump_discharge(&analysis))
}
