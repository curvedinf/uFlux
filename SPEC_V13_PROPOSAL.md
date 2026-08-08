# µFlux v13 Proposal: Label Parameters

## Summary

v13 introduces **label parameters**: labels can declare input bindings in their
definition, and callers pass arguments explicitly. This turns labels into
user-defined ops while preserving µFlux's postfix, stack-backed execution
model.

A label definition of the form:

```uf
add2: a! b!
  ret a@ b@ add
```

declares that `add2` consumes two caller cells, binds them to label-locals `a`
and `b`, and begins execution. A caller writes:

```uf
3 4 _call add2
```

This is symmetric with built-in ops: arguments are pushed first, then the
operation is invoked.

## Motivation

v12 removed raw stack manipulation (`dup`, `drop`, `swp`, `ovr`, `pick`) and
replaced it with named variables and pass-through assignment. The remaining
awkwardness is in passing values into callback labels used by structured
control flow and user subroutines:

```uf
5 'body for
body:
  i!                 ; manually bind the loop index
  i@ print
  0 ret
```

The `i!` inside the body is a workaround: the value was placed on the stack by
`for`, and the body has to know this and assign it. Label parameters make the
contract explicit:

```uf
5 'body for
body: i!
  i@ print
  ret 0
```

The same mechanism unifies `_call` subroutines and structured-control
callbacks under one calling convention.

## Syntax

A label definition consists of a label name, a colon, zero or more parameter
bindings, and the body:

```uf
label: <param-bindings>* <body>
```

Parameter bindings are the same tokens used for pass-through assignment:

- `name!` — bind one cell to local `name`.
- `^name!` — bind one cell to global `name`.
- `_!` — bind and immediately discard one cell.

The first parameter binding receives the first cell pushed by the caller, the
second binding receives the second cell, and so on. Operationally the runtime
pops the caller's stack from right to left so that textual order matches push
order. Bindings must appear contiguously at the start of the label definition;
the first non-binding token ends the parameter list.

Examples:

```uf
; no parameters (v12 behavior)
foo:
  "hi" print ret 0

; one local parameter
print_n: n!
  n@ "%d" format print ret 0

; two local parameters
add2: a! b!
  ret a@ b@ add

; bind and discard an unwanted parameter
ignore_second: first! _!
  first@ print ret 0

; bind to a global (useful for cross-callback shared state)
accumulate: ^sum! delta!
  ^sum@ delta@ add ^sum!
  ret 0
```

## Calling convention

Label parameters are **input-only** and **pop-by-position**. The label declares
how many cells it expects; the caller pushes that many arguments before the
call.

Calling forms:

```uf
3 4 _call add2          ; explicit call
5 'body for             ; structured op passes loop index implicitly
cond 'then 'else if_else ; structured op passes condition result implicitly
```

For `_call`, the compiler checks the declared arity of the target label and
generates the transfer at the call site. For structured ops (`if`, `if_else`,
`while`, `for`), the op implementation is responsible for pushing the values
that the target label declares.

### Arity rules

The binding is **best-effort** at runtime:

- **Too few arguments**: missing bindings receive `null`.
- **Too many arguments**: excess cells are silently discarded after the bindings
  are satisfied.
- **No declared parameters**: all caller cells are discarded on entry (the body
  starts with an empty local stack).

This matches the v12 auto-discard philosophy: unbound values do not abort;
they evaporate at natural boundaries.

Example:

```uf
; only one arg provided, b becomes null
7 _call add2
add2: a! b!
  ret a@ b@ add        ; returns 7 + null -> NaN (per universal coercion)
```

### Stack draining

When a label, task, or callback body returns, its **entire data stack is drained**
except for the single cell named in `ret`. The caller's stack pointer is restored
to its value before the call, and the return value is pushed on top of it.

```uf
foo:
  42           ; pushed but never named in ret
  ret 0        ; caller sees only 0; the 42 is discarded

0 _call foo
add            ; 0 + 0 -> 0, not 0 + 42
```

This makes every call boundary self-contained: callees cannot leak transient
values into the caller's stack. It applies to `_call`, structured-op callbacks,
`weave` tasks, `spawn` bodies, and `try`/`retry` bodies.

