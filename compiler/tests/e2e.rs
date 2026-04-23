//! End-to-end tests — plan A1 Stage 1 task 16, extended in plan A2
//! tasks 24 and 26.
//!
//! Stage 1 task 16: compiles `examples/hello.sigil` with the `sigil`
//! binary, runs the resulting program, and asserts
//! `stdout == "hello, world\n"` with exit code 0.
//!
//! Stage 1 task 0.11: compiles `examples/hello.sigil` to an object
//! file and asserts the stackmap section round-trips through the
//! runtime's parser with the v0 placeholder invariants.
//!
//! Plan A2 task 24: codegen extensions for Stage-2 arithmetic,
//! `if`/`else`, `match`, and the divide-by-zero trap. Exercised via
//! the `if_else_produces_value`, `match_primitive_with_wildcard`, and
//! `mod_by_zero_traps` tests (inline-source programs — cheaper to
//! maintain than dedicated example files, and orthogonal to the
//! canonical Task-26 examples).
//!
//! Plan A2 task 26: `examples/arith.sigil` and
//! `examples/div_by_zero.sigil` ship as canonical user-facing examples
//! of Stage-2 arithmetic. Two e2e tests (`arith_example_exits_26`,
//! `div_by_zero_example_traps`) compile and run those files from disk.
//!
//! Every test that invokes the `sigil` binary goes through
//! [`sigil_binary`]. The helper wraps `env!("CARGO_BIN_EXE_sigil")`
//! plus [`ensure_runtime_staticlib`] behind a `std::sync::Once` so
//! multiple concurrent e2e tests cannot race on the nested
//! `cargo build -p sigil-runtime` invocation, and so a test author
//! cannot forget to ensure the staticlib exists before invoking the
//! compiler.

// `expect`/`unwrap`/`panic!` are fine in tests; the workspace clippy
// rule bans them in compiler source so user-facing errors route through
// `CompilerError`. Test-module code is exempted per plan task 0.2.
#![allow(clippy::disallowed_methods, clippy::disallowed_macros)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;

/// Workspace root — `compiler/tests/` is two levels deep.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler/ has a parent (workspace root)")
        .to_path_buf()
}

/// Returns the path to the compiled `sigil` binary and — on the first
/// call — ensures `libsigil_runtime.a` is present under
/// `target/<profile>/` so `link.rs` can find it.
///
/// Every e2e test that invokes the compiler must route through this
/// helper. A test author who writes
/// `Command::new(env!("CARGO_BIN_EXE_sigil"))` directly bypasses the
/// staticlib check and risks a link-time failure on cold CI runs; the
/// helper makes the omission syntactically impossible.
///
/// The inner `Once` guard is the reviewer-requested (PR #2)
/// serialisation of the nested `cargo build -p sigil-runtime` call:
/// without it, two concurrent e2e tests can both observe
/// `staticlib.exists() == false` and both spawn a cargo subprocess,
/// doubling cold-CI wall time. Cargo's target-dir lock prevents
/// deadlock but not the wasted work. Plan A2 task 26 pick-up from
/// PR #2 review.
fn sigil_binary() -> PathBuf {
    static INIT: Once = Once::new();
    let sigil_bin = PathBuf::from(env!("CARGO_BIN_EXE_sigil"));
    INIT.call_once(|| {
        ensure_runtime_staticlib(&workspace_root(), &sigil_bin);
    });
    sigil_bin
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
///
/// Only called from [`sigil_binary`]'s `Once` init; parallel callers
/// wait on `Once::call_once` and no subprocess race occurs.
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

/// Compile a Sigil source file to a temp binary and run it. Returns
/// `(stdout, stderr, exit_code)` from the child process. Temp output
/// files are cleaned up before returning. `source_path` is passed
/// through to the compiler as-is; the caller is responsible for
/// producing a valid `.sigil` file on disk.
///
/// Panics on compile failure; callers that expect compilation to fail
/// should instead drive the compiler by hand.
fn compile_file_and_run(source_path: &Path, test_name: &str) -> (String, String, i32) {
    let root = workspace_root();
    let sigil_bin = sigil_binary();

    let bin_path =
        std::env::temp_dir().join(format!("sigil_e2e_{}_{}", test_name, std::process::id()));

    let compile = Command::new(&sigil_bin)
        .arg(source_path)
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

    let code = run.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&run.stderr).into_owned();

    let _ = std::fs::remove_file(&bin_path);

    (stdout, stderr, code)
}

