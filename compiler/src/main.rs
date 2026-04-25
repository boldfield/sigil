use std::io::Write;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use sigil_compiler::cli::{self, CompileArgs, DumpColorArgs};
use sigil_compiler::errors::catalog;
use sigil_compiler::pipeline;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match cli::parse(&args) {
        cli::Command::Compile(cargs) => compile(cargs),
        cli::Command::PrintRuntimeStats(cargs) => print_runtime_stats(cargs),
        cli::Command::DumpColor(dargs) => dump_color(dargs),
        cli::Command::Explain(code) => explain(&code),
        cli::Command::Usage => {
            eprintln!("{}", cli::USAGE);
            ExitCode::from(2)
        }
        cli::Command::UsageError(msg) => {
            eprintln!("sigil: {msg}");
            eprintln!();
            eprintln!("{}", cli::USAGE);
            ExitCode::from(2)
        }
    }
}

fn compile(cargs: CompileArgs) -> ExitCode {
    match pipeline::compile(&cargs.input, &cargs.output, cargs.error_format) {
        Ok(_) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(1),
    }
}

fn print_runtime_stats(cargs: CompileArgs) -> ExitCode {
    let compile_status = compile(CompileArgs { ..cargs.clone() });
    if compile_status != ExitCode::SUCCESS {
        return compile_status;
    }
    // Run the compiled program. Exit status mirrors the inner program; the
    // counters dump goes to stderr via `sigil_counter_print_all`. For Stage
    // 1 we invoke the program then read `/proc/self/...` — actually the
    // counters are process-local to the child. We run the child with
    // SIGIL_PRINT_STATS=1; the runtime checks this env var on startup and
    // registers an atexit hook that calls sigil_counter_print_all. Stage 1
    // wires the atexit via a small inline shim in the runtime module.
    let prog = Path::new(&cargs.output);
    let mut cmd = Command::new(prog);
    cmd.env("SIGIL_PRINT_STATS", "1");
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    match cmd.status() {
        Ok(s) => match s.code() {
            Some(c) => ExitCode::from(c as u8),
            None => ExitCode::from(1),
        },
        Err(e) => {
            eprintln!("sigil: cannot run compiled binary: {e}");
            ExitCode::from(1)
        }
    }
}

fn dump_color(dargs: DumpColorArgs) -> ExitCode {
    if let Some(path) = &dargs.output_supplied {
        let stderr = std::io::stderr();
        let mut err = stderr.lock();
        let _ = writeln!(
            err,
            "sigil: warning: `-o {path}` ignored under --dump-color (no executable produced)"
        );
    }
    match pipeline::dump_color(&dargs.input, dargs.error_format) {
        Ok(text) => {
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            let _ = out.write_all(text.as_bytes());
            ExitCode::SUCCESS
        }
        Err(_) => ExitCode::from(1),
    }
}

fn explain(code: &str) -> ExitCode {
    match catalog::lookup(code) {
        Some(entry) => {
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{} — {}", entry.code, entry.short);
            let _ = writeln!(out);
            let _ = writeln!(out, "{}", entry.long);
            let _ = writeln!(out);
            let _ = writeln!(out, "Example fix:");
            for line in entry.fix_example.lines() {
                let _ = writeln!(out, "    {line}");
            }
            ExitCode::SUCCESS
        }
        None => {
            eprintln!("sigil explain: unknown code `{code}`");
            ExitCode::from(1)
        }
    }
}

// Make CompileArgs cloneable for print-runtime-stats reuse.
// (Defined in cli.rs — this is a small forward-compat helper.)
