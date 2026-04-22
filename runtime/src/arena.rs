//! Trampoline arena allocator — plan A1 Stage 1 task 2 (module stub only).
//!
//! Populated by Plan B when the CPS trampoline ships. The module exists
//! now so Plan B doesn't have to refactor around its introduction.
//!
//! Plan B API surface (`sigil_arena_alloc`, `sigil_arena_reset`) is declared
//! as stubs that abort when called. v1 emits no codegen to these symbols,
//! so calling them means a compiler bug or manual test invocation.

/// Allocate `_size` bytes from the current dispatch arena. v1 unused.
///
/// # Safety
///
/// Stub — aborts. Placeholder signature so Plan B additions are ABI-stable.
#[no_mangle]
pub unsafe extern "C" fn sigil_arena_alloc(_size: usize) -> *mut u8 {
    eprintln!("sigil_arena_alloc: Plan B stub — called in v1 which has no CPS trampoline");
    std::process::abort();
}

/// Reset the current dispatch arena (invoked at the top of `run_loop` in
/// Plan B). v1 unused.
///
/// # Safety
///
/// Stub — aborts. Placeholder signature so Plan B additions are ABI-stable.
#[no_mangle]
pub unsafe extern "C" fn sigil_arena_reset() {
    eprintln!("sigil_arena_reset: Plan B stub — called in v1 which has no CPS trampoline");
    std::process::abort();
}
