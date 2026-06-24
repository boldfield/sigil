//! Linker driver — plan A1 Stage 1 task 13.
//!
//! Takes the emitted object file plus the runtime staticlib plus Boehm GC
//! and produces an executable via the host `cc`. Reproducibility flags are
//! enumerated per-host:
//!
//! - Linux: `-Wl,--build-id=none`, `SOURCE_DATE_EPOCH=0` in env.
//! - macOS: `-Wl,-reproducible` (see PLAN_A1_DEVIATIONS.md [Task 13] —
//!   dyld rejects binaries without LC_UUID; `-reproducible` yields a
//!   stable content-hash UUID instead of omitting it).
//! - Both: `TZ=UTC` in the link env.
//!
//! The runtime library is located by looking for `libsigil_runtime.a` in
//! the same `target/<profile>/` directory that built the compiler binary.
//! That keeps the setup simple for development builds and for the e2e
//! test which runs `cargo run` before invoking the compiler.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Invoke `cc` to link `obj_path` with the runtime and libgc, producing
/// an executable at `out_path`. Returns the command stdout/stderr as a
/// single string on failure for diagnostic display.
pub fn link(obj_path: &Path, out_path: &Path) -> Result<(), String> {
    let runtime = locate_runtime_lib()
        .ok_or_else(|| "libsigil_runtime.a not found; build the runtime first".to_string())?;

    let search_paths = pkg_config_search_paths("bdw-gc");
    let argv = build_link_argv(obj_path, out_path, &runtime, &search_paths);

    let mut cmd = Command::new("cc");
    for arg in &argv {
        cmd.arg(arg);
    }
    cmd.env("TZ", "UTC").env("SOURCE_DATE_EPOCH", "0");

    let output = cmd
        .output()
        .map_err(|e| format!("cc invocation failed: {e}"))?;
    if !output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "cc exited {}: stdout={stdout} stderr={stderr}",
            output.status
        ));
    }
    Ok(())
}

/// Build the argument vector for the `cc` linker invocation.
///
/// Pure function — no I/O, no process spawning. The caller passes pre-resolved
/// paths and pkg-config search paths so this can be tested without side effects.
/// The emitted argv is identical to the inline construction that preceded this
/// refactor: obj, runtime, -L search paths, -lgc, -lpthread, -ldl, -lm, -o,
/// out, then platform-specific flags.
fn build_link_argv(
    obj_path: &Path,
    out_path: &Path,
    runtime: &Path,
    search_paths: &[String],
) -> Vec<OsString> {
    let mut argv: Vec<OsString> = Vec::new();

    argv.push(obj_path.as_os_str().to_owned());
    argv.push(runtime.as_os_str().to_owned());

    for sp in search_paths {
        argv.push(format!("-L{sp}").into());
    }

    argv.push("-lgc".into());
    argv.push("-lpthread".into());
    argv.push("-ldl".into());
    argv.push("-lm".into());
    argv.push("-o".into());
    argv.push(out_path.as_os_str().to_owned());

    #[cfg(target_os = "linux")]
    {
        argv.push("-Wl,--build-id=none".into());
        // Rust staticlibs pull in panic_unwind -> _Unwind_* symbols; cc
        // does not autolink libgcc_s when driving ld directly for a
        // non-Rust object. Add it explicitly.
        argv.push("-lgcc_s".into());
        // Plan E2 Phase 1 Task 5 — `-rdynamic` (`-Wl,--export-dynamic`)
        // exports defined symbols into `.dynsym` so the runtime's
        // `dlsym(RTLD_DEFAULT, "sigil_user_main")` lookup can resolve
        // them at safepoint-cross-check time.
        argv.push("-rdynamic".into());
    }

    #[cfg(target_os = "macos")]
    argv.push("-Wl,-reproducible".into());

    argv
}

