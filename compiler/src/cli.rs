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
    Usage,
    UsageError(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileArgs {
    pub input: String,
    pub output: String,
    pub error_format: ErrorFormat,
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

    // Compile modes: `sigil [--print-runtime-stats] <input> -o <output> [--human-errors]`
    let mut print_stats = false;
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
    let output = match output {
        Some(o) => o,
        None => return Command::UsageError("compile: missing -o <output>".into()),
    };
    let compile_args = CompileArgs {
        input,
        output,
        error_format,
    };
    if print_stats {
        Command::PrintRuntimeStats(compile_args)
    } else {
        Command::Compile(compile_args)
    }
}

pub const USAGE: &str = "\
usage:
    sigil <input.sigil> -o <output> [--human-errors]
    sigil --print-runtime-stats <input.sigil> -o <output>
    sigil explain <code>

flags:
    -o <output>              Path for the compiled executable.
    --human-errors           Switch diagnostics from JSON Lines to human text.
    --print-runtime-stats    Compile, run, and print runtime counters at exit.
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
}
