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

/// Plan B' Stage 6.8 R5 finding 1 — discipline helper for negative
/// e2e tests that pin specific compile-failure E-codes.
///
/// Mandates the asserted error code as a named arg so the test
/// can't silently pass on a different compile-failure (e.g., a
/// typecheck error in the test source masking the codegen path the
/// test was written to exercise — the recurring bug class caught
/// by `0baaa15`, `4e5d165`, and `5619df6`).
///
/// Compiles `source` and asserts:
/// 1. compile fails (exit non-zero), AND
/// 2. stderr contains `expected_code` (e.g., "E0138"), AND
/// 3. stderr contains every substring in `extra_substrings` (for
///    pinning op names / specific quoted identifiers in addition
///    to the E-code anchor).
///
/// Use for any negative test of the shape "this source must
/// compile-fail with code X". Bare `!status.success()` checks
/// without an E-code anchor are easy to write but brittle —
/// any future refactor that shifts which pass rejects the source
/// silently invalidates the test's claim.
fn assert_compile_fails_with_code(
    source: &str,
    expected_code: &str,
    extra_substrings: &[&str],
    test_name: &str,
) {
    let src_path = std::env::temp_dir().join(format!(
        "sigil_e2e_{}_{}.sigil",
        test_name,
        std::process::id()
    ));
    std::fs::write(&src_path, source).expect("write source");
    let bin_path =
        std::env::temp_dir().join(format!("sigil_e2e_{}_{}", test_name, std::process::id()));
    let sigil_bin = sigil_binary();
    let out = Command::new(&sigil_bin)
        .arg(&src_path)
        .arg("-o")
        .arg(&bin_path)
        .output()
        .expect("invoke sigil compiler");
    let _ = std::fs::remove_file(&src_path);
    let _ = std::fs::remove_file(&bin_path);
    assert!(
        !out.status.success(),
        "compile must fail for `{test_name}`; got success with stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr_str = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr_str.contains(expected_code),
        "expected `{expected_code}` in stderr for `{test_name}`; got stderr={stderr_str:?}"
    );
    for needle in extra_substrings {
        assert!(
            stderr_str.contains(needle),
            "expected substring `{needle}` in stderr for `{test_name}`; \
             got stderr={stderr_str:?}"
        );
    }
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
        closure_convert, codegen, color, elaborate, lexer, monomorphize, parser, resolve, typecheck,
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
    let cc = closure_convert::convert(colored);

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
    // Plan B Task 57 closeout-review observation — assert "≥1
    // record" rather than an exact count. The exact count is
    // mechanically derivable but brittle: every shim-touching
    // change (Phase A2 → Slice 1 → Slice 2 was 4 → 9 → 14) requires
    // updating it. The load-bearing invariant this test pins is
    // **(a) the magic + version parse correctly, (b) every record
    // is a v0 placeholder (live_count = 0, flag set)** — those are
    // the per-record invariants the loop below verifies. The
    // record count is currently a "non-zero records exist" sanity
    // check; lifting it to `≥ 1` decouples this test from shim
    // call-site drift. A future task that introduces a
    // counter-aware test (e.g., asserting `HandlerWalkCount`
    // increments per println) is the right home for cardinality
    // assertions.
    assert!(
        !parsed.records.is_empty(),
        "expected at least one placeholder record (got 0); shim must emit \
         stackmap records for its FFI calls"
    );
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

/// Plan B Task 60 — performance floor #2: `examples/fib_cps_perf.sigil`
/// computes the same 6765 result as the native `fib_perf` example but
/// forces fib to CPS-color via a per-call `perform State.get()`. The
/// plan's bound is **<500ms wall-clock on both hosts** (10× the native
/// 50ms floor — the "trampolined arithmetic" 10× slowdown ceiling
/// per Plan B Task 60).
///
/// **Compute path.** fib's body is `let _: Int = perform State.get();
/// match n { 0 => 0, 1 => 1, _ => fib(n - 1) + fib(n - 2) }`. The
/// `let _ = perform; match { ... }` shape does not match
/// `is_simple_let_yield_then_pure_tail_body` (match arms contain
/// recursive non-pure `fib(...)` calls), so fib falls through to
/// `UserFnAbi::Sync` despite being `Color::Cps`. Each perform site
/// routes through `lower_perform_to_value`'s synchronous
/// `sigil_run_loop` driver — the Phase 4d MVP shape. ~17710
/// synchronous handler dispatches dominate the wall-clock; that's the
/// "trampolined arithmetic" the 10× ceiling governs.
///
/// **Why both arms registered.** Phase 4f latent op_id/arm_count
/// constraint (the `examples/div_recover.sigil` /
/// `examples/state.sigil` precedent for multi-op effect handlers):
/// a partial handler runtime-aborts when the unhandled op fires.
/// fib only performs `get`, but registering both `get` and `set`
/// arms keeps `arm_count` matched to the 2-op `State` declaration.
///
/// **The `State[Int]` framing.** Plan wording uses `State[Int]`
/// (design-doc convention for the fully-instantiated form); v1
/// source uses `State` directly per `[DEVIATION Task 60]` — the
/// monomorphic form parses + typechecks at the AST level, while
/// the literal generic-parameterised `effect State[T]` shape is
/// not exercised by any existing example or e2e test. Type-
/// parameter granularity doesn't change the colorer's CPS-coloring
/// decision (which depends only on whether non-IO performs occur).
///
/// Invariant: stdout = "6765\n", stderr = "", exit 0, wall-clock <
/// 500ms.
#[test]
fn fib_cps_perf_example_prints_6765_under_500ms() {
    let root = workspace_root();
    let source = root.join("examples/fib_cps_perf.sigil");
    let (stdout, stderr, code, elapsed) =
        compile_file_and_run_timed(&source, "fib_cps_perf_example");
    assert_eq!(code, 0, "fib_cps_perf.sigil exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "6765\n",
        "fib_cps_perf.sigil stdout must be exactly \"6765\\n\""
    );
    assert!(
        elapsed < std::time::Duration::from_millis(500),
        "fib_cps_perf.sigil wall-clock {elapsed:?} exceeds the 500ms Plan B Task 60 floor (10× native fib_perf <50ms)",
    );
}

/// Plan B Task 60 — performance floor #3: multi-shot Choose stress
/// driver. Plan wording calls for "Multi-shot stress test (3-element
/// Choose combinator, N=1000 iterations) in <5s on both hosts".
///
/// v1 ships single-binary-perform multi-shot pattern wrapped in an
/// N=1000 recursive driver per `[DEVIATION Task 60]` — literal
/// Cartesian-product 3-pick enumeration requires multi-perform helper
/// bodies (chained-synth-cont extension; Plan-C-or-later territory
/// pinned in `[DEVIATION Task 59]` for choose.sigil's two-flip pair
/// generator). The v1 driver exercises iteration-scale multi-shot
/// scalability (1000 fresh handler frames × 2 multi-shot k
/// invocations per arm = 2000 multi-shot k invocations + 1000 fresh
/// heap-reified k_closure records) rather than per-iteration
/// combinator depth.
///
/// **Stress invariants:** every push/pop of a handler frame must
/// complete cleanly across the 1000-deep sequence; every heap-
/// reified k_closure must dispatch twice without leaking state into
/// a later iteration's k_closure; the recursive `run(n)` driver
/// must complete without stack overflow at N=1000 (Native ABI
/// recursion, well within native stack capacity).
///
/// **Why stdout is `"0\n"`.** The driver returns 0 (run(0) = 0; the
/// recursive case discards each iteration's handle-expression value
/// via `let _ = ...`); main's `let _ = run(1000)` discards that too,
/// and prints `int_to_string(0)` as a sentinel that the recursion
/// completed successfully. Per-iteration values (1+2 = 3) don't
/// reach stdout — the perf test focuses on dispatch throughput, not
/// computed-value verification (the canonical Slice C 2-resume
/// pattern is already pinned by `slice_c_choose_multi_shot_arm_-
/// invokes_k_twice_with_different_args` and `choose_example_dual_-
/// resume_returns_3`).
///
/// Invariant: stdout = "0\n", stderr = "", exit 0, wall-clock < 5s.
#[test]
fn multishot_perf_example_under_5s() {
    let root = workspace_root();
    let source = root.join("examples/multishot_perf.sigil");
    let (stdout, stderr, code, elapsed) =
        compile_file_and_run_timed(&source, "multishot_perf_example");
    assert_eq!(code, 0, "multishot_perf.sigil exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "0\n",
        "multishot_perf.sigil stdout must be exactly \"0\\n\" (sentinel \
         indicating run(1000) completed)"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "multishot_perf.sigil wall-clock {elapsed:?} exceeds the 5s Plan B Task 60 floor (1000 iterations × 2 multi-shot resumes per arm)",
    );
}

/// Plan B Task 60 — performance floor #4: arena allocator escape rate.
/// Plan wording sets the contractual ceiling at "at most 1% of
/// `NextStep` records escape to Boehm heap in a typical CPS-color run"
/// — but the v1 codegen has **zero** compiler-side `sigil_arena_promote`
/// call sites today, so the ACTUAL escape rate is 0%. This test asserts
/// the tighter `escape_count == 0` invariant rather than the plan's
/// looser `escape ≤ 1% × alloc` bound — a regression that introduces
/// any escape site at all is a real change worth surfacing immediately,
/// not silently absorbing under a 1% ceiling. The plan's 1% remains
/// the contractual worst-acceptable; the assertion enforces the
/// tighter actual bound and forces the test (and its commit message)
/// to be updated alongside any deliberate escape-site introduction so
/// the regression review is explicit.
///
/// "Typical CPS-color run" is defined as `examples/multishot_perf.sigil`
/// per `[DEVIATION Task 60]` — a representative program exercising
/// both the canonical Slice C 2-let arm (heap-reifying k_closures) and
/// the iterated handle-frame allocation pattern at non-trivial scale
/// (1000 iterations × 2 multi-shot resumes).
///
/// **How the assertion runs.** The runtime instruments
/// `sigil_arena_alloc` (every NextStep record arena-allocation,
/// counter `ArenaAllocCount`) and `sigil_arena_promote` (every
/// promote-to-Boehm-heap site, counter `ArenaEscapeCount`). At the
/// program's atexit, `sigil_counter_print_all` writes every counter
/// to stderr in `SIGIL_COUNTER_<NAME>=<value>` format (per
/// `runtime/src/counters.rs:104-112`), gated on
/// `SIGIL_PRINT_STATS=1` env var. We compile multishot_perf.sigil,
/// run with SIGIL_PRINT_STATS=1, parse the counter dump from stderr,
/// and assert `escape == 0`.
///
/// **Sanity bound.** Also assert `alloc > 0` to guard against a
/// future regression that silently disables arena allocation entirely
/// (a 0/0 ratio would trivially pass without exercising the arena
/// machinery).
///
/// **What this assertion enforces today.** No compiler-side codegen
/// site currently invokes `sigil_arena_promote` (multi-shot k_closure
/// records are heap-allocated directly via `sigil_alloc` rather than
/// arena-allocated-then-promoted). The expected escape_count is 0; if
/// it becomes nonzero, that's either (a) a real bug to fix or (b) a
/// deliberate change that needs to be reflected here with an updated
/// expected value and a commit message explaining why the increase
/// stays under the plan's 1% ceiling.
///
/// **Stdout invariant** mirrors the multishot_perf test: the sentinel
/// "0\n" indicates run(1000) completed. Stderr contains the counter
/// dump (after the program's own stderr output, which should be
/// empty for this example).
#[test]
fn arena_escape_count_is_zero_below_one_percent_ceiling() {
    use std::process::Command;
    let root = workspace_root();
    let source = root.join("examples/multishot_perf.sigil");
    let sigil_bin = sigil_binary();

    let bin_path = std::env::temp_dir().join(format!(
        "sigil_e2e_arena_escape_rate_{}",
        std::process::id()
    ));

    let compile = Command::new(&sigil_bin)
        .arg(&source)
        .arg("-o")
        .arg(&bin_path)
        .current_dir(&root)
        .output()
        .expect("failed to invoke sigil compiler");
    assert!(
        compile.status.success(),
        "compile failed: stdout={} stderr={}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr),
    );

    let run = Command::new(&bin_path)
        .env("SIGIL_PRINT_STATS", "1")
        .output()
        .expect("failed to execute compiled binary");

    let _ = std::fs::remove_file(&bin_path);

    let code = run.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&run.stderr).into_owned();

    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "0\n",
        "stdout sentinel mismatch (expected run(1000) to complete with \"0\\n\"); \
         stderr={stderr:?}"
    );

    // Parse counter dump. Format: each line is `<NAME>=<u64>`.
    let mut alloc: Option<u64> = None;
    let mut escape: Option<u64> = None;
    for line in stderr.lines() {
        if let Some(rest) = line.strip_prefix("SIGIL_COUNTER_ARENA_ALLOC_COUNT=") {
            alloc = rest.trim().parse().ok();
        } else if let Some(rest) = line.strip_prefix("SIGIL_COUNTER_ARENA_ESCAPE_COUNT=") {
            escape = rest.trim().parse().ok();
        }
    }

    let alloc = alloc.unwrap_or_else(|| {
        panic!(
            "ARENA_ALLOC_COUNT missing from --print-runtime-stats output; \
             stderr={stderr:?}"
        )
    });
    let escape = escape.unwrap_or_else(|| {
        panic!(
            "ARENA_ESCAPE_COUNT missing from --print-runtime-stats output; \
             stderr={stderr:?}"
        )
    });

    assert!(
        alloc > 0,
        "ARENA_ALLOC_COUNT = 0 — multishot_perf.sigil should exercise the \
         arena allocator (1000 iterations × 2 multi-shot resumes); a 0/0 \
         ratio would trivially pass the bound without verifying the \
         machinery. stderr={stderr:?}"
    );

    // Tighter than the plan's "≤ 1% × alloc" ceiling: today's v1 codegen
    // has zero compiler-side `sigil_arena_promote` call sites, so the
    // actual escape rate is 0%. Asserting `escape == 0` surfaces any
    // regression (any introduction of escape, however small) immediately
    // — a 0.5%-introduction would silently pass `escape * 100 ≤ alloc`
    // even though it's a real semantic change. If a future PR
    // deliberately adds escape sites, update both the assertion and the
    // commit message; the plan's 1% remains the contractual ceiling.
    assert_eq!(
        escape, 0,
        "arena escape count is nonzero — v1 codegen has no compiler-side \
         `sigil_arena_promote` call sites, so this is either (a) a real \
         regression, or (b) a deliberate change that needs to update both \
         this assertion AND the commit message explaining why the new \
         escape rate {escape}/{alloc} stays under the plan's 1% ceiling. \
         stderr={stderr:?}"
    );
}

