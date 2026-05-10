# Spec validation log — run 2026-05-09T21:00:17-0700

Trace: `spec/validation-results-20260509T210017.jsonl`


## Pass rates

| Model | First-compile | First-run | After-edit | Final-pass |
|---|---|---|---|---|
| `claude-opus-4-7` | 57/62 (91.9%) | 57/62 (91.9%) | 5/62 (8.1%) | 62/62 (100.0%) |
| `claude-sonnet-4-6` | 57/62 (91.9%) | 57/62 (91.9%) | 5/62 (8.1%) | 62/62 (100.0%) |

## Per-prompt results

| Prompt | `claude-opus-4-7` first | `claude-sonnet-4-6` first | `claude-opus-4-7` final | `claude-sonnet-4-6` final |
|---|---|---|---|---|
| **P01** — hello world | ✅ | ✅ | ✅ | ✅ |
| **P02** — string concatenation through IO | ✅ | ✅ | ✅ | ✅ |
| **P03** — multi-line output | ✅ | ✅ | ✅ | ✅ |
| **P04** — sum-to-n via recursion | ✅ | ✅ | ✅ | ✅ |
| **P05** — parity check via mod and if/else | ❌ | ❌ | ✅ | ✅ |
| **P06** — multiplication table via nested recursion | ✅ | ✅ | ✅ | ✅ |
| **P07** — safe divide with explicit divisor check | ❌ | ❌ | ✅ | ✅ |
| **P08** — print fib(n) for n = 10..15 | ✅ | ✅ | ✅ | ✅ |
| **P09** — partial application via a returned lambda | ✅ | ✅ | ✅ | ✅ |
| **P10** — compose two lambdas | ✅ | ✅ | ✅ | ✅ |
| **P11** — length of a cons-list via recursive match | ✅ | ✅ | ✅ | ✅ |
| **P12** — sum of a cons-list | ✅ | ✅ | ✅ | ✅ |
| **P13** — Option-returning safe lookup | ✅ | ✅ | ✅ | ✅ |
| **P14** — 2D-point record with match destructuring | ✅ | ✅ | ✅ | ✅ |
| **P15** — map a function over a cons-list | ✅ | ✅ | ✅ | ✅ |
| **P16** — generic identity function applied at Int and String | ✅ | ✅ | ✅ | ✅ |
| **P17** — compose two unary functions across types | ✅ | ✅ | ✅ | ✅ |
| **P18** — Raise[String]-based safe parser for a small grammar | ✅ | ✅ | ✅ | ✅ |
| **P19** — State[Int]-based counter threaded through a list walk | ❌ | ❌ | ✅ | ✅ |
| **P20** — multi-shot Choose finds all (a, b) pairs with a + b == 7 | ✅ | ✅ | ✅ | ✅ |
| **P21** — tuple construction and destructure | ✅ | ✅ | ✅ | ✅ |
| **P22** — `std.pair` accessors | ✅ | ❌ | ✅ | ✅ |
| **P23** — type-parameterized effect row | ✅ | ✅ | ✅ | ✅ |
| **P24** — per-op generic params | ✅ | ✅ | ✅ | ✅ |
| **P25** — row-polymorphic discharger | ✅ | ✅ | ✅ | ✅ |
| **P26** — conditional k-call | ✅ | ✅ | ✅ | ✅ |
| **P27** — `return(v) =>` arm | ✅ | ✅ | ✅ | ✅ |
| **P28** — multi-arm handler with `std.state` | ✅ | ❌ | ✅ | ✅ |
| **P29** — nested handlers on distinct effects | ❌ | ✅ | ✅ | ✅ |
| **P30** — `MutArray` construction and indexed read/write | ✅ | ✅ | ✅ | ✅ |
| **P31** — `MutArray` in-place sum | ✅ | ✅ | ✅ | ✅ |
| **P32** — `StringBuilder` | ✅ | ✅ | ✅ | ✅ |
| **P33** — `MutByteArray` with byte conversion | ✅ | ✅ | ✅ | ✅ |
| **P34** — `ByteArray` checksum | ✅ | ✅ | ✅ | ✅ |
| **P35** — `string_from_bytes` happy path | ✅ | ✅ | ✅ | ✅ |
| **P36** — `std.list.map` + `fold` | ✅ | ✅ | ✅ | ✅ |
| **P37** — `std.list.filter` for evens | ✅ | ✅ | ✅ | ✅ |
| **P38** — `std.list.list_sort_int` | ✅ | ✅ | ✅ | ✅ |
| **P39** — `std.option.unwrap_or` | ✅ | ✅ | ✅ | ✅ |
| **P40** — `std.result` match | ✅ | ✅ | ✅ | ✅ |
| **P41** — `std.string` ops | ✅ | ✅ | ✅ | ✅ |
| **P42** — `std.char` ASCII classifier | ✅ | ✅ | ✅ | ✅ |
| **P43** — `std.format.format_int` | ✅ | ✅ | ✅ | ✅ |
| **P44** — `std.raise.catch` | ✅ | ✅ | ✅ | ✅ |
| **P45** — `std.state.run_state` | ✅ | ✅ | ✅ | ✅ |
| **P46** — `std.choose.all_choices` | ✅ | ✅ | ✅ | ✅ |
| **P47** — `std.choose.first_choice` | ✅ | ✅ | ✅ | ✅ |
| **P48** — `std.array` immutable | ✅ | ✅ | ✅ | ✅ |
| **P49** — `std.map` insert + lookup | ✅ | ✅ | ✅ | ✅ |
| **P50** — `std.env.env_var` | ✅ | ✅ | ✅ | ✅ |
| **P51** — `std.random.run_seeded_random` deterministic | ❌ | ✅ | ✅ | ✅ |
| **P52** — `std.clock.run_frozen_clock` | ✅ | ✅ | ✅ | ✅ |
| **P53** — float arithmetic | ✅ | ✅ | ✅ | ✅ |
| **P54** — `Int64` arithmetic near i64 max | ✅ | ✅ | ✅ | ✅ |
| **P55** — Bool operators | ✅ | ✅ | ✅ | ✅ |
| **P56** — `ArithError` discharge | ✅ | ✅ | ✅ | ✅ |
| **P57** — wrap-on-overflow | ✅ | ✅ | ✅ | ✅ |
| **P58** — 3-arity tuple destructure | ✅ | ✅ | ✅ | ✅ |
| **P59** — nested constructor patterns | ✅ | ✅ | ✅ | ✅ |
| **P60** — char literal patterns | ✅ | ✅ | ✅ | ✅ |
| **P61** — `assert` builtin | ✅ | ✅ | ✅ | ✅ |
| **P62** — multi-import composition | ✅ | ✅ | ✅ | ✅ |

