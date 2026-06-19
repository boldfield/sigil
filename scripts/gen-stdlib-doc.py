#!/usr/bin/env python3
"""Generate docs/stdlib.raw.md — a raw API reference for the Sigil stdlib,
extracted from std/*.sigil at a given tag/ref (public types + function
signatures + each module's purpose). Internal `__`-prefixed helpers are
excluded. Front-matter-less, so GitHub Pages serves it verbatim at
sigillang.ai/stdlib.raw.md for LLM ingestion.

Usage: scripts/gen-stdlib-doc.py [ref]   (ref defaults to v1.2.0)
"""
import subprocess, re, os, sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REF = sys.argv[1] if len(sys.argv) > 1 else "v1.2.0"
OUT = os.path.join(REPO, "docs", "stdlib.raw.md")

def git(*args):
    return subprocess.run(["git", "-C", REPO, *args],
                          capture_output=True, text=True, check=True).stdout

def module_purpose(lines, mod):
    hdr = next((j for j, ln in enumerate(lines)
                if ln.strip().startswith("//") and f"std/{mod}.sigil" in ln), None)
    if hdr is None:
        return ""
    noise = re.compile(r'(Plan [A-Z]|post-PR|PR #\d|Task \d|follow-on|addendum|Tier \d|Stage \d)', re.I)
    k = hdr + 1
    while k < len(lines) and lines[k].strip() == "//":
        k += 1
    desc = []
    while k < len(lines) and lines[k].strip().startswith("//"):
        t = lines[k].strip().lstrip("/").strip()
        if t == "":
            break
        if not desc and noise.search(t):
            k += 1; continue
        desc.append(t); k += 1
        if len(desc) >= 2:
            break
    p = " ".join(desc)
    return re.sub(r'\s*\((?:post-PR|Plan|Tier|Stage|Task)[^)]*\)\.?', '', p).strip()

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
 "with `use std.<name>.{<fn>};`. Signatures show parameter types, the return",
 "type, and the effect row `![...]` (`![]` = pure). Reuse these types and",
 "functions — never redefine `JValue`, `List`, `Option`, etc.",
 "",
]
for path in paths:
    mod = os.path.basename(path)[:-6]
    lines = git("show", f"{REF}:{path}").split("\n")
    doc.append(f"## std.{mod}"); doc.append("")
    p = module_purpose(lines, mod)
    if p:
        doc.append(p); doc.append("")
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