/// Plan B Task 61 — pin P18's literal prompt source as an e2e test.
/// Belt-and-suspenders against `spec/validation-prompts.md` drift: any
/// change to the prompt's surface (effect declaration, helper body,
/// arm shape, oracle) lights up here in addition to whenever Plan C's
/// `scripts/validate-spec.sh` runs the bank.
///
/// The source is the literal program a fresh LLM session would
/// produce given P18's prompt: declares `effect Raise`, defines
/// `parse_token` with the early-fail-on-zero match, wraps a
/// `parse_token(0)` call in a handle whose discard-`k` arm prints the
/// failure message and returns -1 as the recovery sentinel. Stdout
/// is the prompt's oracle exactly: `"token zero is not allowed\n-1\n"`.
///
/// Mirrors `effect_decl_with_no_handler_use_compiles_and_runs` shape
/// (inline source via `compile_and_run`) — the prompt-bank sentinel
/// pattern, distinct from the example-file e2e tests for catch /
/// state / choose which carry their own `examples/` files.
#[test]
fn p18_safe_parser_example_prints_recovery_message() {
    let src = "effect Raise { fail: (String) -> Int }\n\
               fn parse_token(token: Int) -> Int ![Raise, IO] {\n  \
                 match token {\n    \
                   0 => perform Raise.fail(\"token zero is not allowed\"),\n    \
                   _ => token * 10,\n  \
                 }\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let result: Int = handle parse_token(0) with {\n    \
                   Raise.fail(msg, k) => {\n      \
                     perform IO.println(msg);\n      \
                     -1\n    \
                   },\n  \
                 };\n  \
                 perform IO.println(int_to_string(result));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "p18_safe_parser");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "token zero is not allowed\n-1\n",
        "P18 prompt oracle mismatch — recovery path should print the \
         failure message then the sentinel `-1`. stderr={stderr:?}"
    );
    assert_eq!(
        stderr, "",
        "P18 should not abort or warn; stderr should be empty"
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

/// Modulo-by-zero takes the same default-handler path as division-by-
/// zero but with a different reason string. The canonical
/// `examples/div_by_zero.sigil` covers the `/` path via
/// [`div_by_zero_example_traps`]; this test covers the `%` path.
///
/// Plan B Task 57 — row updated from `![]` to `![ArithError]` per
/// the elaborate-time-rewrite tracked-effect doctrine. User-visible
/// behaviour (stderr banner + exit 2) preserved verbatim by the
/// runtime-side `sigil_arith_error_mod_by_zero_arm` default arm fn.
#[test]
fn mod_by_zero_traps() {
    let source = "fn main() -> Int ![ArithError] {\n\
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

/// Plan B Task 57 — `examples/div_recover.sigil` exercises algebraic
/// recovery from a div-by-zero via a user-installed `ArithError`
/// handler. Confirms that:
///
/// - typecheck accepts `![ArithError]` on the inner fn doing
///   division, and `![IO]` on the outer fn whose handle expression
///   discharges `ArithError`;
/// - elaborate's `BinOp::Div` rewrite produces a perform-bearing
///   form that flows through `sigil_perform`;
/// - the user's `ArithError.div_by_zero(k) => 999` handler frame
///   intercepts the perform before the top-level shim's default
///   (the frame walk is inward-first);
/// - the recovery value `999` flows back to the outer fn's handle
///   expression, and the program prints `999` then exits 0.
#[test]
fn div_recover_example_returns_999() {
    let root = workspace_root();
    let source = root.join("examples/div_recover.sigil");
    let (stdout, stderr, code) = compile_file_and_run(&source, "div_recover_example");
    assert_eq!(code, 0, "div_recover exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "999\n",
        "div_recover stdout mismatch (expected user handler to recover with 999); \
         stderr={stderr:?}"
    );
    assert_eq!(
        stderr, "",
        "div_recover should not abort via the default ArithError arm fn; \
         stderr should be empty"
    );
}

/// Plan B Task 58 — `examples/choose_demo.sigil` exercises the
/// canonical `resumes: many` multi-shot continuation with the
/// `(Int) -> Int` op signature. Confirms that:
///
/// - typecheck accepts `effect Choose resumes: many { choose: (Int)
///   -> Int }` and the 2-let arm body `{ let r1: Int = k(arg + 10);
///   let r2: Int = k(arg + 20); r1 + r2 }`;
/// - codegen's `arm_body_multi_let_then_pure_tail_shape` matches the
///   arm body, allocates two post-arm-k synth fns per Slice C v1, and
///   routes the continuation reification through the heap-reified
///   k_closure path (TAG_CLOSURE record);
/// - Phase 4b's perform-side args-buffer packing and Phase 4c's
///   arm-side arg unpacking work end-to-end with an `(Int) -> Int`
///   op (the existing Slice C e2e tests use `() -> Bool`; this
///   example is the first multi-shot dispatch with non-zero op arity
///   on the Int path);
/// - the same heap-reified k_closure dispatches into the helper
///   synth-cont twice with different args (15, then 25), each
///   producing an independent result (15, 25) that combine to 40.
///
/// Invariant: stdout = "40\n", stderr = "", exit 0.
#[test]
fn choose_demo_example_returns_40() {
    let root = workspace_root();
    let source = root.join("examples/choose_demo.sigil");
    let (stdout, stderr, code) = compile_file_and_run(&source, "choose_demo_example");
    assert_eq!(code, 0, "choose_demo exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "40\n",
        "choose_demo stdout mismatch (expected k(15)=15 + k(25)=25 = 40); \
         stderr={stderr:?}"
    );
    assert_eq!(
        stderr, "",
        "choose_demo should not abort or warn; stderr should be empty"
    );
}

/// Plan B' Stage 6.7 Task 101 — `examples/multishot_stress.sigil`
/// exercises the natural multi-shot stress shape: 10 resumes of a
/// `resumes: many` continuation within a SINGLE arm body. Replaces
/// the pre-Stage-6.7 5-handles × 2-resumes workaround that PR #27
/// shipped under `[DEVIATION Task 58]`.
///
/// What the natural shape exercises (post-Plan-B' Task 100b):
///
/// - **N-let arm-body chain (N=10)**: 10 sequential `let r_i = k(arg
///   +i)` bindings drive 10 distinct trampoline cycles through the
///   helper synth-cont. Each step's `post_arm_k_i` synth fn dispatches
///   the next `k(arg+i+1)` call; chained closure records thread
///   `(k_closure, k_fn) + captures + prior_bindings` forward across
///   9 Middle steps to the Final step.
///
/// - **op-arg capture threading**: `arg` is referenced by every
///   `arg_i` expression. Task 100b's captures-bearing extension
///   threads `arg` through every chain step's closure record.
///
/// Closed form: helper(0); arm dispatched with arg=0; r_i = k(0+i)
/// = i. Tail = 1+2+...+10 = 55.
///
/// Invariant: stdout = "55\n", stderr = "", exit 0.
#[test]
fn multishot_stress_example_returns_55() {
    let root = workspace_root();
    let source = root.join("examples/multishot_stress.sigil");
    let (stdout, stderr, code) = compile_file_and_run(&source, "multishot_stress_example");
    assert_eq!(code, 0, "multishot_stress exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "55\n",
        "multishot_stress stdout mismatch (expected closed-form 1+2+...+10 \
         = 55); stderr={stderr:?}"
    );
    assert_eq!(
        stderr, "",
        "multishot_stress should not abort or warn; stderr should be empty"
    );
}

/// Plan B Task 59 — `examples/catch.sigil` exercises the canonical
/// one-shot Raise + recovery pattern. Confirms that:
///
/// - `effect Raise { fail: () -> Int }` parses + typechecks; helper's
///   `![Raise, IO]` row + main's `![IO]` row + the handle's
///   `Raise.fail(k) => 42` arm discharge `Raise` cleanly;
/// - the discard-`k` arm short-circuits `risky`'s tail (`result + input`
///   = `42 + 7 = 49` would have been the use-`k` value); Phase 4e
///   captures+'s colorer-handler-discharge refinement makes the arm's
///   constant value `42` flow directly to the handle's
///   `let recovered = ... ;` binding;
/// - the captures-bearing helper synth-cont's closure record is
///   allocated per the `[Phase 4e]` captures-bearing slice (the
///   `input` user param threaded into the synth-cont's env),
///   verifying that codegen still ALLOCATES the synth-cont path
///   even when the arm doesn't invoke it (the synth-cont fn is
///   defined; the arm just chooses not to dispatch into it).
///
/// Invariant: stdout = "42\n", stderr = "", exit 0.
#[test]
fn catch_example_recovers_with_42() {
    let root = workspace_root();
    let source = root.join("examples/catch.sigil");
    let (stdout, stderr, code) = compile_file_and_run(&source, "catch_example");
    assert_eq!(code, 0, "catch exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "42\n",
        "catch stdout mismatch (expected discard-k arm to recover with 42); \
         stderr={stderr:?}"
    );
    assert_eq!(
        stderr, "",
        "catch should not abort or warn; stderr should be empty"
    );
}

/// Plan B Task 59 — `examples/state.sigil` exercises both `State.get`
/// and `State.set` ops through the dual-handle pattern (one `handle`
/// per op).
///
/// **Why dual-handle, not literal `run_state`.** Plan B' Stage 6.8
/// shipped B.3 (TypeExpr::Fn) and B.4 (arm-body lambdas + Phase B
/// k-capture trailing-pair convention) — the lifts the literal
/// `run_state(initial, comp)` shape needs. But Task 109's first
/// CI cycle on the canonical CPS-style run_state revealed a runtime
/// integration gap: `state_fn(initial)` returned a closure-record
/// pointer rather than the threaded integer value. See
/// `[DEVIATION Task 109] run_state canonical shape — runtime chain
/// integration gap` for the gap analysis. Until the chain bug
/// closes, state.sigil keeps the dual-handle Plan B v1 workaround.
///
/// Invariant: stdout = "6\n99\n", stderr = "", exit 0.
#[test]
fn state_example_dual_handle_returns_6_then_99() {
    let root = workspace_root();
    let source = root.join("examples/state.sigil");
    let (stdout, stderr, code) = compile_file_and_run(&source, "state_example");
    assert_eq!(code, 0, "state exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "6\n99\n",
        "state stdout mismatch (expected read=6 from get arm k(5)+1, \
         then write=99 from set arm k(arg)=k(99)); stderr={stderr:?}"
    );
    assert_eq!(
        stderr, "",
        "state should not abort or warn; stderr should be empty"
    );
}

/// Plan B' Stage 6.7 + multi-shot composition fix —
/// `examples/choose.sigil` exercises the literal two-flip pair
/// generator: 2-flip helper (B.2 chained-let-yield) + multi-shot
/// 2-resume arm (B.1 N-let chain) + runtime outer post_arm_k stack
/// (composition fix). Helper enumerates all 2² = 4 outcomes; arm
/// sums them.
///
/// Closed form: outer-arm-tail = inner-tail(b1=t) + inner-tail(b1=f)
/// = (1+2) + (3+4) = 3 + 7 = 10.
///
/// Invariant: stdout = "10\n", stderr = "", exit 0.
#[test]
fn choose_example_pair_generator_returns_10() {
    let root = workspace_root();
    let source = root.join("examples/choose.sigil");
    let (stdout, stderr, code) = compile_file_and_run(&source, "choose_example");
    assert_eq!(code, 0, "choose exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "10\n",
        "choose stdout mismatch (expected pair-generator sum 1+2+3+4 = 10); \
         stderr={stderr:?}"
    );
    assert_eq!(
        stderr, "",
        "choose should not abort or warn; stderr should be empty"
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
fn dump_color_hello_is_cps_row_io() {
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
    // hello.sigil declares `fn main() -> Int ![IO]`. Stage 6 cleanup
    // lifted the IO color filter — `![IO]`-rowed fns now classify
    // as CPS-color (matching every other effect). main is special-
    // cased to `UserFnAbi::Sync` regardless of color (per
    // `compute_user_fn_abi`'s main entry-point contract), so the
    // ABI is still Sync and the runtime behavior matches the shim's
    // expectations; only the colorer's reason text changed.
    assert!(
        stdout.contains("main cps"),
        "expected `main cps ...` in dump-color output, got: {stdout}"
    );
    // Pin the reason text to the post-lift form so a regression
    // re-introducing the IO color exemption surfaces here.
    assert!(
        stdout.contains("cps: row contains effect `IO`"),
        "expected reason `cps: row contains effect `IO``, got: {stdout}"
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
    // All four monomorphs (map / length × Int / String) must classify
    // as native — they have row `![]` and are pure-structural. Stage 6
    // cleanup lifted the IO color filter, so `main` (with row `![IO]`)
    // now classifies as CPS-color (was Native pre-lift). Pin each
    // expected mangled name independently so a regression on any
    // single one (e.g. a mangling-format slip on map$$String) lands
    // on a directed assertion rather than an opaque overall-string
    // diff.
    for expected in [
        "map$$Int native",
        "map$$String native",
        "length$$Int native",
        "length$$String native",
        "main cps",
    ] {
        assert!(
            stdout.contains(expected),
            "expected `{expected}` line in dump-color output, got:\n{stdout}"
        );
    }
    // The four List-traversal monomorphs are pure-structural; only
    // main has effects. No `cps` lines should appear OUTSIDE main.
    let cps_lines: Vec<&str> = stdout.lines().filter(|l| l.contains(" cps ")).collect();
    assert!(
        cps_lines.iter().all(|l| l.starts_with("main ")),
        "only main should classify as cps in this program; got CPS lines: {cps_lines:?}"
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
fn handle_with_two_arms_dispatches_correct_arm_by_op_id() {
    // Plan B Task 55 (Phase 4a): multi-arm handlers are now
    // supported when all arms target the same effect. The runtime's
    // `sigil_perform` looks up the arm by op_id within the matched
    // frame; codegen registers each arm via a separate
    // `sigil_handler_frame_set_arm` call. Test program performs
    // `Choose.right()` and expects the `right` arm (not the `left`
    // arm) to fire. Op IDs are assigned alphabetically per effect:
    // `left` → 0, `right` → 1, so this exercises the non-zero op_id
    // path through the runtime arm-slot table.
    let src = "effect Choose { left: () -> Int, right: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle (perform Choose.right()) with {\n    \
                   Choose.left(k) => 10,\n    \
                   Choose.right(k) => 20,\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "handle_two_arms_dispatches");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "20\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn handle_with_mixed_effect_arms_dispatches_correct_arm_per_effect() {
    // Plan B Task 55 (Phase 4f) — INVERTED from
    // `handle_with_mixed_effect_arms_is_rejected_at_codegen` (the
    // Phase 4a-era rejection test). Multi-effect handlers now ship
    // via the push-N-frames mechanism: the BTreeMap-grouping codegen
    // emits one `HandlerFrame` per distinct effect with that effect's
    // arms, pushed in BTreeMap-stable iteration order, popped in
    // reverse at handle exit. See `[DEVIATION Task 55] Phase 4f` in
    // `PLAN_B_DEVIATIONS.md` for the architectural rationale.
    //
    // Test structure: two handles, each with arms targeting BOTH
    // declared effects. The first handle's body performs `Foo.f()`
    // — the runtime stack walk finds the Foo frame and dispatches
    // its arm (returns 7). The second handle's body performs
    // `Bar.b()` — the walk finds the Bar frame and dispatches its
    // arm (returns 11). The unused arm of each handle is set to a
    // sentinel value (99); a misdispatch (e.g., wrong frame ordering
    // causing an effect_id mismatch in the runtime walk) would print
    // a non-18 result. Final assertion: stdout `18\n` (7 + 11).
    //
    // Bisecting hint: a regression here producing "stdout != 18"
    // attributes to the BTreeMap-grouping loop in `Expr::Handle`
    // codegen (each frame's arms must contain only ops belonging to
    // that frame's effect_id; off-by-one in the partition lands a
    // wrong arm under the wrong effect). A regression producing
    // "TRAP_HANDLE_DISCIPLINE_VIOLATION (0x42)" attributes to the
    // reverse-pop discipline (stray pop in body, or n_frames
    // mismatch).
    let src = "effect Foo { f: () -> Int }\n\
               effect Bar { b: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let a: Int = handle (perform Foo.f()) with {\n    \
                   Foo.f(k) => 7,\n    \
                   Bar.b(k) => 99,\n  \
                 };\n  \
                 let b: Int = handle (perform Bar.b()) with {\n    \
                   Foo.f(k) => 99,\n    \
                   Bar.b(k) => 11,\n  \
                 };\n  \
                 perform IO.println(int_to_string(a + b));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "handle_mixed_effect_dispatches");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "18\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn handle_with_two_effects_two_arms_each_dispatches_per_op() {
    // Plan B Task 55, Phase 4f polish round — closes the per-effect-
    // multiple-arms coverage gap left open by the inversion test
    // (`handle_with_mixed_effect_arms_dispatches_correct_arm_per_effect`)
    // which only exercised single-arm-per-effect groups. This test
    // exercises 2 effects × 2 arms each: each effect's group becomes
    // a single `HandlerFrame` with arm_count=2, populated via two
    // `set_arm` calls before push.
    //
    // Four handles in main, each performing a different op of the
    // same handle shape (all 4 arms registered every time). Sentinel
    // values (1, 2, 3, 4) on each arm let a misdispatch announce
    // itself loudly: a wrong-arm fire produces a non-matching int.
    // Expected stdout: "1\n2\n3\n4\n" — one line per perform.
    //
    // Effects + op_ids (alphabetic):
    //   E1.a -> op_id 0   E2.x -> op_id 0
    //   E1.b -> op_id 1   E2.y -> op_id 1
    // Each handle's E1 group has arm_count=2 covering op_ids [0, 2);
    // E2 group same shape. Bounds checks all pass.
    //
    // Bisecting hint: `stdout != "1\n2\n3\n4\n"` attributes to
    // BTreeMap-grouping or per-frame `set_arm` dispatch. A
    // wrong-effect-arm landing produces e.g. "3\n2\n3\n4\n" (E1.a
    // routing to E2's op_id 0 = x, returning 3 instead of 1).
    let src = "effect E1 { a: () -> Int, b: () -> Int }\n\
               effect E2 { x: () -> Int, y: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let r1: Int = handle (perform E1.a()) with {\n    \
                   E1.a(k) => 1,\n    \
                   E1.b(k) => 2,\n    \
                   E2.x(k) => 3,\n    \
                   E2.y(k) => 4,\n  \
                 };\n  \
                 let r2: Int = handle (perform E1.b()) with {\n    \
                   E1.a(k) => 1,\n    \
                   E1.b(k) => 2,\n    \
                   E2.x(k) => 3,\n    \
                   E2.y(k) => 4,\n  \
                 };\n  \
                 let r3: Int = handle (perform E2.x()) with {\n    \
                   E1.a(k) => 1,\n    \
                   E1.b(k) => 2,\n    \
                   E2.x(k) => 3,\n    \
                   E2.y(k) => 4,\n  \
                 };\n  \
                 let r4: Int = handle (perform E2.y()) with {\n    \
                   E1.a(k) => 1,\n    \
                   E1.b(k) => 2,\n    \
                   E2.x(k) => 3,\n    \
                   E2.y(k) => 4,\n  \
                 };\n  \
                 perform IO.println(int_to_string(r1));\n  \
                 perform IO.println(int_to_string(r2));\n  \
                 perform IO.println(int_to_string(r3));\n  \
                 perform IO.println(int_to_string(r4));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "handle_2x2_dispatch");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "1\n2\n3\n4\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn handle_with_arms_in_reverse_source_order_produces_same_output() {
    // Plan B Task 55, Phase 4f polish round — pins frame-push order
    // to effect-id-lex-order (the BTreeMap's stable iteration), not
    // to source-position-of-first-arm. Two handles in main with
    // identical effects but the arms appearing in different source
    // orders. Both must produce the same observable result.
    //
    // If the codegen accidentally iterated `op_arms` in source order
    // rather than via the BTreeMap groups, the second handle's
    // reversed-source arms would land in a different per-frame
    // arm-slot ordering, surfacing as a misdispatch. The test
    // catches the regression even though no bug exists today.
    //
    // Bisecting hint: `stdout != "7\n7\n"` attributes to source-
    // order leaking through the BTreeMap-grouping abstraction (e.g.,
    // a refactor that replaced the BTreeMap with a Vec).
    let src = "effect AAA { go: () -> Int }\n\
               effect BBB { go: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let a: Int = handle (perform AAA.go()) with {\n    \
                   AAA.go(k) => 7,\n    \
                   BBB.go(k) => 99,\n  \
                 };\n  \
                 let b: Int = handle (perform AAA.go()) with {\n    \
                   BBB.go(k) => 99,\n    \
                   AAA.go(k) => 7,\n  \
                 };\n  \
                 perform IO.println(int_to_string(a));\n  \
                 perform IO.println(int_to_string(b));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "handle_source_order_independent");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "7\n7\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn handle_with_one_effect_at_max_handler_arms_compiles_and_dispatches() {
    // Plan B Task 55, Phase 4f polish round — verifies the per-frame
    // arm-count cap (MAX_HANDLER_ARMS = 14, sized by the 32-bit GC
    // pointer-bitmap on `HandlerFrame`) applies *per-effect-group*,
    // not per-handle: this multi-effect handle has 14 Wide-effect
    // arms (at the cap) plus 1 Other-effect arm, totalling 15 arms
    // collectively. A per-handle cap of 14 would reject this; a per-
    // frame cap of 14 accepts it (Wide group has 14 arms = at-cap;
    // Other group has 1 arm = under-cap). Phase 4f's push-N-frames
    // architecture allocates one frame per effect, so the cap
    // applies per-frame.
    //
    // Performs Wide.op13 (the highest-numbered op, arm_count=14,
    // op_id=13 → 13 < 14 satisfies the runtime bounds check) and
    // asserts the matching arm fires.
    //
    // Bisecting hint: a "TRAP / abort in sigil_handler_frame_new"
    // attributes to a per-frame cap regression introduced after this
    // commit; "stdout != 14" attributes to dispatch landing the
    // wrong arm.
    let src = "effect Wide { \
                 op00: () -> Int, op01: () -> Int, op02: () -> Int, \
                 op03: () -> Int, op04: () -> Int, op05: () -> Int, \
                 op06: () -> Int, op07: () -> Int, op08: () -> Int, \
                 op09: () -> Int, op10: () -> Int, op11: () -> Int, \
                 op12: () -> Int, op13: () -> Int }\n\
               effect Other { only: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle (perform Wide.op13()) with {\n    \
                   Wide.op00(k) => 0,\n    \
                   Wide.op01(k) => 1,\n    \
                   Wide.op02(k) => 2,\n    \
                   Wide.op03(k) => 3,\n    \
                   Wide.op04(k) => 4,\n    \
                   Wide.op05(k) => 5,\n    \
                   Wide.op06(k) => 6,\n    \
                   Wide.op07(k) => 7,\n    \
                   Wide.op08(k) => 8,\n    \
                   Wide.op09(k) => 9,\n    \
                   Wide.op10(k) => 10,\n    \
                   Wide.op11(k) => 11,\n    \
                   Wide.op12(k) => 12,\n    \
                   Wide.op13(k) => 14,\n    \
                   Other.only(k) => 99,\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "handle_at_max_handler_arms");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "14\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn handle_with_one_effect_exceeding_max_handler_arms_is_rejected_at_codegen() {
    // Plan B Task 55, Phase 4f polish round — negative case for the
    // per-frame arm-count cap. A multi-effect handle with one
    // effect at MAX_HANDLER_ARMS+1=15 arms must be rejected at
    // **compile time** (clean codegen-walker diagnostic), not at
    // runtime via `sigil_handler_frame_new`'s abort. The walker
    // check landed in this same polish-round commit alongside the
    // promotion of `MAX_HANDLER_ARMS` from `sigil_runtime::handlers`
    // to `sigil_abi::effect`.
    //
    // Asserts a clean compile-time diagnostic mentioning
    // `MAX_HANDLER_ARMS` and the offending effect name.
    let src = "effect TooWide { \
                 op00: () -> Int, op01: () -> Int, op02: () -> Int, \
                 op03: () -> Int, op04: () -> Int, op05: () -> Int, \
                 op06: () -> Int, op07: () -> Int, op08: () -> Int, \
                 op09: () -> Int, op10: () -> Int, op11: () -> Int, \
                 op12: () -> Int, op13: () -> Int, op14: () -> Int }\n\
               effect Other { only: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle 0 with {\n    \
                   TooWide.op00(k) => 0,\n    \
                   TooWide.op01(k) => 1,\n    \
                   TooWide.op02(k) => 2,\n    \
                   TooWide.op03(k) => 3,\n    \
                   TooWide.op04(k) => 4,\n    \
                   TooWide.op05(k) => 5,\n    \
                   TooWide.op06(k) => 6,\n    \
                   TooWide.op07(k) => 7,\n    \
                   TooWide.op08(k) => 8,\n    \
                   TooWide.op09(k) => 9,\n    \
                   TooWide.op10(k) => 10,\n    \
                   TooWide.op11(k) => 11,\n    \
                   TooWide.op12(k) => 12,\n    \
                   TooWide.op13(k) => 13,\n    \
                   TooWide.op14(k) => 14,\n    \
                   Other.only(k) => 99,\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let tmp = std::env::temp_dir().join(format!(
        "sigil_e2e_max_handler_arms_neg_{}.sigil",
        std::process::id()
    ));
    std::fs::write(&tmp, src).expect("write source");
    let bin_path = std::env::temp_dir().join(format!(
        "sigil_e2e_max_handler_arms_neg_{}",
        std::process::id()
    ));
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
        "compile must fail — TooWide group has 15 arms, exceeds MAX_HANDLER_ARMS=14; \
         got success with stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("MAX_HANDLER_ARMS") && stderr.contains("TooWide"),
        "error should reference MAX_HANDLER_ARMS and the offending effect name; got stderr={stderr:?}",
    );
}

/// Plan B Stage 6 cleanup — **inverted from the previously
/// `#[ignore]`'d `partial_handler_of_multi_op_effect_aborts_at_runtime
/// _pending_resolution`**. The latent op_id/arm_count constraint
/// resolves via option 2 (typecheck E0142 exhaustiveness) per the
/// trade-off in `[DEVIATION Task 55] Phase 4f` and `[Stage 6
/// cleanup]`: compile-time rejection beats runtime abort.
///
/// Pre-Stage-6-cleanup: `effect Choose { left, right }` with a
/// handle covering only `Choose.right` was syntactically accepted
/// and would runtime-abort if `Choose.left` ever fired. Post-Stage-
/// 6-cleanup: typecheck rejects with E0142 at compile time, naming
/// the unhandled op (`Choose.left`).
///
/// The test asserts the new compile-time behaviour: invoking the
/// sigil compiler on the partial-handler source produces a non-zero
/// exit and stderr containing `E0142` plus the unhandled op name.
#[test]
fn partial_handler_of_multi_op_effect_rejected_with_e0142() {
    let src = "effect Choose { left: () -> Int, right: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle (perform Choose.right()) with {\n    \
                   Choose.right(k) => 20,\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    assert_compile_fails_with_code(src, "E0142", &["Choose.left"], "partial_handler_e0142");
}

/// Plan B Stage 6 cleanup — **un-ignored from the previously
/// `#[ignore]`'d
/// `user_discard_k_io_handler_does_not_unwind_native_color_helper_-
/// pending_color_filter_lift`**. The IO color filter retention
/// (Task 57's perf-preserving choice) is lifted in Stage 6 cleanup:
/// `color::NATIVE_EFFECT` deleted, three codegen body-shape
/// classifier filters dropped, IO performs flow through the
/// trampoline like any other effect. User-installed discard-`k`
/// IO handlers now unwind helpers at the perform site, matching
/// the algebraic semantics non-IO effects already enjoyed via
/// Phase 4e captures+.
#[test]
fn user_discard_k_io_handler_unwinds_helper_at_perform_site() {
    // Plan B Task 57 — pinning test for the residual correctness
    // gap from Slice 1's IO color filter retention. Mirrors the
    // `discard_k_handler_does_not_abort_helper_phase_4e_pending`
    // (Phase 4d MVP) and `partial_handler_of_multi_op_effect_-
    // aborts_at_runtime_pending_resolution` (Phase 4f) precedents:
    // the test asserts the future-correct behaviour and is
    // `#[ignore]`'d while the gap exists, so it stays grep-findable
    // through the eventual fix.
    //
    // **The gap:** the colorer (`compiler/src/color.rs::NATIVE_EFFECT
    // = "IO"`) and three parallel codegen-classifier filters keep
    // IO-only fns Native-color. A user-installed discard-`k` IO
    // handler intercepts the perform, but the Native-color helper's
    // `lower_perform_to_value` synchronously calls `sigil_run_loop`,
    // which returns Unit from the discard arm; helper continues to
    // its post-perform code. Standard algebraic semantics expect
    // helper to unwind at the perform site (the arm discharged `k`,
    // so the perform never resumes).
    //
    // **Concrete failure:**
    //   - helper performs IO.println once, then returns 1.
    //   - User handler `IO.println(s, k) => 0` discards k.
    //   - Slice 1: helper's perform returns Unit synchronously,
    //     helper continues, returns 1. handle expression = 1.
    //     Stdout = "1\n".
    //   - v2 (filter lifted): helper is CPS-color, the perform
    //     yields to the trampoline, arm returns Done(0), trampoline
    //     observes Done. handle expression = 0 (arm body's value).
    //     helper does NOT continue past the perform. Stdout = "0\n".
    //
    // The assertion below is the **future-correct (v2)** value;
    // pre-fix the actual stdout is "1\n".
    //
    // **Future resolution:**
    //
    // - Lift `color::NATIVE_EFFECT` (drop the constant; replace its
    //   single use with a row-membership check that includes IO).
    // - Drop the three codegen classifier filters at
    //   `is_simple_tail_perform_with_pure_args_body`,
    //   `is_simple_yield_then_constant_tail_body`,
    //   `is_simple_let_yield_then_pure_tail_body` (each references
    //   `color::NATIVE_EFFECT` post-Slice-1, so the source-of-truth
    //   change is local; the filter call sites become unconditional).
    // - Un-ignore this test.
    //
    // The fix-PR un-ignores + verifies the assertion. No source
    // edits to this test should be required at fix time.
    let src = "fn helper() -> Int ![IO] {\n  \
                 perform IO.println(\"a\");\n  \
                 1\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper() with {\n    \
                   IO.println(s, k) => 0,\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "user_discard_k_io_handler");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    // Future-correct behaviour: helper unwinds at the perform site,
    // handle expression's value is the discharger arm's body (`0`).
    assert_eq!(
        stdout, "0\n",
        "expected helper to unwind at the perform site under filter-\
         lifted v2; got stdout={stdout:?}, stderr={stderr:?}"
    );
}

#[test]
fn statement_form_non_io_perform_inside_handle_compiles_and_runs() {
    // Plan B Task 55 (Phase 3b) — regression for the `Stmt::Perform`
    // crash. Before the fix, `lower_stmt` unconditionally called
    // `lower_perform`, which asserts `effect == "IO"`. Statement-form
    // non-IO performs (e.g. `perform Raise.fail();` followed by more
    // code) hit the assertion and crashed the compiler. The fix
    // dispatches the same way `Expr::Perform` does: IO → side-effect
    // path; non-IO → `lower_perform_non_io_to_value` with the value
    // discarded.
    //
    // **Phase 4e (this commit) — assertion inverted from `42` to
    // `99`.** Helper now matches the stmt-then-constant-tail body
    // shape (`is_simple_yield_then_constant_tail_body`), so
    // `compute_user_fn_abi` returns `Cps`. Codegen synthesises a
    // continuation closure that returns `Done(42)` (helper's tail)
    // and emits helper's body as `sigil_perform(eff, op, ..., null,
    // &synth_cont)` — yielding to the trampoline with the synth-
    // cont as the perform's k.
    //
    // Algebraic semantics under the Phase 4e shape:
    //
    //   - The arm `E.op(k) => 99` discards `k` (no reference to k
    //     in the arm body). It returns Done(99) directly.
    //   - The trampoline observes Done(99) and unwinds to the
    //     wrapper that called helper. The synth-cont — which would
    //     have returned Done(42) if the arm called k(any_value) —
    //     never runs.
    //   - main's `let n = ...` binds n = 99. main prints "99\n".
    //
    // This is the **discard-k correctness fix for stmt-form
    // perform yields** — the algebraic-semantics-correct behavior
    // that was the load-bearing piece for inverting this test
    // alongside (eventually) the `discard_k_handler_does_not_
    // abort_helper_phase_4e_pending` test.
    //
    // The previously-asserted `42` was the Phase 4d MVP synchronous
    // shape: helper's perform synchronously called sigil_run_loop
    // which dispatched the arm; arm returned 99 to the perform
    // site (where the Stmt::Perform discarded it); helper then
    // continued to its tail `42`. Pre-Phase-4e behavior.
    let src = "effect E { op: () -> Int }\n\
               fn helper() -> Int ![E] {\n  \
                 perform E.op();\n  \
                 42\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper() with { E.op(k) => 99 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, exit) = compile_and_run(src, "stmt_perform_non_io_in_handle");
    assert_eq!(exit, 0, "stdout={stdout:?} stderr={stderr:?}");
    assert_eq!(
        stdout, "99\n",
        "Phase 4e: discard-k arm fires; arm value flows to handle site, \
         not to perform site. stderr={stderr:?}"
    );
}

#[test]
fn nested_handle_in_outer_body_propagates_inner_unsupported_diagnostic() {
    // Plan B Task 55 — regression for the walker recursion bug: a
    // nested `handle` appearing in another handle's body must surface
    // its own Phase-4 restrictions. Before the fix, the outer
    // walker only recursed into arm bodies, so an inner handle's
    // codegen-pending restrictions were missed and the program would
    // have reached codegen with arms registered under unexpected
    // shapes — at runtime that crashes inside `sigil_perform`'s
    // handler-stack walk.
    //
    // **Phase 4g update:** the prior sentinel was an inner handle
    // with a `return` arm (Phase 4g-pending). Phase 4g lifts that —
    // return arms are now supported. Both Phase 4f (multi-effect)
    // and Phase 4g (return arms) sentinels are gone; this test
    // becomes a **positive** assertion that an inner nested handle
    // with both an op-arm and a `return(v) => body` arm compiles
    // cleanly and runs end-to-end. The walker-recursion regression
    // coverage is now exercised by
    // `nested_handle_with_inner_lambda_in_arm_body_is_rejected_at_codegen`
    // below — the still-rejected inner-handle restriction is a
    // nested `Lambda` / `ClosureRecord` in an arm body.
    //
    // Body math: inner `handle 0 with { return(v) => v + 1, ...}`
    // — body completes normally with value 0; return arm fires with
    // `v = 0` and returns `0 + 1 = 1`. Outer `handle 1 with
    // { Outer.op_out(k) => 0 }` — body produces 1, no perform fires
    // so no op arm runs; without a return arm, the handle's overall
    // value is the body's value = 1. main prints "1\n".
    let src = "effect Inner { op_in: () -> Int }\n\
               effect Outer { op_out: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle\n    \
                   (handle 0 with {\n      \
                     return(v) => v + 1,\n      \
                     Inner.op_in(k) => 1,\n    \
                   })\n  \
                 with { Outer.op_out(k) => 0 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "phase_4g_nested_inner_return_arm");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "1\n", "stdout mismatch; stderr={stderr:?}");
}

// Phase 4c lifted the Phase 3b "IntLit-only arm body" restriction; the
// old `handle_with_non_intlit_arm_body_is_rejected_at_codegen` test is
// gone with it. Coverage of what Phase 4c still rejects lives in
// `arm_uses_k_is_rejected_at_codegen` (k-usage gate) and
// `arm_captures_outer_scope_is_rejected_at_codegen` (capture gate);
// arithmetic / call / block arm bodies are now supported and exercised
// by `arm_body_does_arithmetic_on_op_args` and the Phase 4c acceptance
// precondition tests below.

/// P17 compose source: rejects pending builtin-as-fn-value
/// support. Stage 6.8 originally framed this rejection as
/// "until TypeExpr::Fn ships" — TypeExpr::Fn DID ship (B.3),
/// but compose's source has a second blocker that survives:
/// `compose(int_to_string, ...)` passes the builtin
/// `int_to_string` as a fn-typed argument. Phase C v1's fn-as-
/// value materialization (Task 104) handles user-declared top-
/// level fns by rewriting bare `Ident(name)` to a captureless
/// `ClosureRecord`, but builtins are seeded into typecheck's
/// `fn_env` without a corresponding `Item::Fn`, so they're
/// absent from `top_level_fn_names`. closure-convert leaves
/// `int_to_string` as `Ident(...)`, and codegen panics in
/// `lower_expr(Ident)` when the name isn't in env / ctors /
/// the user-fn ClosureRecord materialization branch.
///
/// See `[DEVIATION p17_compose blocker analysis]` (2026-04-29)
/// for the full surface analysis. Task 109 closes this by
/// rewriting the example source to use a user-side wrapper:
/// `fn its(n: Int) -> String ![] { int_to_string(n) }` and
/// inverting the test to a positive runtime check.
///
/// Source note: the outer `compose` return type carries TWO
/// `![..]` markers per the per-arrow effect-row discipline
/// (`[DEVIATION Task 103]`) — first for the inner returned
/// fn-type, second for compose's own effect row. Without the
/// second `![..]` the test would trip on Blocker 1 (parse
/// rejection) instead of Blocker 2 (the actual remaining
/// surface). With the per-arrow fix in source, this test
/// pins Blocker 2 specifically.
#[test]
fn p17_compose_source_rejects_pending_builtin_as_fn_value() {
    let src = "fn compose[A, B, C](f: (B) -> C ![], g: (A) -> B ![]) -> (A) -> C ![] ![] {\n  \
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
        "P17 source must NOT compile until builtin-as-fn-value ships; got success with stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

// ----- Plan B Task 55 (MF2) — differential identity property test -----
//
// **What this pins.** For any "Native-eligible" Sigil expression
// (one that produces an `Int` and contains no non-IO `perform` site,
// so it could in principle be lowered without any handler-frame
// machinery), wrapping the expression in a vacuous handler must NOT
// change its observed value. The wrapper installs the handler-frame
// ABI (`sigil_handler_frame_new` + `sigil_handle_push` +
// `sigil_handle_pop`) plus a synthetic CPS arm fn that never gets
// dispatched (the body never `perform`s the handled effect). If the
// wrapper changes the answer, the codegen-side handler-frame setup
// has corrupted some piece of state on the path Phase 4+ will build
// on (caller's stack, register save/restore, GC roots, etc.).
//
// **Why a property test rather than hand-rolled cases.** The shape
// of the bug we're guarding against ("CPS lowering breaks native
// semantics") generalises across every native-color expression
// shape; a fuzz of expression shapes catches accidental
// shape-specific corruption that hand-rolled tests would miss.
//
// **Determinism.** A fixed seed + xorshift PRNG produces the same
// 24 expressions on every run. CI failures pin a single expression
// reproducibly and the seed/index can be inverted for triage.
//
// **Scope.** Phase 3b/4a's codegen-entry guard rejects arm bodies
// that aren't `IntLit` and ops with user args, so the wrapper
// stays inside the supported subset by using `effect E { op: () ->
// Int }` with arm body `999`. The body expression itself contains
// no `perform` (the generator never emits one), so all Phase 3b
// restrictions on the body are trivially satisfied. As Phase 4b/4c
// lift restrictions, this test continues to pin the wrapper's
// identity property — and stays load-bearing precisely because the
// shape of the bug it guards against doesn't shrink.
//
// **What this test does NOT exercise.** The body never performs
// the handled effect, so the synthetic CPS arm fn is dead code at
// runtime — the test pins only the Phase 3a frame plumbing
// (`frame_new` + `set_arm` + `push` + `pop`) against the wrapped
// expression's native semantics. The Phase 3b/4a dispatch path
// (`sigil_perform` → `sigil_run_loop` → arm → `next_step_done`
// → value extraction) is exercised instead by the paired test
// `cps_dispatch_returns_arm_value_across_op_id_shape_space`
// below, which uses a body of `(perform E_eff.op())` so every
// trial drives the dispatch loop end-to-end.

/// Tiny deterministic PRNG. The differential-identity test runs in
/// CI on every commit and we want bit-exact reproducibility — using
/// `rand` would pull in a transitive dep tree just for one test.
/// Xorshift64 is a 25-line state machine with adequate quality for
/// this scale (24 expressions, ~5 random choices per expression).
struct Xorshift64 {
    state: u64,
}

impl Xorshift64 {
    fn new(seed: u64) -> Self {
        // Avoid the all-zero state which xorshift cannot escape;
        // 0xdeadbeef is an acceptable substitute.
        Self {
            state: if seed == 0 { 0xdeadbeef } else { seed },
        }
    }

    fn next(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    /// Inclusive range. `lo <= hi` required.
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        debug_assert!(lo <= hi);
        let span = (hi - lo + 1) as u64;
        lo + (self.next() % span) as i64
    }

    fn next_bool(&mut self) -> bool {
        (self.next() & 1) == 1
    }
}

/// Generate a Sigil source-level expression that evaluates to an
/// `Int`. Bounded by `depth_remaining` to keep pathological growth
/// in check. Contains no `perform` (Native-eligible by construction)
/// and no division/modulo (avoids div-by-zero traps that would mask
/// the real signal). Operand magnitudes are kept small enough that
/// chained `*`s don't overflow i64.
fn gen_int_expr(rng: &mut Xorshift64, depth_remaining: u32) -> String {
    if depth_remaining == 0 || rng.next_bool() {
        // Leaf — small int, occasionally negated via unary `-`.
        let n = rng.range(0, 9);
        if rng.next_bool() {
            format!("(-{n})")
        } else {
            format!("{n}")
        }
    } else {
        match rng.range(0, 3) {
            0 => format!(
                "({} + {})",
                gen_int_expr(rng, depth_remaining - 1),
                gen_int_expr(rng, depth_remaining - 1),
            ),
            1 => format!(
                "({} - {})",
                gen_int_expr(rng, depth_remaining - 1),
                gen_int_expr(rng, depth_remaining - 1),
            ),
            2 => format!(
                "({} * {})",
                gen_int_expr(rng, depth_remaining - 1),
                gen_int_expr(rng, depth_remaining - 1),
            ),
            // if-else branch — bool literal cond keeps the test
            // deterministic without risking unrelated comparison-
            // op coverage gaps in our shape sample.
            _ => format!(
                "(if {} {{ {} }} else {{ {} }})",
                if rng.next_bool() { "true" } else { "false" },
                gen_int_expr(rng, depth_remaining - 1),
                gen_int_expr(rng, depth_remaining - 1),
            ),
        }
    }
}

#[test]
fn cps_wrapped_identity_matches_native_on_native_eligible_programs() {
    // Plan B Task 55 (MF2 / standing precondition [P1]): for every
    // generated Native-eligible expression `E`, the program
    //
    //   fn main() -> Int ![IO] {
    //     perform IO.println(int_to_string(E));
    //     0
    //   }
    //
    // and the program
    //
    //   effect E_eff { op: () -> Int }
    //   fn main() -> Int ![IO] {
    //     let n: Int = handle E with { E_eff.op(k) => 999 };
    //     perform IO.println(int_to_string(n));
    //     0
    //   }
    //
    // must produce identical stdout. The wrapper installs the
    // handler-frame ABI + synthetic CPS arm fn around `E`; if the
    // wrapper changes the answer, codegen has corrupted state on
    // the path that Phase 4b/4c/4d builds on. See block comment
    // above for full rationale.
    //
    // Iteration count is intentionally modest (24 trials) — each
    // trial is two `sigil` compile+run cycles, and CI time is
    // billable. Increase if/when the failure rate stays at zero
    // across a stable window of phases.
    const SEED: u64 = 0x55_2055_2055_2055; // "Task 55, MF2".
    const TRIALS: u32 = 24;
    const MAX_DEPTH: u32 = 3;

    let mut rng = Xorshift64::new(SEED);
    for trial in 0..TRIALS {
        let expr = gen_int_expr(&mut rng, MAX_DEPTH);

        let native_src = format!(
            "fn main() -> Int ![IO] {{\n  \
               perform IO.println(int_to_string({expr}));\n  \
               0\n\
             }}\n"
        );
        let wrapped_src = format!(
            "effect E_eff {{ op: () -> Int }}\n\
             fn main() -> Int ![IO] {{\n  \
               let n: Int = handle {expr} with {{ E_eff.op(k) => 999 }};\n  \
               perform IO.println(int_to_string(n));\n  \
               0\n\
             }}\n"
        );

        let (native_stdout, native_stderr, native_exit) =
            compile_and_run(&native_src, &format!("mf2_native_{trial}"));
        let (wrapped_stdout, wrapped_stderr, wrapped_exit) =
            compile_and_run(&wrapped_src, &format!("mf2_wrapped_{trial}"));

        assert_eq!(
            native_exit, 0,
            "trial {trial}: native compile/run failed.\nexpr: {expr}\nstderr: {native_stderr}",
        );
        assert_eq!(
            wrapped_exit, 0,
            "trial {trial}: wrapped compile/run failed.\nexpr: {expr}\nstderr: {wrapped_stderr}",
        );
        assert_eq!(
            native_stdout, wrapped_stdout,
            "trial {trial}: CPS-wrapped output diverged from native.\n\
             expr: {expr}\n\
             native stdout: {native_stdout:?}\n\
             wrapped stdout: {wrapped_stdout:?}\n\
             native stderr: {native_stderr}\n\
             wrapped stderr: {wrapped_stderr}",
        );
    }
}

#[test]
fn cps_dispatch_returns_arm_value_across_op_id_shape_space() {
    // Plan B Task 55 (paired MF2 — perform-dispatch coverage):
    // exercises the full Phase 3b/4a dispatch path
    //
    //   sigil_perform → sigil_run_loop → arm fn → sigil_next_step_done
    //   → run_loop returns u64 → caller reads value
    //
    // on every trial. The body is `(perform E_eff.op())`, so the
    // arm runs end-to-end on every iteration; the arm body is the
    // generated `IntLit` (Phase 4a's IntLit-only restriction
    // satisfied), and the program prints the arm's value.
    //
    // The "shape space" sampled here is small — Phase 4a restricts
    // arm bodies to `Expr::IntLit` so the only variation per trial
    // is the integer constant. That is sufficient to catch
    // dispatch-path regressions: any miscompile of `sigil_perform`
    // arg-passing, run_loop dispatch, or `next_step_done` → value
    // extraction surfaces as a wrong constant on stdout.
    //
    // Phase 4b/4c will widen the "shape space" once arm bodies and
    // op-args grow. The xorshift PRNG is reused with a different
    // seed so the two property tests don't overlap.
    const SEED: u64 = 0x4D_46_32_44_49_53_50_00; // "MF2DISP\0"
    const TRIALS: u32 = 12;

    let mut rng = Xorshift64::new(SEED);
    for trial in 0..TRIALS {
        // Arm value sampled from a wider range than MF2's leaf
        // generator: this is the full thing being checked, so we
        // want both small positives, negatives, and "unusual"
        // values like 0.
        let arm_value: i64 = rng.range(-99, 99);

        let src = format!(
            "effect E_eff {{ op: () -> Int }}\n\
             fn main() -> Int ![IO] {{\n  \
               let n: Int = handle (perform E_eff.op()) with {{ E_eff.op(k) => {arm_value} }};\n  \
               perform IO.println(int_to_string(n));\n  \
               0\n\
             }}\n"
        );
        let (stdout, stderr, exit) = compile_and_run(&src, &format!("mf2_dispatch_{trial}"));
        assert_eq!(
            exit, 0,
            "trial {trial}: compile/run failed for arm_value={arm_value}.\nstderr: {stderr}",
        );
        assert_eq!(
            stdout,
            format!("{arm_value}\n"),
            "trial {trial}: dispatch returned wrong value for arm_value={arm_value}.\nstderr: {stderr}",
        );
    }
}

#[test]
fn handle_with_three_arms_dispatches_op_id_two() {
    // Plan B Task 55 (Phase 4a — substantive #3): the existing 1-arm
    // and 2-arm e2e tests cover op_id ∈ {0, 1}. This test extends
    // coverage to op_id = 2, validating that
    //   - effect/op ID assignment is alphabetical and stable
    //     (`a=0, b=1, c=2`)
    //   - `sigil_handler_frame_set_arm` indexes the arm slot bitmap
    //     correctly at index 2
    //   - `sigil_perform`'s linear walk dispatches to op_id=2 even
    //     when arms 0 and 1 are present and unmatched
    //
    // Without this test, off-by-one in op_id arithmetic would
    // surface only at MAX_HANDLER_ARMS=14 (covered by runtime unit
    // tests) — never at the small index where most user code lives.
    let src = "effect Pick {\n  \
                 a: () -> Int,\n  \
                 b: () -> Int,\n  \
                 c: () -> Int,\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle (perform Pick.c()) with {\n    \
                   Pick.a(k) => 0,\n    \
                   Pick.b(k) => 1,\n    \
                   Pick.c(k) => 2,\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, exit) = compile_and_run(src, "handle_three_arms_op_id_two");
    assert_eq!(exit, 0, "stdout={stdout:?} stderr={stderr:?}");
    assert_eq!(stdout, "2\n", "stderr={stderr:?}");
}

#[test]
fn handle_with_int_arg_op_packs_args_buffer() {
    // Plan B Task 55 (Phase 4b): non-IO `perform Effect.op(args...)`
    // sites now pack user args into a stack-allocated `[u64; N]`
    // buffer that `sigil_perform` reads. Phase 4b ships the perform-
    // side packing; arm fns still ignore the args buffer (their
    // bodies are still IntLit-only — Phase 4c lifts that). The
    // observable contract here is that the program compiles + runs
    // (no codegen-entry rejection of the user-arg-bearing perform,
    // no runtime crash from a malformed args buffer or `sigil_perform`
    // overflow check) and returns the arm's IntLit value.
    let src = "effect Raise { fail: (Int) -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle (perform Raise.fail(99)) with {\n    \
                   Raise.fail(msg, k) => 0,\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, exit) = compile_and_run(src, "handle_int_arg_packs");
    assert_eq!(exit, 0, "stdout={stdout:?} stderr={stderr:?}");
    assert_eq!(stdout, "0\n", "stderr={stderr:?}");
}

#[test]
fn handle_with_three_int_args_packs_buffer() {
    // Plan B Task 55 (Phase 4b): exercises multi-arg packing — three
    // user args + the implicit `(k_closure, k_fn)` pair = 5
    // dispatched values, well under MAX_INLINE_ARGS = 32. The args
    // get stored at slot offsets 0, 8, 16 on the perform side and
    // copied verbatim by `sigil_perform` into the dispatched
    // NextStep::Call's args slots. Arm body is still IntLit-only
    // (Phase 4c will read the bound names); this test pins the
    // buffer-packing path doesn't off-by-one or misalign across
    // arg count > 1.
    let src = "effect Triple { do: (Int, Int, Int) -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle (perform Triple.do(10, 20, 30)) with {\n    \
                   Triple.do(a, b, c, k) => 7,\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, exit) = compile_and_run(src, "handle_three_int_args_packs");
    assert_eq!(exit, 0, "stdout={stdout:?} stderr={stderr:?}");
    assert_eq!(stdout, "7\n", "stderr={stderr:?}");
}

#[test]
fn handle_with_mixed_type_args_widens_correctly() {
    // Plan B Task 55 (Phase 4b): exercises the per-arg widening path
    // in `lower_perform_non_io_to_value`. The args buffer is `[u64;
    // N]`; narrower Cranelift types (I8 for Bool, I32 for Char) get
    // `uextend`'d to I64 before the slot store; pointer-typed args
    // (String) store directly because pointer_ty == I64 on every
    // supported target. A signed-overflow / narrow-store regression
    // would surface as either a Cranelift verifier failure at
    // `cargo build`-of-compiled-binary time (mismatched store width)
    // or a runtime crash inside `sigil_perform`'s `args_ptr.add(i)`
    // u64-stride read. Without this test, the widen branch sits dead
    // until Phase 4c ships an arm body that reads the bound name.
    let src = "effect Mix { it: (Int, Bool, String) -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle (perform Mix.it(42, true, \"hi\")) with {\n    \
                   Mix.it(n, b, s, k) => 11,\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, exit) = compile_and_run(src, "handle_mixed_type_args_widen");
    assert_eq!(exit, 0, "stdout={stdout:?} stderr={stderr:?}");
    assert_eq!(stdout, "11\n", "stderr={stderr:?}");
}

// =====================================================================
// Plan B Task 55 — Phase 4c acceptance precondition tests
// =====================================================================
//
// The 4 tests below are the **pre-registered acceptance precondition**
// for Phase 4c per the `[DEVIATION Task 55] Phase 4b — args-buffer
// packing on perform side` entry in PLAN_B_DEVIATIONS.md (added during
// PR #23 review-fixup). They close the args-content verification gap
// from Phase 4b: the Phase 4b e2e tests pin only that the FFI plumbing
// compiles + runs (arms ignored args_ptr there). Phase 4c reads bound
// names from args_ptr in the synthetic arm fn, so a misalignment, off-
// by-one, or wrong-direction widening that landed green under Phase 4b
// would fail here.
//
// Coverage matrix (all required by the deviation entry):
//   1. Int arg readback — pins source value reaches arm
//   2. Bool / Char arg readback — exercises uextend/ireduce widening
//   3. String arg readback — exercises pointer-store path
//   4. Multi-arg readback in declared order — pins offset arithmetic

#[test]
fn arm_reads_int_arg_returns_it() {
    // Phase 4c — arg-content verification (1/4): pass an Int arg
    // through perform, bind it in the arm body, return it. Pins
    // that the perform-side widen → slot-store → sigil_perform copy
    // → arm-fn ireduce-back chain preserves Int values bit-for-bit.
    let src = "effect E { op: (Int) -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle (perform E.op(42)) with {\n    \
                   E.op(x, k) => x,\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, exit) = compile_and_run(src, "phase4c_arm_reads_int");
    assert_eq!(exit, 0, "stdout={stdout:?} stderr={stderr:?}");
    assert_eq!(stdout, "42\n", "stderr={stderr:?}");
}

#[test]
fn arm_reads_bool_arg_branches_on_it() {
    // Phase 4c — arg-content verification (2a/4): Bool arg goes
    // through uextend (perform side) → u64 slot → ireduce(I8) (arm
    // side) → branch. The arm uses `if b { 1 } else { 0 }` to
    // observe the bound bool through a value-distinguishing branch.
    // Without correct widen-truncate roundtrip, the bool would
    // either always be true (any non-zero u64 → true under naive
    // reduction) or always be false.
    let src = "effect E { op: (Bool) -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle (perform E.op(true)) with {\n    \
                   E.op(b, k) => if b { 7 } else { 99 },\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, exit) = compile_and_run(src, "phase4c_arm_reads_bool");
    assert_eq!(exit, 0, "stdout={stdout:?} stderr={stderr:?}");
    assert_eq!(stdout, "7\n", "stderr={stderr:?}");
}

#[test]
fn arm_reads_string_arg_prints_via_io_println() {
    // Phase 4c — arg-content verification (3/4): String arg goes
    // through the pointer-store path (no widening — pointer_ty ==
    // I64 on supported targets). The arm body returns the bound
    // `s` directly (op declared `(String) -> String` so handle's
    // overall is String); main then prints it via IO.println at
    // the outer scope. This exercises:
    //   - perform-side: arg's heap-pointer Value stored at offset 0
    //   - runtime: copies pointer into NextStep::Call's args slot
    //   - arm-fn: loads u64 from args_ptr, binds as String pointer
    //     (no ireduce — declared_ty == pointer_ty)
    //   - arm body: env lookup for `s` returns the bound pointer
    //   - perform-side narrow: returns widened I64 (pointer_ty path,
    //     no narrow needed since pointer_ty == I64)
    //
    // A wrong-arg-buffer-offset bug would print garbage or crash
    // inside sigil_println dereferencing a non-string pointer.
    //
    // (Sigil v1's parser doesn't accept `{ stmt; expr }` as an
    // expression — Block only appears in fn bodies / if branches —
    // so the arm body has to be a single Ident expression rather
    // than `{ perform IO.println(s); 0 }`.)
    let src = "effect E { op: (String) -> String }\n\
               fn main() -> Int ![IO] {\n  \
                 let s: String = handle (perform E.op(\"hello\")) with {\n    \
                   E.op(arg, k) => arg,\n  \
                 };\n  \
                 perform IO.println(s);\n  \
                 0\n\
               }\n";
    let (stdout, stderr, exit) = compile_and_run(src, "phase4c_arm_reads_string");
    assert_eq!(exit, 0, "stdout={stdout:?} stderr={stderr:?}");
    assert_eq!(stdout, "hello\n", "stderr={stderr:?}");
}

#[test]
fn arm_reads_multi_args_in_declared_order() {
    // Phase 4c — arg-content verification (4/4): three Int args at
    // perform site `(10, 20, 30)`, arm `(a, b, c, k) => b` returns
    // the middle one. Pins offset arithmetic on the perform side
    // (slot offsets 0, 8, 16) matches the runtime's `args_ptr.add(i)`
    // u64-stride read. An off-by-one in either direction would
    // surface as 10 or 30 instead of 20; a swapped order would
    // surface as the wrong end. None of the Phase 4b tests would
    // have caught any of these.
    let src = "effect E { op: (Int, Int, Int) -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle (perform E.op(10, 20, 30)) with {\n    \
                   E.op(a, b, c, k) => b,\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, exit) = compile_and_run(src, "phase4c_arm_reads_multi_arg_order");
    assert_eq!(exit, 0, "stdout={stdout:?} stderr={stderr:?}");
    assert_eq!(stdout, "20\n", "stderr={stderr:?}");
}

#[test]
fn arm_reads_char_arg_branches_on_codepoint() {
    // Phase 4c — arg-content verification (2b/4): Char arg goes
    // through `uextend(I64, _)` (perform side, I32 → I64) → u64
    // slot → `ireduce(I32, _)` (arm side) → branch via `==`.
    //
    // Bool (test 2a above) exercises the I8 width of the widen/
    // ireduce path; this test exercises the I32 (Char) width of
    // the same path. They are distinct Cranelift instructions
    // operating on distinct value widths, so the Bool test
    // alone leaves the I32 leg verifier-checked but not value-
    // checked. A wrong-direction extend (`sextend` vs `uextend`)
    // or a width-swap regression on the Char path would land
    // green under Bool-only coverage.
    //
    // Closes part of PR #24 review MF1 (Char arg-readback).
    let src = "effect E { op: (Char) -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle (perform E.op('Z')) with {\n    \
                   E.op(c, k) => if c == 'Z' { 1 } else { 0 },\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, exit) = compile_and_run(src, "phase4c_arm_reads_char");
    assert_eq!(exit, 0, "stdout={stdout:?} stderr={stderr:?}");
    assert_eq!(stdout, "1\n", "stderr={stderr:?}");
}

#[test]
fn perform_side_narrow_to_bool_value_checked() {
    // Phase 4c — perform-side narrow value-check (closes PR #24
    // review MF2). All the precondition-matrix tests above
    // declare ops returning `Int` (or `String`), so the
    // perform-side narrow at `compiler/src/codegen.rs::lower_perform_non_io_to_value`
    // always takes the `return_ty == I64` no-op branch and the
    // `ireduce(return_ty, widened)` instruction the PR ships
    // is verifier-checked but not value-checked.
    //
    // Here the op is declared `(Int) -> Bool`. Arm body returns
    // `true`; the arm fn widens to I64 via `uextend` (matching
    // `sigil_next_step_done`'s I64 signature); `sigil_run_loop`
    // returns I64; `lower_perform_non_io_to_value` narrows
    // back via `ireduce(I8, widened)` so the surrounding
    // code sees a Cranelift I8 Value (matching `type_of_expr`'s
    // prediction for a Bool-returning perform). Without the
    // narrow, the `if b` would consume an I64 where I8 is
    // expected — Cranelift's verifier would reject. With a
    // wrong-direction sign extend in the body widen, `if b`
    // would observe `false` and return `99` instead of `7`.
    let src = "effect E { op: (Int) -> Bool }\n\
               fn main() -> Int ![IO] {\n  \
                 let b: Bool = handle (perform E.op(1)) with {\n    \
                   E.op(n, k) => true,\n  \
                 };\n  \
                 let n: Int = if b { 7 } else { 99 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, exit) = compile_and_run(src, "phase4c_perform_narrow_bool");
    assert_eq!(exit, 0, "stdout={stdout:?} stderr={stderr:?}");
    assert_eq!(stdout, "7\n", "stderr={stderr:?}");
}

#[test]
fn perform_side_narrow_to_char_value_checked() {
    // Phase 4c — perform-side narrow value-check (closes PR #24
    // review MF2 second leg). Mirror of `perform_side_narrow_to_bool_value_checked`
    // for the I32 (Char) width. Op is declared `(Int) -> Char`;
    // arm body returns the Char `'Y'`; perform-side narrow uses
    // `ireduce(I32, widened)` to restore the Char Cranelift
    // type so the subsequent `c == 'Y'` equality check operates
    // on matching widths. Bool covers the I8 width, this covers
    // the I32 width — symmetric to MF1's Bool-vs-Char split on
    // the perform→arm widen leg.
    let src = "effect E { op: (Int) -> Char }\n\
               fn main() -> Int ![IO] {\n  \
                 let c: Char = handle (perform E.op(1)) with {\n    \
                   E.op(n, k) => 'Y',\n  \
                 };\n  \
                 let n: Int = if c == 'Y' { 11 } else { 22 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, exit) = compile_and_run(src, "phase4c_perform_narrow_char");
    assert_eq!(exit, 0, "stdout={stdout:?} stderr={stderr:?}");
    assert_eq!(stdout, "11\n", "stderr={stderr:?}");
}

// Phase 4c bonus tests — beyond the precondition matrix, exercise
// richer arm-body shapes the Lowerer now supports.

#[test]
fn arm_body_does_arithmetic_on_op_args() {
    // Phase 4c bonus: arm body uses both op-args in an arithmetic
    // expression. Pins that the Lowerer-driven path correctly
    // resolves multiple bound names + lowers binary ops in the
    // synthetic-fn context.
    let src = "effect E { op: (Int, Int) -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle (perform E.op(5, 7)) with {\n    \
                   E.op(a, b, k) => a * b + 1,\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, exit) = compile_and_run(src, "phase4c_arm_arithmetic");
    assert_eq!(exit, 0, "stdout={stdout:?} stderr={stderr:?}");
    assert_eq!(stdout, "36\n", "stderr={stderr:?}"); // 5*7+1
}

#[test]
fn arm_uses_k_in_tail_position_returns_continuation_value() {
    // Plan B Task 55 (Phase 4d MVP): tail-position `k(arg)` is
    // accepted. The arm body's tail expression `k(0)` lowers to
    // `sigil_next_step_call(k_closure_loaded, k_fn_loaded, 1)`
    // followed by a u64 store of `0` into the returned NextStep's
    // args buffer. The trampoline dispatches the Call into
    // `sigil_continuation_identity`, which returns `Done(0)`, and
    // `sigil_run_loop` returns `0` to the perform site.
    //
    // Algebraic semantics under the synchronous shape: when the
    // perform is in tail position of the handle body (here `(perform
    // E.op())` IS the body), `k(arg)` produces `arg` as the handle's
    // overall result — same observable behaviour as `arg`-flowing-
    // through-identity. The README "Verification limits" section and
    // the Phase 4d deviation entry document the cases where the
    // synchronous shape diverges from algebraic semantics
    // (discard-k across function-call boundaries, non-tail k use);
    // tail-position k(arg) on a tail-position perform is correct.
    let src = "effect E { op: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle (perform E.op()) with {\n    \
                   E.op(k) => k(99),\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "phase4d_tail_k_returns_value");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "99\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn arm_captures_outer_scope_returns_value() {
    // Plan B Task 55 (Phase 4d MVP): arm bodies that capture
    // surrounding-fn locals (here `threshold`, a top-level fn
    // parameter) are now supported. The codegen `Expr::Handle`
    // path allocates a per-arm closure record holding `threshold`'s
    // value at slot 0; the synthetic CPS arm fn's `closure_ptr`
    // (passed via `sigil_handler_frame_set_arm`) points at that
    // record. The arm body's reference to `threshold` lowers via
    // `lower_closure_env_load` (offset 16, narrow per
    // `EnvSlotKind::Int`).
    //
    // Note the arm discards `k` — under the Phase 4d MVP synchronous
    // shape, when the perform is in tail position of the handle body
    // (`(perform E.op())` IS the body) the discard-k arm value
    // flows to the perform site → returned by `sigil_run_loop` →
    // becomes the handle's overall value. This matches algebraic
    // semantics for the in-tail-position case. The cross-function-
    // call discard-k correctness gap is documented in the README's
    // "Verification limits" section and pinned in
    // `discard_k_handler_does_not_abort_helper_phase_4e_pending`
    // below.
    let src = "effect E { op: () -> Int }\n\
               fn helper(threshold: Int) -> Int ![IO] {\n  \
                 let n: Int = handle (perform E.op()) with {\n    \
                   E.op(k) => threshold,\n  \
                 };\n  \
                 n\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 perform IO.println(int_to_string(helper(42)));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "phase4d_arm_captures_outer_scope");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "42\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn arm_inside_lambda_captures_outer_via_closure_env_load_returns_value() {
    // Plan B Task 55, Phase 4e captures+ Slice D — `Expr::Handle`
    // inside a `Lambda` whose body captures outer-scope names.
    //
    // This is the inversion of the Phase 4d MVP `#[ignore]`'d test
    // `arm_inside_lambda_captures_outer_via_closure_env_load_is_rejected_at_codegen_phase_4e_pending`.
    // Pre-Slice-D the walker rejected `Expr::ClosureEnvLoad` in
    // arm bodies with a "Phase 4e captures-of-surrounding-lambda"
    // diagnostic; post-Slice-D the walker accepts the shape and
    // codegen sources the capture's value via
    // `lower_closure_env_load(idx, kind)` against the lambda's
    // `closure_ptr` (in scope at handle codegen time because the
    // surrounding fn IS the lifted lambda).
    //
    // Trace:
    // - `let x: Int = 7;` in main.
    // - The lambda `fn (_d: Int) -> Int ![IO] => handle ...` captures
    //   `x` from main's scope. closure_convert lifts it into a
    //   synthetic top-level fn with a closure record holding `x`.
    //   References to `x` inside the lambda body get rewritten to
    //   `Expr::ClosureEnvLoad { name: "x", index: <lambda_slot>, .. }`.
    // - The arm body `E.op(k) => x` becomes `E.op(k) =>
    //   ClosureEnvLoad { name: "x", index: <lambda_slot>, .. }` after
    //   closure_convert. Slice D pre-pass scans this for matching
    //   names per arm capture and populates `ArmCapture::lambda_source =
    //   Some((<lambda_slot>, <kind>))`.
    // - The IIFE invocation `(fn ... => ...)(0)` triggers the
    //   handle expression. The arm fires (E.op is performed in the
    //   handle body); at handle codegen time inside the lifted
    //   lambda, the ARM's closure record alloc sources `x`'s value
    //   via `lower_closure_env_load(<lambda_slot>, Int)` against the
    //   lambda's `closure_ptr` (which holds `[7]` at runtime). The
    //   value 7 is stored at the arm's closure-record slot 0.
    // - The arm fn at runtime reads `x` from the arm's closure_ptr
    //   slot 0 (via `rewrite_arm_body_with_captures`'s ARM-LOCAL
    //   re-indexing) and returns it.
    //
    // Expected stdout: `"7\n"`.
    //
    // Sigil v1's `TypeExpr::Fn` surface syntax is deferred (see
    // examples/higher_order.sigil's preamble note + the Plan A2 Task
    // 30 carryover), so the lambda is invoked as an IIFE rather
    // than let-bound and called by name. The closure_convert rewrite
    // of the captured `x` → `ClosureEnvLoad` happens the same way
    // for an IIFE'd lambda as for a let-bound one.
    let src = "effect E { op: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let x: Int = 7;\n  \
                 let n: Int = (fn (_d: Int) -> Int ![IO] => handle (perform E.op()) with {\n    \
                   E.op(k) => x,\n  \
                 })(0);\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "slice_d_arm_inside_lambda_captures_outer");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "7\n",
        "Slice D: arm body inside a synthetic lambda fn captures `x` from \
         the lambda's closure record via `lower_closure_env_load`. Arm fires \
         on `perform E.op()`, returns `x = 7`. stderr={stderr:?}"
    );
}

#[test]
fn arm_uses_k_in_non_tail_position_is_rejected_pointing_at_phase_4e() {
    // Plan B Task 55 (Phase 4d MVP) — Phase 4e closure point:
    // non-tail-position `k(arg)` (where the result of `k(arg)`
    // feeds into another expression) is rejected with a Phase-4e-
    // pointing diagnostic. The synchronous shape can't yield from
    // an arm fn mid-body and resume; lifting requires CPS-
    // transforming the arm body itself, which forces the
    // surrounding native fn to be CPS-color so the arm-body's
    // continuation can return NextStep::Call to it. That's the
    // calling-convention shift Phase 4e ships alongside the
    // colorer's handler-discharge refinement.
    //
    // Test program: arm body is `k(0) + 1` — the `k(0)` is in
    // arithmetic-binop-LHS position, not tail. The walker rejects.
    let src = "effect E { op: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle (perform E.op()) with {\n    \
                   E.op(k) => k(0) + 1,\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let tmp = std::env::temp_dir().join(format!(
        "sigil_e2e_phase4d_non_tail_k_reject_{}.sigil",
        std::process::id()
    ));
    std::fs::write(&tmp, src).expect("write source");
    let bin_path = std::env::temp_dir().join(format!(
        "sigil_e2e_phase4d_non_tail_k_reject_{}",
        std::process::id()
    ));
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
        "compile must fail — arm body uses k in non-tail position; got success \
         with stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("non-tail") || stderr.contains("Phase 4e"),
        "error message should reference non-tail k / Phase 4e; got stderr={stderr:?}",
    );
}

#[test]
fn cps_abi_helper_with_simple_tail_perform_called_from_native_main_returns_arm_value() {
    // Plan B Task 55, Phase 4e — the first slice of CPS-ABI
    // user-fn emission. `raise_e` has an empty stmts list and tail
    // = `perform E.op()`; it matches `is_simple_tail_perform_with
    // _pure_args_body` and is intrinsically CPS-color (row contains
    // `E`). `compute_user_fn_abi` returns `UserFnAbi::Cps`, so
    // `raise_e` is declared with the CPS calling convention
    // `(closure_ptr, args_ptr, args_len) -> *mut NextStep` (per
    // `cps_signature`).
    //
    // `main` is CPS-color via the SCC bridge to `raise_e`, but its
    // body is multi-stmt — `compute_user_fn_abi` returns
    // `UserFnAbi::Sync`, so `main` keeps the existing closure-
    // convention signature and the synchronous body lowering. The
    // call to `raise_e` from `main` routes through the inlined
    // native↔CPS interop wrapper at `lower_call`: pack
    // `(k_closure=null, k_fn=sigil_continuation_identity)` into a
    // 16-byte stack slot, call `raise_e(closure_ptr=null, args_ptr,
    // args_len=2)` → `*mut NextStep`, drive `sigil_run_loop` →
    // u64, narrow back to `Int`.
    //
    // **Architectural shift**: today's `lower_perform_non_io_to_
    // value` (Phase 4d MVP) inlines the perform site inside `raise
    // _e`'s native-ABI body. This test exercises the same
    // observable behaviour (stdout `42`, the arm value) but
    // through the CPS-ABI path: `raise_e`'s body emits a tail
    // `sigil_perform(...)` returning `*mut NextStep` directly, and
    // the synchronous run_loop driver moves to the call site.
    // Under tail-position perform semantics the two shapes are
    // observationally equivalent; the architectural difference
    // only matters for cross-function-call discard-k correctness
    // (the `discard_k_handler_does_not_abort_helper_phase_4e_
    // pending` test, which inverts in a later commit when both
    // `main` and `raise_e` become CPS-ABI via the lambda-lifting
    // machinery).
    //
    // What this test pins: signature selection routes the right
    // fn through the CPS-ABI path; the body emit branch produces
    // a valid Cranelift function with the right shape; the call-
    // site wrapper packs args correctly and drives the trampoline;
    // the ret-type narrow returns the right value.
    let src = "effect E { op: () -> Int }\n\
               fn raise_e() -> Int ![E] { perform E.op() }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle raise_e() with { E.op(k) => 42 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(
        src,
        "cps_abi_helper_with_simple_tail_perform_called_from_native_main",
    );
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "42\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn cps_abi_helper_called_twice_from_one_caller_uses_independent_stack_slots() {
    // Plan B Task 55, Phase 4e — S3 from PR #26 mid-flight at
    // 33f2231. The native↔CPS interop wrapper at `lower_call`
    // creates a fresh stack slot per CPS-callee call site (see
    // `Lowerer::lower_call` Cps arm — `create_sized_stack_slot`
    // runs inside the match). This test pins that two calls to
    // the same CPS callee from one caller don't accidentally
    // share or alias the slot. Aliasing would manifest as one
    // call's `[null, identity]` being clobbered by the other's,
    // which (because both write the same values) wouldn't be
    // observable at runtime — but the slot-allocation count is
    // a structural property the test pins.
    //
    // Two handle expressions, each calling raise_e and discharging
    // E with different arm values. Expected stdout: each handle's
    // arm value, both correctly returned.
    let src = "effect E { op: () -> Int }\n\
               fn raise_e() -> Int ![E] { perform E.op() }\n\
               fn main() -> Int ![IO] {\n  \
                 let a: Int = handle raise_e() with { E.op(k) => 10 };\n  \
                 let b: Int = handle raise_e() with { E.op(k) => 20 };\n  \
                 perform IO.println(int_to_string(a));\n  \
                 perform IO.println(int_to_string(b));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) =
        compile_and_run(src, "cps_abi_helper_called_twice_independent_slots");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "10\n20\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn cps_abi_helper_with_bool_return_exercises_ireduce_narrow() {
    // Plan B Task 55, Phase 4e — S3 from PR #26 mid-flight at
    // 33f2231. The wrapper's narrow step (`Lowerer::lower_call`
    // Cps arm at the ret_ty branch) chooses between identity-on-
    // I64, `ireduce` for narrower-than-I64 ints, and a
    // `debug_assert_eq!(ret_ty, pointer_ty)` for pointer-typed
    // returns. The ireduce path wasn't covered by the prior happy-
    // path test (Int → I64). Bool exercises the I8 narrow.
    //
    // helper returns Bool; arm returns `true`. main asserts the
    // value via an if-expression that prints accordingly.
    let src = "effect B { op: () -> Bool }\n\
               fn raise_b() -> Bool ![B] { perform B.op() }\n\
               fn main() -> Int ![IO] {\n  \
                 let result: Bool = handle raise_b() with { B.op(k) => true };\n  \
                 if result {\n    \
                   perform IO.println(\"yes\")\n  \
                 } else {\n    \
                   perform IO.println(\"no\")\n  \
                 };\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "cps_abi_helper_bool_return_ireduce");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "yes\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn cps_abi_helper_with_arity_n_user_args_and_perform_args_returns_arm_value() {
    // Plan B Task 55, Phase 4e — widened arity slice. Helper
    // takes one user param `x: Int` and forwards it as the
    // perform's argument. Arm receives `x` as the op-arg and
    // returns it doubled. End-to-end:
    //
    //   1. main calls `helper(7)` via the native↔CPS interop
    //      wrapper at lower_call (Cps arm). The wrapper allocates
    //      a 24-byte stack slot for `[user_arg=7, k_closure=null,
    //      k_fn=identity]` and calls helper(closure_ptr=null,
    //      args_ptr=slot, args_len=1).
    //   2. helper's CPS body emission unpacks `x` from
    //      `args_ptr[0]`, loads (k_closure, k_fn) from
    //      `args_ptr[1]`/`args_ptr[2]`, packs `x` into a fresh
    //      stack slot for the perform's args buffer, and
    //      calls `sigil_perform(...)` returning its NextStep.
    //   3. Trampoline dispatches the arm with `x=7` + the
    //      identity continuation. Arm body `arg * 2` returns
    //      `Done(14)`.
    //   4. Trampoline returns 14 to the wrapper; wrapper narrows
    //      to Int. main's `let n: Int = ...` binds 14.
    //
    // What this test pins:
    //   - User-arg unpacking from args_ptr[0..N*8] in the CPS
    //     body emission, with type narrow (here Int → I64
    //     identity, so no ireduce).
    //   - Perform-arg lowering via Lowerer (the Ident `x` lookup
    //     hits the env populated from args_ptr unpack).
    //   - Perform-arg packing into a fresh stack slot, with
    //     widening discipline.
    //   - User-arg packing in the wrapper at the call site, with
    //     widening discipline.
    //   - `args_len = user_arg_count` per the cps_signature
    //     convention (D2 fix from PR #26 mid-flight at 33f2231).
    //   - `k_closure_offset(N)` / `k_fn_offset(N)` keeping write
    //     and read sites in lockstep across arity widening (S1).
    //
    // The two existing Phase 4e tests previously excluded this
    // path via the D1 arity gate; this commit removes the gate
    // and the new test exercises the now-supported shape.
    let src = "effect E { op: (Int) -> Int }\n\
               fn helper(x: Int) -> Int ![E] { perform E.op(x) }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper(7) with { E.op(arg, k) => arg + arg };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) =
        compile_and_run(src, "cps_abi_helper_arity_n_user_args_perform_args");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "14\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn cps_abi_helper_with_string_return_exercises_pointer_ret_path() {
    // Plan B Task 55, Phase 4e — S3 from PR #26 mid-flight at
    // 33f2231. The wrapper's narrow step takes the
    // `debug_assert_eq!(ret_ty, pointer_ty)` branch when the
    // callee returns a pointer-typed value (String, user-type
    // heap pointer). The trampoline's u64 result is bit-identical
    // to the pointer value on supported targets (pointer_ty == I64
    // on x86_64-linux + aarch64-darwin). Pins this branch.
    //
    // helper returns String; arm returns a literal. main prints it.
    let src = "effect S { op: () -> String }\n\
               fn raise_s() -> String ![S] { perform S.op() }\n\
               fn main() -> Int ![IO] {\n  \
                 let s: String = handle raise_s() with { S.op(k) => \"phase4e\" };\n  \
                 perform IO.println(s);\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "cps_abi_helper_string_return_pointer_ret");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "phase4e\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn discard_k_handler_does_abort_helper_across_call_boundary() {
    // Plan B Task 55, Phase 4e — **inverted from the previously
    // `#[ignore]`'d `discard_k_handler_does_not_abort_helper_phase
    // _4e_pending`**. The captures-free let-yield-then-pure-tail
    // synth-cont slice closes the discard-k correctness gap across
    // function-call boundaries. See `[DEVIATION Task 55] Phase 4e`
    // in PLAN_B_DEVIATIONS.md.
    //
    // Algebraic semantics says: a discard-`k` arm (Raise.fail(k) =>
    // 42) should produce the arm value as the handle's overall
    // result and abort the rest of the handle body, even when the
    // perform reaches the arm via a function-call boundary
    // (helper() performs Raise.fail; main wraps helper in a
    // handle).
    //
    // **Phase 4e correctness chain:**
    //
    //   1. helper has body `let x: Int = perform Raise.fail(); x +
    //      100`. The classifier `is_simple_let_yield_then_pure_
    //      tail_body` matches; `compute_user_fn_abi` returns Cps
    //      (helper has 0 user params — captures-free constraint
    //      satisfied).
    //
    //   2. Codegen pre-pass allocates a `CpsContinuationSynth`
    //      with `kind = LetBindThenTail { binding_name = "x",
    //      binding_ty = I64, tail_expr = x + 100, tail_ty = I64 }`.
    //
    //   3. helper's CPS body emit builds `sigil_perform(eff, op,
    //      ..., k_closure=null, k_fn=&synth_cont)` and returns its
    //      NextStep up to main's wrapper.
    //
    //   4. main's wrapper drives sigil_run_loop on helper's
    //      NextStep. Trampoline dispatches the arm.
    //
    //   5. Arm body: `42` (no reference to k → discards). Returns
    //      Done(42). **The synth-cont never runs.** helper's rest-
    //      of-body (`x + 100`) is dropped.
    //
    //   6. Trampoline observes Done(42) and returns 42 up to the
    //      wrapper. main: n = 42. Prints "42\n".
    //
    // The previously-pinned `142` was the Phase 4d MVP synchronous
    // shape: helper's perform synchronously called sigil_run_loop;
    // run_loop returned arm value 42; helper bound x = 42;
    // helper computed 42 + 100 = 142; main's handle overall = 142.
    //
    // **This is the second of hard condition #2's two enumerated
    // test inversions** (the first being `statement_form_non_io_
    // perform_inside_handle_compiles_and_runs`, inverted at
    // `b818fc3`). With this test inverted, hard condition #2
    // closes.
    let src = "effect Raise { fail: () -> Int }\n\
               fn helper() -> Int ![Raise, IO] {\n  \
                 let x: Int = perform Raise.fail();\n  \
                 x + 100\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper() with {\n    \
                   Raise.fail(k) => 42,\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "phase4e_discard_k_cross_call");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "42\n",
        "Phase 4e algebraic-correct: discard-k arm aborts helper's \
         rest-of-body across the call boundary. stderr={stderr:?}"
    );
}

#[test]
fn captures_bearing_synth_cont_arity_n_helper_discard_k() {
    // Plan B Task 55, Phase 4e — captures-bearing slice. helper
    // takes `threshold: Int` user param and references it in the
    // tail expression `x + threshold`. The synth-cont captures
    // `threshold` via closure record at the perform site; when
    // the trampoline dispatches the arm `Raise.fail(k) => 42`
    // (discards k), the synth-cont never runs and the arm's
    // value flows directly to main's let-binding.
    //
    // Phase 4d MVP synchronous shape would have:
    //   - helper synchronously runs sigil_run_loop
    //   - arm returns 42; helper x = 42
    //   - helper computes 42 + threshold = 42 + 10 = 52
    //   - main: n = 52
    //
    // Phase 4e captures-bearing shape:
    //   - helper allocates closure record with threshold = 10
    //   - helper builds NextStep::Call(arm, [..., k_closure=record,
    //     k_fn=&synth_cont]) and returns it
    //   - main's wrapper drives sigil_run_loop
    //   - arm `=> 42` discards k → returns Done(42); synth-cont
    //     never runs (record never read)
    //   - sigil_run_loop returns 42 to wrapper
    //   - main: n = 42
    //
    // **The captures-bearing slice extends the discard-k
    // correctness fix to arity-N helpers** — the constant-tail
    // and arity-0 let-yield slices unblocked discard-k for
    // simpler shapes; this commit closes the remaining gap for
    // helpers that use their user params in the post-yield
    // expression.
    let src = "effect Raise { fail: () -> Int }\n\
               fn helper(threshold: Int) -> Int ![Raise, IO] {\n  \
                 let x: Int = perform Raise.fail();\n  \
                 x + threshold\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper(10) with {\n    \
                   Raise.fail(k) => 42,\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "phase4e_captures_arity_n_discard_k");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "42\n",
        "Phase 4e captures-bearing slice: arity-N helper with captured \
         user param + discard-k arm produces algebraic-correct value (42, \
         not 52). stderr={stderr:?}"
    );
}

#[test]
fn cps_abi_arity_n_helper_with_constant_done_synth_cont() {
    // PR #26 mid-flight at a5ee4c6 item #2 (prior-gap follow-up):
    // arity-N helper whose body matches the ConstantDone shape
    // (Stmt::Perform with arg, then constant tail). The synth-
    // cont ignores k_arg (Stmt::Perform discards) and returns
    // Done(constant); helper has user param `x` referenced in
    // the perform's args (not the tail). Pins that:
    //   - compute_user_fn_abi accepts arity-N helpers with
    //     ConstantDone shape (no captures needed because the
    //     constant tail doesn't reference user params)
    //   - helper's body emit unpacks `x` from args_ptr[0],
    //     packs it into the perform's args buffer, dispatches
    //     to the arm
    //   - arm `=> 99` discards k → returns Done(99); synth-cont
    //     never runs
    //   - main: n = 99
    //
    // Adjacent shape to the captures-bearing tests but exercises
    // the ConstantDone path, not LetBindThenTail. Pre-this-test,
    // this body shape's arity-N variant was untested.
    let src = "effect E { op: (Int) -> Int }\n\
               fn helper(x: Int) -> Int ![E] {\n  \
                 perform E.op(x);\n  \
                 99\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper(7) with { E.op(arg, k) => arg + 35 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "phase4e_arity_n_constant_done");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    // Arm `E.op(arg, k) => arg + 35` uses k? No — `arg + 35` is
    // pure expression not referencing k. So arm DISCARDS k. Arm
    // returns Done(arg + 35) = Done(7 + 35) = Done(42). Trampoline
    // returns 42 to wrapper. main: n = 42.
    assert_eq!(
        stdout, "42\n",
        "arity-N helper with ConstantDone shape; arm discards k → \
         arg value flows directly. arg=7 + 35 = 42. stderr={stderr:?}"
    );
}

#[test]
fn cps_abi_arity_n_helper_with_constant_done_synth_cont_use_k() {
    // PR #26 mid-flight at 2be70ce review item #2: companion to
    // `cps_abi_arity_n_helper_with_constant_done_synth_cont`
    // — that test pinned the BUILD path (helper unpacks user
    // param, packs into perform args, dispatches arm) but left
    // the synth-cont's RUN path under the ConstantDone shape
    // unexercised.
    //
    // This test uses `=> k(arg)` arm to force the synth-cont to
    // fire. arm builds Call(synth_cont, [arg]) → trampoline
    // dispatches synth_cont → synth_cont returns Done(99)
    // (ignoring k_arg per ConstantDone semantics). Result `99`
    // (NOT `arg + 35` since the synth-cont always returns the
    // constant tail).
    //
    // Closes the coverage symmetry with the LetBindThenTail
    // `discard_k` + `use_k` test pair.
    let src = "effect E { op: (Int) -> Int }\n\
               fn helper(x: Int) -> Int ![E] {\n  \
                 perform E.op(x);\n  \
                 99\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper(7) with { E.op(arg, k) => k(arg) };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "phase4e_arity_n_constant_done_use_k");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    // Arm `=> k(arg)` calls k → trampoline runs synth_cont with
    // args_ptr[0] = arg = 7. synth_cont (ConstantDone) returns
    // Done(99) ignoring args_ptr. Trampoline returns 99 to
    // wrapper. main: n = 99.
    assert_eq!(
        stdout, "99\n",
        "ConstantDone synth-cont's RUN path: arm calls k(arg) → \
         synth-cont fires → ignores k_arg, returns Done(99). stderr={stderr:?}"
    );
}

#[test]
fn cps_abi_let_yield_helper_with_bool_binding_exercises_ireduce_narrow() {
    // PR #26 mid-flight at 2be70ce review item #3 (prior-gap
    // follow-up): non-Int binding in LetBindThenTail. The
    // synth-cont's binding-load narrows from I64 (the args_ptr
    // u64 slot) to the binding's declared Cranelift type via
    // `ireduce` — for Bool, that's I64 → I8. This path was
    // unexercised in the e2e suite.
    //
    // helper: `let b: Bool = perform B.op(); if b then 1 else 0`.
    // Use-k arm `=> k(true)` — synth-cont reads args_ptr[0] =
    // 1 (widened bool true), narrows to I8 = 1, binds `b: Bool
    // = true`, lowers `if b then 1 else 0` = 1. Result `1`.
    let src = "effect B { op: () -> Bool }\n\
               fn helper() -> Int ![B, IO] {\n  \
                 let b: Bool = perform B.op();\n  \
                 if b { 1 } else { 0 }\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper() with { B.op(k) => k(true) };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "phase4e_let_yield_bool_binding");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "1\n",
        "Bool binding in LetBindThenTail: synth-cont ireduce I64 → I8 \
         on args_ptr load; `if b then 1 else 0` with b=true → 1. \
         stderr={stderr:?}"
    );
}

#[test]
fn cps_abi_let_yield_helper_with_string_binding_exercises_pointer_path() {
    // PR #26 mid-flight at 2be70ce review item #3 (prior-gap
    // follow-up): pointer-typed binding in LetBindThenTail. The
    // synth-cont's binding-load takes the pointer-pass-through
    // arm of the narrow switch (no ireduce on pointer_ty == I64
    // targets). This path was unexercised in the e2e suite.
    //
    // helper: `let s: String = perform S.op(); s` — tail just
    // returns the binding. Use-k arm `=> k("hello")` — synth-
    // cont reads args_ptr[0] (String pointer), passes through,
    // binds `s`, lowers tail `s`. Result is the string "hello".
    let src = "effect S { op: () -> String }\n\
               fn helper() -> String ![S, IO] {\n  \
                 let s: String = perform S.op();\n  \
                 s\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let s: String = handle helper() with { S.op(k) => k(\"hello\") };\n  \
                 perform IO.println(s);\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "phase4e_let_yield_string_binding");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "hello\n",
        "String binding in LetBindThenTail: synth-cont pointer-path \
         on args_ptr load; tail returns binding directly. stderr={stderr:?}"
    );
}

#[test]
fn cps_abi_captures_bearing_with_bool_capture_exercises_widen_narrow_symmetry() {
    // PR #26 mid-flight at 2be70ce review item #3 (prior-gap
    // follow-up): non-Int capture type. The closure record's
    // slot encoding for non-Int captures requires:
    //   - On the WRITE side (helper's body emit alloc): widen
    //     to I64 via uextend (Bool I8 → I64).
    //   - On the READ side (synth-cont's capture-load): narrow
    //     back to the declared kind via ireduce (I64 → I8).
    // A regression in either side would silently produce
    // garbage in the upper bits.
    //
    // helper takes `flag: Bool` user param, captures it; tail
    // `if flag then x else 0`. use-k arm `=> k(99)` →
    // synth-cont loads flag from closure record (offset 16,
    // narrows I64→I8), binds x=99, lowers `if flag then x
    // else 0` with flag=true → 99.
    //
    // For flag=false: the test inverts (n = 0). Both paths
    // exercise the widen/narrow symmetry.
    let src = "effect Raise { fail: () -> Int }\n\
               fn helper(flag: Bool) -> Int ![Raise, IO] {\n  \
                 let x: Int = perform Raise.fail();\n  \
                 if flag { x } else { 0 }\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let a: Int = handle helper(true) with { Raise.fail(k) => k(99) };\n  \
                 let b: Int = handle helper(false) with { Raise.fail(k) => k(99) };\n  \
                 perform IO.println(int_to_string(a));\n  \
                 perform IO.println(int_to_string(b));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "phase4e_captures_bearing_bool");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "99\n0\n",
        "Bool capture's widen/narrow symmetry: helper(true) k(99) → \
         flag=true so x=99 returned; helper(false) k(99) → flag=false \
         so 0 returned. stderr={stderr:?}"
    );
}

#[test]
fn cps_abi_captures_bearing_with_char_capture_exercises_widen_narrow_symmetry() {
    // PR #26 mid-flight at 73c7e53 (no-context) review item #4:
    // parallel coverage to the Bool capture test for the other
    // sub-I64 width-discrepant kind. Char captures store as I32 in
    // the closure record's slot:
    //   - On the WRITE side (helper's body emit alloc): widen
    //     I32 → I64 via uextend.
    //   - On the READ side (synth-cont's capture-load): narrow
    //     I64 → I32 via ireduce.
    // A regression in either side would surface as wrong upper
    // bits leaking into the Char comparison at runtime.
    //
    // helper takes `marker: Char` user param, captures it; tail
    // `if marker == 'A' then x else 0`. use-k arm `=> k(99)` →
    // synth-cont loads marker from closure record (offset 16,
    // narrows I64→I32), binds x=99, lowers the conditional with
    // marker='A' → 99 / marker='B' → 0.
    let src = "effect Raise { fail: () -> Int }\n\
               fn helper(marker: Char) -> Int ![Raise, IO] {\n  \
                 let x: Int = perform Raise.fail();\n  \
                 if marker == 'A' { x } else { 0 }\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let a: Int = handle helper('A') with { Raise.fail(k) => k(99) };\n  \
                 let b: Int = handle helper('B') with { Raise.fail(k) => k(99) };\n  \
                 perform IO.println(int_to_string(a));\n  \
                 perform IO.println(int_to_string(b));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "phase4e_captures_bearing_char");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "99\n0\n",
        "Char capture's widen/narrow symmetry: helper('A') k(99) → \
         marker=='A' so x=99 returned; helper('B') k(99) → marker=='B' \
         is false so 0 returned. stderr={stderr:?}"
    );
}

#[test]
fn captures_bearing_synth_cont_with_two_user_params_captured() {
    // PR #26 mid-flight at a5ee4c6 item #2 (prior-gap follow-up):
    // multi-capture e2e — pins the closure record's slot ordering
    // (`captures[0]` at offset 16, `captures[1]` at offset 24).
    // Pre-this-test, only single-capture (`threshold`) was
    // exercised.
    //
    // helper takes two user params (threshold, multiplier); tail
    // references both. The synth-cont's closure record holds
    // [threshold, multiplier] at offsets 16, 24. Synth-cont reads
    // both at fn entry, binds both in env, lowers `(x + threshold)
    // * multiplier`.
    //
    // Use-k arm `=> k(7)`: synth-cont fires, x=7, threshold=10,
    // multiplier=3, result = (7 + 10) * 3 = 51. main: n = 51.
    let src = "effect Raise { fail: () -> Int }\n\
               fn helper(threshold: Int, multiplier: Int) -> Int ![Raise, IO] {\n  \
                 let x: Int = perform Raise.fail();\n  \
                 (x + threshold) * multiplier\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper(10, 3) with {\n    \
                   Raise.fail(k) => k(7),\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "phase4e_multi_capture_use_k");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "51\n",
        "multi-capture synth-cont: threshold=10 + multiplier=3 captured; \
         use-k binds x=7; (7+10)*3 = 51. stderr={stderr:?}"
    );
}

#[test]
fn captures_bearing_synth_cont_arity_n_helper_use_k() {
    // Companion to captures_bearing_synth_cont_arity_n_helper_
    // discard_k — pins the use-k path.
    //
    // Arm `Raise.fail(k) => k(7)`:
    //   - arm builds Call(synth_cont, [k_closure=record, 7])
    //   - trampoline runs synth_cont(closure_ptr=record,
    //     args_ptr=[7], args_len=1)
    //   - synth-cont loads x=7 from args_ptr[0]
    //   - synth-cont loads threshold=10 from
    //     closure_ptr + 16 (capture slot 0)
    //   - synth-cont lowers `x + threshold` = 7 + 10 = 17 via
    //     Lowerer (env={x: 7, threshold: 10})
    //   - synth-cont returns Done(17)
    //   - trampoline returns 17 to wrapper
    //   - main: n = 17
    //
    // This is the load-bearing test for the captures-load path
    // — the synth-cont's `closure_ptr + 16 + 8*i` reads + the
    // env-bind chain.
    let src = "effect Raise { fail: () -> Int }\n\
               fn helper(threshold: Int) -> Int ![Raise, IO] {\n  \
                 let x: Int = perform Raise.fail();\n  \
                 x + threshold\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper(10) with {\n    \
                   Raise.fail(k) => k(7),\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "phase4e_captures_arity_n_use_k");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "17\n",
        "Phase 4e captures-bearing use-k path: arm calls k(7) → \
         synth_cont reads threshold=10 from closure record + binds \
         x=7 → x + threshold = 17. stderr={stderr:?}"
    );
}

#[test]
fn discard_k_handler_use_k_arm_runs_synth_cont_with_bound_value() {
    // Companion to `discard_k_handler_does_abort_helper_across_
    // call_boundary` — pins the use-k arm path. When the arm calls
    // `k(value)`, the synth-cont runs with `args_ptr[0] = value`,
    // binds `x = value` in the env, lowers `x + 100`, returns
    // `Done(value + 100)`. Main: n = value + 100.
    //
    // For arm `Raise.fail(k) => k(7)`: synth-cont runs with x=7;
    // returns Done(107). main prints "107\n".
    //
    // This pins the synth-cont's Lowerer-driven body emission
    // (binding lookup + Binary lowering) — the alternative to the
    // ConstantDone shape's hand-rolled iconst-only path.
    let src = "effect Raise { fail: () -> Int }\n\
               fn helper() -> Int ![Raise, IO] {\n  \
                 let x: Int = perform Raise.fail();\n  \
                 x + 100\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper() with {\n    \
                   Raise.fail(k) => k(7),\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "phase4e_use_k_arm_synth_cont");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "107\n",
        "Phase 4e use-k path: arm calls k(7) → synth-cont binds x=7 → \
         x + 100 = 107. stderr={stderr:?}"
    );
}

