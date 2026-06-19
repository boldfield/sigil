# Sigil standard library — raw API reference

Generated from `std/*.sigil` at Sigil v1.2.0. Import a module as
`import std.<name>`; call qualified (`std.<name>.<fn>(...)`) or bind names
with `use std.<name>.{<fn>};`. Signatures show parameter types, the return
type, and the effect row `![...]` (`![]` = pure). Reuse these types and
functions — never redefine `JValue`, `List`, `Option`, etc.

## std.array

Functions:
```
fn array_get_opt[A](arr: Array[A], i: Int) -> Option[A] ![]
fn array_set_opt[A](arr: Array[A], i: Int, val: A) -> Option[Array[A]] ![]
```

## std.byte_array

Functions:
```
fn byte_from_int(n: Int) -> Option[Byte] ![]
fn byte_array_get_opt(ba: ByteArray, i: Int) -> Option[Byte] ![]
fn byte_array_slice_opt(ba: ByteArray, start: Int, end: Int) -> Option[ByteArray] ![]
fn string_from_bytes(ba: ByteArray) -> Option[String] ![]
```

## std.char

**Documentation only.** The `Char` primitive type and its operations are registered at the typechecker as builtins via

## std.choose

`Choose` is the canonical nondeterminism / backtracking effect: a computation performs `Choose.choose(n)` to pick a value

Functions:
```
fn all_choices_helper[A]( k: Continuation[Int, List[A]], i: Int, n: Int, acc: List[A], ) -> List[A] ![]
fn all_choices[A](body: () -> A ![Choose | e]) -> List[A] ![| e]
fn first_choice_helper[A]( k: Continuation[Int, Option[A]], i: Int, n: Int, ) -> Option[A] ![]
fn first_choice[A](body: () -> A ![Choose | e]) -> Option[A] ![| e]
```

## std.clock

`Clock` is a user-declared effect with a single op, `now() -> Int` returning nanoseconds since the Unix epoch. The

Functions:
```
fn now() -> Int ![Clock]
fn run_os_clock[A](body: () -> A ![Clock]) -> A ![]
fn run_frozen_clock[A](timestamp: Int64, body: () -> A ![Clock]) -> A ![]
```

## std.env

User-facing wrappers around the `Env` effect's raw-shape ops. The effect's `args` / `var` / `vars` ops return raw shapes

Functions:
```
fn env_args() -> List[String] ![Env]
fn env_var(name: String) -> Option[String] ![Env]
fn env_vars() -> List[(String, String)] ![Env]
```

## std.float

The `Float` type and its arithmetic/comparison/math/conversion builtins are registered at the typechecker via

Functions:
```
fn string_to_float(s: String) -> Option[Float] ![]
```

## std.format

`format(template, args)` walks a template string and substitutes `{}` placeholders with stringified `FormatArg` values. Closes the

Types:
```
type FormatArg =
  | AInt(Int)
  | AInt64(Int64)
  | AFloat(Float)
  | AString(String)
  | ABool(Bool)
  | AChar(Char)
```

Functions:
```
fn format(template: String, args: List[FormatArg]) -> String ![]
fn format1(template: String, a: FormatArg) -> String ![]
fn format2(template: String, a: FormatArg, b: FormatArg) -> String ![]
fn format3(template: String, a: FormatArg, b: FormatArg, c: FormatArg) -> String ![]
fn format4(template: String, a: FormatArg, b: FormatArg, c: FormatArg, d: FormatArg) -> String ![]
fn format5(template: String, a: FormatArg, b: FormatArg, c: FormatArg, d: FormatArg, e: FormatArg) -> String ![]
fn format6(template: String, a: FormatArg, b: FormatArg, c: FormatArg, d: FormatArg, e: FormatArg, f: FormatArg) -> String ![]
fn format7(template: String, a: FormatArg, b: FormatArg, c: FormatArg, d: FormatArg, e: FormatArg, f: FormatArg, g: FormatArg) -> String ![]
fn format8(template: String, a: FormatArg, b: FormatArg, c: FormatArg, d: FormatArg, e: FormatArg, f: FormatArg, g: FormatArg, h: FormatArg) -> String ![]
fn format_int(template: String, n: Int) -> String ![]
fn format_int64(template: String, n: Int64) -> String ![]
fn format_string(template: String, s: String) -> String ![]
fn format_float(template: String, f: Float) -> String ![]
fn format_bool(template: String, b: Bool) -> String ![]
fn format_char(template: String, c: Char) -> String ![]
```

