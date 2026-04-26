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

/// Like [`compile_file_and_run`] but also returns the wall-clock
/// duration of the child process's `exec(2)`-to-exit run. The
/// compile step is NOT measured; only the compiled-program run is
/// timed. Used by plan A2 task 34's performance-floor test.
fn compile_file_and_run_timed(
    source_path: &Path,
    test_name: &str,
) -> (String, String, i32, std::time::Duration) {
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

    let start = std::time::Instant::now();
    let run = Command::new(&bin_path)
        .output()
        .expect("failed to execute compiled binary");
    let elapsed = start.elapsed();

    let code = run.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&run.stderr).into_owned();

    let _ = std::fs::remove_file(&bin_path);

    (stdout, stderr, code, elapsed)
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

// ===== Plan A2 Task 33 — canonical recursion + higher-order examples =========

/// Plan A2 task 33 — compiles and runs `examples/fibonacci.sigil`. The
/// file's comment documents the invariant: `fib(10) == 55`, exit 55.
/// Exercises multi-arg-capable fn decls + self-referential direct call
/// under the closure calling convention (top-level fn, null closure_ptr).
#[test]
fn fibonacci_example_exits_55() {
    let root = workspace_root();
    let source = root.join("examples/fibonacci.sigil");
    let (_stdout, stderr, code) = compile_file_and_run(&source, "fibonacci_example");
    assert_eq!(code, 55, "fibonacci.sigil exit code; stderr={stderr:?}");
}

/// Plan A2 task 33 — compiles and runs `examples/higher_order.sigil`.
/// The file's comment documents the invariant: `weighted_sum(100, 3, 5)
/// == 130`, exit 130. Exercises lambda syntax, application-site
/// unification, capture analysis, closure conversion (capturing
/// `delta` from the enclosing fn param), GC-heap closure allocation,
/// and the closure-calling-convention ABI via direct-IIFE dispatch
/// inside a recursive user fn.
#[test]
fn higher_order_example_exits_130() {
    let root = workspace_root();
    let source = root.join("examples/higher_order.sigil");
    let (_stdout, stderr, code) = compile_file_and_run(&source, "higher_order_example");
    assert_eq!(code, 130, "higher_order.sigil exit code; stderr={stderr:?}");
}

// ===== Plan A2 Task 34 — performance-floor example ==========================

/// Plan A2 task 34 — compiles and runs `examples/fib_perf.sigil`. The
/// file's comment documents the invariants: stdout `"6765\n"`, exit
/// `0`, and end-to-end wall-clock of the compiled binary under 50ms
/// on both hosts (`x86_64-unknown-linux-gnu` + `aarch64-apple-darwin`).
///
/// Exercises the `int_to_string` builtin wired in this task plus the
/// pre-existing `IO.println` + recursive-fn paths. The performance
/// bound is a normative acceptance criterion from the plan; if this
/// test flakes on CI the remediation is a DEVIATION entry, not a
/// silent relaxation.
#[test]
fn fib_perf_example_prints_6765_under_50ms() {
    let root = workspace_root();
    let source = root.join("examples/fib_perf.sigil");
    let (stdout, stderr, code, elapsed) = compile_file_and_run_timed(&source, "fib_perf_example");
    assert_eq!(code, 0, "fib_perf.sigil exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "6765\n",
        "fib_perf.sigil stdout must be exactly \"6765\\n\""
    );
    assert!(
        elapsed < std::time::Duration::from_millis(50),
        "fib_perf.sigil wall-clock {elapsed:?} exceeds the 50ms plan-A2 floor",
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

/// Plan A2 task 32: a top-level user fn is direct-called from `main`.
/// Every user fn takes a closure_ptr as its first Cranelift argument
/// (always null for direct calls to a top-level fn with no captured
/// env). Verifies the closure-calling-convention ABI reaches the
/// callee's entry block and that the user param lives in block_params[1].
#[test]
fn direct_call_top_level_fn() {
    let source = "fn inc(x: Int) -> Int ![] { x + 1 }\n\
                  fn main() -> Int ![] { inc(41) }\n";
    let (_stdout, stderr, code) = compile_and_run(source, "direct_call_top_level_fn");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr:?}");
    assert_eq!(code, 42, "inc(41) -> 42");
}

/// Plan A2 task 32: recursive direct call. `fact(n)` calls itself with
/// `n-1` until the base-case match arm fires. This exercises the
/// calling-convention under a stack of recursive frames and confirms
/// `user_fn_refs[fact]` resolves correctly inside `fact`'s own body.
#[test]
fn recursion_via_direct_call() {
    let source = "fn fact(n: Int) -> Int ![] {\n\
                    match n {\n\
                      0 => 1,\n\
                      _ => n * fact(n - 1),\n\
                    }\n\
                  }\n\
                  fn main() -> Int ![] { fact(5) }\n";
    let (_stdout, stderr, code) = compile_and_run(source, "recursion_via_direct_call");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr:?}");
    assert_eq!(code, 120, "fact(5) = 120");
}

