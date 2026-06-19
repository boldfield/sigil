#!/usr/bin/env python3
"""Generate docs/stdlib.raw.md — a raw API reference for the Sigil stdlib,
extracted from std/*.sigil at a given tag/ref: public types, function
signatures, AND builtin-effect operations (invoked via `perform`, whose
signatures live in module doc-comments, e.g. `IO.read_line() -> String`).
Internal `__`-prefixed helpers are excluded. Front-matter-less, so GitHub
Pages serves it verbatim at sigillang.ai/stdlib.raw.md for LLM ingestion.

Usage: scripts/gen-stdlib-doc.py [ref]   (ref defaults to v1.2.0)
"""
import subprocess, re, os, sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REF = sys.argv[1] if len(sys.argv) > 1 else "v1.2.0"
OUT = os.path.join(REPO, "docs", "stdlib.raw.md")
NOISE = re.compile(r'(Plan [A-Z]|post-PR|PR #\d|Task \d|follow-on|addendum|'
                   r'Tier \d|Stage \d|Documentation only|DEVIATION)', re.I)
PAREN = re.compile(r'\s*\((?:post-PR|Plan|Tier|Stage|Task)[^)]*\)\.?')

def git(*args):
    return subprocess.run(["git", "-C", REPO, *args],
                          capture_output=True, text=True, check=True).stdout

def module_purpose(lines, mod):
    hdr = next((j for j, ln in enumerate(lines)
                if ln.strip().startswith("//") and f"std/{mod}.sigil" in ln), None)
    if hdr is None:
        return ""
    # 1) prefer the header line's own post-dash description
    m = re.search(r'[—-]\s*(.+)', lines[hdr])
    head = PAREN.sub('', m.group(1).strip()) if m else ""
    if head and not NOISE.search(head):
        return head
    # 2) else the first non-noise comment block after the header
    k = hdr + 1
    while k < len(lines) and lines[k].strip() == "//":
        k += 1
    desc = []
    while k < len(lines) and lines[k].strip().startswith("//"):
        t = lines[k].strip().lstrip("/").strip()
        if t == "":
            break
        if NOISE.search(t):
            if desc:
                break
            k += 1; continue
        desc.append(t); k += 1
        if len(desc) >= 2:
            break
    return PAREN.sub('', " ".join(desc)).strip()

def effect_ops(lines):
    """Backtick-wrapped `Effect.op(args) -> ret` signatures from comments."""
    pat = re.compile(r'`([A-Z]\w*\.\w+\([^`]*\)\s*->\s*[^`]+?)`')
    ops, seen = [], set()
    for ln in lines:
        if not ln.strip().startswith("//"):
            continue
        for m in pat.finditer(ln):
            sig = re.sub(r'\s+', ' ', m.group(1)).strip()
            if sig not in seen:
                seen.add(sig); ops.append(sig)
    return ops

def public_types(lines):
    out, i, n = [], 0, len(lines)
    while i < n:
        if re.match(r'^type \w', lines[i]):
            block = [lines[i].rstrip()]; i += 1
            while i < n and lines[i].strip() != "" and not re.match(r'^(fn|type|import|use|//)', lines[i]):
                block.append(lines[i].rstrip()); i += 1
            out.append("\n".join(block).rstrip()); continue
        i += 1
    return out

def public_fns(lines):
    out, i, n = [], 0, len(lines)
    while i < n:
        m = re.match(r'^fn (\w+)', lines[i])
        if m and not m.group(1).startswith("__"):
            sig = [lines[i]]
            while "{" not in sig[-1] and i + 1 < n:
                i += 1; sig.append(lines[i])
            s = " ".join(x.strip() for x in sig).split("{")[0].strip()
            out.append(re.sub(r'\s+', ' ', s))
        i += 1
    return out

paths = sorted(p for p in git("ls-tree", "-r", "--name-only", REF, "std/").splitlines()
               if p.endswith(".sigil"))
doc = [
 "# Sigil standard library — raw API reference",
 "",
 f"Generated from `std/*.sigil` at Sigil {REF}. Import a module as",
 "`import std.<name>`; call qualified (`std.<name>.<fn>(...)`) or bind names",
 "with `use std.<name>.{<fn>};`. Builtin **effects** (IO, Fs, Env, ...) are",
 "invoked with `perform <Effect>.<op>(...)`. Signatures show parameter types,",
 "the return type, and the effect row `![...]` (`![]` = pure). Reuse these",
 "types and functions — never redefine `JValue`, `List`, `Option`, etc.",
 "",
]
for path in paths:
    mod = os.path.basename(path)[:-6]
    lines = git("show", f"{REF}:{path}").split("\n")
    doc.append(f"## std.{mod}"); doc.append("")
    p = module_purpose(lines, mod)
    if p:
        doc.append(p); doc.append("")
    ops = effect_ops(lines)
    if ops:
        doc.append("Effect operations (invoke with `perform`):"); doc.append("```")
        doc.extend(ops); doc.append("```"); doc.append("")
    ts = public_types(lines)
    if ts:
        doc.append("Types:"); doc.append("```")
        doc.append("\n\n".join(ts)); doc.append("```"); doc.append("")
    fs = public_fns(lines)
    if fs:
        doc.append("Functions:"); doc.append("```")
        doc.extend(fs); doc.append("```"); doc.append("")

with open(OUT, "w") as f:
    f.write("\n".join(doc) + "\n")
print(f"wrote {OUT}: {len(doc)} lines from {len(paths)} modules (ref {REF})")
