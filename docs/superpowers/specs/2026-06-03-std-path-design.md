# `std/path` — design

**Status:** approved (brainstorming complete), ready for implementation plan.

**Goal:** Ship `std/path.sigil`, a pure-string POSIX path-manipulation
module, closing the `std/path` row of spec §14.1 ("Deferred to
follow-up plans"). It is the first item worked from the §14.1 stdlib
backlog.

## Context & decisions

- **Pure string manipulation, no filesystem.** `std.path` never touches
  the filesystem — that is `std.fs`'s job (under the `Fs` effect).
  Every function is pure (`![]`). `std/fs.sigil` already documents the
  gap this fills: *"No path manipulation ops (no `join` / `basename` /
  `normalize`)"*.
- **POSIX, slash-separated only.** Separator is `/`. No Windows `\`
  handling, no drive letters. Matches Sigil's Unix-oriented `Fs` effect.
- **Python `posixpath` (`os.path`) semantics.** Chosen deliberately for
  LLM-authorship: an LLM predicts each function's behavior from its
  `os.path` prior (the dominant path API in training data) without
  reading docs. This includes inheriting `posixpath`'s two warts — the
  absolute-second-arg *reset* in `join`, and empty-string returns from
  `dirname`/`basename` — because those are exactly what the model
  already expects from Python. A "cleaned-up hybrid" was explicitly
  rejected: a custom semantics is unpredictable from any prior. There is
  exactly **one** deliberate divergence — `normalize` collapses 2+
  leading slashes to a single `/` rather than preserving `//` — because
  that posixpath corner has no LLM prior (see the `path_normalize`
  contract). The principle stands: match what the model predicts.
- **Minimalist surface.** Consistent with §13.3's stated philosophy
  (`std.list` "deliberately has no `max`/`min`/`sum`… no methods").
  Seven functions, each a direct `posixpath` equivalent. No convenience
  wrappers (see *Out of scope*).
- **No new types, intrinsics, or language features.** Built entirely on
  existing primitives: `string_concat`, `string_split`,
  `string_substring_opt`, `string_byte_at_opt`, `string_length`
  (confirm exact name during planning), and `std.list`
  (`length`/`reverse`/`fold`/`append`). Returns use built-in tuples and
  `Bool`.

## Public API

All rows are pure `![]`. Names use the `path_` module-prefix
convention. The "posixpath" column is the Python function whose
behavior this matches exactly.