/// Plan A2 task 32: an IIFE with zero captures. Closure conversion
/// hoists the lambda into `$lambda_0`, leaving a `ClosureRecord` with
/// an empty env at the call site. Codegen allocates the record (8-byte
/// header and one code_ptr word, no env words), then direct-calls the
/// synthetic fn with the record ptr as closure_ptr. The fn's body
/// never reads the closure ptr.
#[test]
fn iife_no_captures() {
    let source = "fn main() -> Int ![] {\n\
                    (fn (x: Int) -> Int ![] => x + 1)(41)\n\
                  }\n";
    let (_stdout, stderr, code) = compile_and_run(source, "iife_no_captures");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr:?}");
    assert_eq!(code, 42, "iife returning x+1 applied to 41 -> 42");
}

/// Plan A2 task 32: an IIFE capturing a single Int. The closure
/// record's env has one Int slot; inside the synthetic fn body, the
/// capture is lowered as `ClosureEnvLoad(0, Int)`. The fn adds its
/// param (block_params[1]) to the env-loaded value.
#[test]
fn iife_with_int_capture() {
    let source = "fn main() -> Int ![] {\n\
                    let x: Int = 10;\n\
                    (fn (y: Int) -> Int ![] => x + y)(32)\n\
                  }\n";
    let (_stdout, stderr, code) = compile_and_run(source, "iife_with_int_capture");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr:?}");
    assert_eq!(code, 42, "closure captures x=10, applied y=32 -> 42");
}

/// Plan A2 task 32: nested IIFE with a transitive capture. The inner
/// lambda captures `x` from the outermost scope; closure conversion
/// threads the value through the outer closure's env. Outer lambda
/// has env [Int(x)]; inner lambda's env_exprs in the outer scope
/// become `ClosureEnvLoad(0, Int, "x")` so the x value flows from
/// main → outer's env → inner's env, staying live across two frames.
#[test]
fn nested_iife_transitive_capture() {
    let source = "fn main() -> Int ![] {\n\
                    let x: Int = 7;\n\
                    ((fn (_p: Int) -> Int ![] => (fn (y: Int) -> Int ![] => x + y)(3))(0))\n\
                  }\n";
    let (_stdout, stderr, code) = compile_and_run(source, "nested_iife_transitive_capture");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr:?}");
    assert_eq!(
        code, 10,
        "x=7 threaded through outer closure; inner adds y=3 -> 10"
    );
}

// ===== Plan A3 Task 42 — user-defined sum types + pattern matching ==========

