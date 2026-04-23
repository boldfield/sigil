//! End-to-end test — plan A1 Stage 1 task 16.
//!
//! Compiles `examples/hello.sigil` with the `sigil` binary, runs the
//! resulting program, and asserts stdout == "hello, world\n" with
//! exit code 0. Runs green on both supported hosts.

// `expect`/`unwrap`/`panic!` are fine in tests; the workspace clippy
// rule bans them in compiler source so user-facing errors route through
// `CompilerError`. Test-module code is exempted per plan task 0.2.
#![allow(clippy::disallowed_methods, clippy::disallowed_macros)]

use std::path::PathBuf;
use std::process::Command;

/// Workspace root — `compiler/tests/` is two levels deep.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler/ has a parent (workspace root)")
        .to_path_buf()
}

#[test]
fn hello() {
    let root = workspace_root();
    let sigil_bin = PathBuf::from(env!("CARGO_BIN_EXE_sigil"));
    let source = root.join("examples/hello.sigil");
    let out_path = std::env::temp_dir().join(format!("sigil_e2e_hello_{}", std::process::id(),));

    // Invoke the compiler from the workspace root so the linker can
    // find target/<profile>/libsigil_runtime.a via its relative-path
    // lookup.
    let compile = Command::new(&sigil_bin)
        .arg(&source)
        .arg("-o")
        .arg(&out_path)
        .current_dir(&root)
        .output()
        .expect("failed to invoke sigil compiler");
    assert!(
        compile.status.success(),
        "sigil compilation failed: stdout={} stderr={}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr),
    );

    let run = Command::new(&out_path)
        .output()
        .expect("failed to execute compiled hello binary");
    assert!(
        run.status.success(),
        "compiled hello exited with {} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr),
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "hello, world\n",
        "hello-world stdout mismatch",
    );

    // Best-effort cleanup; ignore errors.
    let _ = std::fs::remove_file(&out_path);
}

/// Compile hello.sigil and compile-only (no link), then inspect the .o
/// file's stackmap section bytes and parse them via the runtime's
/// parser. Asserts the v0 placeholder invariants: magic, version = 0,
/// every record flagged placeholder, live_count = 0.
#[test]
fn stackmap_section_parses_v0_placeholder() {
    use sigil_compiler::{
        closure_convert, codegen, color, cps, elaborate, lexer, monomorphize, parser, resolve,
        typecheck,
    };
    use sigil_runtime::stackmap::{
        parse_section, ParseError, STACKMAP_FLAG_PLACEHOLDER, STACKMAP_VERSION_PLACEHOLDER,
    };

    let root = workspace_root();
    let src = std::fs::read_to_string(root.join("examples/hello.sigil")).expect("read hello.sigil");

    let (toks, lex_errs) = lexer::lex("hello.sigil", &src);
    assert!(lex_errs.is_empty(), "lex errors: {lex_errs:?}");
    let (prog, parse_errs) = parser::parse("hello.sigil", &toks);
    assert!(parse_errs.is_empty(), "parse errors: {parse_errs:?}");
    let (rp, resolve_errs) = resolve::resolve(prog);
    assert!(resolve_errs.is_empty(), "resolve errors: {resolve_errs:?}");
    let (checked, tc_errs) = typecheck::typecheck(rp.program);
    assert!(tc_errs.is_empty(), "tc errors: {tc_errs:?}");
    let anf = elaborate::elaborate(checked);
    let mono = monomorphize::monomorphize(anf);
    let colored = color::infer_colors(mono);
    let cps_ir = cps::transform(colored);
    let cc = closure_convert::convert(cps_ir);

    let obj_path =
        std::env::temp_dir().join(format!("sigil_e2e_stackmap_{}.o", std::process::id()));
    codegen::emit_object(&cc, &obj_path).expect("emit_object");

    let bytes = std::fs::read(&obj_path).expect("read object file");

    // The object-file section we wrote is tagged `__SIGIL,__stackmaps`
    // (Mach-O) or `.sigil_stackmaps` (ELF). Rather than re-parse the
    // enclosing object format here, locate the section by searching for
    // the magic bytes — which are anchored inside the section we wrote,
    // and collision with generated code is vanishingly unlikely for a
    // 4-byte ASCII pattern ("SGST") followed by a zero version word.
    let needle: &[u8] = &[b'S', b'G', b'S', b'T', 0, 0, 0, 0];
    let pos = bytes
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("stackmap magic+version not found in object file");
    let section = &bytes[pos..];
    let parsed = parse_section(section).unwrap_or_else(|e: ParseError| {
        panic!("stackmap parse failed: {e:?}");
    });
    assert_eq!(parsed.version, STACKMAP_VERSION_PLACEHOLDER);
    // hello.sigil's main body hits four call sites: gc_init, user_main,
    // sigil_string_new, sigil_println.
    assert_eq!(parsed.records.len(), 4, "expected 4 placeholder records");
    for r in &parsed.records {
        assert_eq!(r.live_count, 0, "v0 invariant: live_count always 0");
        assert_eq!(
            r.flags & STACKMAP_FLAG_PLACEHOLDER,
            STACKMAP_FLAG_PLACEHOLDER,
            "v0 invariant: placeholder flag set on every record",
        );
    }

    let _ = std::fs::remove_file(&obj_path);
}