## Structured control flow

Label parameters apply directly to callbacks.

### `for`

```uf
5 'body for
body: i!
  i@ print
  ret 0
```

`for` pushes the loop index; `body: i!` binds it.

### `while`

```uf
'cond 'body while
cond:
  ret ^n@ 0 gt
body: i!              ; if while passed an iteration value (it doesn't by default)
  ...
  ret 0
```

The standard `while` op does not pass a value to either callback, so `cond:`
and `body:` typically declare no parameters. A future `rangefold`-style op
would pass the accumulator and index explicitly.

### `if` / `if_else`

```uf
x 0 gt 'pos 'zero if_else
pos: n!
  n@ print ret 0
zero:
  "zero" print ret 0
```

`if_else` consumes the condition and calls one branch. The condition result is
consumed by the op itself; branches declare parameters only if the caller
explicitly pushes additional cells before the `if`/`if_else`:

```uf
42 x 0 gt 'pos 'zero if_else
pos: n! val!
  ...
```

Here `42` and `x` are the arguments passed to whichever branch is taken.

## Return values

A label always returns **exactly one cell**. The `ret` instruction is explicit:
it takes one or more operands and returns the single value they describe.

- `ret expr` — return `expr`.
- `ret a b c` — combine `a`, `b`, and `c` into a list and return that list.
- Bare `ret` — return `null`.

Examples:

```uf
add2: a! b!
  ret a@ b@ add

run_cmd: c!
  c@ shell out! err! code!
  ret out@ err@ code@
```

The caller destructures a multi-value return as usual:

```uf
"cmd" _call run_cmd out! err! code!
```

Because `ret` is explicit, there is no ambiguity about what a label leaves on
the stack. The body may manipulate the stack freely; only the values named in
`ret` are returned.

## Container literals

v13 adds compile-time literals for lists, dicts, and typed arrays/tensors.

### Syntax

```uf
[1 2 3] l!                          ; list
{ "a" 1 "b" 2 } d!                  ; dict
[0 1 2] int array nums!             ; int array
[1.0 2.0 3.0] float tensor t!       ; float tensor
```

**List literal**: `[ expr expr ... ]` evaluates each expression and builds a
list containing the values in the given order.

**Dict literal**: `{ key val key val ... }` builds a dict from alternating keys
and values. The element count must be even; an odd count is a compile error.

**Array/tensor literal**: `[ expr ... ] type array` / `[ expr ... ] type tensor`
creates a typed array or tensor from the list literal.

### Examples

```uf
[] empty_list!                      ; empty list
{} empty_dict!                      ; empty dict
[] int array empty_ints!            ; empty int array

[x@ y@ z@] triplet!                 ; elements may be arbitrary expressions
[[1 2] [3 4]] rows!                 ; nested list literal
{ "name" "bob" "scores" [85 92 78] "ok" 1 } rec!
```

### Array/tensor conversion

`array` and `tensor` are polymorphic in v13:

- `len type array` — v12 behavior: allocate an empty typed array of `len` elements.
- `list type array` — v13 behavior: allocate a typed array and copy the elements
  of `list` into it.

The same applies to `tensor`.

### Interaction with the hidden stack

Inside a literal, each element expression is evaluated normally on the hidden
stack. The closing bracket or brace consumes the resulting cells and leaves one
container handle. This preserves µFlux's stack-backed execution while removing
the need for index-by-index initialization.

Each element expression must net exactly one cell. Multi-value returns inside a
literal are not automatically destructured; use a destructuring bind on the
previous line if needed.

### Dense encoding

The text tokens `[`, `]`, `{`, and `}` require four new glyphs in dense mode.
They should be assigned to unused codepoints with the same single-token-per-glyph
constraint as the rest of the dense encoding.

## Compiler behavior

1. **Parse**: when a label definition is encountered, consume the leading run of
   `name!` / `^name!` / `_!` tokens as parameter bindings. Record the label's
   input arity.

2. **Return checking**: every code path through a label body must end with a
   `ret`. Bare `ret` is allowed and returns `null`. The compiler reports an
   error if a path falls off the end of a label.