## std.fs

User-facing wrappers around the `Fs` effect's raw-shape ops. Each fallible op returns `(Int, T)` where tag 0 = success and tag>0

Types:
```
type FsError =
  | NotFound
  | PermissionDenied
  | AlreadyExists
  | NotADirectory
  | IsADirectory
  | InvalidUtf8
  | Other(String)
```

Functions:
```
fn exists(p: String) -> Bool ![Fs]
fn is_file(p: String) -> Bool ![Fs]
fn is_dir(p: String) -> Bool ![Fs]
fn file_size(p: String) -> Result[Int64, FsError] ![Fs]
fn mkdir(p: String) -> Result[Unit, FsError] ![Fs]
fn read_dir(p: String) -> Result[List[String], FsError] ![Fs]
fn remove_dir(p: String) -> Result[Unit, FsError] ![Fs]
fn read_file(p: String) -> Result[String, FsError] ![Fs]
fn remove_file(p: String) -> Result[Unit, FsError] ![Fs]
fn write_file(p: String, data: String) -> Result[Unit, FsError] ![Fs]
```

## std.int

Safe arithmetic helpers for `Int`. Sigil's binary operators (`+`, `-`) silently wrap on overflow (since `Int` is 63-bit signed,

Functions:
```
fn int_max() -> Int ![]
fn int_min() -> Int ![]
fn int_add_safe(a: Int, b: Int) -> Option[Int] ![]
fn int_sub_safe(a: Int, b: Int) -> Option[Int] ![]
fn checked_div(a: Int, b: Int) -> Result[Int, String] ![]
fn checked_mod(a: Int, b: Int) -> Result[Int, String] ![]
```

## std.int64

This file is listed in `compiler/src/imports.rs::BUILTIN_INJECTED` — `import std.int64` is a no-op at the resolver. The `Int64`

## std.io

injection (vs. full stdlib loading)` in PLAN_B_DEVIATIONS.md, the `effect IO { ... }` declaration is constructed in code by

## std.json

JSON value type, pretty-printer, and parser. Promoted from `examples/json.sigil` (which stays in place as a demo) — the

Types:
```
type JValue =
  | JNull
  | JBool(Bool)
  | JInt(Int)
  | JFloat(Float)
  | JString(String)
  | JArray(JList)
  | JObject(JObject)

type JList =
  | JLNil
  | JLCons(JValue, JList)

type JObject =
  | JONil
  | JOCons(String, JValue, JObject)
```

Functions:
```
fn json_render(v: JValue) -> String ![Mem]
fn json_parse(b: ByteArray) -> Result[JValue, String] ![]
```

## std.list

`List[A]` is Sigil's canonical linked sequence: `Nil` for empty, `Cons(head, tail)` for a non-empty list. Sigil has no `for`/`while`

Types:
```
type List[A] = | Nil | Cons(A, List[A])
```

Functions:
```
fn length[A](xs: List[A]) -> Int ![]
fn map[A, B](xs: List[A], f: (A) -> B ![]) -> List[B] ![]
fn filter[A](xs: List[A], pred: (A) -> Bool ![]) -> List[A] ![]
fn fold[A, B](xs: List[A], init: B, f: (B, A) -> B ![]) -> B ![]
fn reverse[A](xs: List[A]) -> List[A] ![]
fn append[A](xs: List[A], ys: List[A]) -> List[A] ![]
fn range(start: Int, end: Int) -> List[Int] ![]
fn list_sort[T](xs: List[T], cmp: (T, T) -> Ordering ![]) -> List[T] ![]
fn list_sort_int(xs: List[Int]) -> List[Int] ![]
fn list_sort_string(xs: List[String]) -> List[String] ![]
fn list_sort_char(xs: List[Char]) -> List[Char] ![]
fn list_sort_float(xs: List[Float]) -> List[Float] ![]
```

## std.map

`Map[K, V]` is Sigil's persistent (immutable) ordered key-value map. Backed by an AA tree (Andersson 1993, "Balanced Search

Types:
```
type MapNode[K, V] =
  | MapLeaf
  | MapBranch { level: Int, key: K, value: V, left: MapNode[K, V], right: MapNode[K, V] }

