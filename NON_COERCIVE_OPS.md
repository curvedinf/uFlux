# Non-Coercive Ops: Inventory and Coercion Proposals

µFlux's design goal is minimal token count for LLM-authored scripts. Ops that
**die on the wrong collection type** force the LLM to emit explicit conversion
sequences (`list` → `arr`, etc.) that add tokens without expressing new logic.
This document inventories every non-coercive op and proposes what it should
accept to maximally reduce tokens, ordered by impact.

**Principle:** accept any collection that contains (or can be interpreted as)
the expected element type. Coerce once at entry — O(n) copy — then run the
existing fast path on the coerced typed array. Die only on genuinely
incompatible elements (e.g. a string in a numeric vector op), not on a
collection of the right element type under the wrong tag.

**Tag reference:** 5 arr, 6 tensor, 7 list, 8 dict, 9 str, 14 bitmap.

---

## Tier 1 — High Impact: Vector Ops (die on non-arr/tensor)

These are the core numeric array operations. They all require tag 5 (arr) or
6 (tensor) and **die on anything else**. A list of integers (`tag 7`) is the
single most common thing an LLM will build (`list` then `push` in a loop, or
`range`, or `split`), yet every `v*` op rejects it, forcing a manual
conversion that costs a token and requires the LLM to *know* to emit it.

### Scalar broadcast: `vadd` `vsub` `vmul` `vdiv`

Current: `arr scalar → arr'` — arr/tensor only.

**Should:** accept list (tag 7) by coercing to i64 or f64 typed array based on
the first element's tag, then proceed. Result is a typed arr (same element
type). Die only on non-numeric list elements.

Token savings: removes the conversion step entirely. `range 1 100 vadd 5`
just works instead of requiring `range 1 100 toarr vadd 5`.

### Elementwise: `veadd` `vesub` `vemul` `vediv` `vemax` `vemin`

Current: `arr arr → arr'` — both operands must be arr/tensor.

**Should:** accept list for either or both operands. Coerce both to the same
element type (widening int→float if one is float). Result is typed arr. Die on
length mismatch or non-numeric elements as today.

### Comparison → bitmap: `veq` `vlt` `vgt` `vge` `vle`

Current: `arr scalar → bitmap` — arr/tensor only.

**Should:** accept list as the array operand. Coerce to typed array, compare
against scalar, produce bitmap. Same die conditions.

### Reductions: `vsum` `vmean` `vmin` `vmax`

Current: `arr → scalar` — arr/tensor only.

**Should:** accept list. `range 1 101 vsum` produces `5050` directly.

### Higher-order: `vmap` `vfold`

Current: `arr fn → arr'` / `arr init fn → acc` — arr/tensor only.

**Should:** accept list. Coerce to typed array, apply fn elementwise. This is
especially high-impact because `vmap`/`vfold` are the intended replacements
for dedicated math ops (`vsqrt` = `vmap` over libm `sqrt`); gatekeeping them
behind a type check undermines the compactness argument.

### Sort/search: `vargsort` `vsearchsorted`

Current: `arr → idx_arr` / `sorted_arr val → idx` — arr/tensor only.

**Should:** accept list. Coerce, then binary-search or argsort on the typed
array. `vsearchsorted` should additionally accept a scalar in place of a
sorted array if the "array" is actually a single value (degenerate but valid).

### Blend: `vwhere`

Current: `arr arr bm → arr'` — both arrays must be arr/tensor.

**Should:** accept list for either array operand.

### Gather: `vgather`

Current: `arr bm → arr'` — arr/tensor only.

**Should:** accept list as the array operand.

---

## Tier 2 — Medium Impact: Sequence Ops (list-only)

These ops only accept `list` (tag 7) but would work identically on a typed
array. An LLM that has built an arr (e.g. via `ARR` then filled it) cannot
pass it to `filter` without converting to a list first.

### `filter` `some` `every`

Current: `list pred → list'/0/1` — list only.

**Should:** accept arr/tensor. Internally iterate over elements by index
(tag-aware stride). Return type matches input type (arr in → arr out, list in
→ list out). This is zero-cost conceptually — the pred is called per element
either way.

### `group`

Current: `list fn → dict` — list only.

**Should:** accept arr/tensor. Key extraction fn called per element. Values
in the resulting dict are lists (heterogeneous by nature).

### `unique`

Current: `list → list'` — list only.

**Should:** accept arr/tensor. Dedup via dict, preserving element type.

### `flat`

Current: `list → list'` — list only (flatten one level).

**Should:** accept any collection of collections. A list of arrs, an arr of
lists — all should flatten. Return list (result is heterogeneous).

---

## Tier 3 — Lower Impact: Already Tag-Dispatched but Narrow

### `sort`

Current: `seq → seq'` — accepts list and arr (already polymorphic).

**Should:** additionally accept str (sort characters / bytes) and dict (sort
by key). Low priority — current coverage is already the two most common cases.

### `slice`

