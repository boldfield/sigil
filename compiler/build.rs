//! Build script for `sigil-compiler` — Plan A2 task 1.5.5.
//!
//! Ensures `libsigil_runtime.a` is present in `target/<profile>/` before
//! `sigil-compiler`'s own build finishes. The e2e test at
//! `compiler/tests/e2e.rs` invokes the production `sigil` binary, which
//! in turn calls the linker driver in `compiler/src/link.rs` to link
//! against that staticlib. On a cold `cargo test --workspace` the
//! staticlib is not reliably materialised before the e2e test binary
//! links and runs — cargo builds `sigil-runtime` only as an rlib (that's
//! all sigil-compiler's Rust dep graph needs) unless something else
//! forces the staticlib output.
//!
//! See `PLAN_A2_DEVIATIONS.md` ([Task 1.5.5]) for why this shells out to
//! cargo rather than using an artifact dependency (unstable on 1.95.0)
//! or relying on CI restructure alone (doesn't fix local cold builds).
//!
//! Opt-out: set `SIGIL_SKIP_RUNTIME_STATICLIB_BUILD=1` in the environment
//! to skip the staticlib build and trust the caller to have produced it
//! some other way (custom build systems, IDE background indexers, etc.).

// `panic!`, `expect`, `unwrap` are fine in build scripts — they abort
// the build with a clear message, which is the right failure mode here.
// The workspace clippy rules ban them in compiler source so user-facing
// errors route through `CompilerError`; build-time paths are exempt.
#![allow(clippy::disallowed_macros, clippy::disallowed_methods)]

use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Rerun on any change to the runtime's source or build configuration
    // so we re-check staticlib freshness when runtime edits land.
    println!("cargo:rerun-if-changed=../runtime/src");
    println!("cargo:rerun-if-changed=../runtime/Cargo.toml");
    println!("cargo:rerun-if-changed=../runtime/build.rs");
    println!("cargo:rerun-if-env-changed=SIGIL_SKIP_RUNTIME_STATICLIB_BUILD");

    if std::env::var_os("SIGIL_SKIP_RUNTIME_STATICLIB_BUILD").is_some() {
        return;
    }

    // Cargo sets PROFILE to one of `debug` or `release`. In rustc
    // test-harness contexts PROFILE is still the outer profile, which is
    // what `link.rs` looks for under `target/<profile>/`.
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .expect("compiler/ has a parent (workspace root)")
        .to_path_buf();

    let staticlib = workspace_root
        .join("target")
        .join(&profile)
        .join("libsigil_runtime.a");

    if staticlib.exists() {
        // Fast path: staticlib already present. Cargo's rerun-if-changed
        // above will invalidate us if the runtime source changes.
        return;
    }

    // Shell out to cargo to build the staticlib. Cargo 1.74+ supports
    // nested invocations via the jobserver; the per-build-unit lock in
    // the parent cargo does not conflict with a child cargo build of a
    // different package.
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());

    let mut cmd = Command::new(cargo);
    cmd.arg("build").arg("-p").arg("sigil-runtime");
    if profile == "release" {
        cmd.arg("--release");
    }
    cmd.current_dir(&workspace_root);

    let status = cmd
        .status()
        .expect("failed to invoke cargo for sigil-runtime staticlib build");

    if !status.success() {
        panic!(
            "sigil-runtime staticlib build failed (exit {}); \
             set SIGIL_SKIP_RUNTIME_STATICLIB_BUILD=1 to bypass if the \
             staticlib is produced by another mechanism",
            status
        );
    }

    // Post-build sanity check. If the staticlib still isn't there, fail
    // loudly so the contributor has a clear error rather than a cryptic
    // linker failure later.
    if !staticlib.exists() {
        panic!(
            "staticlib {} not found after `cargo build -p sigil-runtime`; \
             check runtime/Cargo.toml crate-type = [\"staticlib\", \"rlib\"]",
            staticlib.display()
        );
    }
}
