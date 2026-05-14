#!/usr/bin/env node
// Plan F1 — qualified-imports migration.
//
// Walks every `.sigil` source in std/ and examples/, plus every Sigil-
// source string literal in compiler/tests/e2e.rs, and inserts the
// `use mod.path.{names};` declarations the new resolver expects. For
// bare identifier references that match exactly one imported module's
// exports, we add the `use` line and leave the call site bare. For
// ambiguous references (the name is exported by two or more imported
// modules in the same file), we log a manual-review entry rather than
// guess.
//
// Run from the repo root:
//
//     node scripts/migrate-to-qualified-imports.mjs --repo .
//
// `--dry-run` reports what would change without modifying files. Use
// it before / after a real run for a sanity check + idempotency check.
//
// Plan author intended Python for this script; the pod build host has
// no Python interpreter so the implementation lives in Node.js (which
// IS in the pod image). Same algorithm.

import fs from "node:fs";
import path from "node:path";
import process from "node:process";

const SIGIL_KEYWORDS = new Set([
  "fn", "let", "perform", "import", "return", "true", "false",
  "if", "else", "match", "type", "effect", "handle", "with",
  "use", "as",
]);

// Primitive type names — always available regardless of imports.
// Plan F1 (2026-05-14) removed Option/Result/Some/None/Ok/Err from
// the auto-prelude — they now live in `std/option.sigil` and
// `std/result.sigil` and require `import std.{option,result}` +
// `use ...{names}` like every other stdlib name.
const SIGIL_BUILTIN_NAMES = new Set([
  "Int", "Int64", "Float", "Bool", "String", "Char", "Byte", "Unit",
  "Array", "MutArray", "ByteArray", "MutByteArray", "StringBuilder",
  "Continuation",
]);

// Plan F1 — typecheck-injected builtin fns. Each is registered in
// `compiler/src/typecheck.rs`'s `register_builtin_*` family; the
// migration treats them as belonging to the matching doc-only stdlib
// module (`std/X.sigil`) so the inserted `use std.X.{name};` line
// resolves via the file-qualified scheme key the typechecker mirrors
// onto these names. Keep in sync with
// `compiler/src/typecheck.rs::BUILTIN_TO_MODULE_FILE`.
const BUILTIN_FNS_BY_MODULE = {
  "std.array": ["array_alloc", "array_empty", "array_length", "array_get", "array_set"],
  "std.mut_array": ["mut_array_new", "mut_array_length", "mut_array_get", "mut_array_set"],
  "std.byte_array": ["byte_array_alloc", "byte_array_empty", "byte_array_length",
    "byte_array_get", "byte_array_concat", "byte_array_slice",
    "string_to_bytes", "string_from_bytes_validate", "string_from_bytes_alloc",
    "byte_in_range", "byte_truncate", "byte_to_int",
    // string_byte_at / string_length are byte-level ops routed here
    // to keep std.ordering off the std.string dep chain. See
    // BUILTIN_TO_MODULE_FILE in compiler/src/typecheck.rs.
    "string_byte_at", "string_length"],
  "std.mut_byte_array": ["mut_byte_array_new", "mut_byte_array_length",
    "mut_byte_array_get", "mut_byte_array_set"],
  "std.int64": ["int64_from_int", "int64_neg", "int64_to_int", "int64_to_string",
    "int64_add", "int64_sub", "int64_mul", "int64_div", "int64_mod",
    "int64_eq", "int64_lt", "int64_le", "int64_gt", "int64_ge"],
  "std.float": ["float_neg", "float_from_int", "float_to_int", "float_to_string",
    "float_add", "float_sub", "float_mul", "float_div",
    "float_eq", "float_lt", "float_le", "float_gt", "float_ge",
    "float_abs", "float_floor", "float_ceil", "float_sqrt",
    "string_to_float_validate", "string_to_float_parse"],
  "std.char": ["char_to_int", "int_to_char", "char_to_string",
    "char_eq", "char_lt", "char_le", "char_gt", "char_ge",
    "is_ascii", "is_ascii_digit", "is_ascii_alpha",
    "is_ascii_alphanumeric", "is_ascii_whitespace",
    "to_lower_ascii", "to_upper_ascii",
    "string_chars", "string_char_at", "string_from_chars"],
  "std.string_builder": ["sb_new", "sb_append", "sb_finalize"],
  "std.string": ["string_concat", "string_substring",
    "string_starts_with", "string_ends_with", "string_contains",
    "string_index_of", "string_trim",
    "string_to_int_validate", "string_to_int_parse"],
  "std.random": ["random_pseudo_int"],
  "std.int": ["int_to_string", "int_add_safe", "int_sub_safe",
    "int_xor", "int_shl", "int_shr", "int_abs"],
  "std.clock": ["now", "clock_os_now"],
  "std.panic": ["panic", "assert"],
  // Effect names. The typechecker registers these in
  // `builtin_effects()`; user code references them as `![IO]`,
  // `![Mem]`, `![ArithError]`. Migration adds the corresponding
  // `use` lines so the bare effect-row reference resolves.
  "std.io": ["IO"],
  "std.mem": ["Mem"],
  "std.raise": ["ArithError"],
  // Ref ops are file-gated to std/state.sigil; user code never
  // references them so we don't list them here.
  "std.ordering": ["string_compare"],
};

