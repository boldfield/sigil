//! End-to-end test — plan A1 Stage 1 task 16.
//!
//! Compiles `examples/hello.sigil` with the `sigil` binary, runs the
//! resulting program, and asserts stdout == "hello, world\n" with
//! exit code 0. Runs green on both supported hosts.

// `expect`/`unwrap` are fine in tests; the workspace clippy rule bans
// them in compiler source so user-facing errors route through
// `CompilerError`. Test-module code is exempted per plan task 0.2.
#![allow(clippy::disallowed_methods)]

use std::path::PathBuf;
use std::process::Command;

/// Workspace root — `compiler/tests/` is two levels deep.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler/ has a parent (workspace root)")
        .to_path_buf()
}

#[test]
fn hello() {
    let root = workspace_root();
    let sigil_bin = PathBuf::from(env!("CARGO_BIN_EXE_sigil"));
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
