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

use std::path::{Path, PathBuf};
use std::process::Command;

/// Invoke `cc` to link `obj_path` with the runtime and libgc, producing
/// an executable at `out_path`. Returns the command stdout/stderr as a
/// single string on failure for diagnostic display.
pub fn link(obj_path: &Path, out_path: &Path) -> Result<(), String> {
    let runtime = locate_runtime_lib()
        .ok_or_else(|| "libsigil_runtime.a not found; build the runtime first".to_string())?;

    let mut cmd = build_cc_command(obj_path, out_path, &runtime);

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

fn locate_gc_lib() -> Option<PathBuf> {
    // Explicit override wins — release-archive consumers can
    // `export SIGIL_GC_LIB=...` to use a custom libgc.a.
    if let Ok(p) = std::env::var("SIGIL_GC_LIB") {
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
    //     lib/libgc.a
    //     std/...
    //
    // walking up one level from the executable's parent and into
    // `lib/` recovers the staticlib. Also try the flat-bundle layout
    // (`libgc.a` next to the binary).
    if let Ok(exe) = std::env::current_exe() {
        let exe_dir = exe.parent().map(PathBuf::from);
        let candidates = [
            // bin/sigil → ../lib/libgc.a
            exe_dir
                .as_ref()
                .and_then(|d| d.parent())
                .map(|p| p.join("lib").join("libgc.a")),
            // flat: sigil + libgc.a in the same dir
            exe_dir.as_ref().map(|d| d.join("libgc.a")),
        ];
        for c in candidates.into_iter().flatten() {
            if c.exists() {
                return Some(c);
            }
        }
    }

    // `cargo build` places libgc.a under target/<profile>/.
    // Walk a few candidate profile directories in preference order.
    for profile in &["release", "debug"] {
        let p = PathBuf::from("target")
            .join(profile)
            .join("libgc.a");
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
            let p = base.join("target").join(profile).join("libgc.a");
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
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

/// Internal helper to build the cc command without executing it. Used by
/// the link function and exposed for testing.
fn build_cc_command(obj_path: &Path, out_path: &Path, runtime: &Path) -> Command {
    let mut cmd = Command::new("cc");
    cmd.arg(obj_path).arg(runtime);

    if let Some(gc_lib) = locate_gc_lib() {
        cmd.arg(gc_lib);
    } else {
        for search_path in pkg_config_search_paths("bdw-gc") {
            cmd.arg(format!("-L{search_path}"));
        }
        cmd.arg("-lgc");
    }

    cmd.arg("-lpthread")
        .arg("-ldl")
        .arg("-lm")
        .arg("-o")
        .arg(out_path)
        .env("TZ", "UTC")
        .env("SOURCE_DATE_EPOCH", "0");

    #[cfg(target_os = "linux")]
    {
        cmd.arg("-Wl,--build-id=none");
        cmd.arg("-lgcc_s");
        cmd.arg("-rdynamic");
    }

    #[cfg(target_os = "macos")]
    cmd.arg("-Wl,-reproducible");

    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_cc_command_includes_static_libgc_when_found() {
        let root = PathBuf::from(".");
        let obj = root.join("test.o");
        let out = root.join("test");
        let runtime = root.join("libsigil_runtime.a");

        let cmd = build_cc_command(&obj, &out, &runtime);
        let args: Vec<std::ffi::OsString> = cmd.get_args().cloned().collect();

        let mut found_libgc_archive = false;
        let mut found_dynamic_lgc = false;

        for arg in &args {
            let arg_str = arg.to_string_lossy();
            if arg_str == "-lgc" {
                found_dynamic_lgc = true;
            }
            if arg_str.ends_with("libgc.a") {
                found_libgc_archive = true;
            }
        }

        if std::path::Path::new("target/release/libgc.a").exists()
            || std::path::Path::new("target/debug/libgc.a").exists()
        {
            assert!(
                found_libgc_archive,
                "when static libgc.a is found, command should include the archive path"
            );
            assert!(
                !found_dynamic_lgc,
                "when static libgc.a is found, command should NOT include -lgc"
            );
        } else {
            assert!(
                found_dynamic_lgc,
                "when static libgc.a is NOT found, command should include -lgc"
            );
        }
    }
}