const IDENT_RE = /[A-Za-z_][A-Za-z0-9_]*/y;
const IMPORT_RE = /^\s*import\s+([a-zA-Z0-9_.]+)(?:\s+as\s+([a-zA-Z_][a-zA-Z0-9_]*))?\s*;?\s*$/;
const USE_RE = /^\s*use\s+[a-zA-Z0-9_.]+\s*\.\s*\{[^}]*\}\s*;?\s*$/;

const FN_DECL_RE = /^fn\s+([A-Za-z_][A-Za-z0-9_]*)/;
const TYPE_DECL_RE = /^type\s+([A-Za-z_][A-Za-z0-9_]*)/;
const EFFECT_DECL_RE = /^effect\s+([A-Za-z_][A-Za-z0-9_]*)/;
const VARIANT_RE = /\|\s*([A-Z][A-Za-z0-9_]*)/g;

// Strip `// ...` line comments — Sigil has no block-comment syntax.
function stripLineComment(line) {
  const idx = line.indexOf("//");
  if (idx === -1) return line;
  return line.slice(0, idx);
}

// Walk `std/<X>.sigil` files and produce { byModule, byName }. Also
// seeds the table with `BUILTIN_FNS_BY_MODULE` entries so typecheck-
// injected builtins (which have no source declaration to scan) are
// addressable by `use std.X.{name}` lines after the migration.
function scanStdlib(repo) {
  const byModule = new Map();
  const byName = new Map();
  for (const [module, names] of Object.entries(BUILTIN_FNS_BY_MODULE)) {
    for (const name of names) {
      addExport(byModule, byName, module, name);
    }
  }
  const stdDir = path.join(repo, "std");
  const files = fs.readdirSync(stdDir).filter((f) => f.endsWith(".sigil")).sort();
  for (const file of files) {
    const module = `std.${file.replace(/\.sigil$/, "")}`;
    const text = fs.readFileSync(path.join(stdDir, file), "utf8");
    let inTypeBody = false;
    for (const rawLine of text.split("\n")) {
      const line = stripLineComment(rawLine).replace(/\s+$/, "");
      if (!line.trim()) continue;
      const mFn = FN_DECL_RE.exec(line);
      if (mFn) {
        inTypeBody = false;
        // Plan F1 excludes `__`-prefixed (private-by-convention)
        // fns from the exports table. Cross-file references to
        // them stay as qualified calls (`std.list.__list_to_array(...)`)
        // — adding a `use mod.{__name};` line would treat a
        // module-internal helper as a public surface. Migration
        // hits one such site (process.sigil → list.sigil's
        // `__list_to_array`), which is hand-edited to qualify the
        // call.
        const name = mFn[1];
        if (!name.startsWith("__")) {
          addExport(byModule, byName, module, name);
        }
        continue;
      }
      const mType = TYPE_DECL_RE.exec(line);
      if (mType) {
        inTypeBody = true;
        addExport(byModule, byName, module, mType[1]);
        for (const v of [...line.matchAll(VARIANT_RE)]) {
          addExport(byModule, byName, module, v[1]);
        }
        continue;
      }
      const mEffect = EFFECT_DECL_RE.exec(line);
      if (mEffect) {
        inTypeBody = false;
        addExport(byModule, byName, module, mEffect[1]);
        continue;
      }
      if (inTypeBody) {
        for (const v of [...line.matchAll(VARIANT_RE)]) {
          addExport(byModule, byName, module, v[1]);
        }
        const trimmed = line.trimStart();
        if (trimmed.startsWith("fn ") || trimmed.startsWith("type ")
          || trimmed.startsWith("effect ") || trimmed.startsWith("import ")
          || trimmed.startsWith("use ")) {
          inTypeBody = false;
        }
      }
    }
  }
  return { byModule, byName };
}

function addExport(byModule, byName, module, name) {
  if (!byModule.has(module)) byModule.set(module, new Set());
  byModule.get(module).add(name);
  if (!byName.has(name)) byName.set(name, new Set());
  byName.get(name).add(module);
}

// Parse `import std.X.Y` (and the new `import std.X.Y as A`) lines.
function parseImports(text) {
  const imports = [];
  for (const line of text.split("\n")) {
    const m = IMPORT_RE.exec(line);
    if (!m) continue;
    const dotted = m[1];
    const alias = m[2] ?? null;
    if (dotted.startsWith("std.")) imports.push({ dotted, alias });
  }
  return imports;
}

