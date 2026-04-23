//! End-to-end tests — plan A1 Stage 1 task 16, extended in plan A2
//! task 24.
//!
//! Original (Stage 1): compiles `examples/hello.sigil` with the `sigil`
//! binary, runs the resulting program, and asserts stdout == "hello,
//! world\n" with exit code 0.
//!
//! Plan A2 additions (Stage 2):
//!
//! - `arith_integer_ops` — compiles a program exercising `+ - * /` and
//!   asserts the exit code matches the expected arithmetic result.
//! - `if_else_produces_value` — verifies `if/else` with a Bool
//!   condition lowers correctly (desugared via elaborate to a `match`,
//!   lowered in codegen to `brif` blocks).
//! - `match_primitive_with_wildcard` — exercises the codegen-`match`
//!   chain with IntLit patterns + a wildcard arm.
//! - `div_by_zero_traps` — asserts the runtime trap prints `sigil:
//!   arithmetic error: division by zero` to stderr and exits with
//!   status 2.
//!
//! All Stage-2 programs are small (a dozen lines) and inlined in the
//! test source rather than added to `examples/`; Task 26 introduces
//! `examples/factorial.sigil` / `arith.sigil` / `div_by_zero.sigil` as
//! the "real" examples.

// `expect`/`unwrap`/`panic!` are fine in tests; the workspace clippy
// rule bans them in compiler source so user-facing errors route through
// `CompilerError`. Test-module code is exempted per plan task 0.2.
#![allow(clippy::disallowed_methods, clippy::disallowed_macros)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// Workspace root — `compiler/tests/` is two levels deep.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler/ has a parent (workspace root)")
        .to_path_buf()
}

/// Cargo builds `sigil-runtime` as an rlib when it's pulled in as a
/// dev-dep of `sigil-compiler`, but `link.rs` links user programs
/// against the **staticlib** (`libsigil_runtime.a`) which may not be
/// present on a cold `cargo test --workspace`. Check here; if missing,
/// invoke `cargo build -p sigil-runtime` at the matching profile.
///
/// Safe at test-run time: the outer cargo has finished its build phase
/// and released the per-build-unit locks, so the nested cargo acquires
/// its own locks without deadlock. (Earlier revisions of this plan
/// attempted the same rebuild from `compiler/build.rs`; that deadlocked
/// on a cold `cargo test --workspace` because the outer cargo still
/// held locks during build-script execution. See PLAN_A2_DEVIATIONS.md
/// [Task 1.5.5] for the detailed history.)
fn ensure_runtime_staticlib(root: &Path, sigil_bin: &Path) {
    // Detect the profile from the `sigil` binary's path
    // (`target/<profile>/sigil`). Default to debug if nothing recognizable
    // is found.
    let profile = sigil_bin
        .ancestors()
        .find_map(|a| match a.file_name().and_then(|s| s.to_str()) {
            Some("debug") => Some("debug"),
            Some("release") => Some("release"),
            _ => None,
        })
        .unwrap_or("debug");

    let staticlib = root.join("target").join(profile).join("libsigil_runtime.a");
    if staticlib.exists() {
        return;
    }

    // Invoke cargo to materialise the staticlib. `CARGO` is set in the
    // env by cargo for child processes; fall back to the PATH name if
    // unset (e.g. when running the test binary directly from disk).
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let mut cmd = Command::new(cargo);
    cmd.arg("build").arg("-p").arg("sigil-runtime");
    if profile == "release" {
        cmd.arg("--release");
    }
    cmd.current_dir(root);

    let status = cmd
        .status()
        .expect("failed to invoke cargo for sigil-runtime staticlib build");
    assert!(
        status.success(),
        "sigil-runtime staticlib build failed (exit {status})"
    );
    assert!(
        staticlib.exists(),
        "staticlib {} not produced after `cargo build -p sigil-runtime`",
        staticlib.display()
    );
}

#[test]
fn hello() {
    let root = workspace_root();
    let sigil_bin = PathBuf::from(env!("CARGO_BIN_EXE_sigil"));
    ensure_runtime_staticlib(&root, &sigil_bin);
    let source = root.join("examples/hello.sigil");
    let out_path = std::env::temp_dir().join(format!("sigil_e2e_hello_{}", std::process::id(),));

    // Invoke the compiler from the workspace root so the linker can
    // find target/<profile>/libsigil_runtime.a via its relative-path
    // lookup.
    let compile = Command::new(&sigil_bin)
        .arg(&source)
        .arg("-o")
        .arg(&out_path)
        .current_dir(&root)
        .output()
        .expect("failed to invoke sigil compiler");
    assert!(
        compile.status.success(),
        "sigil compilation failed: stdout={} stderr={}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr),
    );

    let run = Command::new(&out_path)
        .output()
        .expect("failed to execute compiled hello binary");
    assert!(
        run.status.success(),
        "compiled hello exited with {} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr),
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "hello, world\n",
        "hello-world stdout mismatch",
    );

    // Best-effort cleanup; ignore errors.
    let _ = std::fs::remove_file(&out_path);
}