3. **Multi-value returns**: when `ret` names more than one value, the compiler
   emits code to build a fresh list containing those values in the order given
   and return the list handle.

4. **Codegen for `_call`**: emit code that saves the caller's data-stack
   pointer, pops the declared number of cells from the caller stack and stores
   them into the label's local/global slots before jumping to the body. Missing
   cells are pushed as `null`; excess cells are discarded. On `ret`, restore the
   caller's saved stack pointer and push the single return value.

5. **Codegen for structured ops**: each structured op saves its own stack pointer
   before calling a body, and restores it after the body returns, leaving only
   the body's single return value. The compiler verifies that the callback's
   declared arity matches what the op provides, or applies the best-effort rule.

6. **Scope**: parameters are scoped to the label body, just like locals are
   scoped to a `call` frame. A label body is a fresh frame.

7. **Container literals**: the lexer recognizes `[`, `]`, `{`, and `}` as
   bracket tokens. The parser builds `ListLiteral` and `DictLiteral` AST nodes,
   recursively parsing element expressions until the matching closing token.
   Codegen emits each element expression, then emits a list/dict builder with
   the element count. For arrays and tensors, codegen emits the list builder
   followed by the existing `array` / `tensor` op, which accepts either a length
   (v12 behavior) or a list (v13 behavior) as its top operand.

8. **Weave**: inside a `weave` block, `task name:` introduces a task whose body
   uses the same syntax as a label definition. Input task results are bound by
   parameter tokens after the colon. The parser records task dependencies from
   these bindings; it rejects cycles and unknown inputs. `endt` is removed. If
   `run` names a task, the compiler walks the DAG backward from that terminal
   task; tasks not reachable become orphans. `spawn` target labels are parsed as
   ordinary labels and are subject to the same mandatory-`ret` rule.

## Weave

v13's `weave` is the single construct for task graphs, servers, and composable
multithreaded processes. Task bodies reuse label syntax and semantics.

### Task bodies

A `weave` task body is written as a label definition using `ret` instead of the
v12 `endt` keyword. Task inputs are parameter bindings, just like label
parameters. A task is active from its `name:` through its matching `ret`.

```uf
weave
  task a:
    ret 1
  task b: a!
    ret a@ 2 add
run
```

An input task name bound with `name!` makes that task's result available as a
parameter inside the body. Zero or more input bindings may appear after the
colon; an optional fanout count precedes `task` for parallel workers.

```uf
weave
  task base:
    ret 0
  task step: base!
    ret base@ 1 add
  data 4 task worker: item!    ; 4 workers fan out over `data`
    ret item@ process
run
```

The first input binding drives fanout when a count is present; additional
inputs are broadcast to every worker. Bindings are evaluated left-to-right,
matching the v13 label-parameter convention.

The body must end with `ret`, just like a label. Bare `ret` returns `null`.
Falling off the end of a task body is a compile error. The value named in `ret`
becomes the task result; with fanout, each worker's `ret` value becomes one
element of the published result list.

`spawn` bodies are already label-shaped. In v13 a spawn target label must also
end with an explicit `ret`. Bare `ret` returns `null`. Falling off the end is a
compile error. The value named in `ret` is enqueued on the returned channel.

```uf
'worker spawn
worker: n!
  ret n@ 2 mul
```

This removes the v12 "top-of-stack at body end is the result" convention for
both `weave` tasks and `spawn` bodies. Each task and spawn body drains its data
stack on `ret`, leaving only the explicit return value, just like a label call.

### Terminal task and orphan tasks

`run` may name a terminal task. The compiler builds the DAG backward from the
terminal; only reachable tasks execute. Tasks not reachable from the terminal
are **orphans** — they exist in scope, can be referenced by `'name` and called
by address, but do not auto-run.

```uf
weave
  task data:
    ret [1 2 3]
  task process: data!
    ret data@ 2 mul
  task summary: data! process!
    ret data@ process@ add
run summary              ; data -> process -> summary
```

The terminal task's result is left on the stack after `run`.

If `run` is used without a name, all tasks execute (v12 compatibility).

Orphan tasks enable server-style dispatchers where some tasks are only invoked
at runtime by address:

