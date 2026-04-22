# Sigil spec validation prompts

Each prompt is run against a fresh LLM session given only `spec/language.md`.
The session produces a program; `scripts/validate-spec.sh` (added in Plan C)
compiles it, runs it, and checks the output against the oracle.

Plan A1 seeds three prompts whose feature surface is covered by the hello-world
vertical slice. Plans A2, A3, B, and C add the remaining 17 prompts as their
feature surface enables them; the full bank reaches 20 before Plan C can
declare the spec validated.

Each prompt carries:
- **Prompt:** exactly what the fresh LLM session receives (after `spec/language.md`).
- **Oracle (stdout):** required stdout bytes, whitespace-sensitive.
- **Oracle (exit):** required process exit code.

If a prompt gains additional grading criteria (e.g., stderr must be empty), add
them as `Oracle (stderr):` / `Oracle (notes):` lines in the same block.

## P01 — hello world

**Prompt:** Write a Sigil program that prints exactly the text `hello, world`
followed by a newline, then exits with status 0.

**Oracle (stdout):**
```
hello, world
```

**Oracle (exit):** `0`

## P02 — string concatenation through IO

**Prompt:** Write a Sigil program that prints the string `hello, world` on a
single line by constructing the string from two parts (`"hello, "` and
`"world"`) via the stdlib's `string_concat` primitive, then exits with
status 0.

**Oracle (stdout):**
```
hello, world
```

**Oracle (exit):** `0`

**Oracle (notes):** If the compiler does not yet expose `string_concat`
(Plan A1 may not — the primitive is required from Plan A2 onward), this
prompt is graded only against "program compiles"; the run portion is
deferred until the feature lands.

## P03 — multi-line output

**Prompt:** Write a Sigil program that calls `perform IO.println` twice, once
with `"first"` and once with `"second"`, then exits with status 0.

**Oracle (stdout):**
```
first
second
```

**Oracle (exit):** `0`
