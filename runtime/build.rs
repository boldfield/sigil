use std::path::PathBuf;
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
    // then emit -lgc. On Ubuntu, libgc-dev drops libgc.so into the
    // default search path so pkg-config returns nothing and -lgc suffices.
    // See PLAN_A1_DEVIATIONS.md ([Task 2, Task 13]) for the rationale.
    emit_pkg_config_search_paths("bdw-gc");
    println!("cargo:rustc-link-lib=gc");
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
