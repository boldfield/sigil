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

    let mut cmd = Command::new("cc");
    cmd.arg(obj_path).arg(&runtime);

    // On macOS Homebrew installs libgc outside the default linker search
    // path. Query pkg-config for `-L` entries and pass them through before
    // `-lgc`. Graceful fallback: if pkg-config is missing or has no entry
    // for bdw-gc we proceed with the bare `-lgc`, which works on Ubuntu
    // where apt places libgc on the default path.
    // See PLAN_A1_DEVIATIONS.md ([Task 2, Task 13]) for the rationale.
    for search_path in pkg_config_search_paths("bdw-gc") {
        cmd.arg(format!("-L{search_path}"));
    }

    cmd.arg("-lgc")
        .arg("-lpthread")
        .arg("-ldl")
        .arg("-lm")
        .arg("-o")
        .arg(out_path)
        .env("TZ", "UTC")
        .env("SOURCE_DATE_EPOCH", "0");

    #[cfg(target_os = "linux")]
    {
        cmd.arg("-Wl,--build-id=none");
        // Rust staticlibs pull in panic_unwind -> _Unwind_* symbols; cc
        // does not autolink libgcc_s when driving ld directly for a
        // non-Rust object. Add it explicitly.
        cmd.arg("-lgcc_s");
        // Plan E2 Phase 1 Task 5 — `-rdynamic` (`-Wl,--export-dynamic`)
        // exports defined symbols into `.dynsym` so the runtime's
        // `dlsym(RTLD_DEFAULT, "sigil_user_main")` lookup can resolve
        // them at safepoint-cross-check time. Without it,
        // `dlsym(RTLD_DEFAULT, ...)` returns NULL for every emitted
        // function, the stackmap index has zero resolved records, and
        // the cross-check goes silently vacuous on Linux (PR #163
        // review M1). macOS doesn't need an equivalent — all global
        // symbols in Mach-O binaries are dlsym-able by default.
        cmd.arg("-rdynamic");
    }

    #[cfg(target_os = "macos")]
    cmd.arg("-Wl,-reproducible");

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

