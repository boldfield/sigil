fn main() {
    // Link against the system Boehm GC on every platform. The compiler's
    // linker driver also passes -lgc when linking user programs, so the
    // flag is duplicated: once here for `cargo test` builds that exercise
    // runtime FFI symbols, once at user-program link time.
    println!("cargo:rustc-link-lib=gc");
}