/// `examples/option_demo.sigil` — the canonical Plan A3 end-to-end
/// example. Declares `type Option = | None | Some(Int)`, writes a
/// match-based `unwrap_or`, and prints two results. Exercises both
/// Unit and Positional variant allocation (task 41.1) plus the match
/// decision-tree lowerer (task 41.2): discriminant compare + field
/// load for `Some(n)` and nullary-ctor promotion for `None`.
#[test]
fn option_demo_example_prints_42_and_minus_one() {
    let root = workspace_root();
    let source = root.join("examples/option_demo.sigil");
    let (stdout, stderr, code) = compile_file_and_run(&source, "option_demo_example");
    assert_eq!(code, 0, "option_demo.sigil exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "42\n-1\n",
        "option_demo.sigil stdout must print unwrap_or(Some(42), 0) and unwrap_or(None, -1)"
    );
}

// ===== Plan A3 Tasks 43 + 44 — recursive sum-type + perf floor ==============

/// `examples/tree.sigil` — recursive sum type with nested constructor
/// patterns. `sum_tree(build(15))` folds across a depth-15 full binary
/// tree (65,535 nodes total, 32,767 internal) and prints the exact sum
/// `2^15 - 1 = 32767`. Both the correctness assertion (Task 43) and
/// the 500ms wall-clock bound (Task 44) are pinned here so the single
/// example carries both plan-level invariants.
///
/// The bound is a normative acceptance criterion from the plan; a flake
/// should land as a DEVIATION entry, not a silent relaxation.
#[test]
fn tree_example_prints_32767_under_500ms() {
    let root = workspace_root();
    let source = root.join("examples/tree.sigil");
    let (stdout, stderr, code, elapsed) = compile_file_and_run_timed(&source, "tree_example");
    assert_eq!(code, 0, "tree.sigil exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "32767\n",
        "tree.sigil stdout must be exactly \"32767\\n\" (sum of a full depth-15 tree with 1 at every internal node)"
    );
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "tree.sigil wall-clock {elapsed:?} exceeds the 500ms Plan A3 floor",
    );
}

// ===== Plan A3 Task 45 — E0120 non-exhaustive-match regression ==============

/// Compile a deliberately non-exhaustive `match` on `Option` with
/// `--human-errors` and assert the compile failure surfaces E0120 plus
/// the counterexample witness `None` in stderr. This pins both the
/// code-emission path (which Plan B refines) and the witness-string
/// generator (Task 38.4) against silent regression.
#[test]
fn e0120_non_exhaustive_match_names_witness_in_stderr() {
    let source = "type Option = | None | Some(Int)\n\
                  fn f(o: Option) -> Int ![] {\n  \
                    match o {\n    \
                      Some(n) => n,\n  \
                    }\n\
                  }\n\
                  fn main() -> Int ![] { 0 }\n";

    let src_path = std::env::temp_dir().join(format!(
        "sigil_e2e_e0120_non_exhaustive_{}.sigil",
        std::process::id()
    ));
    std::fs::write(&src_path, source).expect("write source");
    let bin_path = std::env::temp_dir().join(format!(
        "sigil_e2e_e0120_non_exhaustive_{}",
        std::process::id()
    ));

    let root = workspace_root();
    let sigil_bin = sigil_binary();
    let compile = Command::new(&sigil_bin)
        .arg(&src_path)
        .arg("-o")
        .arg(&bin_path)
        .arg("--human-errors")
        .current_dir(&root)
        .output()
        .expect("failed to invoke sigil compiler");

    let _ = std::fs::remove_file(&src_path);
    let _ = std::fs::remove_file(&bin_path);

    assert!(
        !compile.status.success(),
        "compile must fail for a non-exhaustive Option match; stdout={} stderr={}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr),
    );
    let stderr = String::from_utf8_lossy(&compile.stderr);
    assert!(
        stderr.contains("E0120"),
        "stderr missing E0120 code: {stderr}"
    );
    // The witness string names the uncovered variant. `None` is a Unit
    // variant so the witness is the bare constructor name (no
    // parentheses / braces).
    assert!(
        stderr.contains("None"),
        "stderr missing witness `None`: {stderr}"
    );
}

