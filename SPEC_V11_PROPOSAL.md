# µFlux v11 Proposal: Implicit Local Variables and Universal Postfix

This proposal describes a major simplification of the µFlux surface language:

1. **Implicit local variables** — variables are local to the current subroutine call by default, with no explicit scope declarations.
2. **Universal postfix rule** — every operation follows the same pattern: operands first, operation second. There are no infix operators and no special function-call syntax.

Both features compile to the existing v10 instruction set and runtime.

---

## 1. Implicit Local Variables

### Motivation

The v10 stack model is fast and uniform, but it forces the author to reason about stack positions explicitly. For LLM-authored scripts this is error-prone. The goal is to keep the small core while allowing ordinary named variables in the common case.

### Syntax

No braces, no `let`. A variable is introduced simply by assigning to it:

```uf
square:
  x!              ; pop argument into local x
  x x *           ; compute x*x, postfix
  0 ret
```

```uf
hypot:
  y!              ; y is on top of the stack
  x!              ; x is below it
  x x * y y * +   ; x^2 + y^2
  sqrt            ; square root (imported from libm)
  0 ret
```

Globals keep the v10 static-variable behavior but require an explicit marker:

```uf
^counter!              ; global counter
counter 1 + ^counter!  ; update global
```

| syntax | meaning |
|--------|---------|
| `x!` | store top-of-stack into local `x` |
| `x@` | push local `x` |
| `^x!` | store into global `x` |
| `^x@` | push global `x` |

### Scope and lifetime

A local variable exists from its first assignment until the nearest enclosing `RET`. The implicit scope boundary is the **subroutine call**:

- Every `CALL` creates a fresh local-variable frame sized for the target label.
- Every `RET` destroys the frame for the label it is returning from.
- `if` / `while` / `for` body labels are *not* separate subroutine calls; they are continuations inside the same call and share the caller’s frame.
- Recursion works automatically because each recursive `CALL` pushes a new frame.

### Runtime model

The `Ctx` gains a local-variable array and a frame-base stack:

```c
Cell* locals;       // flat array of local cells
long  local_base;   // start of current call's frame
long* local_frames; // saved base values
long  local_fsp;    // frame-stack pointer
```

At compile time, local variables are collected **per label**. Every distinct local name within a label gets an integer slot ID unique to that label. Access becomes a single array index:

```c
// x!  ->  cx->locals[cx->local_base + id_x] = pop(cx);
// x@  ->  pushc(cx, cx->locals[cx->local_base + id_x]);
```

The compiler also records the number of slots used by each label (e.g. `uf_local_counts[label_pc]`). Only `CALL` moves `local_base`. It pushes a frame marker onto the call stack, saves the old base, and bumps the base by the target label’s local count:

```c
// caller
size_t frame_size = uf_local_counts[target_pc];
cx->cs[cx->csp++] = NULL;                    // frame marker
cx->cs[cx->csp++] = &&K_after_call;
cx->local_frames[cx->local_fsp++] = cx->local_base;
cx->local_base += frame_size;
goto L_target;
K_after_call: ;
```

`RET` checks for the marker and restores the previous frame when it crosses a `CALL` boundary:

```c
if(cx->csp==0) return;
const void* r = cx->cs[--cx->csp];
if(r==NULL){
  cx->local_base = cx->local_frames[--c->local_fsp];
  r = cx->cs[--cx->csp];
}
if(r==NULL) return;
goto *r;
```

`if` / `while` / `for` continuations push only a normal return address, so returning from an inline body label does **not** pop the caller’s frame. The entry point (e.g. `uflux_run(cx, pc)` or exported wrappers) pushes the entry label’s frame before execution begins.

### Implementation notes

- **Lexer:** `x!` / `x@` remain local tokens. Add `^x!` / `^x@` for global tokens.
- **Parser:** for each label, collect the local variable names it uses and assign integer slot IDs. Emit `Ins::LocalSet(id)` / `Ins::LocalGet(id)` for locals, existing `Ins::SetV` / `Ins::GetV` for globals. Record a per-label local count.
- **Codegen:** emit the new `CALL` prologue (look up the target label’s local count) / `RET` epilogue and the array-indexed local accesses.
- **GC:** mark `locals[local_base .. local_base+uf_local_counts[current_label_pc])` as roots. Because frames are per label, only the active frame(s) need to be scanned.
- **Try/retry:** save and restore `local_base` in `UfTry`.

### Trade-offs

- A label used both as a `CALL` target and as an `if` / `while` body will see a fresh frame in the call case and the caller’s frame in the jump case. The recommended style is to reserve `CALL` for true subroutines and keep body labels simple.
- A called word cannot mutate its caller’s locals directly; values must be returned on the stack. This matches ordinary function semantics.

---

## 2. Universal Postfix Rule

### Motivation

Infix math notation carries a lot of special cases: some operators are binary-infix, some functions are prefix-with-parentheses, some functions are postfix, and precedence must be memorized. v11 removes all of that. Every operation follows one rule:

> **Operands first, operation second.**

This makes the language internally consistent and easy to parse.

### The rule

- Binary operations: `x y +`, `x y *`, `x y pow`.
- Unary operations: `x sin`, `x cos`, `x log`, `x sqrt`.
- Variable assignment: `value x!`.
- Variable fetch: `x@`.
- Method dispatch: `object method`.
- Container operations: `list value push`, `map key get`.
- Control flow: `'cond 'body while`.

There is no infix, no prefix-with-parentheses, and no special math-inline grammar. A "formula" is just a sequence of postfix tokens on one line:

