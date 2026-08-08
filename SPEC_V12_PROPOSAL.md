# µFlux v12 Proposal: Managed Hidden Stack

## Summary

v12 redefines µFlux as a language based on a **managed hidden stack**. The data stack is still the runtime execution substrate, but it is managed by the compiler and runtime; the programmer never sees or manipulates it directly. All value flow in source code is explicit through:

- named local/global variables,
- literal constants,
- the return value of the immediately preceding op.

Programmers reason about variables and op results, not about stack positions, depth, or ordering. Unbound values are auto-discarded at natural boundaries.

## Motivation

Prior revisions described µFlux as a "stack-based language," which placed the stack at the center of the programming model. In practice this made the language hard to author reliably: every op changes an invisible stack, and operations like `dup`, `ovr`, `swp`, `pick`, and `drop` exist only to repair ordering mistakes. LLM-authored code fails repeatedly on exactly these mechanics.

v12 changes the framing: µFlux is based on a managed hidden stack. The stack is an implementation detail, not a user-facing abstraction. The programmer works with named variables and explicit op results; the compiler maps those onto the hidden stack. This removes the escape hatch that v11 left in place and makes the named-variable style the only way to write code.

## Changes

1. **Retire stack-manipulation opcodes**
   - `dup` (1), `ovr` (2), `drop` (3), `swp` (4), `pick` (5) become compile-time errors.
   - Their indices are retired, following the existing "retired indices never reused" convention.

2. **Assignment and container stores are pass-through**
   - `value x!` stores `value` in local `x` and leaves `value` on top.
   - `value ^x!` stores in global `x` and leaves `value` on top.
   - `h k v set` stores `v` in the container and leaves `v` on top.

3. **Auto-discard of unbound values**
   - Any value still unbound at a natural boundary (end of statement line, before `ret`, end of task body) is silently dropped.
   - There is no explicit `drop` opcode.

4. **Multi-return ops return one tuple/list**
   - `sh`, `try`, `match`, `next`, `scan`, etc. push a single list value instead of multiple separate stack items.
   - Destructure with a positional bind list, using `_!` to discard a slot.
   - Bind arity does not need to match tuple length: excess binds receive `null`; unbound trailing slots are auto-discarded.
   - Explicit `list N get` remains available for partial or dynamic extraction.

5. **User words returning multiple values** must explicitly construct and return a list/tuple.

6. **Universal coercion** replaces strict per-op type checking. Ops coerce their inputs based on context; incompatible values become `NaN` or `"NaN"` and propagate instead of dying, following JavaScript-style permissiveness.

7. **Smart `print`** replaces format-string `print` for the common case. `print` consumes the top-of-stack value and emits a type-aware, recursive, human-readable representation. Formatted output moves to `printf`.

## Execution model

The language is postfix: operands appear before the operation that consumes them. However, the programmer does not manage a stack. Instead, the compiler maintains the hidden stack automatically:

- A literal or variable fetch pushes its value onto the hidden stack.
- An op consumes the required operands from the hidden stack and pushes its result(s).
- `x!` stores the current top-of-stack value into local `x` and leaves it on the hidden stack.
- At natural boundaries, any value still on the hidden stack that has not been bound to a variable is auto-discarded.

The hidden stack is therefore an internal buffer for passing values between consecutive ops, not a user-visible data structure.

## Universal coercion

v12 adopts the coercion rules from `COERCION_IMPROVEMENTS.md` as part of the language. The goal is the same as the managed hidden stack: the LLM should not need to track types. It passes whatever it has, and the op coerces.

### Principles

- Coercion rules are uniform. There are no per-op coercion exceptions.
- Incompatible values coerce to `NaN` (numeric context) or `"NaN"` (string context) and propagate, rather than aborting.
- Truthiness follows JavaScript: `0`, `""`, `null`, `NaN`, and empty collections are falsy; everything else is truthy.

### Numeric context