| Function | Type | posixpath |
|---|---|---|
| `path_join(a, b)` | `(String, String) -> String` | `os.path.join` (binary) |
| `path_split(p)` | `(String) -> (String, String)` | `os.path.split` → `(head, tail)` |
| `path_basename(p)` | `(String) -> String` | `os.path.basename` (= `split`'s `tail`) |
| `path_dirname(p)` | `(String) -> String` | `os.path.dirname` (= `split`'s `head`) |
| `path_splitext(p)` | `(String) -> (String, String)` | `os.path.splitext` → `(root, ext)` |
| `path_normalize(p)` | `(String) -> String` | `os.path.normpath` |
| `path_is_absolute(p)` | `(String) -> Bool` | `os.path.isabs` |

## Semantics contract (the test oracle)

These cases are normative. Each becomes an assertion in the e2e suite.
All match CPython's `posixpath` exactly.

### `path_join(a, b)`
- If `b` is absolute (starts with `/`), the result is `b` (**reset**).
- Else if `a` is empty, the result is `b`.
- Else if `a` ends with `/`, the result is `a ++ b`.
- Else the result is `a ++ "/" ++ b`.

```
path_join("a", "b")    = "a/b"
path_join("a/", "b")   = "a/b"
path_join("a", "/b")   = "/b"      # absolute resets
path_join("", "b")     = "b"
path_join("a", "")     = "a/"
path_join("a/b", "c")  = "a/b/c"
path_join("/", "a")    = "/a"
```

### `path_split(p)` → `(head, tail)`
Split at the final `/`. `tail` is the substring after it (the last
component, possibly empty); `head` is everything up to and including it,
with trailing slashes then stripped from `head` **unless** `head` is all
slashes.

```
path_split("a/b/c") = ("a/b", "c")
path_split("a/b/")  = ("a/b", "")     # trailing slash -> empty tail
path_split("a")     = ("", "a")
path_split("/a")    = ("/", "a")
path_split("/")     = ("/", "")       # head is all-slashes -> not stripped
path_split("a//b")  = ("a", "b")      # head "a/" -> stripped to "a"
path_split("//a")   = ("//", "a")     # head all-slashes -> kept
path_split("")      = ("", "")
```

### `path_basename(p)` = `snd(path_split(p))`, `path_dirname(p)` = `fst(path_split(p))`
```
path_basename("/a/b")  = "b"     path_dirname("/a/b") = "/a"
path_basename("/a/b/") = ""      path_dirname("/a/b/")= "/a/b"
path_basename("/")     = ""      path_dirname("/")    = "/"
path_basename("a")     = "a"     path_dirname("a")    = ""
```

### `path_splitext(p)` → `(root, ext)`
`ext` is the final `.`-suffix of the **basename**, *including* the dot,
or `""` if none. Leading dots in the basename are ignored (a dotfile
like `.bashrc` has no extension). `root ++ ext == p` always.

```
path_splitext("a.tar.gz") = ("a.tar", ".gz")
path_splitext("a")        = ("a", "")
path_splitext("a.")       = ("a", ".")        # trailing dot -> ext "."
path_splitext(".bashrc")  = (".bashrc", "")   # leading-dot dotfile, no ext
path_splitext("a/.b")     = ("a/.b", "")      # dotfile in basename
path_splitext("/a/b.c")   = ("/a/b", ".c")
path_splitext("a..b")     = ("a.", ".b")
```

### `path_normalize(p)` (= `os.path.normpath`)
Collapse repeated slashes, drop `.` components, resolve `..` against the
preceding non-`..` component. A leading `..` with no parent above it is
preserved unless the path is absolute (where `..` above `/` is `/`).
The empty string normalizes to `"."`. Trailing slash removed.

```
path_normalize("a/../b")  = "b"
path_normalize("a/./b")   = "a/b"
path_normalize("a//b")    = "a/b"
path_normalize("a/b/")    = "a/b"
path_normalize("a/..")    = "."
path_normalize("")        = "."
path_normalize(".")       = "."
path_normalize("./a")     = "a"
path_normalize("../a")    = "../a"     # leading .. preserved (relative)
path_normalize("/../a")   = "/a"       # .. above root is root
path_normalize("/")       = "/"
path_normalize("/a/b/..") = "/a"
path_normalize("//a")     = "/a"       # see divergence note below
path_normalize("///a")    = "/a"
```

**One intentional divergence from `posixpath`.** CPython's `normpath`
*preserves* exactly two leading slashes (`normpath("//a") == "//a"`, a
POSIX implementation-defined corner) but collapses three or more
(`normpath("///a") == "/a"`). We collapse **any** run of 2+ leading
slashes to a single `/`. Rationale: this is the one place the
"inherit posixpath's warts" rule is overruled by the higher rule it
serves — *match the LLM's prediction*. The `//`-is-special behavior has
no meaningful LLM prior and is surprising even to humans; collapsing is
what a model (and a person) actually expects. This is the **only**
deliberate divergence; everything else — the `join` reset, the
empty-string `dirname`/`basename` returns — is reproduced faithfully.

### `path_is_absolute(p)` (= `os.path.isabs`)
True iff `p` starts with `/`.
```
path_is_absolute("/a") = true   path_is_absolute("a") = false
path_is_absolute("/")  = true   path_is_absolute("")  = false
```

## Implementation notes

Pure Sigil source, recursion + accumulators (no loops). Sketches to
de-risk the plan; the contract above is authoritative.

- **`path_is_absolute`**: `string_byte_at_opt(p, 0)` == `Some('/')`.
- **`path_join`**: branch on `path_is_absolute(b)`, then on `a` empty /
  `a` ends-with-`/` (check last byte), then `string_concat`.
- **`path_split`**: find the last `/` index (scan right-to-left or fold
  tracking last index); `tail = substring(last+1, len)`; `head` =
  substring up to `last+1`, then strip trailing `/` unless all-slashes.
  `basename`/`dirname` are thin wrappers (`snd`/`fst`).
- **`path_splitext`**: operate on the basename region only; find the
  last `.` after the first non-dot character of the basename; if found,
  split there (dot goes to `ext`), else `ext = ""`.
- **`path_normalize`**: `is_abs = path_is_absolute(p)`; split on `/`
  into components; fold, pushing real names onto a stack, dropping `.`
  and empty components, and for `..` popping the stack unless the top is
  `..` or (for relative paths) the stack is empty / (for absolute) at
  root; rejoin with `/`, prepend `/` if absolute; map the empty result
  to `"."`.

## Out of scope (YAGNI — confirmed)

- **`path_extension`** convenience — `snd(path_splitext(p))` already
  yields it; §13.3 will show that.
- **`path_join_all(parts)`** variadic — `fold(parts, "", path_join)`
  reproduces `posixpath.join`'s left-to-right reduction; §13.3 will show
  the fold.
- Windows/`\` semantics, drive letters.
- Anything filesystem-touching (`abspath`, `realpath`, symlink
  resolution, `exists`) — those require the `Fs` effect and belong in a
  later `std.fs` extension, not this pure module.

## Testing & docs

- **e2e:** add `import std.path` inline tests to
  `compiler/tests/e2e.rs`, one assertion per row of the semantics
  contract above (compile + run + assert stdout). Group by function.
- **smoke:** add `examples/path_demo.sigil` exercising the seven
  functions with an oracle, so `smoke.sh` and `reproducibility.sh` cover
  it.
- **spec:** add a `std.path` block to the §13.3 quick-reference table
  listing the seven functions, and a one-line note flagging the
  `path_join` reset (`join("a","/b") == "/b"`). `std/path.sigil` carries
  full per-function doc comments (the canonical reference).
- No `CAPABILITIES.md` / `SIGIL_FOR_LLMS.md` change required for a
  single additive pure module (revisit if it changes a headline claim).

## Deliverables / file map

| File | Change |
|---|---|
| `std/path.sigil` | New — the 7 functions + doc comments + `// sigil: 0.1` header |
| `compiler/tests/e2e.rs` | New tests — one per contract row |
| `examples/path_demo.sigil` | New — smoke/reproducibility coverage |
| `spec/language.md` | §13.3 — add `std.path` rows + the join-reset note |

(Whether `std.path` needs registration anywhere beyond being a
`std/*.sigil` source file — e.g. a module allow-list — is an
implementation detail to confirm in the plan by following how a recent
module like `std.set` is wired.)
