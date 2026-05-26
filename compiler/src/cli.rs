//! CLI argument parsing — plan A1 Stage 1 task 3.
//!
//! Minimal hand-rolled parser; no external dependency because clap is not
//! on the allowed-dependencies list. The surface is intentionally small
//! and will grow with each plan.

use crate::errors::ErrorFormat;

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Compile(CompileArgs),
    Explain(String),
    PrintRuntimeStats(CompileArgs),
    /// Plan B Task 50 — `sigil <input> --dump-color`. Runs the front
    /// end through color inference and prints one line per monomorph
    /// to stdout, then exits without codegen. The output value `-o`
    /// is optional in this mode (color analysis produces no
    /// executable). Required for diagnosing performance-floor misses
    /// and for the Stage 5 color-decision review checkpoint.
    DumpColor(DumpColorArgs),
    /// Plan E3 Phase 1 — `sigil <input> --dump-discharge`. Runs the
    /// front end through color inference + Plan E3's per-call-site
    /// discharge analysis and prints one line per top-level-fn call
    /// site to stdout, then exits without codegen. Used to inventory
    /// `FullyDischarged` Cps-color call sites for the Phase-2
    /// activation review checkpoint. Same `-o` semantics as
    /// `--dump-color`: accepted for shell-history ergonomics, warned
    /// about because no executable is produced.
    DumpDischarge(DumpDischargeArgs),
    Usage,
    UsageError(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DumpColorArgs {
    pub input: String,
    pub error_format: ErrorFormat,
    /// Set when the user passed `-o <path>` alongside `--dump-color`.
    /// Color analysis emits no executable; the driver in `main.rs`
    /// uses this flag to print a stderr warning so the misuse is
    /// visible rather than silent.
    pub output_supplied: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DumpDischargeArgs {
    pub input: String,
    pub error_format: ErrorFormat,
    /// Mirrors [`DumpColorArgs::output_supplied`]: `-o` is accepted
    /// under `--dump-discharge` so users can re-use shell history with
    /// a `-o` already typed, but the path is recorded so `main.rs` can
    /// print a stderr warning. Discharge analysis emits no executable.
    pub output_supplied: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileArgs {
    pub input: String,
    pub output: String,
    pub error_format: ErrorFormat,
    /// When set, emit `<output>.symtab` next to the executable. The
    /// sidecar maps text-section offsets to demangled function names so
    /// the runtime profiler (`SIGIL_CPU_PROFILE`, `SIGIL_ALLOC_PROFILE`)
    /// can resolve sampled PCs. Default off — symtab emission is a
    /// profiling concern, not a default compile step.
    pub emit_symbol_table: bool,
}

pub fn parse(args: &[String]) -> Command {
    if args.is_empty() {
        return Command::Usage;
    }

    // `sigil explain <code>` takes the explain path.
    if args[0] == "explain" {
        return match args.get(1) {
            Some(code) => Command::Explain(code.clone()),
            None => Command::UsageError("explain: missing <code> argument".into()),
        };
    }

    // Compile modes:
    //   sigil [--print-runtime-stats] <input> -o <output> [--human-errors]
    //   sigil <input> --dump-color [--human-errors]
    let mut print_stats = false;
    let mut dump_color = false;
    let mut dump_discharge = false;
    let mut emit_symbol_table = false;
    let mut input: Option<String> = None;
    let mut output: Option<String> = None;
    let mut error_format = ErrorFormat::JsonLines;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => {
                i += 1;
                match args.get(i) {
                    Some(o) => output = Some(o.clone()),
                    None => return Command::UsageError("-o: missing <output> argument".into()),
                }
            }
            "--human-errors" => {
                error_format = ErrorFormat::Human;
            }
            "--print-runtime-stats" => {
                print_stats = true;
            }
            "--dump-color" => {
                dump_color = true;
            }
            "--dump-discharge" => {
                dump_discharge = true;
            }
            "--emit-symbol-table" => {
                emit_symbol_table = true;
            }
            arg if arg.starts_with("--") => {
                return Command::UsageError(format!("unknown flag `{arg}`"));
            }
            arg => {
                if input.is_none() {
                    input = Some(arg.to_string());
                } else {
                    return Command::UsageError(format!(
                        "unexpected positional argument `{arg}` (input already set)",
                    ));
                }
            }
        }
        i += 1;
    }

    let input = match input {
        Some(i) => i,
        None => return Command::UsageError("compile: missing <input.sigil>".into()),
    };

    if dump_color && dump_discharge {
        return Command::UsageError("--dump-color cannot be combined with --dump-discharge".into());
    }
    if dump_color {
        if print_stats {
            return Command::UsageError(
                "--dump-color cannot be combined with --print-runtime-stats".into(),
            );
        }
        if emit_symbol_table {
            return Command::UsageError(
                "--dump-color cannot be combined with --emit-symbol-table".into(),
            );
        }
        // `-o <path>` is accepted under --dump-color for shell-history
        // ergonomics, but color analysis emits no executable. The
        // driver in `main.rs` prints a stderr warning when this is
        // set so the misuse is visible.
        return Command::DumpColor(DumpColorArgs {
            input,
            error_format,
            output_supplied: output,
        });
    }

    if dump_discharge {
        if print_stats {
            return Command::UsageError(
                "--dump-discharge cannot be combined with --print-runtime-stats".into(),
            );
        }
        if emit_symbol_table {
            return Command::UsageError(
                "--dump-discharge cannot be combined with --emit-symbol-table".into(),
            );
        }
        return Command::DumpDischarge(DumpDischargeArgs {
            input,
            error_format,
            output_supplied: output,
        });
    }

    let output = match output {
        Some(o) => o,
        None => return Command::UsageError("compile: missing -o <output>".into()),
    };
    let compile_args = CompileArgs {
        input,
        output,
        error_format,
        emit_symbol_table,
    };
    if print_stats {
        Command::PrintRuntimeStats(compile_args)
    } else {
        Command::Compile(compile_args)
    }
}

pub const USAGE: &str = "\
usage:
    sigil <input.sigil> -o <output> [--human-errors] [--emit-symbol-table]
    sigil --print-runtime-stats <input.sigil> -o <output>
    sigil <input.sigil> --dump-color [--human-errors]
    sigil <input.sigil> --dump-discharge [--human-errors]
    sigil explain <code>

flags:
    -o <output>              Path for the compiled executable.
    --human-errors           Switch diagnostics from JSON Lines to human text.
    --print-runtime-stats    Compile, run, and print runtime counters at exit.
    --dump-color             Run color inference and print one line per monomorph
                             (`<name> native|cps <reason>`) to stdout. No codegen.
    --dump-discharge         Run color inference + per-call-site discharge analysis
                             (Plan E3 Phase 1) and print one line per top-level-fn
                             call site to stdout, with a trailing `# summary:` line.
                             No codegen.
    --emit-symbol-table      Write `<output>.symtab` next to the executable: one
                             tab-separated line per function symbol
                             (`<text_offset_hex>\\t<size_hex>\\t<demangled_name>`),
                             sorted by ascending text offset. Consumed by the
                             runtime profiler (SIGIL_CPU_PROFILE / SIGIL_ALLOC_PROFILE).

environment variables (compile-time):
    SIGIL_QUIET_AUTO_CPS_FALLBACK
                             If set to any non-empty value, suppresses the
                             `W0002` info diagnostic emitted when the auto-CPS
                             gate routes a body shape it can't lower to Sync ABI.
                             Run `sigil explain W0002` for the full diagnostic.
                             Use in batch builds that want clean stderr; the
                             demotion still happens, you just don't see the note.
    SIGIL_RUNTIME_LIB        Override the path to the runtime static library
                             linked into compiled binaries (default: bundled).
";

#[cfg(test)]
#[allow(clippy::disallowed_methods, clippy::disallowed_macros)]
mod tests {
    use super::*;

    fn parse_argv(words: &[&str]) -> Command {
        let v: Vec<String> = words.iter().map(|s| (*s).to_string()).collect();
        parse(&v)
    }

    #[test]
    fn compile_default_format() {
        let c = parse_argv(&["hello.sigil", "-o", "/tmp/hello"]);
        assert_eq!(
            c,
            Command::Compile(CompileArgs {
                input: "hello.sigil".into(),
                output: "/tmp/hello".into(),
                error_format: ErrorFormat::JsonLines,
                emit_symbol_table: false,
            })
        );
    }

    #[test]
    fn compile_human_errors() {
        let c = parse_argv(&["hello.sigil", "-o", "/tmp/hello", "--human-errors"]);
        let expected = Command::Compile(CompileArgs {
            input: "hello.sigil".into(),
            output: "/tmp/hello".into(),
            error_format: ErrorFormat::Human,
            emit_symbol_table: false,
        });
        assert_eq!(c, expected);
    }

    #[test]
    fn explain_path() {
        assert_eq!(
            parse_argv(&["explain", "E0010"]),
            Command::Explain("E0010".into())
        );
        assert_eq!(
            parse_argv(&["explain"]),
            Command::UsageError("explain: missing <code> argument".into()),
        );
    }

    #[test]
    fn missing_output_is_usage_error() {
        assert!(matches!(
            parse_argv(&["hello.sigil"]),
            Command::UsageError(_)
        ));
    }

    #[test]
    fn unknown_flag() {
        assert!(matches!(
            parse_argv(&["hello.sigil", "-o", "out", "--no-such-flag"]),
            Command::UsageError(_)
        ));
    }

    #[test]
    fn print_runtime_stats() {
        let c = parse_argv(&["--print-runtime-stats", "hello.sigil", "-o", "/tmp/hello"]);
        assert!(matches!(c, Command::PrintRuntimeStats(_)));
    }

    #[test]
    fn empty_is_usage() {
        assert_eq!(parse_argv(&[]), Command::Usage);
    }

    #[test]
    fn dump_color_default_format() {
        let c = parse_argv(&["hello.sigil", "--dump-color"]);
        assert_eq!(
            c,
            Command::DumpColor(DumpColorArgs {
                input: "hello.sigil".into(),
                error_format: ErrorFormat::JsonLines,
                output_supplied: None,
            })
        );
    }

    #[test]
    fn dump_color_with_human_errors() {
        let c = parse_argv(&["hello.sigil", "--dump-color", "--human-errors"]);
        assert_eq!(
            c,
            Command::DumpColor(DumpColorArgs {
                input: "hello.sigil".into(),
                error_format: ErrorFormat::Human,
                output_supplied: None,
            })
        );
    }

    #[test]
    fn dump_color_records_dash_o_for_warning() {
        // `-o` is accepted under --dump-color (shell-history
        // ergonomics) but the path is recorded so `main.rs` can warn
        // on stderr. The misuse is visible rather than silent.
        let c = parse_argv(&["hello.sigil", "-o", "/tmp/x", "--dump-color"]);
        match c {
            Command::DumpColor(args) => {
                assert_eq!(args.output_supplied.as_deref(), Some("/tmp/x"));
            }
            other => panic!("expected DumpColor, got {other:?}"),
        }
    }

    #[test]
    fn dump_color_conflicts_with_print_runtime_stats() {
        let c = parse_argv(&["hello.sigil", "--dump-color", "--print-runtime-stats"]);
        assert!(matches!(c, Command::UsageError(_)));
    }

    #[test]
    fn emit_symbol_table_default_off() {
        let c = parse_argv(&["hello.sigil", "-o", "/tmp/hello"]);
        match c {
            Command::Compile(args) => assert!(!args.emit_symbol_table),
            other => panic!("expected Compile, got {other:?}"),
        }
    }

    #[test]
    fn emit_symbol_table_flag_sets_field() {
        let c = parse_argv(&["hello.sigil", "-o", "/tmp/hello", "--emit-symbol-table"]);
        match c {
            Command::Compile(args) => assert!(args.emit_symbol_table),
            other => panic!("expected Compile, got {other:?}"),
        }
    }

    #[test]
    fn emit_symbol_table_conflicts_with_dump_color() {
        let c = parse_argv(&["hello.sigil", "--dump-color", "--emit-symbol-table"]);
        assert!(matches!(c, Command::UsageError(_)));
    }

    #[test]
    fn dump_discharge_default_format() {
        let c = parse_argv(&["hello.sigil", "--dump-discharge"]);
        assert_eq!(
            c,
            Command::DumpDischarge(DumpDischargeArgs {
                input: "hello.sigil".into(),
                error_format: ErrorFormat::JsonLines,
                output_supplied: None,
            })
        );
    }

    #[test]
    fn dump_discharge_with_human_errors() {
        let c = parse_argv(&["hello.sigil", "--dump-discharge", "--human-errors"]);
        assert_eq!(
            c,
            Command::DumpDischarge(DumpDischargeArgs {
                input: "hello.sigil".into(),
                error_format: ErrorFormat::Human,
                output_supplied: None,
            })
        );
    }

    #[test]
    fn dump_discharge_records_dash_o_for_warning() {
        let c = parse_argv(&["hello.sigil", "-o", "/tmp/x", "--dump-discharge"]);
        match c {
            Command::DumpDischarge(args) => {
                assert_eq!(args.output_supplied.as_deref(), Some("/tmp/x"));
            }
            other => panic!("expected DumpDischarge, got {other:?}"),
        }
    }

    #[test]
    fn dump_discharge_conflicts_with_dump_color() {
        let c = parse_argv(&["hello.sigil", "--dump-color", "--dump-discharge"]);
        assert!(matches!(c, Command::UsageError(_)));
    }

    #[test]
    fn dump_discharge_conflicts_with_print_runtime_stats() {
        let c = parse_argv(&["hello.sigil", "--dump-discharge", "--print-runtime-stats"]);
        assert!(matches!(c, Command::UsageError(_)));
    }

    #[test]
    fn dump_discharge_conflicts_with_emit_symbol_table() {
        let c = parse_argv(&["hello.sigil", "--dump-discharge", "--emit-symbol-table"]);
        assert!(matches!(c, Command::UsageError(_)));
    }
}