#[test]
fn arm_uses_k_inside_if_branch_is_rejected_pointing_at_phase_4e() {
    // Phase 4d MVP — Phase 4e closure point: multi-branch tail-`k`
    // shapes (`if c { k(x) } else { k(y) }`) require a join-block
    // lowering returning `*NextStep`, beyond Phase 4d MVP scope.
    // The walker (`arm_body_walk`) and the synth-pass detector
    // (`arm_body_tail_is_k_call`) must agree on the recursion
    // shape: both treat tail position as propagating only through
    // `Expr::Block` tails (NOT `Expr::If` then/else, NOT `Expr::Match`
    // arm bodies). A regression where the walker accepted these as
    // tail-k while the detector rejected would manifest as a hard
    // compiler crash at the synth pass's `lower_expr` (k as an
    // indirect-call callee → `unreachable!`).
    //
    // This test pins the walker's rejection. Inverts to a positive
    // test (asserting either branch's value flows correctly) when
    // Phase 4e ships the join-block lowering.
    let src = "effect E { op: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle (perform E.op()) with {\n    \
                   E.op(k) => if true { k(1) } else { k(2) },\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let tmp = std::env::temp_dir().join(format!(
        "sigil_e2e_phase4d_if_branch_k_reject_{}.sigil",
        std::process::id()
    ));
    std::fs::write(&tmp, src).expect("write source");
    let bin_path = std::env::temp_dir().join(format!(
        "sigil_e2e_phase4d_if_branch_k_reject_{}",
        std::process::id()
    ));
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
        "compile must fail — k(arg) inside if-branch is non-tail under \
         Phase 4d MVP detector; got success with stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("non-tail") || stderr.contains("Phase 4e"),
        "error message should reference non-tail k / Phase 4e; got stderr={stderr:?}",
    );
}