Current: `seq a b → seq'` — tag-dispatched: str/arr/list.

**Should:** additionally accept tensor and bitmap (rare, but conceptually
valid — a sub-tensor or a bitmap range). Very low priority.

---

## Tier 4 — Do Not Coerce (correct to be strict)

These ops should remain non-coercive. Coercion would either defeat their
purpose, introduce ambiguity, or save zero tokens.

### `vget` `vset`

Direct typed-array access with **no handle validation** — the whole point is
zero-overhead raw indexing. Adding a coercion branch and type check defeats
this. If you need safe/polymorphic access, use `get`/`set` (the uniform
container protocol).

### `vand` `vor` `vnot` `vcount`

Bitmap ops (tag 14). Bitmaps are a dense u64-word array with a specific
encoding — coercing a list of booleans into a bitmap is a distinct operation
with its own cost and semantics. Should remain strict.

### Bitmap-requiring operand of `vgather` / `vwhere`

The bitmap operand in these ops is a structural mask, not a data collection.
It must be a real bitmap. The *array* operand should coerce (Tier 1); the
bitmap operand should not.

### Arithmetic: `add` `sub` `mul` `div` `rem` `and` `or` `xor` `shl` `shr` `bnot` `inc` `dec`

Scalar operations. Coercion here means numeric type widening (int↔float),
which is a separate concern from collection coercion. `eq`/`lt`/`gt` already
handle mixed int/float. The pure-int ops (`or`/`xor`/`shl`/`bnot`) are
intentionally strict per SPEC ("int ops die on float/pointer operands").

### String/regex ops (`match` `replace` `rsplit` `glob` `split` `join` `find` `repl` `trim` `up` `down` `starts` `ends`)

All operands must be strings. There is no meaningful coercion from a
non-string collection. `join` already takes `list sep → str`.

### Raw memory: `loadx` `storex` `buf` `bufcopy` `malloc` `free`

These operate on raw untagged pointers by design. Any type checking would
defeat them.

### Control flow: `if` `ifelse` `while` `for` `break` `cont` `try` `retry` `call` `ret` `spawn` `weave`/`task`/`endt`/`wrun`

Structural — no collection inputs to coerce.

### Container constructors: `dict` `list` `arr` `tensor` `obj` `chan` `atom` `buf` `bloom`

Take no collection input (or take a size/capacity scalar). Nothing to coerce.

### Channel/atom ops: `enq` `deq` `close` `aget` `aset` `aadd` `cas` `push` `pop`

Operate on a specific container type by design. Coercion would be meaningless
(e.g., `enq` on a dict?).

### I/O & FFI: `sh` `shp` `exec` `slurp` `spit` `argv` `print` `scan` `import` `export` `extern` `use` `sys`

Non-collection domain. Nothing to coerce.

### JSON: `json` `unjson`

`json` takes str; `unjson` takes any value and serializes. Already maximally
polymorphic on the value side.

### Iterators: `iter` `next` `collect` `imap` `ifilter` `femit`

`iter` already accepts every collection type. The rest operate on iterators.
Already coercive in the right way.

### Large-data ops: `mmap` `feach` `ffold` `fsplit` `fget` `fatoi` `fatof` `fsget` `fbyte` `fmatch` `fcount` `faddto` `addto`

Operate on file paths or field indices, not collections. `addto` takes a dict
by design (it's dict-specific accumulation).

### Graph: `bfs` `dfs` `wfind`

Take a start node and a function address. No collection-coercion angle.

### Conversion: `atoi` `atof` `itoa` `ftoa`

Scalar type conversions. Nothing to coerce.

### Convenience: `hasargs` `argi` `sortkeys` `topn` `rangefold` `now` `time` `timef`

No collection-coercion angle. `sortkeys`/`topn` are dict-specific by design.

---

## Summary Table

| Op | Current input | Proposed coercion | Token impact |
|----|--------------|-------------------|-------------|
| `vadd` `vsub` `vmul` `vdiv` | arr/tensor | + list | High |
| `veadd`–`vediv` `vemax` `vemin` | arr/tensor ×2 | + list (either/both) | High |
| `veq` `vlt` `vgt` `vge` `vle` | arr/tensor | + list | High |
| `vsum` `vmean` `vmin` `vmax` | arr/tensor | + list | High |
| `vmap` `vfold` | arr/tensor | + list | High |
| `vargsort` `vsearchsorted` | arr/tensor | + list | Medium |
| `vgather` `vwhere` | arr/tensor (+bitmap) | + list (array operand only) | Medium |
| `filter` `some` `every` | list | + arr/tensor | Medium |
| `group` `unique` `flat` | list | + arr/tensor | Medium |
| `sort` | list, arr | + str, dict | Low |
| `slice` | str, arr, list | + tensor, bitmap | Low |
| `vget` `vset` | arr (raw) | **no change** | — |
| `vand` `vor` `vnot` `vcount` | bitmap | **no change** | — |