// ===== Plan B Task 50 — `--dump-color` ====================================

/// `sigil <input> --dump-color` runs the front end through color
/// inference and prints one stable line per monomorph to stdout, then
/// exits 0 without producing an executable. The hello-world example
/// has a single `main` fn with row `![IO]`, which the color analysis
/// classifies as native: pure row, leaf call graph (modulo perform IO
/// which the local analysis treats as part of the IO-only row, not a
/// non-IO body site).
#[test]
fn dump_color_hello_is_native_row_io() {
    let root = workspace_root();
    let source = root.join("examples/hello.sigil");
    let sigil_bin = sigil_binary();
    let out = Command::new(&sigil_bin)
        .arg(&source)
        .arg("--dump-color")
        .output()
        .expect("invoke sigil --dump-color");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "--dump-color exit; stdout={stdout:?}, stderr={stderr:?}"
    );
    // hello.sigil declares `fn main() -> Int ![IO]`. Native row + leaf
    // call graph + perform IO is entirely consistent with the native
    // classification (Plan B Task 50 spec: row is `![IO]` → native).
    assert!(
        stdout.contains("main native"),
        "expected `main native ...` in dump-color output, got: {stdout}"
    );
    // The reason text is stable; pin it loosely so the test survives
    // future tweaks to the wording but catches accidental category
    // flips.
    assert!(
        stdout.contains("native: row is `![IO]`"),
        "expected reason `native: row is ![IO]`, got: {stdout}"
    );
}

/// A multi-fn program: `helper` is pure, `main` calls `helper`. Both
/// classify as native; `main`'s reason is the local "native" reason
/// (not the transitive-CPS branch). The dump comes back in program
/// order.
#[test]
fn dump_color_multi_fn_pure_program() {
    let src = r#"
        fn helper(n: Int) -> Int ![] { n + 1 }
        fn main() -> Int ![] { helper(41) }
    "#;
    let tmp = std::env::temp_dir().join(format!(
        "sigil_e2e_dump_color_pure_{}.sigil",
        std::process::id()
    ));
    std::fs::write(&tmp, src).expect("write tmp source");
    let sigil_bin = sigil_binary();
    let out = Command::new(&sigil_bin)
        .arg(&tmp)
        .arg("--dump-color")
        .output()
        .expect("invoke sigil --dump-color");
    let _ = std::fs::remove_file(&tmp);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "exit; stdout={stdout:?}, stderr={stderr:?}"
    );
    // Program order: helper then main.
    assert_eq!(
        stdout.trim_end(),
        "helper native native: pure row\nmain native native: pure row",
        "dump-color output mismatch: {stdout}",
    );
}

// ===== Plan B Task 51 — `examples/generic_map.sigil` =======================

/// `examples/generic_map.sigil` — first user-authored generic syntax to
/// flow through the full Sigil pipeline (lex → parse → resolve →
/// typecheck → elaborate → monomorphize → color → codegen). Pinned by
/// the PR #17 reviewer as the canonical reproducibility checkpoint for
/// PR #16 (Task 49)'s `$$` mangling format: prior tests stop at the
/// monomorph-IR level, and prior end-to-end examples (`option_demo`,
/// `tree`, `higher_order`, `arith`, `fib_perf`) declare no generic
/// parameters. This example crosses that gap by declaring `type
/// List[A]`, `fn map[A]`, and `fn length[A]`, instantiating each at
/// `Int` and `String` in `main`.
///
/// The expected stdout is exactly `"3\n2\n"` — `length(map(Cons(10,
/// Cons(20, Cons(30, Nil)))))` for the Int instantiation and
/// `length(map(Cons("a", Cons("b", Nil))))` for the String. The shapes
/// are deliberately different (3 vs 2) so a copy-paste error between
/// the two list literals would surface as a length mismatch.
#[test]
fn generic_map_example_prints_3_and_2() {
    let root = workspace_root();
    let source = root.join("examples/generic_map.sigil");
    let (stdout, stderr, code) = compile_file_and_run(&source, "generic_map_example");
    assert_eq!(code, 0, "generic_map.sigil exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "3\n2\n",
        "generic_map.sigil stdout must print length(map(ints))=3 then length(map(strs))=2",
    );
}