```uf
weave
  task setup:
    ret listener_fd
  task handle: setup!
    'routes _call dispatch
    ret null
  task routes:             ; orphan — not reachable from setup
    ret { "/" 'index }
  task index:              ; orphan — reachable only by address
    ret "hello"
run setup
```

### Graceful shutdown

A bare `shutdown` inside any task body signals the enclosing `weave` to drain
and exit. `shutdown` is an op with stack effect `→` (it does not return a
value). After `shutdown`, execution continues until the task's `ret`, then the
weave joins all running tasks and `run` returns.

```uf
weave
  task setup:
    ret 0
  task tick: setup!
    counter@ 1 add counter!
    counter@ 100 gte 'shut if
    ret null
shut:
  shutdown
  ret null
run setup
```

In server weaves, `shutdown` is typically called from a request handler when a
shutdown path is hit.

### Stdlib weaves and mixins (future)

The design goal is to ship reusable `http`, `timer`, and `fswatch` weaves as
pure µFlux code, plus cross-cutting mixin tasks (auth, rate limiting, logging,
etc.) composed through the existing callback-handle pattern (`next@ _call`).
These are not part of the v13 language spec itself; they will be layered on top
once the core weave semantics are stable. Inheritable weaves are deferred to a
future revision.

## Text opcode naming

v13 renames text-mode opcodes to full English words or `snake_case` phrases.
Acronyms and very common short forms are kept when they are immediately
recognizable to a general programmer. Where another mainstream language has an
identical operation, the name matches that language.

Immediate-operand opcodes retain the `_` prefix (e.g. `_array` was `_arr`).
Directives (`import`, `export`, `use`, `mod`, `pub`, `struct`, `macro`) and
type keywords (`int`, `float`, `ptr`, `byte`, `str`) are unchanged.

### No inline math symbols

v12 allowed `+`, `-`, `*`, and `&` as inline operators for `add`, `sub`, `mul`,
and `and`. v13 removes these symbol operators. `-` is now **only** part of a
number literal (e.g. `-5`). All arithmetic and bitwise ops are written as
3-letter words:

```uf
5 3 add          ; was 5 3 + or 5 3 add
5 3 sub          ; was 5 3 - or 5 3 sub
5 3 mul          ; was 5 3 * or 5 3 mul
2 3 pow          ; 8
9 sqrt           ; 3
x y and          ; was x y & or x y and
```

This removes the `-` sign/SUB disambiguation rule and the lookback state it
required in the lexer.

### Arithmetic and comparison

| old      | new                | notes                              |
|----------|--------------------|------------------------------------|
| `add`    | `add`              | 3-letter form kept                 |
| `sub`    | `sub`              | 3-letter form kept                 |
| `mul`    | `mul`              | 3-letter form kept                 |
| `div`    | `div`              | 3-letter form kept                 |
| `rem`    | `rem`              | 3-letter form kept                 |
| `pow`    | `pow`              | new in v13; C/Python `pow`         |
| `sqrt`   | `sqrt`             | new in v13; C/Python/Rust          |
| `inc`    | `inc`              | 3-letter form kept                 |
| `dec`    | `dec`              | 3-letter form kept                 |
| `neg`    | `neg`              | 3-letter form kept                 |
| `eq`     | `eq`               | 3-letter form kept                 |
| `lt`     | `lt`               | 3-letter form kept                 |
| `lte`    | `lte`              | new in v13                         |
| `gt`     | `gt`               | 3-letter form kept                 |
| `gte`    | `gte`              | new in v13                         |
| `not`    | `not`              | 3-letter form kept                 |
| `or`     | `or`               | 3-letter form kept                 |
| `xor`    | `xor`              | 3-letter form kept                 |
| `shl`    | `shl`              | 3-letter form kept                 |
| `shr`    | `shr`              | 3-letter form kept                 |
| `bnot`   | `bnot`             | 3-letter form kept                 |
| `orelse` | `orelse`           | 3-letter form kept                 |

### Containers and sequences