/// Write `source` to a temp `.sigil` file and run
/// [`compile_file_and_run`] on it. Convenience for tests whose source
/// is an inline string literal.
fn compile_and_run(source: &str, test_name: &str) -> (String, String, i32) {
    let src_path = std::env::temp_dir().join(format!(
        "sigil_e2e_{}_{}.sigil",
        test_name,
        std::process::id()
    ));
    std::fs::write(&src_path, source).expect("write source");
    let out = compile_file_and_run(&src_path, test_name);
    let _ = std::fs::remove_file(&src_path);
    out
}

#[test]
fn hello() {
    let root = workspace_root();
    let source = root.join("examples/hello.sigil");
    let (stdout, stderr, code) = compile_file_and_run(&source, "hello");
    assert_eq!(code, 0, "hello exit code; stderr={stderr:?}");
    assert_eq!(stdout, "hello, world\n", "hello-world stdout mismatch");
}

/// Compile `examples/hello.sigil` (compile-only, no link), then
/// inspect the `.o` file's stackmap section bytes and parse them via
/// the runtime's parser. Asserts the v0 placeholder invariants:
/// magic, version = 0, every record flagged placeholder,
/// `live_count = 0`.
#[test]
fn stackmap_section_parses_v0_placeholder() {
    use sigil_compiler::{
        closure_convert, codegen, color, cps, elaborate, lexer, monomorphize, parser, resolve,
        typecheck,
    };
    use sigil_runtime::stackmap::{
        parse_section, ParseError, STACKMAP_FLAG_PLACEHOLDER, STACKMAP_VERSION_PLACEHOLDER,
    };

    // The helper does not invoke the compiler binary, but it does read
    // the staticlib indirectly via link.rs downstream; route through
    // sigil_binary() anyway so the Once guarantee holds across every
    // e2e entry point.
    let _ = sigil_binary();

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

// ===== Plan A2 Task 26 — canonical Stage-2 examples =========================

/// Plan A2 task 26 — compiles and runs `examples/arith.sigil`. The
/// file's comment documents the invariant: exit code 26.
#[test]
fn arith_example_exits_26() {
    let root = workspace_root();
    let source = root.join("examples/arith.sigil");
    let (_stdout, stderr, code) = compile_file_and_run(&source, "arith_example");
    assert_eq!(code, 26, "arith.sigil exit code; stderr={stderr:?}");
}

/// Plan A2 task 26 — compiles and runs `examples/div_by_zero.sigil`.
/// Verifies the runtime trap: stderr banner and exit status 2.
#[test]
fn div_by_zero_example_traps() {
    let root = workspace_root();
    let source = root.join("examples/div_by_zero.sigil");
    let (_stdout, stderr, code) = compile_file_and_run(&source, "div_by_zero_example");
    assert_eq!(code, 2, "div_by_zero.sigil exits with 2");
    assert!(
        stderr.contains("sigil: arithmetic error: division by zero"),
        "stderr missing arith-error banner: {stderr:?}"
    );
}

// ===== Plan A2 Task 24 — Stage-2 codegen additional coverage ================
//
// These tests use inline-source programs so the canonical
// `examples/` directory stays minimal (per plan scope). They exist to
// pin codegen behaviour for Stage-2 shapes that the Task-26 example
// files don't exercise: `if`/`else`, `match` with a wildcard, and the
// modulo-by-zero variant of the arith trap.

/// `if`/`else` lowering. Elaborate desugars the `if` into a
/// `match`-on-`Bool`; codegen emits compare + `brif` to two arm bodies
/// joining at a continue block.
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

/// `match` chain with IntLit patterns and a wildcard. Codegen emits a
/// compare + `brif` per literal pattern, a wildcard jump for the final
/// arm, and a continue block that produces the arm's body value.
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

/// Modulo-by-zero takes the same runtime-trap path as division-by-zero
/// but with a different reason string. The canonical
/// `examples/div_by_zero.sigil` covers the `/` path via
/// [`div_by_zero_example_traps`]; this test covers the `%` path.
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