// Extract identifier tokens from Sigil source, skipping comments,
// string literals, and char literals. Keywords are filtered out.
function extractIdentifiers(text) {
  const out = [];
  let i = 0;
  const n = text.length;
  // Lines starting with `import` or `use` are control-plane (they
  // bind names rather than reference them) and would otherwise
  // pollute the bare-identifier set with module path segments
  // ("std", "map", etc. from `import std.map`).
  let atLineStart = true;
  while (i < n) {
    const c = text[i];
    if (atLineStart) {
      // Skip leading whitespace to find the first non-blank token.
      let j = i;
      while (j < n && (text[j] === " " || text[j] === "\t")) j++;
      const rest = text.slice(j);
      if (rest.startsWith("import") || rest.startsWith("use")) {
        const lineEnd = text.indexOf("\n", j);
        i = lineEnd === -1 ? n : lineEnd + 1;
        continue;
      }
      atLineStart = false;
    }
    if (c === "\n") {
      atLineStart = true;
      i++;
      continue;
    }
    if (c === "/" && text[i + 1] === "/") {
      const j = text.indexOf("\n", i);
      if (j === -1) break;
      i = j;
      continue;
    }
    if (c === '"') {
      i++;
      while (i < n) {
        if (text[i] === "\\" && i + 1 < n) { i += 2; continue; }
        if (text[i] === '"') { i++; break; }
        i++;
      }
      continue;
    }
    if (c === "'") {
      i++;
      while (i < n) {
        if (text[i] === "\\" && i + 1 < n) { i += 2; continue; }
        if (text[i] === "'") { i++; break; }
        i++;
      }
      continue;
    }
    if (/[A-Za-z_]/.test(c)) {
      IDENT_RE.lastIndex = i;
      const m = IDENT_RE.exec(text);
      if (m) {
        const tok = m[0];
        let end = m.index + tok.length;
        // Qualified-path skip: if this IDENT is followed by `.IDENT`,
        // we treat the whole `a.b.c` chain as a single qualified
        // reference. The migration shouldn't suggest a `use` line for
        // a name that's already qualified at every use site, and the
        // intermediate `b`, `c` aren't bare references.
        let isQualified = false;
        let scan = end;
        while (scan < n && text[scan] === ".") {
          const tailCh = text[scan + 1];
          if (!tailCh || !/[A-Za-z_]/.test(tailCh)) break;
          IDENT_RE.lastIndex = scan + 1;
          const m2 = IDENT_RE.exec(text);
          if (!m2) break;
          isQualified = true;
          scan = m2.index + m2[0].length;
        }
        if (!isQualified && !SIGIL_KEYWORDS.has(tok)) {
          out.push(tok);
        }
        i = scan;
        continue;
      }
    }
    i++;
  }
  return out;
}

// Parse existing `use mod.{name1, name2 as alias};` lines into a
// {module -> Set<localName>} map. The migration merges these with
// computed bindings so re-running on already-migrated source is a
// no-op.
//
// The regex matches both the single-brace form (the normal case)
// and the doubled-brace form (`use mod.{{...}};` — what format!()
// targets contain after migration's brace-escape pass). Without the
// double-brace form, the strip step would miss those lines and the
// migration would emit DUPLICATE use lines on re-run.
const USE_LINE_PARSE_RE =
  /^\s*use\s+([a-zA-Z0-9_.]+)\.\s*\{\{?([^}]*)\}\}?\s*;?\s*$/;
function parseUseLines(text) {
  const result = new Map();
  for (const line of text.split("\n")) {
    const m = USE_LINE_PARSE_RE.exec(line);
    if (!m) continue;
    const module = m[1];
    const names = m[2]
      .split(",")
      .map((s) => s.trim())
      .filter((s) => s.length > 0)
      .map((s) => {
        // Strip `name as alias` — we track local names but use the
        // source name for the migration's bookkeeping.
        const parts = s.split(/\s+as\s+/);
        return parts[0];
      });
    if (!result.has(module)) result.set(module, new Set());
    for (const n of names) result.get(module).add(n);
  }
  return result;
}

// True iff the file has any `use` line at all — kept for parity
// with prior script versions; callers no longer treat this as a
// hard skip.
function alreadyMigrated(text) {
  for (const line of text.split("\n")) {
    if (USE_RE.test(line)) return true;
  }
  return false;
}