/// `sigil examples/generic_map.sigil --dump-color` — verifies that
/// Task 50's per-monomorph color inference classifies all four
/// monomorphs (`map$$Int`, `map$$String`, `length$$Int`,
/// `length$$String`) plus `main` as native. The bodies have row
/// `![]` and recurse only on Native peers; `main` has row `![IO]`
/// and contains no `perform` to a non-IO effect, so it also lands
/// native via the IO-row classification rule.
///
/// This pins the discriminating contract that color inference is
/// per-monomorph (not per-source-fn): all four List-related clones
/// share a single source declaration but each gets an independent
/// color decision, all of which must come back native here. Any
/// future regression that pessimizes generic-fn color via name-based
/// rather than instantiation-based analysis will surface as a `cps`
/// classification on at least one of the four clones.
#[test]
fn generic_map_dump_color_all_native() {
    let root = workspace_root();
    let source = root.join("examples/generic_map.sigil");
    let sigil_bin = sigil_binary();
    let out = Command::new(&sigil_bin)
        .arg(&source)
        .arg("--dump-color")
        .output()
        .expect("invoke sigil --dump-color");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "exit; stdout={stdout:?}, stderr={stderr:?}"
    );
    // All four monomorphs + main must classify as native. Pin each
    // expected mangled name independently so a regression on any
    // single one (e.g. a mangling-format slip on map$$String) lands
    // on a directed assertion rather than a opaque overall-string
    // diff.
    for expected in [
        "map$$Int native",
        "map$$String native",
        "length$$Int native",
        "length$$String native",
        "main native",
    ] {
        assert!(
            stdout.contains(expected),
            "expected `{expected}` line in dump-color output, got:\n{stdout}"
        );
    }
    // No CPS classifications should appear — this program is purely
    // structural and should not require the trampoline.
    assert!(
        !stdout.contains(" cps "),
        "no monomorph should classify as cps in this program; got:\n{stdout}"
    );
}

// ===== Plan B Task 52 — P16 prompt regression =============================