| old          | new                | notes                              |
|--------------|--------------------|------------------------------------|
| `get`        | `get`              | common short form kept             |
| `set`        | `set`              | common short form kept             |
| `getq`       | `get_or_zero`      | non-throwing get                   |
| `has`        | `contains`         | Python `in` / JS `includes`        |
| `keys`       | `keys`             | Python/JS precedent                |
| `len`        | `length`           | Python uses `len`; full word here  |
| `arr`/`_arr` | `array`/`_array`   |                                    |
| `tensor`     | `tensor`           | standard ML term kept              |
| `list`       | `list`             |                                    |
| `dict`       | `dict`             | Python precedent                   |
| `push`       | `append`           | Python list.append                 |
| `pop`        | `pop`              | common short form kept             |
| `cat`        | `concat`           | Python/JS/Rust precedent           |
| `clone`      | `copy`             | Python `copy.copy`                 |
| `cast`/`_cast` | `cast`/`_cast`   | common short form kept             |
| `del`        | `remove`           |                                    |
| `range`      | `range`            | Python precedent                   |
| `sort`       | `sort`             | Python/JS precedent                |
| `filter`     | `filter`           | Python/JS precedent                |
| `some`       | `any`              | Python `any`                       |
| `every`      | `all`              | Python `all`                       |
| `group`      | `group_by`         | SQL/Rust itertools                 |
| `agg`        | `aggregate`        |                                    |
| `unique`     | `unique`           | numpy/pandas precedent             |
| `flat`       | `flatten`          |                                    |
| `chunk`      | `chunk`            | Rust itertools precedent           |
| `collect`    | `collect`          | Rust precedent                     |
| `addto`      | `add_to`           |                                    |
| `faddto`     | `field_add_to`     |                                    |
| `finc`       | `field_inc`        |                                    |
| `sortkeys`   | `sort_keys`        |                                    |
| `topn`       | `top_n`            |                                    |

### Strings

| old        | new                | notes                              |
|------------|--------------------|------------------------------------|
| `match`    | `regex_match`      |                                    |
| `replace`  | `regex_replace`    |                                    |
| `rsplit`   | `regex_split`      |                                    |
| `glob`     | `glob_match`       |                                    |
| `split`    | `split`            | Python/JS precedent                |
| `join`     | `join`             | Python/JS precedent                |
| `slice`    | `slice`            | Python/Go precedent                |
| `find`     | `find`             | Python/JS precedent                |
| `repl`     | `replace_all`      | literal replace                    |
| `trim`     | `trim`             | Python/JS precedent                |
| `up`       | `uppercase`        |                                    |
| `down`     | `lowercase`        |                                    |
| `starts`   | `starts_with`      | Rust precedent                     |
| `ends`     | `ends_with`        | Rust precedent                     |
| `atoi`     | `parse_int`        |                                    |
| `atof`     | `parse_float`      |                                    |
| `itoa`     | `format_int`       |                                    |
| `ftoa`     | `format_float`     |                                    |

### I/O and processes

| old        | new                | notes                              |
|------------|--------------------|------------------------------------|
| `print`    | `print`            |                                    |
| `fmt`      | `format`           | Python `.format`                   |
| `scan`     | `scan`             | C `scanf` precedent                |
| `slurp`    | `read_file`        |                                    |
| `spit`     | `write_file`       |                                    |
| `sh`       | `shell`            |                                    |
| `shp`      | `shell_stream`     | stream stdout lines                |
| `exec`     | `execute`          |                                    |
| `argv`     | `argv`             | C precedent                        |
| `hasargs`  | `has_args`         |                                    |
| `argi`     | `arg_index`        |                                    |

### Arrays and numeric collections