```uf
x x * y y * + sqrt
```

### Examples

```uf
square:
  x!
  x x *
  0 ret
```

```uf
hypot:
  y!
  x!
  x x * y y * + sqrt
  0 ret
```

```uf
area:
  r!
  3.14159 r * r *
  0 ret
```

```uf
in_range:
  x! min! max!
  x min >= x max <= and
  0 ret
```

```uf
USE"m"
wave:
  t!
  2 sin 2 cos *      ; sin(2) * cos(2)
  0 ret
```

```uf
; word results are bound to locals first, then used
avg:
  xs!
  xs len n!
  xs sum s!
  s n /
  0 ret
```

### Order of operations

There is no implicit order of operations. Evaluation order is explicit in the token sequence:

```uf
2 3 + 4 *     ; (2+3)*4
2 3 4 * +     ; 2+(3*4)
```

Parentheses are not needed because the order is already explicit. For readability, sub-expressions can be factored into named locals.

### Implementation notes

- **No infix parser is needed.** The existing token-by-token parser is sufficient.
- **Math symbols remain opcodes.** `+ - * / < > == && || & | ^ ~ << >>` stay in the source-level opcode table and are used postfix.
- **Math functions remain imported words.** `sin`, `cos`, `sqrt`, `pow`, etc. are ordinary words called in postfix position.
- **Dense/text encoding:** no new glyphs or delimiters are required beyond the `^` prefix for globals.

### Trade-offs

- Authors must write postfix formulas instead of infix. This is less familiar than `x + y` but consistent with the rest of the language.
- No precedence table to memorize or implement.
- The parser stays simple and fast.

---

## 3. Basic-Block C Compilation

### Motivation

The µFlux direct-threaded interpreter is fast, but the stack protocol imposes a hard ceiling for numeric kernels. Each opcode boundary forces intermediate values through the data stack via `push`/`pop`, preventing the C compiler from keeping them in registers or folding expressions. For example, `sm += v[j] / ea` compiles to roughly ten stack operations plus two function calls, while C++ does it in three instructions.

v11 makes this bottleneck addressable: implicit locals give named slots, and universal postfix makes every straight-line sequence a predictable stack transformation. A second compilation pass can turn those sequences into straight-line C.

### The idea

Within a basic block — a straight-line sequence with no `CALL`, `RET`, `if`, or `while` — the compiler symbolically simulates the data stack. Each stack entry becomes a C expression or a C local temporary. The generated C does not touch the interpreter stack for arithmetic, locals, or typed-array access; it only spills back to the stack at block boundaries.

For the sequence:

```uf
v j vget ea / sm + ^sm!
```

The compiler emits:

```c
double s0 = v->data[j];
sm = sm + s0 / ea;
```

Control flow stays in the existing threaded model. A compiled block is just another C label that executes and then dispatches to the next threaded label.

### Example

A spectral-norm style kernel written in v11:

```uf
inner:
  n! v! sm! ea!
  0 i!
  'cond 'body while
  sm@
  0 ret

cond:
  i@ n@ <
  ret

body:
  v i@ vget ea / sm + ^sm!
  i 1 + ^i!
  0 ret
```

The `body` label contains no control flow, so it can be compiled as one C basic block:

```c
L_body:
  sm = sm + v->data[i] / ea;
  i = i + 1;
  goto L_cond;
```

The `while` dispatcher still jumps to `cond`; only the straight-line body is flattened.

### Implementation notes

- **Block identification:** split each label into basic blocks at `CALL`, `RET`, `if`, `while`, and any other control-flow opcode.
- **Stack simulation:** maintain a symbolic stack of C expressions. `push` introduces an expression; `pop` consumes one. Locals and globals introduce their C-array accesses.
- **Type specialization:** for typed arrays (`vget`/`vset`), generate the matching C type. For untyped cells, keep the existing `Cell` representation.
- **Spilling:** at the end of a block, any values still on the symbolic stack are written back to the real data stack so the next threaded label sees a consistent interpreter state.
- **Fallback:** blocks that cannot be compiled safely (unknown types, complex opcodes, insufficient type information) remain ordinary threaded labels.

### Interaction with v11 locals

Implicit locals make block compilation simpler. Because `x!` and `x@` map to known array slots, the compiler can treat them as ordinary C variables when profitable:

```uf
x@ 2 * y!
```

becomes:

```c
y = x * 2;
```

Locals that are only used within one block can be promoted to C locals; locals that escape across blocks stay in the `locals` array.

### Trade-offs

- **Biggest practical speedup** for numeric kernels without requiring a full JIT.
- **C compiler does the optimization:** register allocation, loop-invariant motion, and vectorization happen automatically.
- **Code size increases:** each compiled block is a separate C function/label.
- **Compilation time increases:** the compiler must generate and compile C for blocks, not just emit label tables.
- **Dynamic features are harder:** redefinition, `eval`, and late binding require invalidating compiled blocks.

This is an optional backend. The default interpreter remains direct-threaded; block compilation is enabled for words or labels the compiler marks as hot.

---

## 4. Backward Compatibility

v11 is intentionally **not** backward compatible with v10:

- `x!` / `x@` change from global static variables to call-local variables.
- Globals must be rewritten with the `^` prefix.
- Any infix math that was previously written as inline source must be rewritten in postfix.
- Existing dense/text encodings for variable access need a new glyph assignment for the `^` prefix.

Because the project is still pre-deployment, this breaking change is acceptable in exchange for a substantially simpler and more consistent surface language.