/// Locate the static Boehm GC archive (`libgc.a`) using the same strategy
/// as [`locate_runtime_lib`]. Resolution order:
///
/// 1. `SIGIL_GC_LIB` env override — must be an absolute path that exists.
/// 2. Release-archive layout: `<exe_dir>/../lib/libgc.a`.
/// 3. Flat layout: `libgc.a` beside the binary.
/// 4. `target/release/libgc.a` then `target/debug/libgc.a` (relative to cwd,
///    then via `CARGO_MANIFEST_DIR` workspace root).
///
/// Returns `None` if no candidate path exists.
#[allow(dead_code)]
pub(crate) fn locate_gc_lib() -> Option<PathBuf> {
    // Explicit override — must be an absolute path so that callers cannot
    // accidentally resolve a relative path against an unexpected cwd.
    if let Ok(p) = std::env::var("SIGIL_GC_LIB") {
        let path = PathBuf::from(&p);
        if path.is_absolute() && path.exists() {
            return Some(path);
        }
    }

    // Release-archive layout and flat layout, both derived from the
    // current executable's directory.
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

    // `cargo build` places libgc.a under target/<profile>/ when the
    // static-Boehm build script is used.
    for profile in &["release", "debug"] {
        let p = PathBuf::from("target").join(profile).join("libgc.a");
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

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod locate_gc_lib_tests {
    use super::locate_gc_lib;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Mutex;

    // All tests that invoke locate_gc_lib() or touch process-global state
    // (SIGIL_GC_LIB env var, files reachable via current_exe()) must hold
    // this lock for the duration of the test. cargo test runs tests in
    // parallel by default; without this mutex, env mutations in one test
    // leak into another.
    static GLOBAL_STATE_LOCK: Mutex<()> = Mutex::new(());

    // RAII guard: restores an env var to its previous value on drop.
    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, val: &str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::set_var(key, val);
            Self { key, prev }
        }

        fn remove(key: &'static str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    // RAII guard: removes a file on drop (best-effort).
    struct FileGuard(PathBuf);

    impl FileGuard {
        fn create(path: PathBuf) -> std::io::Result<Self> {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, b"")?;
            Ok(Self(path))
        }
    }

    impl Drop for FileGuard {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    // Return the workspace root (parent of CARGO_MANIFEST_DIR).
    // In sigil-compiler tests, CARGO_MANIFEST_DIR == `<workspace>/compiler/`.
    fn workspace_root() -> PathBuf {
        let manifest = std::env::var("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR must be set by cargo test");
        PathBuf::from(manifest)
            .parent()
            .map(PathBuf::from)
            .expect("CARGO_MANIFEST_DIR must have a parent")
    }

    // Outcome 1: SIGIL_GC_LIB (absolute) wins over all other paths.
    #[test]
    fn override_wins() {
        let _lock = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join("sigil_gc_override_wins");
        fs::create_dir_all(&dir).unwrap();
        let libgc = dir.join("libgc.a");
        let _file = FileGuard::create(libgc.clone()).unwrap();
        let _env = EnvGuard::set("SIGIL_GC_LIB", libgc.to_str().unwrap());
        assert_eq!(locate_gc_lib(), Some(libgc));
    }

    // Outcome 1 (negative): a relative SIGIL_GC_LIB is ignored.
    #[test]
    fn override_relative_ignored() {
        let _lock = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::set("SIGIL_GC_LIB", "relative/libgc.a");
        // Must not return the relative value; anything returned must be absolute.
        if let Some(p) = locate_gc_lib() {
            assert!(
                p.is_absolute(),
                "locate_gc_lib must return an absolute path"
            );
            assert!(
                p.to_str().map(|s| !s.contains("relative")).unwrap_or(true),
                "locate_gc_lib must not return the relative SIGIL_GC_LIB value"
            );
        }
    }

    // Outcome 2: release-archive layout — exe_dir/../lib/libgc.a.
    #[test]
    fn release_archive_layout() {
        let _lock = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::remove("SIGIL_GC_LIB");

        let exe = std::env::current_exe().expect("current_exe must succeed");
        let exe_dir = exe.parent().expect("exe must have a parent dir");
        let lib_dir = exe_dir
            .parent()
            .expect("exe_dir must have a parent")
            .join("lib");
        let libgc = lib_dir.join("libgc.a");

        let _file = FileGuard::create(libgc.clone()).unwrap();
        assert_eq!(locate_gc_lib(), Some(libgc));
    }

    // Outcome 3: flat layout — libgc.a beside the binary.
    #[test]
    fn flat_layout() {
        let _lock = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::remove("SIGIL_GC_LIB");

        let exe = std::env::current_exe().expect("current_exe must succeed");
        let exe_dir = exe.parent().expect("exe must have a parent dir");

        // The release-archive path must not exist so the flat path is reached.
        let release_archive = exe_dir.parent().map(|p| p.join("lib").join("libgc.a"));
        assert!(
            release_archive
                .as_ref()
                .map(|p| !p.exists())
                .unwrap_or(true),
            "release-archive libgc.a must not pre-exist for this test"
        );

        let libgc = exe_dir.join("libgc.a");
        let _file = FileGuard::create(libgc.clone()).unwrap();
        assert_eq!(locate_gc_lib(), Some(libgc));
    }

    // Outcome 4a: target/release/libgc.a (via CARGO_MANIFEST_DIR fallback).
    #[test]
    fn cargo_target_release() {
        let _lock = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::remove("SIGIL_GC_LIB");

        let root = workspace_root();
        let libgc = root.join("target").join("release").join("libgc.a");
        let _file = FileGuard::create(libgc.clone()).unwrap();
        assert_eq!(locate_gc_lib(), Some(libgc));
    }

    // Outcome 4b: target/debug/libgc.a (release path must be absent).
    #[test]
    fn cargo_target_debug() {
        let _lock = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::remove("SIGIL_GC_LIB");

        let root = workspace_root();
        let release_path = root.join("target").join("release").join("libgc.a");
        assert!(
            !release_path.exists(),
            "target/release/libgc.a must not pre-exist for this test"
        );

        let libgc = root.join("target").join("debug").join("libgc.a");
        let _file = FileGuard::create(libgc.clone()).unwrap();
        assert_eq!(locate_gc_lib(), Some(libgc));
    }

    // Outcome 5: None — no candidate path exists anywhere.
    #[test]
    fn returns_none_when_absent() {
        let _lock = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _env = EnvGuard::remove("SIGIL_GC_LIB");

        let exe = std::env::current_exe().ok();
        let exe_dir = exe.as_ref().and_then(|e| e.parent()).map(PathBuf::from);
        let root = workspace_root();

        let candidates: Vec<PathBuf> = [
            exe_dir
                .as_ref()
                .and_then(|d| d.parent())
                .map(|p| p.join("lib").join("libgc.a")),
            exe_dir.as_ref().map(|d| d.join("libgc.a")),
            Some(root.join("target").join("release").join("libgc.a")),
            Some(root.join("target").join("debug").join("libgc.a")),
        ]
        .into_iter()
        .flatten()
        .collect();

        for c in &candidates {
            assert!(
                !c.exists(),
                "unexpected libgc.a at {} — remove it first",
                c.display()
            );
        }

        assert_eq!(locate_gc_lib(), None);
    }
}