| old              | new                      | notes                        |
|------------------|--------------------------|------------------------------|
| `vadd`           | `scalar_add`             | arr + scalar                 |
| `vsub`           | `scalar_sub`             |                              |
| `vmul`           | `scalar_mul`             |                              |
| `vdiv`           | `scalar_div`             |                              |
| `veadd`          | `array_add`              | elementwise                  |
| `vesub`          | `array_sub`              |                              |
| `vemul`          | `array_mul`              |                              |
| `vediv`          | `array_div`              |                              |
| `vemax`          | `array_max`              | elementwise                  |
| `vemin`          | `array_min`              | elementwise                  |
| `veq`            | `scalar_eq`              | arr == scalar → bitmap       |
| `vlt`            | `scalar_lt`              |                              |
| `vgt`            | `scalar_gt`              |                              |
| `vge`            | `scalar_gte`             |                              |
| `vle`            | `scalar_lte`             |                              |
| `vsum`           | `sum`                    | reduction                    |
| `vmean`          | `mean`                   | reduction                    |
| `vmin`           | `min`                    | reduction                    |
| `vmax`           | `max`                    | reduction                    |
| `vmap`           | `array_map`              |                              |
| `vfold`          | `array_reduce`           |                              |
| `vargsort`       | `array_argsort`          |                              |
| `vsearchsorted`  | `array_search_sorted`    |                              |
| `vwhere`         | `array_where`            |                              |
| `vgather`        | `array_gather`           | arr + bitmap → arr'          |
| `vget`           | `array_get`              | direct typed array read      |
| `vset`           | `array_set`              | direct typed array write     |
| `rangefold`      | `range_reduce`           |                              |

### Bitmaps

| old        | new                | notes                              |
|------------|--------------------|------------------------------------|
| `vand`     | `bitmap_and`       |                                    |
| `vor`      | `bitmap_or`        |                                    |
| `vnot`     | `bitmap_not`       |                                    |
| `vcount`   | `bitmap_count`     | popcount                           |

### Concurrency

| old        | new                | notes                              |
|------------|--------------------|------------------------------------|
| `spawn`    | `spawn`            |                                    |
| `chan`     | `channel`          |                                    |
| `enq`      | `enqueue`          |                                    |
| `deq`      | `dequeue`          |                                    |
| `close`    | `close`            |                                    |
| `atom`     | `atomic`           |                                  |
| `aget`     | `atomic_get`       |                                    |
| `aset`     | `atomic_set`       |                                    |
| `aadd`     | `atomic_add`       |                                    |
| `cas`      | `cas`              | standard acronym kept              |
| `try`      | `try`              |                                    |
| `retry`    | `retry`            |                                    |

### Time

| old        | new                | notes                              |
|------------|--------------------|------------------------------------|
| `now`      | `now`              |                                    |
| `time`     | `parse_time`       |                                  |
| `timef`    | `format_time`      |                                    |

### JSON

| old        | new                | notes                              |
|------------|--------------------|------------------------------------|
| `json`     | `parse_json`       |                                    |
| `unjson`   | `to_json`          |                                  |

### Iterators

| old        | new                | notes                              |
|------------|--------------------|------------------------------------|
| `iter`     | `iter`             |                                    |
| `next`     | `next`             | Python/JS precedent                |
| `imap`     | `iter_map`         |                                  |
| `ifilter`  | `iter_filter`      |                                    |

### File streaming

| old        | new                | notes                              |
|------------|--------------------|------------------------------------|
| `feach`    | `file_each_line`   |                                    |
| `ffold`    | `file_fold_lines`  |                                    |
| `fsplit`   | `file_split_lines` |                                    |
| `fmatch`   | `file_match_lines` |                                  |
| `femit`    | `file_emit`        | stream iterable to file            |

### Field accessors

These operate on fields produced by `file_split_lines` and similar ops:

| old        | new                | notes                              |
|------------|--------------------|------------------------------------|
| `fget`     | `field_get`        |                                  |
| `fatoi`    | `field_int`        |                                  |
| `fatof`    | `field_float`      |                                    |
| `fsget`    | `field_slice`      |                                    |
| `fbyte`    | `field_byte`       |                                    |

### Graph traversal

| old        | new                | notes                              |
|------------|--------------------|------------------------------------|
| `bfs`      | `bfs`              | standard acronym kept              |
| `dfs`      | `dfs`              | standard acronym kept              |
| `wfind`    | `find_first`       | BFS early exit                     |

### Bloom filters

| old        | new                | notes                              |
|------------|--------------------|------------------------------------|
| `bloom`    | `bloom`            | standard term kept                 |
| `badd`     | `bloom_add`        |                                    |
| `btest`    | `bloom_test`       |                                    |

### Memory and FFI