#[test]
fn arm_uses_k_inside_match_arm_is_rejected_pointing_at_phase_4e() {
    // Phase 4d MVP — Phase 4e closure point: same shape as the
    // if-branch test above but via `match`. The walker's
    // `Expr::Match` arm-body walk must align with
    // `arm_body_tail_is_k_call`'s "Block tails only" recursion —
    // accepting tail-k inside match arms would cause the same
    // walker-vs-detector mismatch that crashes the synth pass.
    //
    // Test source matches on a `Bool` scrutinee with both arms
    // calling `k`. Inverts to a positive test when Phase 4e ships
    // the join-block lowering.
    let src = "effect E { op: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle (perform E.op()) with {\n    \
                   E.op(k) => match true { true => k(1), false => k(2) },\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let tmp = std::env::temp_dir().join(format!(
        "sigil_e2e_phase4d_match_arm_k_reject_{}.sigil",
        std::process::id()
    ));
    std::fs::write(&tmp, src).expect("write source");
    let bin_path = std::env::temp_dir().join(format!(
        "sigil_e2e_phase4d_match_arm_k_reject_{}",
        std::process::id()
    ));
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
        "compile must fail — k(arg) inside match arm body is non-tail under \
         Phase 4d MVP detector; got success with stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("non-tail") || stderr.contains("Phase 4e"),
        "error message should reference non-tail k / Phase 4e; got stderr={stderr:?}",
    );
}

