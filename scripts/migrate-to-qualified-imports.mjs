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

// Primitive / auto-prelude names — always available regardless of imports.
const SIGIL_BUILTIN_NAMES = new Set([
  "Int", "Int64", "Float", "Bool", "String", "Char", "Byte", "Unit",
  "Array", "MutArray", "ByteArray", "MutByteArray", "StringBuilder",
  "Continuation",
  "Option", "Result", "Some", "None", "Ok", "Err",
]);

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

// Walk `std/<X>.sigil` files and produce { byModule, byName }.
function scanStdlib(repo) {
  const byModule = new Map();
  const byName = new Map();
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
        const name = mFn[1];
        if (!name.startsWith("__")) addExport(byModule, byName, module, name);
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

// Idempotency probe: skip files that already contain a `use mod.{..};`.
function alreadyMigrated(text) {
  for (const line of text.split("\n")) {
    if (USE_RE.test(line)) return true;
  }
  return false;
}

// Migrate a single .sigil source. Returns { text, manual }.
function migrateSigilSource(text, exports, fileLabel) {
  const imports = parseImports(text);
  const importedModules = imports.map((i) => i.dotted);
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

  if (useLinesByModule.size === 0) return { text, manual };

  const newUseLines = [];
  for (const module of [...useLinesByModule.keys()].sort()) {
    const names = [...new Set(useLinesByModule.get(module))].sort();
    newUseLines.push(`use ${module}.{${names.join(", ")}};`);
  }

  // Insert after the last `import` line.
  const srcLines = text.split("\n");
  // Note: trailing newline produces an empty final element with split.
  let lastImportIdx = -1;
  for (let idx = 0; idx < srcLines.length; idx++) {
    if (IMPORT_RE.test(srcLines[idx])) lastImportIdx = idx;
  }
  if (lastImportIdx === -1) {
    // No imports → unusual but possible. Emit at top.
    return { text: newUseLines.join("\n") + "\n" + text, manual };
  }
  const out = [];
  for (let idx = 0; idx < srcLines.length; idx++) {
    out.push(srcLines[idx]);
    if (idx === lastImportIdx) {
      for (const u of newUseLines) out.push(u);
    }
  }
  return { text: out.join("\n"), manual };
}

// Naive raw / plain Rust-string-literal scanner. We look for raw
// `r"..."` / `r#"..."#` first and plain `"..."` second. Heuristic
// for "is this Sigil source": the literal contains `import std.` or
// `fn main()`.
function looksLikeSigil(body) {
  return body.includes("import std.") || body.includes("fn main()");
}

// Walk Rust source character-by-character, applying `bodyTransform` to
// the body of every string literal (plain `"..."`, raw `r"..."`,
// raw-with-hashes `r#"..."#`). Comments, character literals, and
// non-string code pass through verbatim. The transform receives the
// raw body (with escapes intact for plain strings) and returns the
// replacement body; it may also return `null` to indicate "leave the
// literal untouched." Comments are skipped first so a `///` doc line
// containing `"import std..."` doesn't masquerade as a string.
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
                const newBody = bodyTransform("raw", body, hashes);
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
      const newBody = bodyTransform("plain", body, 0);
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
  text = walkRustStrings(text, (kind, body) => {
    if (!looksLikeSigil(body)) return null;
    // Format-string heuristic: bodies containing `{{` or `}}` are
    // almost certainly `format!()` / `println!()` arguments. Inserting
    // `use std.X.{name};` would be interpreted as a format placeholder
    // by Rust at compile time. Plan: log to manual-review so the
    // author edits the format string by hand.
    if (body.includes("{{") || body.includes("}}")) {
      manual.push(`${fileLabel}: ${kind}-string looks like a format!()/println!() target (contains \`{{\` / \`}}\`) — migrate manually`);
      return null;
    }
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
    if (alreadyMigrated(decoded)) return null;
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
    if (kind === "raw") return newDecoded;
    // Re-encode plain string. Escape backslash, double-quote, and
    // newline. For multi-line bodies, emit Rust's line-continuation
    // form `\n\<NL>               ` so the test source stays readable.
    // The 15-space indent matches the typical `let src =` indent the
    // e2e.rs tests use.
    const escaped = newDecoded
      .replace(/\\/g, "\\\\")
      .replace(/"/g, '\\"');
    if (!escaped.includes("\n")) {
      return escaped.replace(/\n/g, "\\n");
    }
    const lines = escaped.split("\n");
    return lines
      .map((line, idx) => (idx === lines.length - 1 ? line : line + "\\n\\\n               "))
      .join("");
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
    if (alreadyMigrated(original)) {
      if (args.verbose) console.log(`skip (already migrated): ${path.relative(repo, p)}`);
      continue;
    }
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

  // e2e.rs pass.
  const e2ePath = path.join(repo, "compiler", "tests", "e2e.rs");
  let e2eRewrites = 0;
  if (fs.existsSync(e2ePath)) {
    const original = fs.readFileSync(e2ePath, "utf8");
    const { text: newText, manual } = migrateE2eSource(
      original, exports, path.relative(repo, e2ePath),
    );
    for (const m of manual) manualAll.push(m);
    if (newText !== original) {
      if (!args.dryRun) fs.writeFileSync(e2ePath, newText);
      e2eRewrites = 1;
    }
  }

  console.log("");
  console.log("=== migration report ===");
  console.log(`.sigil files touched: ${rewrites}`);
  console.log(`e2e.rs touched: ${e2eRewrites}`);
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