/// Compile hello.sigil and compile-only (no link), then inspect the .o
/// file's stackmap section bytes and parse them via the runtime's
/// parser. Asserts the v0 placeholder invariants: magic, version = 0,
/// every record flagged placeholder, live_count = 0.
#[test]
fn stackmap_section_parses_v0_placeholder() {
    use sigil_compiler::{
        closure_convert, codegen, color, cps, elaborate, lexer, monomorphize, parser, resolve,
        typecheck,
    };
    use sigil_runtime::stackmap::{
        parse_section, ParseError, STACKMAP_FLAG_PLACEHOLDER, STACKMAP_VERSION_PLACEHOLDER,
    };

    let root = workspace_root();
    let src = std::fs::read_to_string(root.join("examples/hello.sigil")).expect("read hello.sigil");

    let (toks, lex_errs) = lexer::lex("hello.sigil", &src);
    assert!(lex_errs.is_empty(), "lex errors: {lex_errs:?}");
    let (prog, parse_errs) = parser::parse("hello.sigil", &toks);
    assert!(parse_errs.is_empty(), "parse errors: {parse_errs:?}");
    let (rp, resolve_errs) = resolve::resolve(prog);
    assert!(resolve_errs.is_empty(), "resolve errors: {resolve_errs:?}");
    let (checked, tc_errs) = typecheck::typecheck(rp.program);
    assert!(tc_errs.is_empty(), "tc errors: {tc_errs:?}");
    let anf = elaborate::elaborate(checked);
    let mono = monomorphize::monomorphize(anf);
    let colored = color::infer_colors(mono);
    let cps_ir = cps::transform(colored);
    let cc = closure_convert::convert(cps_ir);

    let obj_path =
        std::env::temp_dir().join(format!("sigil_e2e_stackmap_{}.o", std::process::id()));
    codegen::emit_object(&cc, &obj_path).expect("emit_object");

    let bytes = std::fs::read(&obj_path).expect("read object file");

    // The object-file section we wrote is tagged `__SIGIL,__stackmaps`
    // (Mach-O) or `.sigil_stackmaps` (ELF). Rather than re-parse the
    // enclosing object format here, locate the section by searching for
    // the magic bytes — which are anchored inside the section we wrote,
    // and collision with generated code is vanishingly unlikely for a
    // 4-byte ASCII pattern ("SGST") followed by a zero version word.
    let needle: &[u8] = &[b'S', b'G', b'S', b'T', 0, 0, 0, 0];
    let pos = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("stackmap magic+version not found in object file");
    let section = &bytes[pos..];
    let parsed = parse_section(section).unwrap_or_else(|e: ParseError| {
        panic!("stackmap parse failed: {e:?}");
    });
    assert_eq!(parsed.version, STACKMAP_VERSION_PLACEHOLDER);
    // hello.sigil's main body hits four call sites: gc_init, user_main,
    // sigil_string_new, sigil_println.
    assert_eq!(parsed.records.len(), 4, "expected 4 placeholder records");
    for r in &parsed.records {
        assert_eq!(r.live_count, 0, "v0 invariant: live_count always 0");
        assert_eq!(
            r.flags & STACKMAP_FLAG_PLACEHOLDER,
            STACKMAP_FLAG_PLACEHOLDER,
            "v0 invariant: placeholder flag set on every record",
        );
    }

    let _ = std::fs::remove_file(&obj_path);
}