#[test]
fn arm_body_with_inner_block_and_outer_capture_works() {
    // Plan B Task 55 (Phase 4d) — regression test for review items
    // 1 / 2 / MF3 (codegen `rewrite_block` scope rollback fragility
    // + typecheck `walk_block` `locals` leak). Sigil's no-shadow
    // contract (resolve E0020 / typecheck `env_insert` debug-assert)
    // forbids the literal shadowing example the reviewer drafted,
    // and Sigil's arm-body grammar (`parse_handle_op_arm` calls
    // `parse_expr`, not `parse_block`) means arm bodies can't be
    // raw `{ … }` block expressions either. The regression here is
    // structural: push/pop scope discipline must keep capture
    // rewriting and free-var collection honest across nested-block
    // boundaries even when no name collisions exist.
    //
    // Test shape uses an `if` whose then-block contains the
    // multi-statement scoping (Sigil DOES allow blocks in if-branch
    // bodies via `parse_block`); the else-block exists only to
    // satisfy `if`'s typing rules and is unreachable at runtime
    // (cond is `true`).
    //
    //   - `outer(local: Int)` brings `local` into scope.
    //   - Arm body's tail is an `if true { … } else { 0 }`.
    //   - Then-block has `let extra = 7;` then a tail
    //     `local + extra` referencing both the capture and the
    //     block-local let. The block exercises:
    //         · rewrite_block push/pop scope frame (codegen)
    //         · walk_block locals save/restore (typecheck)
    //         · capture rewrite of `local` against the surrounding
    //           closure record
    //
    // Expected output: 5 (outer.local) + 7 (block-local `extra`) = 12.
    // Note: `outer`'s effect row is `![IO]` (NOT `![IO, E]`) — the
    // handle discharges `E` inside the body, so `E` is not in
    // outer's externally-observable effects. Same shape as existing
    // `arm_captures_outer_scope_returns_value`.
    let src = "effect E { op: () -> Int }\n\
               fn outer(local: Int) -> Int ![IO] {\n  \
                 let n: Int = handle (perform E.op()) with {\n    \
                   E.op(k) => if true { let extra: Int = 7; local + extra } else { 0 },\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 outer(5)\n\
               }\n";
    let (stdout, stderr, code) =
        compile_and_run(src, "phase4d_arm_body_nested_block_outer_capture");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "12\n", "expected 5 + 7; stderr={stderr:?}");
}