/// P16 from `spec/validation-prompts.md`: generic identity at `Int` and
/// `String` in the same program. Prompt-bank prose claims the program
/// pins "Algorithm W's fresh-var-per-call instantiation plus
/// reachability-bounded specialization produce *exactly two* monomorph
/// clones (`id$$Int`, `id$$String`) — not one polymorphic body, not
/// three from double-counted call sites." This test makes that claim
/// substantive by:
///
///   (a) Compiling P16's program through the full pipeline and
///       asserting stdout exactly `"42\nsigil\n"` (oracle from the
///       prompt).
///   (b) Running `--dump-color` on the same source and asserting the
///       monomorph set is exactly `{id$$Int, id$$String, main}` —
///       three lines, no fourth from a double-counted call, no leftover
///       unmonomorphized `id`. A regression that produced an extra
///       `id$$Int` clone (e.g. fresh-var collisions causing two distinct
///       Int instantiations) would surface as a 4th line; a regression
///       that left an unmonomorphized polymorphic body would surface as
///       a bare `id native` line.
#[test]
fn p16_generic_id_at_int_and_string_oracle() {
    let src = "fn id[A](x: A) -> A ![] { x }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = id(42);\n  \
                 let s: String = id(\"sigil\");\n  \
                 perform IO.println(int_to_string(n));\n  \
                 perform IO.println(s);\n  \
                 0\n\
               }\n";
    let tmp = std::env::temp_dir().join(format!(
        "sigil_e2e_p16_generic_id_{}.sigil",
        std::process::id()
    ));
    std::fs::write(&tmp, src).expect("write P16 source");
    let (stdout, stderr, code) = compile_file_and_run(&tmp, "p16_generic_id_oracle");
    assert_eq!(code, 0, "P16 exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "42\nsigil\n",
        "P16 oracle stdout must be exactly the prompt-bank-documented bytes",
    );

    let sigil_bin = sigil_binary();
    let dump = Command::new(&sigil_bin)
        .arg(&tmp)
        .arg("--dump-color")
        .output()
        .expect("invoke sigil --dump-color on P16");
    let _ = std::fs::remove_file(&tmp);
    let dump_stdout = String::from_utf8_lossy(&dump.stdout);
    assert!(
        dump.status.success(),
        "--dump-color exit; stdout={dump_stdout:?}"
    );
    let lines: Vec<&str> = dump_stdout.lines().collect();
    assert_eq!(
        lines.len(),
        3,
        "P16 must produce exactly 3 monomorph lines (id$$Int, id$$String, main); got {} lines:\n{dump_stdout}",
        lines.len(),
    );
    let starts: Vec<&str> = lines
        .iter()
        .map(|l| l.split_whitespace().next().unwrap_or(""))
        .collect();
    let expected = ["id$$Int", "id$$String", "main"];
    for name in expected {
        assert!(
            starts.contains(&name),
            "expected `{name}` in dump-color monomorph set; got {starts:?}\nfull dump:\n{dump_stdout}"
        );
    }
    assert!(
        !dump_stdout.contains("\nid native") && !dump_stdout.starts_with("id native"),
        "no bare `id native` line allowed (would mean monomorphization left a polymorphic body); got:\n{dump_stdout}"
    );
}

// ===== Plan B Task 52 — P17 surface-syntax-pending pin ====================

/// P17 from `spec/validation-prompts.md`: generic `compose` over two
/// unary functions across types. Per the prompt's oracle notes, P17
/// requires `TypeExpr::Fn` surface syntax (function types in parameter
/// / return / `let`-binding positions), which Sigil v1 has not shipped
/// (P09 / P10 deferred this to Plan A3; A3 did not deliver). Until the
/// surface lands, P17 is graded only against "program rejects with the
/// missing-surface diagnostic, not silently accepted."
///
/// This test pins the contract that the P17 source — exactly as the
/// prompt asks the LLM to produce it — fails to compile. The specific
/// error code Sigil emits for `(B) -> C ![]` in a parameter position
/// is implementation detail (could be a parser error or a typecheck
/// error against the missing surface form); this test just asserts
/// that the front end rejects the program and doesn't quietly accept
/// it. Once `TypeExpr::Fn` ships, the test should be inverted to assert
/// success against the prompt's stdout oracle.
#[test]
fn effect_decl_with_no_handler_use_compiles_and_runs() {
    // Plan B Task 55 (foundation phase): an `effect` declaration that
    // is never used by `handle` or `perform` flows through the
    // pipeline as a no-op. The codegen-entry walker no longer
    // short-circuits on `Item::Effect`, monomorphize/color/closure-
    // convert pass it through unchanged, and codegen emits no
    // additional symbols for it. The program below should compile
    // cleanly and behave identically to the IO-only program (`hello,
    // world\n` to stdout, exit 0).
    let src = "effect Raise { fail: (String) -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 perform IO.println(\"hello, world\");\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "effect_decl_no_use");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "hello, world\n",
        "stdout mismatch; stderr={stderr:?}"
    );
}