// Migrate a single .sigil source. Returns { text, manual }.
//
// Auto-import: bare references to names in `BUILTIN_FNS_BY_MODULE`
// that don't yet have a corresponding `import std.X` line in the
// file gain BOTH the import and the use binding, so e2e-test
// fixtures that bare-reference `string_concat` / `int_to_string` /
// etc. don't need to be hand-edited. The auto-import only kicks in
// for unambiguous builtin names (the typecheck-injected fns whose
// stdlib module is fixed by `BUILTIN_FNS_BY_MODULE`); user-fn
// references still need an explicit import.
function migrateSigilSource(text, exports, fileLabel) {
  // Strip any existing `use` lines from the input so the migration
  // produces fresh consolidated lines. Subsequent re-runs of the
  // migration will then be no-ops (idempotent). Existing `use`s'
  // local names are folded into the migration's name set via
  // `parseUseLines` below so we don't lose user-meaningful bindings.
  const existingUseBindings = parseUseLines(text);
  text = text
    .split("\n")
    .filter((line) => !USE_LINE_PARSE_RE.test(line))
    .join("\n");

  const imports = parseImports(text);
  const importedSetInitial = new Set(imports.map((i) => i.dotted));
  // Self-import guard: a stdlib file `std/<X>.sigil` must not get an
  // auto-injected `import std.X` (that's a circular self-import, E0033).
  // The `fileLabel` carries the repo-relative path; derive the module
  // name from `std/<name>.sigil` if applicable.
  const m = /(?:^|[/\\])std[/\\]([^/\\]+)\.sigil(?:$|:)/.exec(fileLabel);
  const selfModule = m ? `std.${m[1]}` : null;
  // User-declared-name shadow detection: collect names declared as
  // `fn X`, `type X`, `effect X`, or `type X = | A | B` (variants)
  // in the source. The auto-import path skips names that collide
  // with these so a user's local declaration keeps shadowing the
  // stdlib version without forcing an explicit import / use line.
  const userFnNames = new Set();
  const FN_DECL_LINE_RE = /^\s*fn\s+([A-Za-z_][A-Za-z0-9_]*)/;
  const TYPE_DECL_LINE_RE = /^\s*type\s+([A-Za-z_][A-Za-z0-9_]*)/;
  const EFFECT_DECL_LINE_RE = /^\s*effect\s+([A-Za-z_][A-Za-z0-9_]*)/;
  const VARIANT_DECL_LINE_RE_G = /\|\s*([A-Z][A-Za-z0-9_]*)/g;
  for (const line of text.split("\n")) {
    const m2 = FN_DECL_LINE_RE.exec(line);
    if (m2) userFnNames.add(m2[1]);
    const mt = TYPE_DECL_LINE_RE.exec(line);
    if (mt) userFnNames.add(mt[1]);
    const me = EFFECT_DECL_LINE_RE.exec(line);
    if (me) userFnNames.add(me[1]);
    for (const v of line.matchAll(VARIANT_DECL_LINE_RE_G)) {
      userFnNames.add(v[1]);
    }
  }
  // Build a map from each name to its module — for unambiguous
  // names — so bare references can auto-promote to an import + use.
  // Includes both typecheck-injected builtins (BUILTIN_FNS_BY_MODULE)
  // and stdlib-source fns scanned from `std/*.sigil`. Ambiguous
  // names (multiple modules export them, e.g. `map`) are excluded
  // here so the migration falls through to bucketing + manual review.
  const builtinNameToModule = new Map();
  for (const [module, names] of Object.entries(BUILTIN_FNS_BY_MODULE)) {
    for (const name of names) builtinNameToModule.set(name, module);
  }
  for (const [name, mods] of exports.byName.entries()) {
    if (mods.size === 1) {
      const module = [...mods][0];
      // Don't override builtins' module assignment.
      if (!builtinNameToModule.has(name)) {
        builtinNameToModule.set(name, module);
      }
    }
  }

  // Scan the body's bare identifiers first so we know which modules
  // need to be auto-imported.
  const allIdents = extractIdentifiers(text);
  const seenForAutoImport = new Set();
  const autoImports = new Set();
  for (const tok of allIdents) {
    if (seenForAutoImport.has(tok)) continue;
    seenForAutoImport.add(tok);
    if (SIGIL_BUILTIN_NAMES.has(tok)) continue;
    // Don't auto-import for a builtin name that this file ALSO
    // declares as a user fn — that's the explicit-shadow case
    // (`fn int_to_string(s: String) -> String { s }` overriding
    // the stdlib int_to_string).
    if (userFnNames.has(tok)) continue;
    const module = builtinNameToModule.get(tok);
    if (module && module !== selfModule && !importedSetInitial.has(module)) {
      autoImports.add(module);
    }
  }

  const importedModules = [...imports.map((i) => i.dotted), ...autoImports];
  if (importedModules.length === 0) return { text, manual: [] };
  const importedSet = new Set(importedModules);

  // Pre-filter the global name → producers map down to the modules
  // imported by this file (else we'd over-attribute names to modules
  // that aren't visible).
  const relevant = new Map();
  for (const [name, mods] of exports.byName.entries()) {
    const inter = new Set();
    for (const m of mods) if (importedSet.has(m)) inter.add(m);
    if (inter.size > 0) relevant.set(name, inter);
  }

  const idents = extractIdentifiers(text);
  const seen = new Set();
  const inOrder = [];
  for (const tok of idents) {
    if (seen.has(tok)) continue;
    if (SIGIL_BUILTIN_NAMES.has(tok)) continue;
    seen.add(tok);
    inOrder.push(tok);
  }

  const useLinesByModule = new Map();
  const manual = [];
  for (const name of inOrder) {
    // User-fn shadow: skip — the user has their own definition
    // and adding a `use mod.{name}` line would either collide
    // with the user fn (use-line E0147) or hijack their bare
    // call to the stdlib version.
    if (userFnNames.has(name)) continue;
    const producers = relevant.get(name);
    if (!producers) continue;
    if (producers.size === 1) {
      const module = [...producers][0];
      if (!useLinesByModule.has(module)) useLinesByModule.set(module, []);
      useLinesByModule.get(module).push(name);
    } else {
      manual.push(
        `${fileLabel}: bare \`${name}\` exported by [${[...producers].sort().join(", ")}] — disambiguate manually`,
      );
    }
  }

  // Note: we deliberately do NOT fold the original source's
  // `existingUseBindings` back in. The bucketing above is the
  // authoritative computation; preserving stale entries (e.g.,
  // `use std.string.{string_length}` after `string_length`'s home
  // moved to `std.byte_array`) would produce duplicate-use E0147
  // errors. Names that aren't reachable via `extractIdentifiers` +
  // bucketing weren't actually used by the file in any case.
  void existingUseBindings;
  if (useLinesByModule.size === 0 && autoImports.size === 0) {
    return { text, manual };
  }

  const newUseLines = [];
  for (const module of [...useLinesByModule.keys()].sort()) {
    const names = [...new Set(useLinesByModule.get(module))].sort();
    newUseLines.push(`use ${module}.{${names.join(", ")}};`);
  }

  // Build the import-injection prefix for modules we synthesised.
  // Emit them at the top so they precede any existing imports; the
  // `use` lines slot in after the last import below.
  const newImportLines = [];
  for (const module of [...autoImports].sort()) {
    newImportLines.push(`import ${module}`);
  }

  // Insert auto-import lines at the top, then add use lines after
  // the (possibly extended) last import line.
  const srcLines = text.split("\n");
  // Find an insertion point for auto-imports. If the file starts
  // with a `// sigil:` pragma or a doc comment block, insert AFTER
  // them; otherwise insert at the top.
  let insertImportsBefore = 0;
  while (insertImportsBefore < srcLines.length) {
    const l = srcLines[insertImportsBefore].trim();
    if (l === "" || l.startsWith("//")) {
      insertImportsBefore++;
      continue;
    }
    break;
  }
  // Build the resulting line list.
  const out = [];
  let lastImportIdx = -1;
  // Append auto-imports first, then resume from the original lines.
  for (let idx = 0; idx < insertImportsBefore; idx++) {
    out.push(srcLines[idx]);
  }
  for (const imp of newImportLines) {
    out.push(imp);
    lastImportIdx = out.length - 1;
  }
  // Walk the rest of the original file, tracking the absolute index
  // of any existing import line in the output for the `use`-insert
  // pass below.
  for (let idx = insertImportsBefore; idx < srcLines.length; idx++) {
    out.push(srcLines[idx]);
    if (IMPORT_RE.test(srcLines[idx])) {
      lastImportIdx = out.length - 1;
    }
  }
  if (newUseLines.length === 0) {
    return { text: out.join("\n"), manual };
  }
  if (lastImportIdx === -1) {
    // No imports at all (auto or pre-existing). Emit use lines at
    // the top.
    return { text: newUseLines.join("\n") + "\n" + out.join("\n"), manual };
  }
  const final = [];
  for (let idx = 0; idx < out.length; idx++) {
    final.push(out[idx]);
    if (idx === lastImportIdx) {
      for (const u of newUseLines) final.push(u);
    }
  }
  return { text: final.join("\n"), manual };
}