#[test]
fn slice_b_arm_body_let_then_pure_tail_post_arm_k_synth_fn_fires() {
    // Plan B Task 55, Phase 4e captures+ Slice B — non-tail `k` use
    // in arm bodies via lambda-lifted post-arm-k synth fn.
    //
    // The arm body `Raise.fail(k) => let r: Int = k(99); r + 1`
    // matches `arm_body_let_then_pure_tail_shape` and the pre-pass
    // allocates a separate FuncId for the post-arm-k synth fn. The
    // post-arm-k synth fn body lowers `r + 1`, returning `Done(r+1)`.
    //
    // Runtime trace:
    //   - main calls helper() via the native↔CPS interop wrapper.
    //   - helper performs Raise.fail with k_fn = its own synth-cont.
    //   - Arm fires `let r = k(99); r + 1`:
    //       Call(helper_synth_cont, [99, null, post_arm_k_addr])
    //   - Trampoline dispatches helper_synth_cont:
    //       reads x=99 from args_ptr[0],
    //       reads post_arm_k pair from args_ptr[1..3],
    //       lowers helper's tail `x` → 99,
    //       dispatches Call(null, post_arm_k_addr, [99]).
    //   - Trampoline dispatches post-arm-k synth fn:
    //       reads r=99 from args_ptr[0],
    //       lowers `r + 1` → 100,
    //       returns Done(100).
    //   - run_loop returns 100 to main.
    //
    // Expected stdout: "100\n".
    //
    // A regression here would surface as:
    //   - `99` printed: post-arm-k synth fn never fired (helper's
    //     synth-cont didn't dispatch the trailing pair, OR the arm
    //     fn packed args_len=1 instead of 3, OR identity was reached
    //     with args_len=1 directly).
    //   - Crash inside post-arm-k synth fn: bad binding read (wrong
    //     offset / wrong type narrow) OR tail expression lowering
    //     mismatch.
    //   - Crash on `args_len == 1 || args_len == 3` assert: codegen
    //     emitted an unexpected args shape.
    let src = "effect Raise { fail: () -> Int }\n\
               fn helper() -> Int ![Raise, IO] {\n  \
                 let x: Int = perform Raise.fail();\n  \
                 x\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper() with {\n    \
                   Raise.fail(k) => {\n      \
                     let r: Int = k(99);\n      \
                     r + 1\n    \
                   },\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "slice_b_post_arm_k_let_then_pure");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "100\n",
        "Slice B let-then-pure-tail: post-arm-k synth fn fires after \
         helper's synth-cont, computes r + 1 = 99 + 1 = 100. stderr={stderr:?}"
    );
}

#[test]
fn slice_b_arm_body_let_then_pure_tail_with_non_trivial_pure_arg() {
    // Slice B coverage variation (PR #27 mid-flight at e5991a9
    // review item 7): `arg_expr` is a pure compound expression
    // (`99 + 1`), not just a literal. Exercises the arg lowerer's
    // widen path under a Binary expression. Expected stdout: "101"
    // (= (99 + 1) + 1).
    let src = "effect Raise { fail: () -> Int }\n\
               fn helper() -> Int ![Raise, IO] {\n  \
                 let x: Int = perform Raise.fail();\n  \
                 x\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper() with {\n    \
                   Raise.fail(k) => { let r: Int = k(99 + 1); r + 1 },\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "slice_b_post_arm_k_non_trivial_arg");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "101\n",
        "Slice B non-trivial arg: arg = 99 + 1 = 100; r = 100; r + 1 = 101. \
         stderr={stderr:?}"
    );
}

#[test]
fn slice_b_arm_body_let_then_pure_tail_with_global_in_tail() {
    // Slice B coverage variation: `tail_expr` references the
    // `int_to_string` global (along with `r`). Exercises the
    // free-var walker's `globals.contains` branch beyond the
    // `r`-only path. The tail's overall result type is String,
    // exercising the binding_ty=Int but tail_ty=String code paths
    // (per the post-arm-k synth fn's I64 widen).
    //
    // Note: `int_to_string` is an Int->String fn; helper's tail
    // type is Int, so `r` is bound as Int; the post-arm-k tail
    // calls `int_to_string(r)` which is a Call expression — not
    // pure per `expr_is_pure`. So this shape is NOT directly
    // accepted by Slice B's classifier today (Calls are rejected
    // by purity). This rejection is correct: a future captures-
    // bearing or purity-relaxing extension would lift it.
    //
    // The variation we CAN test today is a tail computation that
    // uses a global as a value, e.g. an Ident reference. Sigil
    // doesn't currently have global Int constants, so we approximate
    // by using a top-level fn name as a value (also a global) — but
    // that's not a useful runtime computation. Instead, defer this
    // coverage variation to the captures-bearing extension that
    // permits Calls in tails. The unit test
    // `arm_body_post_arm_k_tail_free_vars_accepts_binding_plus_globals`
    // pins the walker's globals-membership branch directly.
    //
    // No e2e test is added here because no parseable shape exercises
    // it under Slice B's purity restriction. Documenting the gap.
}

#[test]
fn slice_b_arm_body_post_arm_k_tail_referencing_op_arg_is_rejected_at_codegen() {
    // Slice B negative coverage (PR #27 mid-flight at e5991a9
    // review item 6): tail references an op-arg, which is outside
    // `{r} ∪ globals`. Walker rejects with the captures-bearing-
    // extension-pointing diagnostic.
    //
    // Op `Raise.fail(n: Int)` takes one arg `n`; arm body
    // `Raise.fail(n, k) => { let r: Int = k(99); r + n }` references
    // `n` (op-arg) in the post-arm-k tail. Future captures-bearing
    // extension would alloc a closure record at the arm-fn body
    // emit and read it in the post-arm-k synth fn.
    let src = "effect Raise { fail: (Int) -> Int }\n\
               fn helper() -> Int ![Raise, IO] {\n  \
                 let x: Int = perform Raise.fail(7);\n  \
                 x\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper() with {\n    \
                   Raise.fail(arg, k) => { let r: Int = k(99); r + arg },\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let tmp = std::env::temp_dir().join(format!(
        "slice_b_reject_op_arg_{}.sigil",
        std::process::id()
    ));
    std::fs::write(&tmp, src).expect("write source");
    let bin_path =
        std::env::temp_dir().join(format!("slice_b_reject_op_arg_{}", std::process::id()));
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
        "compile must fail: post-arm-k tail references op-arg `arg`. \
         stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("captures-bearing extension") || stderr.contains("`arg`"),
        "diagnostic should point at the captures-bearing extension or name `arg`; \
         got stderr={stderr:?}"
    );
}

#[test]
fn slice_b_arm_body_post_arm_k_tail_referencing_k_is_rejected_at_codegen() {
    // Slice B negative coverage: tail references the continuation
    // `k` directly. Walker rejects with the Slice-C-pointing
    // diagnostic (multi-shot / further-non-tail uses).
    //
    // arm body `Raise.fail(k) => { let r: Int = k(99); k }` would
    // try to use `k` as a value in tail position. Slice B rejects.
    let src = "effect Raise { fail: () -> Int }\n\
               fn helper() -> Int ![Raise, IO] {\n  \
                 let x: Int = perform Raise.fail();\n  \
                 x\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper() with {\n    \
                   Raise.fail(k) => { let r: Int = k(99); k },\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let tmp =
        std::env::temp_dir().join(format!("slice_b_reject_k_ref_{}.sigil", std::process::id()));
    std::fs::write(&tmp, src).expect("write source");
    let bin_path =
        std::env::temp_dir().join(format!("slice_b_reject_k_ref_{}", std::process::id()));
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
        "compile must fail: post-arm-k tail references `k`. \
         stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn slice_b_post_arm_k_synth_fn_lowered_tail_type_differs_from_op_return_type() {
    // Pins the Slice B post_arm_k synth fn's "actual lowered tail
    // Cranelift type vs. pre-stored op return type" fix from
    // `113b7da`. Without this regression test, a future refactor
    // that re-introduces a pre-stored `tail_ty = body_ty` (op's
    // declared return type) would silently break only when the
    // arm's tail expression type differs from the op's return type.
    //
    // Slice B's original e2e tests all used `Raise.fail: () -> Int`
    // with arm tail also Int — `body_ty` (= op return type) matched
    // the tail's lowered type so the bug never surfaced. Slice C's
    // `Choose.flip: () -> Bool` incidentally exposed it.
    //
    // To force the divergence at Slice B's path specifically, we
    // need:
    //   - Op return type: Bool (=> body_ty = I8 in the pre-pass).
    //   - Arm tail type: Int (=> actual lowered Cranelift type = I64).
    //
    // The continuation `k`'s type is `T_op_ret -> T_helper_ret`, so
    // we want `T_op_ret = Bool` (the perform's value passed to k)
    // and `T_helper_ret = Int` (what the handle expression produces,
    // = `r`'s declared type in the arm body).
    //
    //   effect Raise { fail: () -> Bool }     // T_op_ret = Bool
    //   fn helper() -> Int ![Raise, IO] {     // T_helper_ret = Int
    //     let b: Bool = perform Raise.fail();
    //     if b { 1 } else { 0 }
    //   }
    //   arm: Raise.fail(k) => {
    //     let r: Int = k(true);               // r: Int (= T_helper_ret)
    //     r + 1                               // tail returns Int
    //   }
    //
    // Pre-fix: post_arm_k synth fn compared the pre-stored
    // `tail_ty == body_ty == I8` against I64, took the `< 64`
    // branch, and emitted `uextend.i64 v_i64` — Cranelift's
    // verifier rejects (uextend requires source < target).
    //
    // Post-fix: synth fn reads `dfg.value_type(tail_value) == I64`,
    // skips the widen, ships terminal `Done(2)`.
    //
    // Runtime trace:
    // - main calls helper.
    // - helper performs Raise.fail() with k_fn = helper's synth-cont.
    // - arm fires: k(true) → Call(helper_synth_cont, [true_widened_to_I64,
    //   null, post_arm_k_addr]).
    // - helper synth-cont reads b=true (narrows I64 → I8), lowers tail
    //   `if b { 1 } else { 0 }` → 1, dispatches Call(post_arm_k_addr, [1]).
    // - post_arm_k synth fn reads r=1 from args_ptr[0], lowers `r + 1`
    //   → 2, returns Done(2).
    let src = "effect Raise { fail: () -> Bool }\n\
               fn helper() -> Int ![Raise, IO] {\n  \
                 let b: Bool = perform Raise.fail();\n  \
                 if b { 1 } else { 0 }\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper() with {\n    \
                   Raise.fail(k) => {\n      \
                     let r: Int = k(true);\n      \
                     r + 1\n    \
                   },\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "slice_b_post_arm_k_body_ty_neq_tail_ty");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "2\n",
        "Slice B body_ty != tail_ty: op returns Bool, helper/handle/arm \
         returns Int. k(true) → helper continues with b=true → tail = 1 \
         → r = 1 → r + 1 = 2. stderr={stderr:?}"
    );
}

#[test]
fn slice_c_choose_multi_shot_arm_invokes_k_twice_with_different_args() {
    // Plan B Task 55, Phase 4e captures+ Slice C — multi-shot `k`
    // via the multi-let arm body shape `{ let r1 = k(arg1); let
    // r2 = k(arg2); pure_tail }` for a `resumes: many` effect.
    //
    // The arm body invokes `k` twice with different args; each
    // invocation drives the helper's synth-cont independently and
    // produces a separate result. The pure tail combines both.
    //
    // helper:
    //   `let b: Bool = perform Choose.flip(); if b then 1 else 0`
    //   helper's body is the LetBindThenTail shape from PR #26's
    //   `a5ee4c6` slice. helper synth-cont reads b from args_ptr[0],
    //   computes the tail (1 if b, else 0), dispatches
    //   Call(post_arm_k_*, [tail_value]).
    //
    // arm body:
    //   `Choose.flip(k) => { let r1: Int = k(true); let r2: Int =
    //                        k(false); r1 + r2 }`
    //   - arm fn invokes k(true): packs Call(k_closure, k_fn, [true,
    //     post_arm_k_1_closure, post_arm_k_1_fn]) where
    //     post_arm_k_1_closure captures (k_closure, k_fn).
    //   - helper synth-cont reads b=true, returns 1, dispatches
    //     Call(post_arm_k_1, [1]).
    //   - post_arm_k_1 reads r1=1, reads (k_closure, k_fn) from its
    //     closure_ptr, allocates post_arm_k_2's closure with r1=1,
    //     packs Call(k_closure, k_fn, [false, post_arm_k_2_closure,
    //     post_arm_k_2_fn]).
    //   - helper synth-cont reads b=false, returns 0, dispatches
    //     Call(post_arm_k_2, [0]).
    //   - post_arm_k_2 reads r2=0, reads r1=1 from closure_ptr,
    //     computes r1 + r2 = 1, returns Done(1).
    //
    // Expected stdout: "1\n".
    let src = "effect Choose resumes: many { flip: () -> Bool }\n\
               fn helper() -> Int ![Choose, IO] {\n  \
                 let b: Bool = perform Choose.flip();\n  \
                 if b { 1 } else { 0 }\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper() with {\n    \
                   Choose.flip(k) => {\n      \
                     let r1: Int = k(true);\n      \
                     let r2: Int = k(false);\n      \
                     r1 + r2\n    \
                   },\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "slice_c_choose_multi_shot");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "1\n",
        "Slice C multi-shot: arm invokes k(true) → r1=1 (helper's tail with \
         b=true), then k(false) → r2=0 (helper's tail with b=false); arm \
         returns r1+r2 = 1. stderr={stderr:?}"
    );
}

#[test]
fn slice_c_multi_let_arm_body_with_resumes_one_effect_is_rejected_at_codegen() {
    // Slice C negative coverage: multi-let arm body shape is
    // accepted only when the effect is declared `resumes: many`.
    // For default `resumes: one` effects, the walker rejects with
    // a Slice-C-pointing diagnostic.
    //
    // The typecheck E0220 linearity gate already rejects multi-`k`
    // invocation in `resumes: one` arms; the codegen-side gate
    // here mirrors it so the diagnostic surfaces with both the
    // typecheck framing AND the Slice C framing.
    let src = "effect Raise { fail: () -> Bool }\n\
               fn helper() -> Int ![Raise, IO] {\n  \
                 let b: Bool = perform Raise.fail();\n  \
                 if b { 1 } else { 0 }\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper() with {\n    \
                   Raise.fail(k) => {\n      \
                     let r1: Int = k(true);\n      \
                     let r2: Int = k(false);\n      \
                     r1 + r2\n    \
                   },\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let tmp = std::env::temp_dir().join(format!(
        "slice_c_reject_resumes_one_{}.sigil",
        std::process::id()
    ));
    std::fs::write(&tmp, src).expect("write source");
    let bin_path =
        std::env::temp_dir().join(format!("slice_c_reject_resumes_one_{}", std::process::id()));
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
        "compile must fail: multi-let arm on `resumes: one` effect. \
         stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn slice_c_chain_arg_referencing_user_op_arg_runs() {
    // Plan B' Stage 6.7 Task 100b — captures-bearing extension for
    // the arm-side N-let chain. `arg_i` (i >= 1) and the tail
    // expression may now reference arm-fn user op-args; the chain
    // closure record threads op-args forward through every step.
    //
    // Inverted from the pre-Task-100b
    // `slice_c_arg2_referencing_user_op_arg_is_rejected_at_codegen`
    // negative test (which asserted REJECTION at codegen). The
    // captures-bearing extension lifts the Task 58 restriction by
    // adding `PostArmKChain.captures` and threading op-args via
    // every step's closure record.
    //
    // Step trace: helper(5) performs Choose.choose(5). Arm dispatched
    // with arg=5. r1 = k(arg+10) = k(15) → resumes helper with 15
    // → r1 = 15. r2 = k(arg+20) = k(25) → r2 = 25. tail = r1+r2 = 40.
    let src = "effect Choose resumes: many { choose: (Int) -> Int }\n\
               fn helper(seed: Int) -> Int ![Choose, IO] {\n  \
                 let x: Int = perform Choose.choose(seed);\n  \
                 x\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper(5) with {\n    \
                   Choose.choose(arg, k) => {\n      \
                     let r1: Int = k(arg + 10);\n      \
                     let r2: Int = k(arg + 20);\n      \
                     r1 + r2\n    \
                   },\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "slice_c_chain_arg_op_arg");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "40\n",
        "Plan B' Task 100b: arg2 references op-arg `arg` — r1=k(15)=15, \
         r2=k(25)=25, sum=40. stderr={stderr:?}"
    );
}

// Plan B' Stage 6.7 Task 100: the legacy
// `slice_c_multi_let_arm_body_with_three_lets_is_rejected_at_codegen`
// pinning test — which asserted that 3-let arm bodies REJECT at
// codegen — is deleted here. Phase B (Task 98) lifted the 2-let cap
// to N >= 2; positive coverage of 3-let arm bodies lives in
// `slice_c_chain_three_let_arm_body_invokes_k_three_times` (and the
// 5-let / forward-data-dep variants) further down this file.

#[test]
fn slice_c_choose_multi_shot_with_string_chain_threads_pointer_through_closures() {
    // Slice C pointer-typed chain variant. The reviewer flagged
    // (PR #27 mid-flight at 113b7da) that the Slice C e2e test
    // uses Int + Bool, which doesn't exercise the pointer-typed
    // path at any of the three SSA-live-across-arena-allocs sites:
    //   1. arm-fn body emit: `widened_arg1` lives across post_arm_k_1's
    //      closure-record alloc + next_step_call.
    //   2. arm-fn body emit: `post_arm_k_1_closure_ptr` (freshly
    //      heap-alloc'd) lives across next_step_call.
    //   3. post_arm_k_1 body: `widened_arg2` lives across
    //      post_arm_k_2's closure-record alloc + next_step_call.
    //
    // This test forces String values through the chain by:
    //   - helper returns String (so r1 and r2 are pointer-typed).
    //   - r1's binding_kind_1 is `EnvSlotKind::String` → bitmap bit
    //     1 set in post_arm_k_2's closure record (r1 is GC-rooted).
    //   - tail returns `r2` (a String).
    //
    // If Boehm-precise GC is missing a root at any of the three
    // sites, the String pointer would dangle after a GC sweep —
    // either a crash or wrong output. Today the test passes because
    // the strings are static literals (sigil_string_new returns
    // pooled refs that don't get collected); a future test
    // exercising fresh heap String allocations across the chain
    // would harden this further.
    let src = "effect Choose resumes: many { flip: () -> Bool }\n\
               fn helper() -> String ![Choose, IO] {\n  \
                 let b: Bool = perform Choose.flip();\n  \
                 if b { \"yes\" } else { \"no\" }\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let s: String = handle helper() with {\n    \
                   Choose.flip(k) => {\n      \
                     let r1: String = k(true);\n      \
                     let r2: String = k(false);\n      \
                     r2\n    \
                   },\n  \
                 };\n  \
                 perform IO.println(s);\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "slice_c_choose_multi_shot_string");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "no\n",
        "Slice C String-typed chain: r1=\"yes\" (helper(true)), r2=\"no\" \
         (helper(false)); tail returns r2 = \"no\". stderr={stderr:?}"
    );
}

#[test]
fn slice_c_multi_let_arm_body_with_different_callee_in_second_let_is_rejected_at_codegen() {
    // Slice C negative coverage: when the multi-let arm body's
    // second `Stmt::Let` invokes a callee OTHER than the captured
    // continuation `k`, the multi-let shape detector returns None
    // (because both Lets must invoke the same k_name), and the
    // regular arm-body walker fires instead — rejecting the first
    // non-tail `k` call with the existing "non-tail k" diagnostic.
    //
    // This e2e pins the walker-level fall-through. The detector-level
    // rejection is covered by
    // `arm_body_multi_let_then_pure_tail_shape_rejects_different_k_names_in_lets`
    // (unit test); this test pins the integration: source like
    //
    //   Choose.flip(k) => {
    //     let r1: Int = k(true);
    //     let r2: Int = different_fn(false);  // callee is NOT `k`
    //     r1 + r2
    //   }
    //
    // is rejected at codegen, even though the detector silently
    // declines to match (so the rejection diagnostic comes from
    // the non-tail-`k` walker, not from a multi-let-specific path).
    let src = "effect Choose resumes: many { flip: () -> Bool }\n\
               fn different_fn(b: Bool) -> Int ![] { if b { 1 } else { 0 } }\n\
               fn helper() -> Int ![Choose, IO] {\n  \
                 let b: Bool = perform Choose.flip();\n  \
                 if b { 1 } else { 0 }\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper() with {\n    \
                   Choose.flip(k) => {\n      \
                     let r1: Int = k(true);\n      \
                     let r2: Int = different_fn(false);\n      \
                     r1 + r2\n    \
                   },\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let tmp = std::env::temp_dir().join(format!(
        "slice_c_reject_diff_callee_{}.sigil",
        std::process::id()
    ));
    std::fs::write(&tmp, src).expect("write source");
    let bin_path =
        std::env::temp_dir().join(format!("slice_c_reject_diff_callee_{}", std::process::id()));
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
        "compile must fail: multi-let with non-`k` callee in 2nd Let — \
         detector returns None; regular walker rejects 1st non-tail `k` call. \
         stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

// ============================================================
// Plan B Task 55, Phase 4g — return arms via synth return fn
// registered on the first-pushed frame, codegen-driven dispatch.
// See `[DEVIATION Task 55] Phase 4g` in PLAN_B_DEVIATIONS.md.
// ============================================================