`add`, `sub`, `mul`, `div`, `rem`, comparisons, and vector ops coerce operands to numbers:

| Input | Result |
|-------|--------|
| int `42` | `42` |
| float `3.14` | `3.14` |
| str `"42"` | `42` |
| str `""` / whitespace | `0` |
| str `"hello"` | `NaN` |
| `null` / `0` handle | `0` |
| bool | `1` or `0` |
| list `[5]` | unwrap single element |
| list `[]` / multi-element / dict / obj | `0` / `NaN` |

### String context

`cat`, `join`, `split`, `fmt`, `print` coerce operands to strings:

| Input | Result |
|-------|--------|
| int | decimal string |
| float | decimal string or `"NaN"` |
| `null` / `0` handle | `"null"` |
| bool | `"true"` / `"false"` |
| list | `"1,2,3"` style join |
| dict / obj | `"[object Object]"` |

### Collection context

- Vector ops expecting `arr` coerce lists, dicts (values), strings (char codes), bitmaps (set-bit indices), and scalars (single-element array).
- Bitmap ops coerce collection elements to bits via truthiness.

### Equality

- `eq` uses loose equality (`==`): `"3" eq 3` → `1`, `null eq 0` → `1`.
- `seq` uses strict equality (`===`): types and values must match.
- `sne` is strict inequality.

### When coercion still dies

Only when the result is genuinely undefined, never on input type:

- Integer truncation of `NaN` or `Infinity` (`div`, `rem`).
- `vmean` / `vmin` / `vmax` on empty collection.
- Length mismatch in elementwise ops (`veadd`, etc.).

## Syntax

Binding remains postfix:

```uf
value x!                     ; store and pass through
5 x! 6 y!                    ; x=5, y=6
"cmd" sh out! err! status!   ; destructuring bind
"cmd" sh out! _! status!     ; bind stdout, discard stderr, bind status
"cmd" sh _! _! status!       ; discard stdout/stderr, bind status
```

The parser matches bindings positionally to the op's return values. `_!` means "discard this slot" (the Thanos-snap bind). If the pattern has fewer binds than the tuple has slots, the trailing unbound values are auto-discarded. If the pattern has more binds than the tuple has slots, the extra binds receive `null`.

## Examples

### Shell command

```uf
"ls -la" sh out! err! status!
out print
```

### Try / error containment

```uf
'risky try r!
r 1 get ok!
ok 'handle if
r 0 get value!
```

### Chained assignment

```uf
data 0 get x! double
x next_op
```

### Container store chain

```uf
dict key value set process
```

### Explicit tuple destructuring

```uf
"cmd" sh result!
result 0 get out!
result 1 get err!
result 2 get status!
out print
```

## Chaining and error resolution

v12 removes manual stack manipulation, but the hidden stack still enforces operand availability. The following table shows how common chaining patterns resolve:

| Code | Resolution |
|------|-----------|
| `op1 op2 op3` (extra values produced) | Unbound values auto-discarded at the boundary. No error. |
| `op1 op2` (op2 needs more than op1 gives) | **Underflow error** — v12 removes shuffling, not logic errors. |
| `5 x! op` | `x!` leaves `5`; `op` consumes it if its input arity is 1. |
| `5 x! 6 y!` | `x=5`, `y=6`; leftover `[5, 6]` auto-discarded at boundary. |
| `5 x! y!` | `x=5`, `y=5` — pass-through gotcha. Both get the same value. |
| `"cmd" sh out! status! process` | `err` auto-discarded; `process` underflows because destructuring leaves empty stack. |
| `"cmd" sh out! err! status! code!` | `code = null`; stack empty. |
| `"cmd" sh out! _! status!` | Normal destructuring. `_!` discards `err`. |
| `"hello" 5 +` | `"hello"` coerces to `NaN`; result is `NaN`. No die. |
| `"5" 3 +` | `"5"` coerces to `5`; result is `8`. |
| `data len` | Length produced, nothing binds it, auto-discarded. |
| `side_effect next_op` | `next_op` underflows if it needs input and `side_effect` produced none. |