// Naive raw / plain Rust-string-literal scanner. We look for raw
// `r"..."` / `r#"..."#` first and plain `"..."` second. Heuristic
// for "is this Sigil source": the literal contains `import std.` or
// `fn main()`.
// True iff `body` looks like a Sigil source program (not a Rust
// diagnostic message that happens to contain "fn main()" or
// "import std." inside backtick-quoted code suggestions).
//
// The heuristic requires both:
//   - A Sigil top-level form (`import std.`, `use `, `fn `,
//     `type `, `effect `) near the start of the body, after any
//     leading whitespace / line continuations.
//   - A `\\n` escape (i.e., the Rust-source representation of a
//     newline) somewhere in the body. Single-line strings — even
//     ones containing `fn main()` as a documentation snippet —
//     are skipped.
function looksLikeSigil(body) {
  const hasNewline = body.includes("\\n") || body.includes("\n");
  if (!hasNewline) return false;
  // Find the first non-whitespace content (skipping ` ` and
  // line-continuation `\` + spaces).
  let i = 0;
  while (i < body.length) {
    const c = body[i];
    if (c === " " || c === "\t") { i++; continue; }
    if (c === "\\" && (body[i + 1] === "n" || body[i + 1] === " ")) {
      i += 2; continue;
    }
    if (c === "\\" && body[i + 1] === "\\") { break; }
    break;
  }
  const head = body.slice(i, i + 40);
  return (
    head.startsWith("import std.") ||
    head.startsWith("use ") ||
    head.startsWith("fn ") ||
    head.startsWith("type ") ||
    head.startsWith("effect ")
  );
}

// Walk Rust source character-by-character, applying `bodyTransform` to
// the body of every string literal (plain `"..."`, raw `r"..."`,
// raw-with-hashes `r#"..."#`). Comments, character literals, and
// non-string code pass through verbatim. The transform receives the
// raw body (with escapes intact for plain strings), a `kind` of
// `"raw"` or `"plain"`, a hash count for raw strings, AND an
// `isFormatTarget` flag (true if the string is the first argument
// of a Rust format-style macro like `format!()` / `println!()` /
// `eprintln!()` / `write!()` / `writeln!()` / `panic!()`).
// Comments are skipped first so a `///` doc line containing
// `"import std..."` doesn't masquerade as a string.