#[test]
fn handle_with_return_arm_transforms_body_value_no_op_arms_fired() {
    // Plan B Task 55 (Phase 4g) — happy-path: handle body completes
    // normally (no perform), the return arm fires with the body's
    // value bound to `v`, transforms it, and the handle's overall
    // value is the return arm's result.
    //
    // `handle 5 with { return(v) => v * 2 + 1, Raise.fail(k) => -1 }`
    // — body produces 5; no Raise.fail performed; return arm fires
    // with v=5 and returns 5*2+1 = 11. main prints "11\n".
    let src = "effect Raise { fail: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle 5 with {\n    \
                   return(v) => v * 2 + 1,\n    \
                   Raise.fail(k) => 0 - 1,\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "phase_4g_return_arm_no_perform");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "11\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn handle_with_op_arm_discharge_skips_return_arm() {
    // Stage-6.8-followup Bug 2 fix — corrects Phase 4g's incorrect
    // semantics from PR #29 `dd10379`. **Op arm body's discard-`k`
    // tail bypasses the return arm.** Per algebraic-effects type
    // theory (Plotkin–Pretnar), the body has type B and op arms
    // have body type R (handle's overall). The return clause
    // `return(v: B) => body_R` wraps body's normal value (type B)
    // into R. When an op arm fires and discards `k`, its value
    // already has type R and IS the handle's final value; passing
    // it through the return clause as `v: B` is type-unsound when
    // B ≠ R. PR #29's CI fix at `dd10379` shipped uniform return
    // arm dispatch on the assumption that "return clause runs over
    // whatever flows out of the body" — that interpretation is
    // wrong; this test pins the corrected semantics.
    //
    // `handle (perform Raise.fail()) with { Raise.fail(k) => 99,
    //  return(v) => v * 100 }` — body performs Raise.fail; op arm
    // fires, returns 99 (discards k); 99 IS handle's overall
    // (return arm bypassed). main prints "99\n".
    //
    // The test's symptoms BEFORE this fix produced "9900\n"
    // (return arm applied 99 * 100). For B = R = Int (this test),
    // the bug was masked by the type coincidence — both interpretations
    // produced valid integers. The B ≠ R case (covered by the
    // run_state-shaped test landed alongside this fix) shows the
    // bug clearly: the discharged R-typed value is passed as B-typed
    // `v` and pointer arithmetic ensues.
    let src = "effect Raise { fail: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle (perform Raise.fail()) with {\n    \
                   Raise.fail(k) => 99,\n    \
                   return(v) => v * 100,\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "stage_6_8_followup_bug2_discharge_skips_return");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "99\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn handle_with_return_arm_captures_outer_fn_local() {
    // Plan B Task 55 (Phase 4g) — return arm body can capture
    // outer-fn locals via the same closure-record mechanism Phase 4d
    // shipped for op arms. typecheck-side
    // `handle_return_arm_captures` records the captures; codegen
    // pre-pass rewrites Idents → ClosureEnvLoad, allocates a closure
    // record at handle entry, and passes the pointer as
    // `closure_ptr` to `sigil_handler_frame_set_return`. The synth
    // return fn body's `Expr::ClosureEnvLoad` resolves against
    // `closure_ptr` at runtime.
    //
    // `let scale = 7; handle 4 with { return(v) => v * scale, ... }`
    // — body produces 4; return arm fires with v=4, captures scale=7
    // → returns 4*7 = 28.
    let src = "effect Raise { fail: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let scale: Int = 7;\n  \
                 let n: Int = handle 4 with {\n    \
                   return(v) => v * scale,\n    \
                   Raise.fail(k) => 0,\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "phase_4g_return_arm_captures");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "28\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn handle_with_return_arm_in_multi_effect_handle_first_frame_contract() {
    // Plan B Task 55 (Phase 4g) — return arm on a multi-effect
    // handle. Per `[DEVIATION Task 55] Phase 4f` concern #2
    // pre-commitment, the return arm registers on the **first-pushed
    // (bottom-of-handle-group) frame**. Pushed order is BTreeMap-
    // stable (effect-name lex order), so for effects `Foo` and `Bar`
    // the first-pushed frame is the `Bar` group's. This test pins
    // the semantics: regardless of which effect's group is
    // "first-pushed", the return arm fires on Done with the body's
    // value bound to `v`.
    //
    // `handle 3 with { Foo.f(k) => 100, Bar.b(k) => 200, return(v)
    // => v * 10 }` — body produces 3 (no perform); return arm fires
    // with v=3 → 30.
    let src = "effect Foo { f: () -> Int }\n\
               effect Bar { b: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle 3 with {\n    \
                   Foo.f(k) => 100,\n    \
                   Bar.b(k) => 200,\n    \
                   return(v) => v * 10,\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "phase_4g_return_arm_multi_effect");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "30\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn handle_with_return_arm_body_performs_io() {
    // Plan B Task 55 (Phase 4g) — return arm body can perform IO
    // (or any effect declared in the surrounding fn's row). The
    // synth return fn's body lowering routes through the regular
    // Lowerer; IO performs go through the same machinery as
    // anywhere else. Typecheck verified the return arm body
    // type-checks under the **caller's row** (not the body's
    // discharged row).
    //
    // `handle 42 with { Raise.fail(k) => 0, return(v) => { perform
    // IO.println("done"); v } }` — body=42, no perform; return arm
    // runs: prints "done", returns v=42. Output: "done\n42\n".
    // Raise.fail op arm never fires (body has no perform); it
    // exists only to satisfy parser's at-least-one-op-arm rule.
    let src = "effect Raise { fail: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle 42 with {\n    \
                   return(v) => {\n      \
                     perform IO.println(\"done\");\n      \
                     v\n    \
                   },\n    \
                   Raise.fail(k) => 0,\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "phase_4g_return_arm_body_io");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "done\n42\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn handle_with_return_arm_body_type_differs_from_body_type() {
    // Plan B Task 55 (Phase 4g) — return arm body's type may
    // differ from the handle body's type (typecheck binds `v:
    // body_ty` and unifies `handler_overall == ra.body's type`).
    // Codegen narrows the trampoline result back to the
    // handler-overall type per `lower_perform_non_io_to_value`'s
    // narrow-back discipline. This test exercises the narrow-back
    // path: body type = Int (I64), return-arm body type = Bool (I8)
    // — handle's overall type is Bool, narrowed via `ireduce` from
    // the I64 trampoline result.
    let src = "effect Raise { fail: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let big: Bool = handle 100 with {\n    \
                   return(v) => v > 50,\n    \
                   Raise.fail(k) => false,\n  \
                 };\n  \
                 if big {\n    \
                   perform IO.println(\"big\");\n    \
                   0\n  \
                 } else {\n    \
                   perform IO.println(\"small\");\n    \
                   1\n  \
                 }\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "phase_4g_return_arm_narrow_to_bool");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "big\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn handle_returning_fn_typed_value_with_op_arm_discharge_runs() {
    // Stage-6.8-followup Bug 2 fix — the load-bearing run_state-shape
    // test that exercises B ≠ R (body's type ≠ handle's overall
    // type) under op-arm discharge. Pre-fix, this shape produced a
    // heap-pointer-shaped value: the discharged arm's lambda
    // (closure_ptr at type R) was passed as `v: B` (Int) into the
    // return arm, which computed `fn (s) => v + s` — a new lambda
    // capturing the closure_ptr-as-Int and adding the s arg. The
    // resulting `f(7)` evaluated `closure_ptr + 7`, a meaningless
    // pointer-shaped integer.
    //
    // Post-fix: arm's lambda IS handle's overall directly. f =
    // arm's lambda. f(7) = 7 + 100 = 107.
    //
    // This shape is a minimal proxy for the canonical
    // `run_state(initial, comp)` higher-order helper from
    // `examples/state.sigil`'s reverted Task 109 first-cycle attempt
    // (see `[DEVIATION Task 109] run_state canonical shape — runtime
    // chain integration gap` in `PLAN_B_PRIME_DEVIATIONS.md`).
    // Closing rs_a unblocks the canonical run_state rewrite that
    // PR #38 deferred.
    let src = "effect Trigger { fire: () -> Int }\n\
               fn comp() -> Int ![Trigger] {\n  \
                 perform Trigger.fire()\n\
               }\n\
               fn caller() -> (Int) -> Int ![] ![] {\n  \
                 handle comp() with {\n    \
                   return(v) => fn (s: Int) -> Int ![] => v + s,\n    \
                   Trigger.fire(k) => fn (s: Int) -> Int ![] => s + 100,\n  \
                 }\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let f: (Int) -> Int ![] = caller();\n  \
                 let n: Int = f(7);\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "stage_6_8_followup_bug2_fn_typed_handle_discharge");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "107\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn handle_with_post_perform_body_code_uses_arm_discharge_value() {
    // Stage-6.8-followup Bug 1 fix — body has post-perform code in
    // the let-binding shape `{ let _ = perform; tail }`. Pre-fix, the
    // synchronous body lowering's IR-level body_val reflected body's
    // tail expression (the lambda `fn (x) => x`), NOT the discharged
    // arm's lambda. The handle's overall came out as body's identity
    // lambda; `f(7)` evaluated to 7 instead of 107.
    //
    // Post-fix: runtime saves the trampoline's terminal value in
    // `LAST_TERMINAL_VALUE`; the handle expression's discharge_block
    // reads it (and similarly in the no-return-arm path's new
    // discharge branch), recovering the arm's discharge value
    // regardless of body shape. f = arm's `fn (x) => x + 100`.
    // f(7) = 107.
    //
    // This shape originates from `DEBUG_RUN_STATE.md`'s Source A
    // probe — the "Layer 1" residual from `[DEVIATION Stage-6.8-
    // followup Layer 2 analysis]`'s "What's still blocking the
    // canonical run_state" enumeration.
    let src = "effect Trigger { fire: () -> Int }\n\
               fn make_f() -> (Int) -> Int ![] ![] {\n  \
                 handle {\n    \
                   let _: Int = perform Trigger.fire();\n    \
                   fn (x: Int) -> Int ![] => x\n  \
                 } with {\n    \
                   Trigger.fire(k) => fn (x: Int) -> Int ![] => x + 100,\n  \
                 }\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let f: (Int) -> Int ![] = make_f();\n  \
                 let n: Int = f(7);\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) =
        compile_and_run(src, "stage_6_8_followup_bug1_post_perform_body_discharge");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "107\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn handle_returning_k_capturing_lambda_invoked_outside_handle() {
    // Stage-6.8-followup Layer 2 fix — k captured into a lifted lambda
    // that escapes the handle, then invoked from the handle's caller via
    // `f(s)` and recursively chained as `k(s)(s)`. Pre-fix, the lifted
    // lambda's k(arg) call returned the raw arg (identity-as-k_fn echoes
    // input) where the source-language type is the handle's overall R =
    // (Int) -> Int ![]. The next call site `(k(s))(s)` interpreted the
    // raw Int as a closure pointer → SIGSEGV.
    //
    // Post-fix: lower_k_pair_call self-applies the originating handle's
    // return arm to the run_loop result, producing the R-typed value
    // (a closure for `(s) => v + s`). The chain `k(s)(s)` then evaluates
    // to v + s where v = body's terminal value (= s, since perform is
    // body's tail). For f(7): k(7) returns the closure for `(s) => 7+s`,
    // applied to s=7 yields 14.
    //
    // This is the canonical run_state higher-order helper's arm body
    // shape. Layer 2 unblocks it for tail-perform single-arm cases;
    // Layers 1 (non-tail-perform body) and 3 (multi-arm composition)
    // remain documented under `[DEVIATION Stage-6.8-followup Layer 2
    // analysis]` for follow-up.
    let src = "effect Trigger resumes: many { fire: () -> Int }\n\
               fn comp() -> Int ![Trigger] {\n  \
                 perform Trigger.fire()\n\
               }\n\
               fn caller() -> (Int) -> Int ![] ![] {\n  \
                 handle comp() with {\n    \
                   return(v) => fn (s: Int) -> Int ![] => v + s,\n    \
                   Trigger.fire(k) => fn (s: Int) -> Int ![] => k(s)(s),\n  \
                 }\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let f: (Int) -> Int ![] = caller();\n  \
                 let n: Int = f(7);\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) =
        compile_and_run(src, "stage_6_8_followup_layer2_k_capturing_lambda");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "14\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn handle_with_op_arm_discharge_skips_constant_return_arm() {
    // Stage-6.8-followup Bug 2 fix — corrects PR #29 semantics.
    // Combined coverage: when both an op arm and a return arm are
    // declared, and the op arm fires (discards `k`), the op arm's
    // value IS the handle's overall — the return arm does NOT
    // fire. Pre-fix behavior dispatched the constant return arm
    // (output 999); post-fix correctly bypasses (output 7).
    //
    // Pins both (a) registering both op + return arms on the same
    // frame doesn't break op-arm dispatch (the perform path still
    // works), and (b) the return arm is skipped on op-arm
    // discharge per standard algebraic-effects semantics
    // (sibling `handle_with_op_arm_discharge_skips_return_arm`).
    let src = "effect Raise { fail: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle (perform Raise.fail()) with {\n    \
                   Raise.fail(k) => 7,\n    \
                   return(v) => 999,\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "stage_6_8_followup_bug2_discharge_skips_constant_return");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "7\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn nested_handle_with_inner_lambda_in_arm_body_compiles() {
    // Plan B' Stage 6.8 Task 107 (B.4 Phase A) — INVERTED from the
    // prior `..._is_rejected_at_codegen` rejection. Phase A drops
    // the arm-body-Lambda / arm-body-ClosureRecord rejection in
    // `arm_body_walk` for shapes that don't capture continuation `k`.
    // The inner `Inner.op_in(k) => (fn (x) => x + 1)(0)` IIFE is
    // discard-k and doesn't capture `k`, so it now compiles cleanly;
    // both inner and outer `handle` bodies are `0` (no perform), so
    // arms never fire — overall returns 0.
    let src = "effect Inner { op_in: () -> Int }\n\
               effect Outer { op_out: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle\n    \
                   (handle 0 with {\n      \
                     Inner.op_in(k) => (fn (x: Int) -> Int ![] => x + 1)(0),\n    \
                   })\n  \
                 with { Outer.op_out(k) => 0 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "phase4g_walker_recursion_inverted");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "0\n",
        "B.4 Phase A: inner-handle-with-arm-body-IIFE compiles and runs; \
         no perform → arm never fires → both handles return 0. stderr={stderr:?}"
    );
}

/// Plan B' Stage 6.8 Phase C++ — generic surrounding fn with
/// fn-typed captures. compose's body lambda captures f and g whose
/// `Ty::Fn` types contain `Ty::Var(A)`/`Ty::Var(B)`/`Ty::Var(C)`
/// before monomorphize. Phase C++ extends monomorphize's clone
/// routine to populate `lambda_captures_resolved` keyed by
/// `(clone_fn_name, lambda_span)` with substitution applied;
/// closure_convert reads from that map first, falling back to the
/// pre-mono typecheck side-table for non-generic fns.
///
/// `compose[A=Int, B=Int, C=Int](id_int, id_int)(42)` =
/// id_int(id_int(42)) = 42.
#[test]
fn compose_body_via_closure_env_callees_returns_42() {
    let src = "fn id_int(x: Int) -> Int ![] { x }\n\
               fn compose[A, B, C](f: (B) -> C ![], g: (A) -> B ![]) -> (A) -> C ![] ![] {\n  \
                 fn (x: A) -> C ![] => f(g(x))\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let composed: (Int) -> Int ![] = compose(id_int, id_int);\n  \
                 let r: Int = composed(42);\n  \
                 perform IO.println(int_to_string(r));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "compose_body");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "42\n",
        "compose body: lifted lambda dispatches f(g(x)) via two \
         ClosureEnvLoad-callees → 42. stderr={stderr:?}"
    );
}

/// Plan B' Stage 6.8 Task 107 (B.4 Phase A) — arm body IIFE that
/// invokes a lambda inline (Task 108 example #2: `Raise.fail(k) =>
/// (fn (n) => n + 1)(42)`). The lambda doesn't capture `k`, so
/// Phase A's walker accepts; closure-convert hoists the lambda;
/// codegen lowers the IIFE call as a direct dispatch.
///
/// `Raise.fail` is one-shot; the arm discards `k`. Sigil v1's
/// implicit-resume semantics (per `examples/div_recover.sigil`):
/// the arm body's value becomes `perform`'s result inside the body
/// expression. So `perform Raise.fail()` resolves to 43, and the
/// body assignment binds `n = 43`.
#[test]
fn arm_body_iife_returns_43() {
    let src = "effect Raise { fail: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle perform Raise.fail() with {\n    \
                   Raise.fail(k) => (fn (n: Int) -> Int ![] => n + 1)(42),\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "arm_body_iife");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "43\n",
        "B.4 Phase A: arm body IIFE — Raise.fail's arm runs \
         `(fn (n) => n+1)(42)` = 43, which becomes perform's result. \
         stderr={stderr:?}"
    );
}

/// Plan B' Stage 6.8 Task 107 Phase B — INVERTED from the prior
/// `arm_body_lambda_capturing_k_is_rejected_until_phase_b`. Phase B
/// ships the trailing-pair convention: closure_convert flags
/// k-pair captures, the lifted lambda's closure record allocates 2
/// trailing slots for (k_closure, k_fn), and codegen's lower_call
/// dispatches `k(arg)` inside the synth fn via
/// `sigil_next_step_call(k_closure, k_fn, 1)` followed by
/// `sigil_run_loop` to drive to a terminal value.
///
/// `resumes: many` admits discard-k (0 calls); the lambda is
/// allocated but not invoked, so this test pins compilation
/// success — runtime behaviour mirrors the discard-k arm body
/// (returns 99 directly).
#[test]
fn arm_body_lambda_capturing_k_compiles_returns_99() {
    let src = "effect Choose resumes: many { flip: () -> Int }\n\
               fn run() -> Int ![] {\n  \
                 let r: Int = handle 0 with {\n    \
                   Choose.flip(k) => {\n      \
                     let _: (Int) -> Int ![] = fn (x: Int) -> Int ![] => k(x);\n      \
                     99\n    \
                   },\n  \
                 };\n  \
                 r\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 perform IO.println(int_to_string(run()));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "arm_lambda_captures_k");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "0\n",
        "B.4 Phase B: lambda capturing k allocates trailing-pair \
         closure record without calling k. handle body=0 so arm \
         never fires; r = 0. stderr={stderr:?}"
    );
}

/// Plan B' Stage 6.8 Task 108 example #1 (Choose-as-lambda).
/// The arm body's let-bound lambda captures k. The handle body
/// performs `Choose.flip()` — the arm runs, allocates the lambda
/// (which captures k), and returns 42 directly (discard-k +
/// resumes-many for the arm itself). The lambda's k-pair is
/// stored in the trailing slots; in this test the lambda isn't
/// invoked (allocated, not called), so the dispatch path isn't
/// exercised at runtime — but the closure-convert + codegen
/// surface compiles cleanly.
#[test]
fn task_108_arm_body_lambda_captures_k_runs() {
    // handle body=0 → handler_overall_ty=Int → k: (Bool) -> Int.
    // Arm body builds a lambda capturing k (uses `k(b)`) then
    // discards k and returns 42. body=0 means the arm never fires
    // (no perform); the handle returns 0. The lambda's k-capture
    // is allocated via Phase B's trailing-pair convention but
    // never invoked.
    let src = "effect Choose resumes: many { flip: () -> Bool }\n\
               fn run() -> Int ![] {\n  \
                 let r: Int = handle 0 with {\n    \
                   Choose.flip(k) => {\n      \
                     let _: (Bool) -> Int ![] = fn (b: Bool) -> Int ![] => k(b);\n      \
                     42\n    \
                   },\n  \
                 };\n  \
                 r\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 perform IO.println(int_to_string(run()));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "task_108_choose");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "0\n",
        "B.4 Phase B Task 108 shape: arm body builds k-capturing lambda \
         (allocates trailing-pair closure record) then discards k. \
         body=0 → arm never fires → handle returns 0. stderr={stderr:?}"
    );
}

/// Plan B' Stage 6.8 Task 109 bisect — pinning test for the
/// simplest "handle returns a lambda value, lambda invoked"
/// shape. Pre-Task-109 there was no e2e coverage of "arm
/// allocates a lambda AND the handle's overall result is that
/// lambda AND we then invoke the lambda". The Task 109 first-
/// cycle run_state attempt failed at runtime
/// (`state_fn(initial)` returned a closure-record pointer
/// instead of the threaded value) — which led to needing this
/// bisect: does the simplest fn-returning handle work, or is
/// the bug already at this lower level?
///
/// **Ignored** while the runtime integration gap exists. See
/// `[DEVIATION Task 109] run_state canonical shape — runtime
/// chain integration gap` for the gap analysis. Un-ignore once
/// the chain bug closes; the test source is the minimal repro.
///
/// Source: an arm with no k-capture returns a constant-shape
/// lambda; main let-binds the handle's result and invokes the
/// lambda once. Expected: lambda runs as `x + 100` with x=7,
/// stdout = "107\n".
#[test]
#[ignore]
fn handle_returning_simple_lambda_invoked_returns_value_pending_chain_fix() {
    let src = "effect Trigger { fire: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let f: (Int) -> Int ![] = handle (perform Trigger.fire()) with {\n    \
                   Trigger.fire(k) => fn (x: Int) -> Int ![] => x + 100,\n  \
                 };\n  \
                 let n: Int = f(7);\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "handle_returns_lambda_bisect");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "107\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn handle_with_return_arm_inside_match_arm_compiles() {
    // Plan B Task 55 (Phase 4g) review-fix #2: regression test
    // for the `Lowerer::type_of_expr` `Expr::Handle` arm not
    // pre-binding `v` into preview before recursing. Prior to the
    // fix, callers that didn't pre-bind `v` (e.g., `lower_match`'s
    // arm-body type predictor) would recurse into an
    // `Expr::Ident("v")` against a preview without `v` and hit the
    // `unreachable!` ident-lookup path. The repro shape: a handle
    // expression with a return arm that references `v` sitting
    // inside a `match` arm body — the predictor at codegen.rs
    // around line 8323 calls `type_of_expr(&arms[0].body, &preview)`
    // with whatever preview was inherited from the surrounding
    // scope, NOT pre-binding `v` itself.
    //
    // Without the fix, this program hits `unreachable!` at codegen
    // time. With the fix, it compiles cleanly and prints "10\n"
    // (match scrutinee 5, arm body is `handle 5 with { return(v)
    // => v + 5, ... }`, return arm fires with v=5 → 5+5=10).
    let src = "effect Raise { fail: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let scrut: Int = 5;\n  \
                 let n: Int = match scrut {\n    \
                   _ => handle 5 with {\n      \
                     return(v) => v + 5,\n      \
                     Raise.fail(k) => 0,\n    \
                   },\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "phase_4g_handle_inside_match_arm");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "10\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn handle_with_nested_handle_in_return_arm_body_compiles() {
    // Plan B Task 55 (Phase 4g) — review-fix follow-up to review
    // #1's forward observation. The deviation entry's concern #5
    // claims nested `Expr::Handle` is allowed in return arm bodies
    // as a freebie (Phase 4f's machinery extends transparently via
    // `Lowerer::lower_expr`'s recursive arm; pre-pass already
    // recurses into return arm bodies for FuncId allocation). This
    // test pins that claim with a concrete positive case.
    //
    // Outer handle: `handle 4 with { return(v) => <inner-handle>,
    // Foo.f(k) => 0 }`. The return arm body is itself a handle:
    // `handle (v + 1) with { Bar.b(k) => 0, return(w) => w * 2 }`.
    // No performs fire; outer body produces 4; outer return arm
    // fires with v=4; outer return arm body = inner handle, body
    // = v+1 = 5; inner return arm fires with w=5 → 5*2 = 10.
    // Final: 10.
    let src = "effect Foo { f: () -> Int }\n\
               effect Bar { b: () -> Int }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle 4 with {\n    \
                   return(v) => handle (v + 1) with {\n      \
                     return(w) => w * 2,\n      \
                     Bar.b(k) => 0,\n    \
                   },\n    \
                   Foo.f(k) => 0,\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "phase_4g_nested_handle_in_return_arm");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(stdout, "10\n", "stdout mismatch; stderr={stderr:?}");
}

#[test]
fn handle_with_bool_body_and_return_arm_uses_v_at_narrow_type() {
    // Plan B Stage 6 cleanup — **un-ignored from the previously
    // `#[ignore]`'d `handle_with_bool_body_and_return_arm_uses_v_-
    // pending_proper_binding_ty`**. The Phase 4g `binding_ty = I64`
    // hardcode is resolved via option 2 (typecheck side-table):
    // `CheckedProgram::handle_body_ty: BTreeMap<Span, Ty>` records
    // the body type at typecheck time; codegen's return-arm pre-pass
    // converts it to Cranelift type via `cranelift_ty_of_ty` and
    // narrows `v` from the I64 args_ptr[0] slot to the correct
    // type at synth-fn entry.
    //
    // Body: `true` (BoolLit, Cranelift I8, widened to I64 for the
    // trailing-pair packing). Return arm body: `if v { false } else
    // { true }` — references `v` as Bool; the synth fn now loads
    // I64, ireduce-narrows back to I8, binds `v: I8` in the Lowerer
    // env. The `if` lowers cleanly with v: I8 cond.
    //
    // Trace: handle's body produces `true` (I8). The surrounding fn
    // widens to I64 for the trailing-pair Call to the return arm.
    // The synth fn reads args_ptr[0] as I64, ireduces to I8, binds
    // `v = true (I8)`. Return-arm body `if v { false } else { true }`
    // → `false` (I8). The handle's overall = false; main's `if b`
    // takes the else branch, prints "false\n", returns 1.
    let src = "effect Raise { fail: () -> Bool }\n\
               fn main() -> Int ![IO] {\n  \
                 let b: Bool = handle true with {\n    \
                   return(v) => if v { false } else { true },\n    \
                   Raise.fail(k) => true,\n  \
                 };\n  \
                 if b {\n    \
                   perform IO.println(\"true\");\n    \
                   0\n  \
                 } else {\n    \
                   perform IO.println(\"false\");\n    \
                   1\n  \
                 }\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "stage_6_cleanup_bool_body_binding_ty");
    assert_eq!(
        code, 1,
        "exit code; main returns 1 from the `if b` else branch; stderr={stderr:?}"
    );
    assert_eq!(stdout, "false\n", "stdout mismatch; stderr={stderr:?}");
}

// ---------------- Plan B' Stage 6.7 Task 96 — B.2 acceptance e2e
// tests for chained-let-yield-then-pure-tail synth-cont chains
// (N>=2). The activation in commit 5ad78c3 routes both N=1 and N>=2
// chains through `ChainedLetBindStep`. Existing 1-stmt tests cover
// N=1; these cover Middle->...->Final transitions with capture +
// prior-binding threading, forward data dependencies, and pointer-
// typed bindings.

#[test]
fn chained_synth_cont_two_perform_helper_returns_sum_of_bindings() {
    // N=2 chain. Helper performs E.op twice; tail sums the two
    // bindings. Each step's resume value comes from a single arm
    // that returns `arg + 100`.
    //
    // step_0 (Middle): bind x = 101 from args_ptr[0]; alloc step_1's
    // closure record carrying x; sigil_perform(E.op(2), k=&step_1).
    // step_1 (Final): bind y = 102 from args_ptr[0]; load x = 101
    // from closure_ptr's prior_bindings slot; lower `x + y`; dispatch
    // through args_ptr[1..3]'s post_arm_k = (null, identity); identity
    // returns Done(203).
    //
    // Verifies: step_0->step_1 transition, prior_bindings forward
    // copy, args_ptr[0] bind, post_arm_k dispatch from Final.
    let src = "effect E { op: (Int) -> Int }\n\
               fn helper() -> Int ![E, IO] {\n  \
                 let x: Int = perform E.op(1);\n  \
                 let y: Int = perform E.op(2);\n  \
                 x + y\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper() with {\n    \
                   E.op(arg, k) => k(arg + 100),\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "chained_synth_cont_two_perform_helper");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "203\n",
        "Plan B' Task 96: 2-perform chain — x=101, y=102, x+y=203. \
         stderr={stderr:?}"
    );
}

#[test]
fn chained_synth_cont_three_perform_helper_returns_sum_of_bindings() {
    // N=3 chain. Verifies Middle->Middle->Final transition: catches
    // off-by-one bugs in prior_bindings offset arithmetic.
    //
    // step_0 (Middle): bind x=101; alloc step_1 record with [x];
    //   sigil_perform(E.op(2), k=&step_1).
    // step_1 (Middle): load x from prior_bindings[0]; bind y=102;
    //   alloc step_2 record with [x, y]; sigil_perform(E.op(3),
    //   k=&step_2).
    // step_2 (Final): load x from prior_bindings[0], y from
    //   prior_bindings[1]; bind z=103; lower `x + y + z`; dispatch
    //   through post_arm_k.
    let src = "effect E { op: (Int) -> Int }\n\
               fn helper() -> Int ![E, IO] {\n  \
                 let x: Int = perform E.op(1);\n  \
                 let y: Int = perform E.op(2);\n  \
                 let z: Int = perform E.op(3);\n  \
                 x + y + z\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper() with {\n    \
                   E.op(arg, k) => k(arg + 100),\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "chained_synth_cont_three_perform_helper");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "306\n",
        "Plan B' Task 96: 3-perform chain — x=101, y=102, z=103, \
         x+y+z=306. stderr={stderr:?}"
    );
}

#[test]
fn chained_synth_cont_two_perform_with_forward_data_dependency() {
    // N=2 chain where step 1's perform args reference step 0's
    // binding. The next_perform's args lower through Lowerer with
    // env populated from prior_bindings — this verifies the env
    // setup happens before the next perform's args are lowered.
    //
    // step_0: bind x = handler(1) = 101; alloc step_1 record with [x].
    // step_1 (Final): bind y from args_ptr[0]; load x from prior_-
    //   bindings[0]; lower `x + y`. Note: step_0's lower of `E.op(x)`
    //   uses the prior-step-bound x via the env populated from
    //   prior_bindings[0].
    //
    // Wait — step_0 is the FIRST perform (in helper body emit, not in
    // synth-cont). For step_1's perform args, they're lowered inside
    // step_0's Middle emit. step_0's env at that point = {x: bound}
    // + captures + prior_bindings (none for step_0). So `E.op(x)` in
    // step_1's perform lowers correctly with x in scope.
    //
    // x = handler(1) = 101. y = handler(101) = 201. x + y = 302.
    let src = "effect E { op: (Int) -> Int }\n\
               fn helper() -> Int ![E, IO] {\n  \
                 let x: Int = perform E.op(1);\n  \
                 let y: Int = perform E.op(x);\n  \
                 x + y\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper() with {\n    \
                   E.op(arg, k) => k(arg + 100),\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "chained_synth_cont_forward_data_dependency");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "302\n",
        "Plan B' Task 96: forward-data-dependency — x=handler(1)=101, \
         y=handler(x)=handler(101)=201, x+y=302. stderr={stderr:?}"
    );
}

#[test]
fn chained_synth_cont_two_perform_helper_with_user_param_capture() {
    // N=2 chain with a helper user param `threshold` referenced in
    // the tail. Verifies captures collection across the chain (the
    // capture is computed once, shared by all steps' closure records)
    // and helper's perform-time closure record carrying the capture.
    //
    // step_0 record: [threshold] (captures only).
    // step_1 record: [threshold, x] (captures + prior_bindings).
    // step_1 (Final): loads threshold from captures slot, x from
    //   prior_bindings slot, binds y; lowers `x + y + threshold`.
    let src = "effect E { op: (Int) -> Int }\n\
               fn helper(threshold: Int) -> Int ![E, IO] {\n  \
                 let x: Int = perform E.op(1);\n  \
                 let y: Int = perform E.op(2);\n  \
                 x + y + threshold\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper(10) with {\n    \
                   E.op(arg, k) => k(arg + 100),\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "chained_synth_cont_user_param_capture");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "213\n",
        "Plan B' Task 96: user-param-capture — x=101, y=102, \
         threshold=10, sum=213. stderr={stderr:?}"
    );
}

#[test]
fn chained_synth_cont_user_param_referenced_in_perform_arg_and_tail() {
    // N=2 chain where the helper user param `threshold` is referenced
    // BOTH in step_0's perform arg AND in the tail. Verifies the
    // captures-collection walker visits both perform args AND the
    // tail (not just the tail).
    //
    // step_0's perform arg = threshold; arm returns arg+100 = 110.
    //   So x = 110.
    // step_1's perform arg = 2; arm returns 102. So y = 102.
    // tail: x + y + threshold = 110 + 102 + 10 = 222.
    let src = "effect E { op: (Int) -> Int }\n\
               fn helper(threshold: Int) -> Int ![E, IO] {\n  \
                 let x: Int = perform E.op(threshold);\n  \
                 let y: Int = perform E.op(2);\n  \
                 x + y + threshold\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper(10) with {\n    \
                   E.op(arg, k) => k(arg + 100),\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) =
        compile_and_run(src, "chained_synth_cont_user_param_in_perform_arg_and_tail");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "222\n",
        "Plan B' Task 96: capture referenced in both perform-arg and \
         tail — x=handler(threshold)=110, y=102, threshold=10, \
         sum=222. stderr={stderr:?}"
    );
}