/// Compile the given Sigil source to a temp binary and run it.
/// Returns `(stdout, stderr, exit_code)` from the child process. The
/// temp files are cleaned up before returning.
///
/// Panics on compile failure; the caller should pre-validate the
/// program is expected to compile. For programs expected to *run* and
/// exit with a non-zero code (e.g. the div-by-zero trap), check
/// `exit_code` against the expected sentinel.
fn compile_and_run(source: &str, test_name: &str) -> (String, String, i32) {
    let root = workspace_root();
    let sigil_bin = PathBuf::from(env!("CARGO_BIN_EXE_sigil"));
    ensure_runtime_staticlib(&root, &sigil_bin);

    let src_path = std::env::temp_dir().join(format!(
        "sigil_e2e_{}_{}.sigil",
        test_name,
        std::process::id()
    ));
    let bin_path =
        std::env::temp_dir().join(format!("sigil_e2e_{}_{}", test_name, std::process::id()));
    std::fs::write(&src_path, source).expect("write source");

    let compile = Command::new(&sigil_bin)
        .arg(&src_path)
        .arg("-o")
        .arg(&bin_path)
        .current_dir(&root)
        .output()
        .expect("failed to invoke sigil compiler");
    assert!(
        compile.status.success(),
        "compile failed for {test_name}: stdout={} stderr={}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr),
    );

    let run = Command::new(&bin_path)
        .output()
        .expect("failed to execute compiled binary");

    // Use exit code 0..=255 as per Unix; fallback to -1 on signal.
    let code = run.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&run.stderr).into_owned();

    let _ = std::fs::remove_file(&src_path);
    let _ = std::fs::remove_file(&bin_path);

    (stdout, stderr, code)
}

/// Plan A2 task 24 — `iadd`/`isub`/`imul`/`sdiv` codegen.
///
/// `(10 + 20 * 2) / 2 = 25`. The program returns the result as the
/// process exit status; Unix masks to the low 8 bits, so the test
/// asserts `25 & 0xff == 25`.
#[test]
fn arith_integer_ops() {
    let source = "fn main() -> Int ![] {\n\
                    let n: Int = (10 + 20 * 2) / 2;\n\
                    n\n\
                  }\n";
    let (_stdout, _stderr, code) = compile_and_run(source, "arith");
    assert_eq!(code, 25, "arith exit code");
}

/// Plan A2 task 24 — `if`/`else` lowering. Elaborate desugars the
/// `if` into a `match`-on-`Bool`; codegen emits compare + `brif` to
/// two arm bodies joining at a continue block.
#[test]
fn if_else_produces_value() {
    let source = "fn main() -> Int ![] {\n\
                    let n: Int = 5;\n\
                    let r: Int = if n > 0 { n * 2 } else { -n };\n\
                    r\n\
                  }\n";
    let (_stdout, _stderr, code) = compile_and_run(source, "if_else");
    assert_eq!(code, 10, "n=5 → n*2 = 10");
}

/// Plan A2 task 24 — `match` chain with IntLit patterns and a
/// wildcard. Codegen emits a compare + `brif` per literal pattern, a
/// wildcard jump for the final arm, and a continue block that
/// produces the arm's body value.
#[test]
fn match_primitive_with_wildcard() {
    let source = "fn main() -> Int ![] {\n\
                    let n: Int = 2;\n\
                    let r: Int = match n {\n\
                      0 => 100,\n\
                      1 => 50,\n\
                      _ => 17,\n\
                    };\n\
                    r\n\
                  }\n";
    let (_stdout, _stderr, code) = compile_and_run(source, "match");
    assert_eq!(code, 17, "n=2 hits wildcard → 17");
}

/// Plan A2 task 24 + task 25 — division by zero traps via
/// `sigil_panic_arith_error`. Exit code is 2; stderr contains the
/// runtime's arith-error banner.
#[test]
fn div_by_zero_traps() {
    let source = "fn main() -> Int ![] {\n\
                    let a: Int = 10;\n\
                    let b: Int = 0;\n\
                    let q: Int = a / b;\n\
                    q\n\
                  }\n";
    let (_stdout, stderr, code) = compile_and_run(source, "div_by_zero");
    assert_eq!(code, 2, "div-by-zero exits with 2");
    assert!(
        stderr.contains("division by zero"),
        "stderr missing arith-error banner: {stderr:?}"
    );
    assert!(
        stderr.contains("sigil: arithmetic error"),
        "stderr missing `sigil: arithmetic error` prefix: {stderr:?}"
    );
}

/// Plan A2 task 24 + task 25 — modulo by zero takes the same trap
/// path with a different reason string.
#[test]
fn mod_by_zero_traps() {
    let source = "fn main() -> Int ![] {\n\
                    let a: Int = 10;\n\
                    let b: Int = 0;\n\
                    let r: Int = a % b;\n\
                    r\n\
                  }\n";
    let (_stdout, stderr, code) = compile_and_run(source, "mod_by_zero");
    assert_eq!(code, 2, "mod-by-zero exits with 2");
    assert!(
        stderr.contains("remainder by zero"),
        "stderr missing mod-zero banner: {stderr:?}"
    );
}