fn pkg_config_search_paths(pkg: &str) -> Vec<String> {
    let output = match Command::new("pkg-config").args(["--libs", pkg]).output() {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    if !output.status.success() {
        return Vec::new();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .split_ascii_whitespace()
        .filter_map(|token| token.strip_prefix("-L").map(str::to_owned))
        .filter(|path| !path.is_empty())
        .collect()
}

fn locate_runtime_lib() -> Option<PathBuf> {
    // Explicit override wins — release-archive consumers who unpack to
    // a non-standard prefix can `export SIGIL_RUNTIME_LIB=...` rather
    // than restructure their tree to match a built-in lookup path.
    if let Ok(p) = std::env::var("SIGIL_RUNTIME_LIB") {
        let path = PathBuf::from(p);
        if path.exists() {
            return Some(path);
        }
    }

    // Release-archive layout: when the `sigil` binary ships in a
    // tarball like
    //
    //   sigil-<version>-<triple>/
    //     bin/sigil
    //     lib/libsigil_runtime.a
    //     std/...
    //
    // walking up one level from the executable's parent and into
    // `lib/` recovers the staticlib. Also try the flat-bundle layout
    // (`libsigil_runtime.a` next to the binary).
    if let Ok(exe) = std::env::current_exe() {
        let exe_dir = exe.parent().map(PathBuf::from);
        let candidates = [
            // bin/sigil → ../lib/libsigil_runtime.a
            exe_dir
                .as_ref()
                .and_then(|d| d.parent())
                .map(|p| p.join("lib").join("libsigil_runtime.a")),
            // flat: sigil + libsigil_runtime.a in the same dir
            exe_dir.as_ref().map(|d| d.join("libsigil_runtime.a")),
        ];
        for c in candidates.into_iter().flatten() {
            if c.exists() {
                return Some(c);
            }
        }
    }

    // `cargo build` places libsigil_runtime.a under target/<profile>/.
    // Walk a few candidate profile directories in preference order.
    for profile in &["release", "debug"] {
        let p = PathBuf::from("target")
            .join(profile)
            .join("libsigil_runtime.a");
        if p.exists() {
            return Some(p);
        }
    }
    // CARGO_MANIFEST_DIR fallback if invoked from a subdir.
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        let base = Path::new(&manifest)
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        for profile in &["release", "debug"] {
            let p = base.join("target").join(profile).join("libsigil_runtime.a");
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::Path;

    #[test]
    fn test_build_link_argv_default() {
        let obj = Path::new("/tmp/out.o");
        let out = Path::new("/tmp/out");
        let runtime = Path::new("/tmp/libsigil_runtime.a");
        let search_paths: Vec<String> = vec![];

        let argv = build_link_argv(obj, out, runtime, &search_paths);

        let mut expected: Vec<OsString> = vec![
            OsString::from("/tmp/out.o"),
            OsString::from("/tmp/libsigil_runtime.a"),
            OsString::from("-lgc"),
            OsString::from("-lpthread"),
            OsString::from("-ldl"),
            OsString::from("-lm"),
            OsString::from("-o"),
            OsString::from("/tmp/out"),
        ];

        #[cfg(target_os = "linux")]
        {
            expected.push(OsString::from("-Wl,--build-id=none"));
            expected.push(OsString::from("-lgcc_s"));
            expected.push(OsString::from("-rdynamic"));
        }

        #[cfg(target_os = "macos")]
        expected.push(OsString::from("-Wl,-reproducible"));

        assert_eq!(argv, expected);
    }

    #[test]
    fn test_build_link_argv_with_search_paths() {
        let obj = Path::new("/tmp/out.o");
        let out = Path::new("/tmp/out");
        let runtime = Path::new("/tmp/libsigil_runtime.a");
        let search_paths = vec![
            "/usr/local/lib".to_string(),
            "/opt/homebrew/lib".to_string(),
        ];

        let argv = build_link_argv(obj, out, runtime, &search_paths);

        let mut expected: Vec<OsString> = vec![
            OsString::from("/tmp/out.o"),
            OsString::from("/tmp/libsigil_runtime.a"),
            OsString::from("-L/usr/local/lib"),
            OsString::from("-L/opt/homebrew/lib"),
            OsString::from("-lgc"),
            OsString::from("-lpthread"),
            OsString::from("-ldl"),
            OsString::from("-lm"),
            OsString::from("-o"),
            OsString::from("/tmp/out"),
        ];

        #[cfg(target_os = "linux")]
        {
            expected.push(OsString::from("-Wl,--build-id=none"));
            expected.push(OsString::from("-lgcc_s"));
            expected.push(OsString::from("-rdynamic"));
        }

        #[cfg(target_os = "macos")]
        expected.push(OsString::from("-Wl,-reproducible"));

        assert_eq!(argv, expected);
    }
}
