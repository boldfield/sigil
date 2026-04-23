use std::process::Command;

fn main() {
    // Link against the system Boehm GC on every platform. The compiler's
    // linker driver also passes -lgc when linking user programs, so the
    // flag is duplicated: once here for `cargo test` builds that exercise
    // runtime FFI symbols, once at user-program link time.
    //
    // On macOS, Homebrew installs libgc under `$(brew --prefix)/opt/bdw-gc/lib`
    // which is not on the linker's default search path. On Ubuntu the apt
    // package `libgc-dev` drops libgc into `/usr/lib/<triple>/` which *is*
    // default. We reconcile by querying `pkg-config bdw-gc` and emitting
    // any `-L<dir>` flags it reports. If `pkg-config` is missing or has no
    // entry for `bdw-gc` we silently fall back to the bare `-lgc`.
    // See PLAN_A1_DEVIATIONS.md ([Task 2, Task 13]) for the rationale.
    emit_pkg_config_search_paths("bdw-gc");
    println!("cargo:rustc-link-lib=gc");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
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
