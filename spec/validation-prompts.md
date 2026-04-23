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

## P05 — parity check via mod and if/else

**Prompt:** Write a Sigil program that assigns the integer `14` to a
variable `n`, uses the `%` operator and `==` to check whether `n` is
even (i.e. `n % 2 == 0`), prints `even` on a single line if it is and
`odd` otherwise via `perform IO.println`, then returns `0` as the
process exit status.

**Oracle (stdout):**
```
even
```

**Oracle (exit):** `0`

**Oracle (notes):** Exercises Stage-2 features — the `%` operator, the
`==` comparison on `Int`, a `let` binding of type `Bool`, and
`if`/`else` as an expression whose branches unify to `String`. The
prompt deliberately hard-codes `n = 14` so the oracle is
deterministic; generalising to runtime-varied input arrives with Plan
B's effect handlers.

## P07 — safe divide with explicit divisor check

**Prompt:** Write a Sigil program that assigns `n = 42` and `d = 0`,
then computes a value `q`: when `d == 0`, `q` should be `-1`;
otherwise `q` should be `n / d`. The program must not trigger the
runtime's division-by-zero trap. Return `q` as the process exit
status (no stdout output).

**Oracle (stdout):** *(empty)*

**Oracle (exit):** `255`

**Oracle (notes):** Unix exit codes are unsigned 8-bit, so `-1`
surfaces as `255` to a calling shell. The prompt tests the Stage-2
"explicit guard around `/`" pattern: without the guard the runtime
trap (E0401) fires and the process exits with status `2`. A correct
program threads a `Bool` through `if`/`else` to select between `-1`
and `n / d`, dodging the trap entirely. Plan B replaces this pattern
with a `Raise[ArithError]` effect the caller can handle; until then,
`if` guards are the only tool.
