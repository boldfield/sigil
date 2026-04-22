use std::io::Write;
use std::process::ExitCode;

use sigil_compiler::errors::catalog;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [cmd, code] if cmd == "explain" => explain(code),
        _ => {
            // Compile mode and the full flag surface (-o, --human-errors, etc.)
            // are wired in Stage 1 task 3. For now report cleanly so nothing
            // upstream mistakes this stub for the real compiler.
            eprintln!("sigil: compile pipeline not yet wired; Stage 1 task 3 pending.");
            eprintln!("usage: sigil explain <code>");
            ExitCode::from(2)
        }
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