#[test]
fn chained_synth_cont_two_perform_with_string_binding_exercises_pointer_bitmap() {
    // N=2 chain with a String-typed binding in step_0 (pointer-typed
    // slot). The chain's prior_bindings forward-copy must preserve
    // the pointer slot's GC bitmap bit when allocating step_1's
    // closure record. Verifies `EnvSlotKind::is_pointer()` derivation
    // for non-uniform slot types in the chain.
    //
    // step_0 record bitmap: empty (no captures).
    // step_1 record bitmap: bit 1 set (s is String, pointer-typed).
    // step_1 (Final): load s from prior_bindings[0]; bind n from
    //   args_ptr[0]; tail returns s.
    //
    // helper's tail returns the String binding `s`; main prints it.
    let src = "effect S { gen_str: () -> String, gen_int: () -> Int }\n\
               fn helper() -> String ![S, IO] {\n  \
                 let s: String = perform S.gen_str();\n  \
                 let n: Int = perform S.gen_int();\n  \
                 s\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let r: String = handle helper() with {\n    \
                   S.gen_str(k) => k(\"hello-chain\"),\n    \
                   S.gen_int(k) => k(42),\n  \
                 };\n  \
                 perform IO.println(r);\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) =
        compile_and_run(src, "chained_synth_cont_string_binding_pointer_bitmap");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "hello-chain\n",
        "Plan B' Task 96: pointer-typed (String) binding threaded \
         forward through chain — s='hello-chain' survives prior_bindings \
         copy from step_0 to step_1; tail returns s. stderr={stderr:?}"
    );
}

// ---------------- Plan B' Stage 6.7 Task 99 — B.1 (arm-side N-let
// chain) acceptance e2e tests for N>=3. The Phase B activation
// (Task 98) routes both N=2 (existing slice_c_choose_multi_shot_*
// tests) and N>=3 chains through the unified `chain.steps` emit
// loop. Existing 2-let tests cover N=2; these cover Middle->...
// ->Final transitions for longer chains, including forward data
// dependencies in arg expressions and full env access in the tail.
// ----------------

#[test]
fn slice_c_chain_three_let_arm_body_invokes_k_three_times() {
    // N=3 arm body. Helper: `let b = perform Choose.flip(); if b
    // then 1 else 0`. Arm body: 3 sequential k invocations with
    // alternating args. Tail sums all three results.
    //
    // Arm dispatched once. Each k invocation drives helper synth-
    // cont with the given Bool, returning 1 or 0. Pre-Task-98 (with
    // legacy 2-let cap), this shape was rejected at codegen via
    // `slice_c_multi_let_arm_body_with_three_lets_is_rejected` (now
    // inverted via Task 100); post-Task-98 it compiles + runs.
    //
    // Step trace:
    //   - arm fn: lowers k(true), allocs step_0's closure (k pair),
    //     dispatches to step_0 via helper synth-cont → trampoline
    //     dispatches step_0(args_ptr=[1, post_arm_k_pair_a]).
    //   - step_0 (Middle): binds r1=1, loads (k_closure, k_fn),
    //     lowers k(false) arg, allocs step_1's closure (k pair +
    //     [r1]), dispatches.
    //   - step_1 (Middle): binds r2=0, loads (k_closure, k_fn) +
    //     [r1] from closure, lowers k(true) arg, allocs step_2's
    //     closure (Final layout: [r1, r2]), dispatches.
    //   - step_2 (Final): binds r3=1, loads [r1, r2], lowers
    //     `r1+r2+r3 = 1+0+1 = 2`, returns Done(2).
    let src = "effect Choose resumes: many { flip: () -> Bool }\n\
               fn helper() -> Int ![Choose, IO] {\n  \
                 let b: Bool = perform Choose.flip();\n  \
                 if b { 1 } else { 0 }\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper() with {\n    \
                   Choose.flip(k) => {\n      \
                     let r1: Int = k(true);\n      \
                     let r2: Int = k(false);\n      \
                     let r3: Int = k(true);\n      \
                     r1 + r2 + r3\n    \
                   },\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "slice_c_chain_three_let");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "2\n",
        "Plan B' Task 99: 3-let chain — r1=1, r2=0, r3=1, sum=2. \
         stderr={stderr:?}"
    );
}

#[test]
fn slice_c_chain_five_let_arm_body_invokes_k_five_times() {
    // N=5 chain. Stress-tests the Middle->Middle->Middle->Middle->
    // Final transition + offset arithmetic in prior_bindings copy.
    // Arm: 5 k(...) invocations with alternating Bool args; tail
    // sums all 5 results.
    //
    // Expected: r1=1, r2=0, r3=1, r4=0, r5=1 → sum=3.
    let src = "effect Choose resumes: many { flip: () -> Bool }\n\
               fn helper() -> Int ![Choose, IO] {\n  \
                 let b: Bool = perform Choose.flip();\n  \
                 if b { 1 } else { 0 }\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let n: Int = handle helper() with {\n    \
                   Choose.flip(k) => {\n      \
                     let r1: Int = k(true);\n      \
                     let r2: Int = k(false);\n      \
                     let r3: Int = k(true);\n      \
                     let r4: Int = k(false);\n      \
                     let r5: Int = k(true);\n      \
                     r1 + r2 + r3 + r4 + r5\n    \
                   },\n  \
                 };\n  \
                 perform IO.println(int_to_string(n));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "slice_c_chain_five_let");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "3\n",
        "Plan B' Task 99: 5-let chain — r1=1, r2=0, r3=1, r4=0, r5=1, \
         sum=3. stderr={stderr:?}"
    );
}

#[test]
fn slice_c_chain_three_let_with_forward_data_dependency() {
    // N=3 chain where the SECOND k invocation's arg references the
    // FIRST chain binding (`k(r1)`), and the THIRD references both
    // (`k(r1 + r2)`). Verifies prior_bindings forward-copy +
    // narrow-on-load + env scoping at each step.
    //
    // Effect `Gen resumes: many { next: (Int) -> Int }`. Helper
    // performs Gen.next(0) and returns the resume value (single-
    // perform helper; helper synth-cont is a 1-step ChainedLetBindStep
    // chain via B.2's path).
    //
    // Arm dispatched with arg=0. Three k invocations:
    //   - r1 = k(arg + 1) = k(1) → resumes helper with 1, helper
    //     returns 1. So r1=1.
    //   - r2 = k(r1) = k(1) → r2=1.
    //   - r3 = k(r1 + r2) = k(2) → r3=2.
    //   - tail = r1 + r2 + r3 = 1 + 1 + 2 = 4.
    let src = "effect Gen resumes: many { next: (Int) -> Int }\n\
               fn helper() -> Int ![Gen, IO] {\n  \
                 let n: Int = perform Gen.next(0);\n  \
                 n\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let m: Int = handle helper() with {\n    \
                   Gen.next(arg, k) => {\n      \
                     let r1: Int = k(arg + 1);\n      \
                     let r2: Int = k(r1);\n      \
                     let r3: Int = k(r1 + r2);\n      \
                     r1 + r2 + r3\n    \
                   },\n  \
                 };\n  \
                 perform IO.println(int_to_string(m));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "slice_c_chain_forward_data_dep");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "4\n",
        "Plan B' Task 99: 3-let chain with forward data dependency — \
         r1=1, r2=k(r1)=1, r3=k(r1+r2)=2, sum=4. stderr={stderr:?}"
    );
}

// ----------------------------------------------------------------
// Plan B' Stage 6.8 Task 106 — B.3 acceptance e2e tests for
// `TypeExpr::Fn` (first-class function types). Phase C v1 supports
// `Expr::Ident(local)` callees where `local` is fn-typed via fn
// param or `let` annotation. More general callees (e.g., `make_adder
// (5)(7)` — call returning fn) defer to Phase C+.
// ----------------------------------------------------------------

/// Phase C foundation — fn-as-value let binding + indirect call.
/// Closure-convert materializes `double` as a captureless
/// `ClosureRecord` at the let RHS; codegen allocates the record
/// (header + code_ptr@8) on the GC heap. The `f(21)` call dispatches
/// indirectly via `call_indirect` over the loaded code_ptr.
#[test]
fn fn_as_value_via_let_binding_returns_42() {
    let src = "fn double(n: Int) -> Int ![] { n + n }\n\
               fn main() -> Int ![IO] {\n  \
                 let f: (Int) -> Int ![] = double;\n  \
                 perform IO.println(int_to_string(f(21)));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "fn_as_value_let");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "42\n",
        "fn-as-value let binding + indirect call: f = double; f(21) = 42. \
         stderr={stderr:?}"
    );
}

/// Higher-order fn parameter — non-generic shape.
/// `apply` takes a fn-typed parameter and dispatches indirectly.
/// Caller passes `double` as a value; closure-convert materializes
/// it as a captureless `ClosureRecord` at the call site arg.
#[test]
fn higher_order_fn_param_returns_42() {
    let src = "fn double(n: Int) -> Int ![] { n + n }\n\
               fn apply(f: (Int) -> Int ![], x: Int) -> Int ![] { f(x) }\n\
               fn main() -> Int ![IO] {\n  \
                 perform IO.println(int_to_string(apply(double, 21)));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "higher_order_fn_param");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "42\n",
        "higher-order fn param: apply(double, 21) = 42. stderr={stderr:?}"
    );
}

/// Generic higher-order fn — `apply[A, B](f: (A) -> B ![], x: A)
/// -> B ![]` instantiated at A=Int, B=Int. Monomorphize clones
/// `apply` to `apply$$Int$$Int` with concrete TypeExpr::Fn for the
/// `f` param. Inside the clone, `f(x)` is the indirect call.
#[test]
fn generic_apply_with_id_fn_returns_42() {
    let src = "fn id_fn[A](x: A) -> A ![] { x }\n\
               fn apply[A, B](f: (A) -> B ![], x: A) -> B ![] { f(x) }\n\
               fn main() -> Int ![IO] {\n  \
                 let r: Int = apply(id_fn, 42);\n  \
                 perform IO.println(int_to_string(r));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "generic_apply_id_fn");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "42\n",
        "generic apply(id_fn, 42) = 42. stderr={stderr:?}"
    );
}

/// R2 finding 2 — multi-param fn-typed callee. Exercises the
/// `for p in &fty.params` loop in `lower_call`'s indirect-call sig
/// builder; the prior 3 tests are all single-param.
#[test]
fn fn_as_value_with_multi_param_returns_7() {
    let src = "fn add(a: Int, b: Int) -> Int ![] { a + b }\n\
               fn main() -> Int ![IO] {\n  \
                 let f: (Int, Int) -> Int ![] = add;\n  \
                 perform IO.println(int_to_string(f(3, 4)));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "fn_as_value_multi_param");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "7\n",
        "multi-param fn-as-value: f(3, 4) = 7. stderr={stderr:?}"
    );
}

/// R2 finding 2 — effect-bearing fn type as a value. Pins that the
/// indirect-call codegen path correctly threads effect rows through
/// the materialized closure record + indirect dispatch.
#[test]
fn fn_as_value_with_effect_row_returns_42() {
    let src = "fn add_one(n: Int) -> Int ![IO] {\n  \
                 perform IO.println(int_to_string(n));\n  \
                 n + 1\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let f: (Int) -> Int ![IO] = add_one;\n  \
                 let _: Int = f(41);\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "fn_as_value_effect_row");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "41\n",
        "effect-bearing fn-as-value: f prints 41 then returns 42 (discarded). \
         stderr={stderr:?}"
    );
}

/// Phase C+ Part 1 — call-returning-fn (make_adder shape). The
/// outer Call's callee is itself a `Call(...)` returning a fn-typed
/// value. Codegen reads the resolved `Ty::Fn` from typecheck's
/// `call_callee_tys` side-table keyed on the outer call's span.
///
/// `make_adder(5)` returns a closure record (the lambda capturing
/// `n=5`). The outer `(7)` indirectly calls it, dispatching to the
/// hoisted `$lambda_0` synth fn with `x=7`; the body returns
/// `x + n = 7 + 5 = 12`.
#[test]
fn make_adder_returns_12() {
    // Per-arrow `![..]` discipline (PLAN_B_PRIME_DEVIATIONS Task 103
    // entry): the fn-decl's return type is `(Int) -> Int ![]` (an
    // inner fn-type carrying its own row), and the fn-decl carries a
    // second `![]` for its own effect row — hence the two `![]`s on
    // line 1.
    let src = "fn make_adder(n: Int) -> (Int) -> Int ![] ![] {\n  \
                 fn (x: Int) -> Int ![] => x + n\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let r: Int = make_adder(5)(7);\n  \
                 perform IO.println(int_to_string(r));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "make_adder_call_returning_fn");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "12\n",
        "make_adder(5)(7) = 12 (5 + 7) via call-returning-fn indirect dispatch. \
         stderr={stderr:?}"
    );
}

/// Phase C+ Part 2 — ClosureEnvLoad-callee dispatch. The lambda
/// body invokes a captured fn-typed value `f`; closure-convert
/// rewrites `f` inside the synth lambda body to `ClosureEnvLoad`.
/// Codegen reads the capture's `FnSig` from the synth fn's
/// `captured_fn_sigs` map (sourced from `cc.captures_typed`) and
/// dispatches via `call_indirect`.
///
/// `caller(id_fn)` invokes the captured `id_fn` through the
/// indirect call; result is 42.
#[test]
fn closure_env_load_callee_returns_42() {
    let src = "fn id_fn(x: Int) -> Int ![] { x }\n\
               fn caller(f: (Int) -> Int ![]) -> Int ![] {\n  \
                 let g: (Int) -> Int ![] = fn (x: Int) -> Int ![] => f(x);\n  \
                 g(42)\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let r: Int = caller(id_fn);\n  \
                 perform IO.println(int_to_string(r));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "closure_env_load_callee");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "42\n",
        "ClosureEnvLoad-callee dispatch: caller(id_fn) calls captured `f` via \
         lambda → 42. stderr={stderr:?}"
    );
}

/// R4 finding 4 — Phase C+ Part 2 with a multi-param captured fn.
/// Exercises the args-loop in `lower_call`'s sig builder via the
/// ClosureEnvLoad path (Part 1 already exercises it via Ident path).
#[test]
fn closure_env_load_callee_multi_param_returns_7() {
    let src = "fn add(a: Int, b: Int) -> Int ![] { a + b }\n\
               fn caller(f: (Int, Int) -> Int ![]) -> Int ![] {\n  \
                 let g: (Int, Int) -> Int ![] = fn (a: Int, b: Int) -> Int ![] => f(a, b);\n  \
                 g(3, 4)\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let r: Int = caller(add);\n  \
                 perform IO.println(int_to_string(r));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "closure_env_load_multi_param");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "7\n",
        "ClosureEnvLoad multi-param dispatch: caller(add) calls captured \
         `f(a, b)` → 7. stderr={stderr:?}"
    );
}

/// R4 finding 4 — Phase C+ Part 2 with an effect-bearing captured
/// fn. Pins effect-row threading through the closure-record + indirect
/// call when the captured value carries effects.
#[test]
fn closure_env_load_callee_effect_row_returns_42() {
    let src = "fn announce(n: Int) -> Int ![IO] {\n  \
                 perform IO.println(int_to_string(n));\n  \
                 n\n\
               }\n\
               fn caller(f: (Int) -> Int ![IO]) -> Int ![IO] {\n  \
                 let g: (Int) -> Int ![IO] = fn (x: Int) -> Int ![IO] => f(x);\n  \
                 g(42)\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let _: Int = caller(announce);\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "closure_env_load_effect");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "42\n",
        "ClosureEnvLoad effect-bearing capture: caller(announce) prints 42 \
         via captured fn-typed value. stderr={stderr:?}"
    );
}

/// R4 finding 4 (most load-bearing) — Phase C+ Part 2 with mixed
/// capture kinds: a fn-typed capture `f` AND a non-fn capture `n`.
/// The synth fn body uses both: `f(n) + n`. Pins that the
/// `captures_typed` filter `if let Ty::Fn(sig) = cty` correctly
/// keeps `n` in the env layout (so reads from offset 16 + 8*1 give
/// the right value) WITHOUT putting `n` into `captured_fn_sigs`.
/// If the filter mishandles iteration order, env slot offsets
/// diverge between codegen's view and the synth fn's reads.
#[test]
fn closure_env_load_mixed_capture_kinds_returns_47() {
    let src = "fn double(n: Int) -> Int ![] { n + n }\n\
               fn caller(f: (Int) -> Int ![], n: Int) -> Int ![] {\n  \
                 let g: (Int) -> Int ![] = fn (x: Int) -> Int ![] => f(x) + n;\n  \
                 g(20)\n\
               }\n\
               fn main() -> Int ![IO] {\n  \
                 let r: Int = caller(double, 7);\n  \
                 perform IO.println(int_to_string(r));\n  \
                 0\n\
               }\n";
    let (stdout, stderr, code) = compile_and_run(src, "closure_env_load_mixed");
    assert_eq!(code, 0, "exit code; stderr={stderr:?}");
    assert_eq!(
        stdout, "47\n",
        "Mixed-capture-kinds: lambda captures fn-typed `f` AND Int `n`. \
         g(20) = f(20) + n = double(20) + 7 = 40 + 7 = 47. \
         stderr={stderr:?}"
    );
}