// Recognise the format-macro context by looking back from the
// string-literal start for `<macro>!(...)` opener. The string is a
// format target iff the macro name is one of the formatting macros
// listed below and the literal is the FIRST argument (no preceding
// `, ` between the `(` and the `"` start).
const FORMAT_MACROS = new Set([
  "format",
  "println",
  "print",
  "eprintln",
  "eprint",
  "write",
  "writeln",
  "panic",
  "todo",
  "unimplemented",
  "unreachable",
  "format_args",
  "assert",
  "assert_eq",
  "assert_ne",
  "debug_assert",
  "debug_assert_eq",
  "debug_assert_ne",
]);
function isFormatMacroContext(text, stringStartIdx) {
  // Walk backwards from stringStartIdx, skipping whitespace and any
  // immediately preceding `,` (so the literal can be a non-first
  // arg too — `format!(verb, "{x} hello")`).
  let i = stringStartIdx - 1;
  // First skip whitespace.
  while (i >= 0 && /\s/.test(text[i])) i--;
  if (i < 0) return false;
  // For `format!("...")`, we must have `(` here (the macro's open
  // paren) — direct adjacency means we're the first arg.
  // For `format!(name = "...")` or `format!(other_arg, "...")`, we
  // skip back past the previous arg + `,` + the `(`.
  // The simplest sound check: walk back to find the nearest `!`
  // followed by `(` with only commas and balanced parens / strings
  // between, then take the identifier preceding `!`.
  let depth = 0;
  while (i >= 0) {
    const c = text[i];
    if (c === ")") {
      depth++;
      i--;
      continue;
    }
    if (c === "(") {
      if (depth === 0) {
        // Found the opening paren of the enclosing call.
        i--;
        if (i >= 0 && text[i] === "!") {
          // Macro call. Read the identifier preceding `!`.
          i--;
          let end = i;
          while (i >= 0 && /[A-Za-z0-9_]/.test(text[i])) i--;
          const name = text.slice(i + 1, end + 1);
          return FORMAT_MACROS.has(name);
        }
        return false;
      }
      depth--;
      i--;
      continue;
    }
    // String literal — skip over it backwards crudely. Find the
    // opening quote.
    if (c === '"') {
      let j = i - 1;
      while (j >= 0 && !(text[j] === '"' && (j === 0 || text[j - 1] !== "\\"))) {
        j--;
      }
      i = j - 1;
      continue;
    }
    i--;
  }
  return false;
}

function walkRustStrings(text, bodyTransform) {
  const out = [];
  let i = 0;
  const n = text.length;
  while (i < n) {
    const c = text[i];
    // Block comment `/* ... */` (rare in Rust unit tests but cheap to
    // handle).
    if (c === "/" && text[i + 1] === "*") {
      const end = text.indexOf("*/", i + 2);
      if (end === -1) { out.push(text.slice(i)); break; }
      out.push(text.slice(i, end + 2));
      i = end + 2;
      continue;
    }
    // Line comment `// ...` or doc-comment `/// ...`.
    if (c === "/" && text[i + 1] === "/") {
      const eol = text.indexOf("\n", i);
      if (eol === -1) { out.push(text.slice(i)); break; }
      out.push(text.slice(i, eol));
      i = eol;
      continue;
    }
    // Char literal `'x'` / `'\n'` / `'\\'`. Always 3-4 chars; we
    // scan minimally to find the closing `'`.
    if (c === "'") {
      const start = i;
      i++;
      while (i < n && text[i] !== "'") {
        if (text[i] === "\\" && i + 1 < n) i += 2;
        else i++;
      }
      if (i < n) i++; // consume closing `'`
      out.push(text.slice(start, i));
      continue;
    }
    // Raw string `r"..."` / `r#"..."#` / `r##"..."##` etc.
    if (c === "r" && (text[i + 1] === '"' || text[i + 1] === "#")) {
      // Confirm we're not in the middle of an identifier (`raw_var`).
      const prev = i > 0 ? text[i - 1] : "";
      if (!/[A-Za-z0-9_]/.test(prev)) {
        let j = i + 1;
        let hashes = 0;
        while (text[j] === "#") { hashes++; j++; }
        if (text[j] === '"') {
          const bodyStart = j + 1;
          // Find matching `"###...` close.
          let k = bodyStart;
          while (k < n) {
            if (text[k] === '"') {
              let close = 0;
              let m = k + 1;
              while (close < hashes && text[m] === "#") { close++; m++; }
              if (close === hashes) {
                const body = text.slice(bodyStart, k);
                const isFormatTarget = isFormatMacroContext(text, i);
                const newBody = bodyTransform("raw", body, hashes, isFormatTarget);
                const hash = "#".repeat(hashes);
                if (newBody === null || newBody === body) {
                  out.push(text.slice(i, m));
                } else {
                  out.push(`r${hash}"${newBody}"${hash}`);
                }
                i = m;
                break;
              }
            }
            k++;
          }
          if (k >= n) { out.push(text.slice(i)); break; }
          continue;
        }
      }
    }
    // Plain string `"..."` with `\\`/`\"` escapes.
    if (c === '"') {
      const bodyStart = i + 1;
      let k = bodyStart;
      while (k < n) {
        if (text[k] === "\\" && k + 1 < n) { k += 2; continue; }
        if (text[k] === '"') break;
        k++;
      }
      if (k >= n) { out.push(text.slice(i)); break; }
      const body = text.slice(bodyStart, k);
      const isFormatTarget = isFormatMacroContext(text, i);
      const newBody = bodyTransform("plain", body, 0, isFormatTarget);
      if (newBody === null || newBody === body) {
        out.push(text.slice(i, k + 1));
      } else {
        out.push(`"${newBody}"`);
      }
      i = k + 1;
      continue;
    }
    out.push(c);
    i++;
  }
  return out.join("");
}