#[test]
fn handle_with_no_perform_in_body_compiles_and_runs() {
    // Plan B Task 55 (Phase 2 minimum): a `handle <body> with { ...
    // }` expression where the body contains no non-IO `perform`
    // compiles to just the body's value (the handler is statically
    // optimised away — its arms are dead code at runtime). The
    // program below uses an effect declaration AND a handle
    // expression for the first time end-to-end. The handle's body is
    // the literal `42`; the `Raise.fail` arm is never invoked. Final
    // stdout: `42\n` (via int_to_string + IO.println), exit 0.
    let src = "effect Raise { fail: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle 42 with { Raise.fail(k) => 0 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "handle_no_perform");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "42\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn handle_with_non_io_perform_runs_arm_and_returns_value() {
    // Plan B Task 55 (Phase 3b): handle expression whose body
    // performs the handled effect now compiles and runs end-to-end.
    // The body's `perform Raise.fail()` calls `sigil_perform`, which
    // walks the handler stack, finds the frame's `Raise.fail` arm
    // (a synthetic CPS fn registered via `sigil_handler_frame_set_arm`),
    // invokes it with packed `(k_closure, k_fn)` args (both null —
    // the arm ignores `k`), and the arm returns
    // `sigil_next_step_done(42)`. The native code reads the value
    // from the returned `*mut NextStep` and treats it as the
    // perform's value, which is the handle's value. Final stdout:
    // `42\n`, exit 0.
    let src = "effect Raise { fail: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle (perform Raise.fail()) with { Raise.fail(k) => 42 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "handle_perform_arm_value");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "42\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn handle_with_arm_that_uses_k_is_rejected_at_codegen() {
    // Plan B Task 55 (Phase 3b restriction): arm bodies must be
    // literal `Int` expressions (Phase 4+ adds richer arm bodies +
    // `k`-using arms via continuation reification + the trampoline).
    // Until then, an arm body referencing `k` (or any non-literal
    // shape) is rejected at codegen entry by
    // `unsupported_handle_construct`.
    let src = "effect Raise { fail: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle 42 with { Raise.fail(k) => k(0) };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let tmp = std::env::temp_dir().join(format!(
        "sigil_e2e_handle_k_reject_{}.sigil",
        std::process::id()
    ));
    std::fs::write(&tmp, src).expect("write source");
    let bin_path =
        std::env::temp_dir().join(format!("sigil_e2e_handle_k_reject_{}", std::process::id()));
    let sigil_bin = sigil_binary();
    let out = Command::new(&sigil_bin)
        .arg(&tmp)
        .arg("-o")
        .arg(&bin_path)
        .arg("--human-errors")
        .output()
        .expect("invoke sigil");
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(&bin_path);
    assert!(
        !out.status.success(),
        "compile must fail until Phase 4+ lands; got success with stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Task 55") || stderr.contains("Phase 4") || stderr.contains("IntLit"),
        "error message should reference Plan B Task 55 / Phase 4 / arm-body restriction; got stderr={stderr:?}",
    );
}

#[test]
fn p17_compose_source_rejects_until_typeexpr_fn_ships() {
    let src = "fn compose[A, B, C](f: (B) -> C ![], g: (A) -> B ![]) -> (A) -> C ![] {\n  \
                 fn (x: A) -> C ![] => f(g(x))\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let inc_then_format: (Int) -> String ![] =\n    \
                   compose(int_to_string, fn (n: Int) -> Int ![] => n + 1);\n  \
                 perform IO.println(inc_then_format(41));\n  \
                 0\n\
               }\n";
    let tmp = std::env::temp_dir().join(format!(
        "sigil_e2e_p17_compose_{}.sigil",
        std::process::id()
    ));
    std::fs::write(&tmp, src).expect("write P17 source");
    let bin_path =
        std::env::temp_dir().join(format!("sigil_e2e_p17_compose_{}", std::process::id()));
    let sigil_bin = sigil_binary();
    let out = Command::new(&sigil_bin)
        .arg(&tmp)
        .arg("-o")
        .arg(&bin_path)
        .arg("--human-errors")
        .output()
        .expect("invoke sigil on P17 source");
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(&bin_path);
    assert!(
        !out.status.success(),
        "P17 source must NOT compile until TypeExpr::Fn ships; got success with stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}