| old          | new                | notes                            |
|--------------|--------------------|----------------------------------|
| `buf`        | `buffer`           |                                  |
| `bufcopy`    | `copy_memory`      |                                  |
| `loadx`      | `load`             | raw memory read                  |
| `storex`     | `store`            | raw memory write                 |
| `malloc`     | `malloc`           | C precedent                      |
| `free`       | `free`             | C precedent                      |
| `mmap`       | `mmap`             | POSIX precedent                  |
| `sys`/`_sys` | `syscall`/`_syscall` |                              |
| `sizeof`/`_sizeof` | `size_of`/`_size_of` | Rust precedent         |
| `offset`/`_offset` | `offset`/`_offset` | keep short form      |

### Type and control

| old        | new                | notes                              |
|------------|--------------------|------------------------------------|
| `typeof`   | `type_of`          | Rust `std::any::type_name` style   |
| `if`       | `if`               |                                    |
| `ifelse`   | `if_else`          |                                  |
| `while`    | `while`            |                                    |
| `for`      | `for`              |                                    |
| `break`    | `break`            |                                    |
| `cont`     | `continue`         | full word                          |
| `ret`      | `ret`              | common short form kept             |
| `gc`       | `gc`               | standard acronym kept              |

### Weave (v13)

| old        | new                | notes                              |
|------------|--------------------|------------------------------------|
| `weave`    | `weave`            |                                    |
| `task`     | `task`             |                                    |
| `run`     | `run`              | run the weave DAG                  |
| `end`      | `shutdown`         | graceful weave shutdown            |

### Equality helpers

| old        | new                | notes                              |
|------------|--------------------|------------------------------------|
| `seq`      | `structural_equal` |                                  |
| `sne`      | `structural_not_equal` |                              |

## Migration from v12

v12 labels implicitly have arity 0. A v12 program:

```uf
5 'body for
body:
  i!
  i@ print
  0 ret
```

becomes the more explicit v13:

```uf
5 'body for
body: i!
  i@ print
  ret 0
```

Most v12 code continues to work because labels with no parameter bindings are
arity 0 and discard any caller cells on entry — preserving the v12 behavior
where the body manually binds values from the stack.

v12 `weave` task bodies using `endt`:

```uf
weave
  task a 1 endt
  a task b 2 add endt
run
```

become v13 label-shaped bodies with explicit `ret` and parameter bindings:

```uf
weave
  task a:
    ret 1
  task b: a!
    ret a@ 2 add
run
```

## Variadic input

v13 does not provide rest parameters. A label that needs a variable number of
inputs should receive them as a single list or array argument, constructed with
the existing container ops:

```uf
[0 1 2] int array nums!
nums@ _call sum print

sum: arr!
  ret arr@ 0 'add array_reduce

add: a! b!
  ret a@ b@ add
```

The caller is responsible for packing the values; the callee declares exactly
one parameter and operates on the collection.

## Recursion

With label parameters, `_call` supports direct and indirect recursion. A label
calls itself or another label the same way it calls any other label:

```uf
10 _call fact print

fact: n!
  n@ 1 lte 'base 'rec if_else
base:
  ret 1
rec:
  n@ n@ 1 sub _call fact mul
  ret
```

Recursion uses the same call stack as structured control flow.

### Stack and cell capacity as errors

Overflowing any fixed-size execution resource is a **defined runtime error** in
v13. Implementations must detect the overflow and report it rather than invoke
undefined behavior. The two resources are:

- **Call stack** (`cs`): one entry is pushed for every active `_call`,
  structured-op callback invocation, `try`/`retry` body, or `spawn`/`weave`
  entry. Exceeding its capacity is a recoverable error (reported by `die`,
  caught by the nearest `try`/`retry` if present).

- **Data stack / cell capacity** (`ds`): one cell is pushed for every value
  currently live on the hidden stack. Exceeding its capacity is likewise a
  defined error.

A conforming v13 implementation must document the capacity of each stack. It may
use fixed sizes, command-line flags, environment variables, or dynamic growth,
provided that exhaustion is always reported as an error and never silently
overflows a buffer.

Tail-call optimization is **not required** by v13, but it is a valid and
encouraged implementation optimization.

## Open questions

None. v13 is ready for implementation.