type Map[K, V] = { cmp: (K, K) -> Ordering ![], root: MapNode[K, V], size: Int }
```

Functions:
```
fn map_empty[K, V](cmp: (K, K) -> Ordering ![]) -> Map[K, V] ![]
fn map_size[K, V](m: Map[K, V]) -> Int ![]
fn map_is_empty[K, V](m: Map[K, V]) -> Bool ![]
fn map_get[K, V](m: Map[K, V], k: K) -> Option[V] ![]
fn map_contains[K, V](m: Map[K, V], k: K) -> Bool ![]
fn map_insert[K, V](m: Map[K, V], k: K, v: V) -> Map[K, V] ![]
fn map_remove[K, V](m: Map[K, V], k: K) -> Map[K, V] ![]
fn map_to_list[K, V](m: Map[K, V]) -> List[(K, V)] ![]
fn map_keys[K, V](m: Map[K, V]) -> List[K] ![]
fn map_values[K, V](m: Map[K, V]) -> List[V] ![]
fn map_from_list[K, V](xs: List[(K, V)], cmp: (K, K) -> Ordering ![]) -> Map[K, V] ![]
fn map_fold[K, V, B](m: Map[K, V], init: B, f: (B, K, V) -> B ![]) -> B ![]
fn map_map[K, V, W](m: Map[K, V], f: (V) -> W ![]) -> Map[K, W] ![]
fn map_filter[K, V](m: Map[K, V], pred: (K, V) -> Bool ![]) -> Map[K, V] ![]
fn map_int_keys[V]() -> Map[Int, V] ![]
fn map_string_keys[V]() -> Map[String, V] ![]
fn map_char_keys[V]() -> Map[Char, V] ![]
```

## std.mem

**Documentation only.** The `Mem` effect ships as a synthetic builtin in Plan C Task 66 (`[DEVIATION Task 66]` in

## std.mut_array

Functions:
```
fn mut_array_get_opt[A](arr: MutArray[A], i: Int) -> Option[A] ![Mem]
fn mut_array_set_opt[A](arr: MutArray[A], i: Int, val: A) -> Option[Unit] ![Mem]
```

## std.mut_byte_array

Functions:
```
fn mut_byte_array_get_opt(ba: MutByteArray, i: Int) -> Option[Byte] ![Mem]
fn mut_byte_array_set_opt(ba: MutByteArray, i: Int, val: Byte) -> Option[Unit] ![Mem]
```

## std.option

Types:
```
type Option[A] = | None | Some(A)
```

Functions:
```
fn map[A, B](o: Option[A], f: (A) -> B ![]) -> Option[B] ![]
fn and_then[A, B](o: Option[A], f: (A) -> Option[B] ![]) -> Option[B] ![]
fn unwrap_or[A](o: Option[A], default: A) -> A ![]
```

## std.ordering

`Ordering` is a three-way comparison result. Comparators return `Ordering` instead of `Int` (-1 / 0 / +1) so callers

Types:
```
type Ordering = | Less | Equal | Greater
```

Functions:
```
fn int_compare(a: Int, b: Int) -> Ordering ![]
fn string_compare(a: String, b: String) -> Ordering ![]
fn char_compare(a: Char, b: Char) -> Ordering ![]
fn bool_compare(a: Bool, b: Bool) -> Ordering ![]
fn float_compare(a: Float, b: Float) -> Ordering ![]
fn int64_compare(a: Int64, b: Int64) -> Ordering ![]
```

## std.pair

`Pair[A, B]` is informal shorthand for the binary tuple type `(A, B)`. This module provides `fst` and `snd` accessors over

Functions:
```
fn fst[A, B](p: (A, B)) -> A ![]
fn snd[A, B](p: (A, B)) -> B ![]
```

## std.panic

This module is a documentation-only header. Both functions are builtin-injected: their schemes live in

## std.path

Slash-separated, no filesystem access (that is std.fs's job). Every function is pure (![]). Semantics match Python's `posixpath`

Functions:
```
fn path_is_absolute(p: String) -> Bool ![]
fn path_join(a: String, b: String) -> String ![]
fn path_split(p: String) -> (String, String) ![]
fn path_basename(p: String) -> String ![]
fn path_dirname(p: String) -> String ![]
fn path_splitext(p: String) -> (String, String) ![]
fn path_normalize(p: String) -> String ![]
```

## std.process

User-facing wrapper around the `Process` effect's `run` op. The effect-op return is a flat 4-tuple `(error_tag, exit_code, stdout,

Types:
```
type ProcessError =
  | NotFound
  | PermissionDenied
  | Other(String)
```

Functions:
```
fn run(cmd: String, args: Array[String]) -> Result[(Int, String, String), ProcessError] ![Process]
fn run_list(cmd: String, args: List[String]) -> Result[(Int, String, String), ProcessError] ![Process]
```

## std.raise

`Raise[E]` is the canonical exception-style effect: a thunk that might raise an `E`-typed error short-circuits its enclosing

Functions:
```
fn raise[A, E](e: E) -> A ![Raise[E]]
fn catch[A, E](body: () -> A ![Raise[E] | e]) -> Result[A, E] ![| e]
```

## std.random

`Random` is a user-declared effect with a single op, `rand_int() -> Int`. The stdlib ships two handlers:

Functions:
```
fn random_int() -> Int ![Random]
fn run_pseudo_random[A](body: () -> A ![Random]) -> A ![]
fn xorshift_step(n: Int) -> Int ![]
fn run_seeded_random[A](seed: Int64, body: () -> A ![Random]) -> A ![Mem]
```

## std.result

Types:
```
type Result[A, E] = | Ok(A) | Err(E)
```

Functions:
```
fn map[A, B, E](r: Result[A, E], f: (A) -> B ![]) -> Result[B, E] ![]
fn map_err[A, E, F](r: Result[A, E], f: (E) -> F ![]) -> Result[A, F] ![]
fn and_then[A, B, E](r: Result[A, E], f: (A) -> Result[B, E] ![]) -> Result[B, E] ![]
```

## std.set

`Set[T]` is a thin pure-Sigil layer over `Map[T, Unit]`. Closes the deduplication / membership-testing gap. Every op

Types:
```
type Set[T] = { underlying: Map[T, Unit] }
```

Functions:
```
fn set_empty[T](cmp: (T, T) -> Ordering ![]) -> Set[T] ![]
fn set_size[T](s: Set[T]) -> Int ![]
fn set_is_empty[T](s: Set[T]) -> Bool ![]
fn set_contains[T](s: Set[T], x: T) -> Bool ![]
fn set_insert[T](s: Set[T], x: T) -> Set[T] ![]
fn set_remove[T](s: Set[T], x: T) -> Set[T] ![]
fn set_to_list[T](s: Set[T]) -> List[T] ![]
fn set_from_list[T](xs: List[T], cmp: (T, T) -> Ordering ![]) -> Set[T] ![]
fn set_fold[T, B](s: Set[T], init: B, f: (B, T) -> B ![]) -> B ![]
fn set_filter[T](s: Set[T], pred: (T) -> Bool ![]) -> Set[T] ![]
fn set_union[T](a: Set[T], b: Set[T]) -> Set[T] ![]
fn set_intersect[T](a: Set[T], b: Set[T]) -> Set[T] ![]
fn set_difference[T](a: Set[T], b: Set[T]) -> Set[T] ![]
fn set_subset[T](a: Set[T], b: Set[T]) -> Bool ![]
fn set_eq[T](a: Set[T], b: Set[T]) -> Bool ![]
fn set_int() -> Set[Int] ![]
fn set_string() -> Set[String] ![]
fn set_char() -> Set[Char] ![]
```

## std.state

`State[S]` is the canonical mutable-state-via-effect surface, parametric over the state type `S`. A computation accesses an

Functions:
```
fn run_state[A, S](initial: S, body: () -> A ![State[S] | e]) -> (A, S) ![| e]
```

## std.string

follow-up: `string_split` / `string_replace`).

Types:
```
type ParseError = | Empty | NonDecimal | Overflow
```

Functions:
```
fn string_to_int(s: String) -> Result[Int, ParseError] ![]
fn string_split(s: String, sep: String) -> List[String] ![]
fn string_replace(s: String, find: String, replace: String) -> String ![]
fn string_byte_at_opt(s: String, i: Int) -> Option[Byte] ![]
fn string_substring_opt(s: String, start: Int, end: Int) -> Option[String] ![]
```

## std.string_builder

Listed in `compiler/src/imports.rs::BUILTIN_INJECTED` — `import std.string_builder` is a no-op at the resolver. The