### Takeaways

- Auto-discard prevents stack pollution from unbound values.
- Underflow errors still occur when an op does not receive enough inputs.
- Pass-through assignment is the main semantic shift from v11.
- Destructuring fully consumes the tuple; bound values must be fetched by name.

## Smart printing

v12 redefines `print` as the default observation op. The goal is that an LLM can append `print` to any expression and immediately understand the resulting data.

### Behavior

- `print` consumes the top-of-stack value.
- It emits a type-aware, recursive representation.
- Formatted output remains available under `printf` (the old `print` behavior).

### Output format

| Input | Output |
|-------|--------|
| int `42` | `42` |
| float `3.14` | `3.14` |
| float `NaN` | `NaN` |
| bool `1` | `true` |
| bool `0` | `false` |
| null / `0` handle | `null` |
| str `hello` | `"hello"` |
| list `[1 2 3]` | `[1, 2, 3]` |
| dict `{a:1, b:2}` | `{a: 1, b: 2}` |
| arr of int | `[1, 2, 3]` |
| arr of float | `[1.0, 2.0, 3.0]` |
| tensor | `[[...], [...]]` |
| obj / struct | `{field1: val1, field2: val2}` |
| chan | `<chan>` |
| atom | `<atom: 7>` |
| bitmap | `<bitmap: 10110>` |

### Nested collections

`print` recurses into nested structures with sensible defaults:

```uf
[1 [2 3] 4] print                 ; [1, [2, 2], 4]
{a: [1 2], b: {c: 3}} print       ; {a: [1, 2], b: {c: 3}}
```

### Safety limits

To prevent runaway output:

- Maximum recursion depth (e.g., 8).
- Maximum elements printed per collection (e.g., 100), with `<... N more>` for overflow.
- Circular references rendered as `<cycle>`.

### Examples

```uf
42 print                          ; 42
"hello" print                     ; "hello"
[1 2 3] print                     ; [1, 2, 3]
{a: 1, b: [2 3]} print            ; {a: 1, b: [2, 3]}

; formatted output still available
"score: %d" 42 printf
```

### Stack effect

`print` consumes one value and returns nothing. To inspect a value without losing it, bind it first:

```uf
data d!
d@ print
; d still available
```

## Runtime and codegen impact

- Remove the five opcodes from `comp/src/lex.rs` opcode tables.
- The data stack still exists as the hidden evaluation mechanism; the threaded interpreter and basic-block codegen remain largely intact.
- `SETV` / local-store codegen and the container `set` opcode must emit the stored value back to the hidden stack.
- Auto-discard requires the compiler to track unbound hidden-stack values at boundaries and emit the equivalent of a drop in generated code.
- Multi-return ops change from pushing N values to pushing one list value.

## Backward compatibility

v12 is not backward compatible with v11:

- The five retired opcodes are compile-time errors.
- Assignment no longer consumes TOS; v11 code that relied on `x!` to clear the stack changes meaning.
- Multi-return ops change their stack effect.
- User words that returned multiple implicit stack values must be rewritten to return an explicit list/tuple.
- Type-strict op behavior is replaced by universal coercion; code that relied on ops dying on type mismatch will now receive `NaN` or `"NaN"` instead.
- `print` becomes the smart observation op; old format-string `print` moves to `printf`.

## Open questions

1. **Auto-discard boundaries**: exact rules for where unbound values are discarded (newlines, `ret`, task body ends, weave boundaries).
2. **Tuple representation**: reuse `list`, introduce a dedicated tuple type, or use `dict` with positional keys?
3. **Container `set` arity**: should `set` always return `v`, or only in named-operand contexts?
4. **Direct single-slot extraction shorthand**: keep only `list N get`, or add `.N` / `#N` sugar for extracting one slot from the preceding tuple?