function migrateE2eSource(text, exports, fileLabel) {
  const manual = [];
  text = walkRustStrings(text, (kind, body, _hashes, isFormatTarget) => {
    if (!looksLikeSigil(body)) return null;
    // `isFormatTarget` is true iff the surrounding Rust macro is a
    // formatting macro (`format!()`, `println!()`, etc.). When set,
    // the inserted `use mod.{name};` lines emit `{{` / `}}` so the
    // macro sees literal braces instead of `{name}` placeholders.
    //
    // The pre-Plan-F1 heuristic checked the body content for `{{` /
    // `}}` patterns — false-positives on plain Sigil source with
    // adjacent `}}` from nested match arms. The new heuristic
    // inspects the wrapping macro context (via
    // `isFormatMacroContext` in `walkRustStrings`).
    let decoded;
    if (kind === "raw") {
      decoded = body;
    } else {
      // Plain-string escape decoder: handles `\n`, `\t`, `\r`, `\\`,
      // `\"`, `\0`, and the multi-line-string line-continuation form
      // `\<newline>` (which eats the newline and any whitespace up to
      // the next non-whitespace char — Rust's standard literal
      // behavior for `"first\<NL>  second"` -> `"firstsecond"`).
      // Other escapes pass through verbatim.
      let s = "";
      let i = 0;
      while (i < body.length) {
        const c = body[i];
        if (c === "\\" && i + 1 < body.length) {
          const nx = body[i + 1];
          if (nx === "n") { s += "\n"; i += 2; }
          else if (nx === "t") { s += "\t"; i += 2; }
          else if (nx === "r") { s += "\r"; i += 2; }
          else if (nx === "\\") { s += "\\"; i += 2; }
          else if (nx === '"') { s += '"'; i += 2; }
          else if (nx === "0") { s += "\0"; i += 2; }
          else if (nx === "\n") {
            // Line continuation: consume newline + following
            // whitespace.
            i += 2;
            while (i < body.length && (body[i] === " " || body[i] === "\t")) i++;
          } else { s += nx; i += 2; }
        } else {
          s += c;
          i++;
        }
      }
      decoded = s;
    }
    // alreadyMigrated check is intentionally omitted here:
    // `migrateSigilSource` strips and re-emits use lines so the
    // operation is idempotent on already-migrated literals.
    let newDecoded;
    try {
      const result = migrateSigilSource(
        decoded, exports, `${fileLabel}:${kind}-string`,
      );
      newDecoded = result.text;
      for (const m of result.manual) manual.push(m);
    } catch (e) {
      manual.push(`${fileLabel}: ${kind}-string transform failed: ${e.message}`);
      return null;
    }
    if (newDecoded === decoded) return null;
    if (isFormatTarget) {
      // The newly-added use lines need their braces escaped so the
      // surrounding `format!()` / `println!()` macro doesn't
      // interpret `{name}` as a positional placeholder. The body
      // we got from `migrateSigilSource` has the new use lines
      // appended with single-brace forms; locate them and double
      // the braces. The body's pre-existing `{{` / `}}` (literal-
      // brace escapes) stay untouched because they're not in the
      // newly-added lines.
      //
      // Strategy: scan line-by-line, double-brace every line that
      // begins with `use ` and contains `{`/`}`. Existing user
      // `use` lines (rare in format!() targets) already had
      // `{{` / `}}` if they were format-escaped, so doubling the
      // single-brace form is the right call here.
      const bodyLines = newDecoded.split("\n");
      const escaped = bodyLines.map((l) => {
        const trimmed = l.trimStart();
        if (trimmed.startsWith("use ") && (l.includes("{") || l.includes("}"))) {
          return l.replace(/\{/g, "{{").replace(/\}/g, "}}");
        }
        return l;
      });
      newDecoded = escaped.join("\n");
    }
    if (kind === "raw") return newDecoded;
    // Re-encode plain string. Escape backslash and double-quote
    // first; then emit each newline as `\n` (preserving the
    // original count) plus a Rust line-continuation `\<NL>...`
    // EXCEPT for the trailing-content newline, which gets a plain
    // `\n` so the decoded body doesn't gain or lose a final
    // newline relative to the original.
    const escaped = newDecoded
      .replace(/\\/g, "\\\\")
      .replace(/"/g, '\\"');
    if (!escaped.includes("\n")) {
      return escaped;
    }
    const lines = escaped.split("\n");
    // `lines.length - 1` newline positions. For each position i:
    //   - if i == lines.length - 2 AND lines[lines.length - 1] is
    //     empty (i.e., the original body ended with `\n`), emit
    //     just `\n` (trailing newline preserved).
    //   - otherwise emit `\n\<NL>               ` (continuation).
    const out = [];
    for (let idx = 0; idx < lines.length; idx++) {
      out.push(lines[idx]);
      if (idx === lines.length - 1) break;
      const isTrailingNewline =
        idx === lines.length - 2 && lines[lines.length - 1] === "";
      if (isTrailingNewline) {
        out.push("\\n");
      } else {
        out.push("\\n\\\n               ");
      }
    }
    return out.join("");
  });
  return { text, manual };
}

function parseArgs(argv) {
  const args = { repo: ".", dryRun: false, verbose: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--repo") args.repo = argv[++i];
    else if (a === "--dry-run") args.dryRun = true;
    else if (a === "--verbose") args.verbose = true;
    else if (a === "--help" || a === "-h") {
      console.log("usage: migrate-to-qualified-imports.mjs [--repo PATH] [--dry-run] [--verbose]");
      process.exit(0);
    } else {
      console.error(`unknown arg: ${a}`);
      process.exit(2);
    }
  }
  return args;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const repo = path.resolve(args.repo);
  if (!fs.existsSync(path.join(repo, "compiler", "src", "typecheck.rs"))) {
    console.error(`error: --repo ${repo} doesn't look like a sigil checkout`);
    process.exit(2);
  }

  console.log(`Scanning stdlib exports under ${path.join(repo, "std")}`);
  const exports = scanStdlib(repo);
  const exportsJsonPath = path.join(repo, "scripts", "stdlib-exports.json");
  const serializable = {};
  for (const [m, ns] of [...exports.byModule.entries()].sort()) {
    serializable[m] = [...ns].sort();
  }
  if (!args.dryRun) {
    fs.writeFileSync(exportsJsonPath, JSON.stringify(serializable, null, 2) + "\n");
  }
  const totalNames = Object.values(serializable).reduce((s, ns) => s + ns.length, 0);
  console.log(`Wrote stdlib-exports.json (${Object.keys(serializable).length} modules, ${totalNames} symbols)`);

  // Collect .sigil sources from std/ and examples/.
  const sigilFiles = [];
  for (const sub of ["std", "examples"]) {
    const dir = path.join(repo, sub);
    for (const f of fs.readdirSync(dir).filter((f) => f.endsWith(".sigil")).sort()) {
      sigilFiles.push(path.join(dir, f));
    }
  }

  const manualAll = [];
  let rewrites = 0;
  for (const p of sigilFiles) {
    const original = fs.readFileSync(p, "utf8");
    // alreadyMigrated short-circuit removed: migrateSigilSource is
    // idempotent (strips + re-emits use lines).
    const { text: newText, manual } = migrateSigilSource(
      original, exports, path.relative(repo, p),
    );
    for (const m of manual) manualAll.push(m);
    if (newText !== original) {
      if (!args.dryRun) fs.writeFileSync(p, newText);
      rewrites++;
      if (args.verbose) console.log(`rewrote: ${path.relative(repo, p)}`);
    }
  }

  // Rust-source-with-inline-Sigil pass. Walks every `.rs` file in
  // `compiler/src/` and `compiler/tests/` that contains an `import
  // std.` token (the cheap pre-filter for Sigil-source-bearing
  // files) and applies the same migration to every string literal
  // whose body looks like Sigil.
  //
  // `compiler/src/parser.rs` and `compiler/src/lexer.rs` are skipped
  // because their inline Sigil literals are parser-specific test
  // fixtures (e.g. `"use std.list.{map};\nfn main() ..."` testing
  // the use-decl grammar) — the migration's strip-and-re-emit
  // would erase the test-data shape we're trying to assert.
  const SKIP_RUST_FILES = new Set([
    "compiler/src/parser.rs",
    "compiler/src/lexer.rs",
    "compiler/src/resolve.rs",
    "compiler/src/imports.rs",
    // typecheck.rs holds the unit-test corpus: many tests
    // deliberately omit imports to assert unknown-name / missing-
    // effect diagnostics. The migration would re-add the imports
    // and defeat the test premise; the few inline Sigil sources
    // that DO want migration are intra-file edited.
    "compiler/src/typecheck.rs",
  ]);
  const rustCandidates = [];
  for (const sub of ["compiler/src", "compiler/tests"]) {
    const dir = path.join(repo, sub);
    if (!fs.existsSync(dir)) continue;
    for (const f of fs.readdirSync(dir)) {
      if (!f.endsWith(".rs")) continue;
      const rel = path.relative(repo, path.join(dir, f));
      if (SKIP_RUST_FILES.has(rel)) continue;
      rustCandidates.push(path.join(dir, f));
    }
  }
  let rustRewrites = 0;
  for (const p of rustCandidates) {
    const original = fs.readFileSync(p, "utf8");
    if (!original.includes("import std.")) continue;
    const { text: newText, manual } = migrateE2eSource(
      original, exports, path.relative(repo, p),
    );
    for (const m of manual) manualAll.push(m);
    if (newText !== original) {
      if (!args.dryRun) fs.writeFileSync(p, newText);
      rustRewrites++;
    }
  }

  console.log("");
  console.log("=== migration report ===");
  console.log(`.sigil files touched: ${rewrites}`);
  console.log(`Rust-source files touched: ${rustRewrites}`);
  console.log(`manual-review entries: ${manualAll.length}`);
  if (manualAll.length > 0) {
    console.log("");
    console.log("manual-review log (first 50 entries):");
    for (const e of manualAll.slice(0, 50)) console.log(`  - ${e}`);
    if (manualAll.length > 50) console.log(`  ... and ${manualAll.length - 50} more`);
  }
  if (args.dryRun) console.log("\n(dry-run — no files were written)");
}

main();
