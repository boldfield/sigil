use std::path::PathBuf;
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
    //
    // When only the versioned runtime library (`libgc.so.1`, from `libgc1`)
    // is present but the unversioned linker symlink (`libgc.so`, from
    // `libgc-dev`) is absent, the bare `-lgc` fails. Detect this and create
    // a `libgc.so` symlink in OUT_DIR so the linker resolves it without
    // requiring the dev package.
    // See PLAN_A1_DEVIATIONS.md ([Task 2, Task 13]) for the rationale.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    emit_pkg_config_search_paths("bdw-gc");
    maybe_emit_versioned_gc_symlink();
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

/// On Linux, when `libgc-dev` is absent, `libgc.so` (the linker symlink) is
/// missing even though `libgc.so.1` (from `libgc1`) is present. Create a
/// `libgc.so` symlink in OUT_DIR and emit a search-path directive so the
/// linker resolves `-lgc` without root or the dev package.
fn maybe_emit_versioned_gc_symlink() {
    // Only needed when building for Linux on a Unix host.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }
    #[cfg(unix)]
    {
        let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
        let multiarch = match arch.as_str() {
            "x86_64" => "x86_64-linux-gnu",
            "aarch64" => "aarch64-linux-gnu",
            "arm" => "arm-linux-gnueabihf",
            _ => "",
        };

        let mut search_dirs: Vec<PathBuf> = Vec::new();
        if !multiarch.is_empty() {
            search_dirs.push(PathBuf::from(format!("/usr/lib/{multiarch}")));
        }
        search_dirs.push(PathBuf::from("/usr/lib"));
        search_dirs.push(PathBuf::from("/usr/local/lib"));

        // If the unversioned symlink already exists, the default search path
        // resolves it; nothing to do.
        for dir in &search_dirs {
            if dir.join("libgc.so").exists() {
                return;
            }
        }

        // Find the versioned library and create a local libgc.so symlink.
        let out_dir = match std::env::var("OUT_DIR") {
            Ok(d) => PathBuf::from(d),
            Err(_) => return,
        };
        for dir in &search_dirs {
            for name in &["libgc.so.1", "libgc.so.2"] {
                let versioned = dir.join(name);
                if versioned.exists() {
                    let link = out_dir.join("libgc.so");
                    if !link.exists() {
                        let _ = std::os::unix::fs::symlink(&versioned, &link);
                    }
                    if link.exists() {
                        println!("cargo:rustc-link-search=native={}", out_dir.display());
                    }
                    return;
                }
            }
        }
    }
}
