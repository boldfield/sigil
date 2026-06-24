use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    println!("cargo:rerun-if-env-changed=SIGIL_GC_LIB");

    // When SIGIL_GC_LIB points to a static archive, link it statically.
    // This mirrors the compiler linker driver's locate_gc_lib() behavior
    // and lets `cargo test` work in environments that have a static
    // libgc.a (e.g. built via scripts/build-static-boehm.sh) but lack
    // the libgc-dev package (which provides the libgc.so linker stub
    // required for dynamic -lgc linking).
    if let Ok(path) = std::env::var("SIGIL_GC_LIB") {
        let p = PathBuf::from(&path);
        if p.exists() {
            if let Some(dir) = p.parent() {
                println!("cargo:rustc-link-search=native={}", dir.display());
            }
            println!("cargo:rustc-link-lib=static=gc");
            return;
        }
    }

    // Dynamic fallback: query pkg-config for macOS Homebrew -L paths,
    // then emit -lgc. On Ubuntu with libgc-dev, libgc.so is on the
    // default search path so pkg-config returns nothing and -lgc suffices.
    // See PLAN_A1_DEVIATIONS.md ([Task 2, Task 13]) for the rationale.
    emit_pkg_config_search_paths("bdw-gc");

    // Linux only: when only the libgc1 runtime package is installed
    // (no libgc-dev), `libgc.so` does not exist — only `libgc.so.1`.
    // The `-lgc` emitted below would then fail at link time.  Work around
    // this by creating a `libgc.so` stub symlink in OUT_DIR and adding it
    // to the native link-search path so the linker finds it.  The resulting
    // binary gets `NEEDED: libgc.so.1` (the SONAME embedded in the shared
    // library) and resolves it at runtime via ld.so — same outcome as
    // using `-lgc` with libgc-dev installed.
    #[cfg(target_os = "linux")]
    try_add_libgc_so_stub();

    println!("cargo:rustc-link-lib=gc");
}

/// On Linux: if `libgc.so` is not findable in the standard search paths but a
/// versioned `libgc.so.1` exists, create a stub symlink in `OUT_DIR` and emit
/// a `rustc-link-search` directive so the linker can satisfy `-lgc`.
#[cfg(target_os = "linux")]
fn try_add_libgc_so_stub() {
    // If libgc.so is already reachable we do not need a stub.
    let search_dirs = [
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
        "/usr/lib",
        "/lib/x86_64-linux-gnu",
        "/lib/aarch64-linux-gnu",
        "/lib",
    ];
    if search_dirs
        .iter()
        .any(|d| Path::new(d).join("libgc.so").exists())
    {
        return;
    }

    // Look for the versioned library installed by the libgc1 runtime package.
    let so1_candidates = [
        "/lib/x86_64-linux-gnu/libgc.so.1",
        "/usr/lib/x86_64-linux-gnu/libgc.so.1",
        "/lib/aarch64-linux-gnu/libgc.so.1",
        "/usr/lib/aarch64-linux-gnu/libgc.so.1",
        "/usr/lib/libgc.so.1",
        "/lib/libgc.so.1",
    ];
    let versioned = so1_candidates
        .iter()
        .find(|&&p| Path::new(p).exists())
        .copied();
    let Some(versioned) = versioned else { return };

    let out_dir = match std::env::var("OUT_DIR") {
        Ok(d) => d,
        Err(_) => return,
    };
    let stub = PathBuf::from(&out_dir).join("libgc.so");
    if !stub.exists() {
        let _ = std::os::unix::fs::symlink(versioned, &stub);
    }
    println!("cargo:rustc-link-search=native={out_dir}");
}

fn emit_pkg_config_search_paths(pkg: &str) {
    let output = match Command::new("pkg-config").args(["--libs", pkg]).output() {
        Ok(o) => o,
        Err(_) => return,
    };
    if !output.status.success() {
        return;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for token in stdout.split_ascii_whitespace() {
        if let Some(path) = token.strip_prefix("-L") {
            if !path.is_empty() {
                println!("cargo:rustc-link-search=native={path}");
            }
        }
    }
}
