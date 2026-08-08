# Universal Coercion Spec

µFlux exists to minimize tokens emitted by an LLM. The LLM should never need
to know or care what tag a collection carries or what type its elements are.
It passes whatever it has, and the op coerces. The rules are uniform and apply
identically to every op — there are no per-op coercion exceptions.

Coercion is based on JavaScript semantics: extremely permissive, never dies on
type. Incompatible values coerce to `NaN` (numeric context) or `"NaN"` (string
context) and propagate, rather than aborting.

## To Number (numeric context: arithmetic, comparison, vector ops)

| Input | Result |
|-------|--------|
| int `42` | `42` |
| float `3.14` | `3.14` |
| str `"42"` | `42` |
| str `"3.14"` | `3.14` |
| str `"0x1A"` | `26` (hex prefix recognized) |
| str `""` / whitespace-only | `0` |
| str `"hello"` | `NaN` (propagates, does not die) |
| `null` / `0` handle | `0` |
| bool `1`/`true` | `1` |
| bool `0`/`false` | `0` |
| list `[5]` (single elem) | `Number(5)` — unwrap |
| list `[]` (empty) | `0` |
| list `[1,2]` (multi) | `NaN` |
| dict / obj | `NaN` |

`NaN` is a valid float cell value. Arithmetic with `NaN` yields `NaN`. Ops
that require an integer result (e.g. integer division, `vargsort` index
output) coerce `NaN` → die, since there is no sensible integer — but only at
the point of integer truncation, never at type-check time.

## To String (string context: join, cat, split, format)

| Input | Result |
|-------|--------|
| int `42` | `"42"` |
| float `3.14` | `"3.14"` |
| float `NaN` | `"NaN"` |
| `null` / `0` handle | `"null"` |
| bool `1`/`true` | `"true"` |
| bool `0`/`false` | `"false"` |
| list `[1, 2, 3]` | `"1,2,3"` (elem String(), join with `,`) |
| list `[]` | `""` |
| dict / obj | `"[object Object]"` |

## To Boolean (truthiness: conditions, filter preds, bitmaps)

| Input | Result |
|-------|--------|
| `0`, `""`, `null`, `NaN`, empty collection | falsy (`0`) |
| everything else | truthy (`1`) |

## Collection Coercion

When an op's native type is a typed array but receives another collection:

| Input collection | Coerced to arr via |
|-----------------|-------------------|
| arr / tensor | direct (fast path, no copy) |
| list | each element → Number |
| dict | values (insertion order), each → Number |
| str | char codes (one byte per element) |
| bitmap | set-bit indices |
| chan / iter | drain, collect, each → Number |
| scalar (non-collection) | single-element array `[Number(x)]` |

When an op needs a list but gets another collection: wrap elements as cells.
When an op needs a bitmap but gets another collection: each element → one bit
(truthy=set, falsy=clear).

`cat` with a string operand coerces both sides to string (JS `+` semantics).

## Looseness

- `eq` uses JS loose equality (`==`): `"3" eq 3` → `1`, `null eq 0` → `1`.
- `seq` uses JS strict equality (`===`): `"3" seq 3` → `0`, `3 seq 3` → `1`.
  No coercion — types must match AND values must be equal. Two handles are
  `seq` iff same pointer (same object identity).
- `sne` is the complement (`!==`): `"3" sne 3` → `1`, `3 sne 3` → `0`.
- `lt`/`gt`/`vle`/`vge` coerce both sides to Number for comparison.
- `add` with any string operand → string concat (JS `+`).
- `and`/`or` use JS truthiness (`&&`/`||`), not bitwise.

## When Coercion Dies

Only when the *result* is genuinely undefined, never on input type:

- Integer truncation of `NaN` or `Infinity` (`div`, `rem`).
- `vmean`/`vmin`/`vmax` on empty collection (no element, divide by zero).
- Length mismatch in elementwise ops (`veadd` etc.).
- `NaN` as a sort key is placed last (does not die; JS `Array.sort`).

---

## Native Types per Op

### Core stack & I/O

`add` `sub` `mul` `div` `rem` — int/float scalar
`and` `or` `xor` — int scalar
`not` `bnot` — int scalar
`shl` `shr` — int scalar
`inc` `dec` — int scalar
`eq` `lt` `gt` `lte` `gte` — scalar (num or str)
`seq` `sne` — scalar
`cat` — str/arr/list
`fmt` `print` — scalar args + str fmt
`scan` — str fmt

### Containers

`dict` `list` `arr` `tensor` `obj` — constructors
`get` `getq` `set` `del` `has` — tag-dispatched
`len` — any collection
`keys` — dict/obj
`clone` — any handle
`push` `pop` — list
`enq` `deq` `close` — chan
`atom` `aget` `aset` `aadd` `cas` — atom

### Vector ops

`vadd` `vsub` `vmul` `vdiv` — arr + scalar
`veadd` `vesub` `vemul` `vediv` `vemax` `vemin` — arr × arr
`veq` `vlt` `vgt` `vge` `vle` — arr + scalar
`vand` `vor` `vnot` — bitmap × bitmap
`vcount` — bitmap
`vgather` — arr + bitmap
`vwhere` — arr arr + bitmap
`vsum` `vmean` `vmin` `vmax` — arr
`vmap` `vfold` — arr + fn
`vargsort` `vsearchsorted` — arr
`vget` `vset` — arr (raw)

### Sequence ops

`sort` — list/arr
`filter` `some` `every` — list
`group` — list + fn
`unique` — list
`flat` — list
`chunk` — seq + size
`slice` — str/arr/list
`range` — constructor

### String & regex

`split` — str + str
`join` — list + str
`find` — str + str
`repl` — str ×2
`match` `replace` `rsplit` `glob` — str + str
`trim` `up` `down` `starts` `ends` — str

### Conversion

`atoi` `atof` — str
`itoa` `ftoa` — scalar

### Large-data, graph, JSON, iterators, threads

`mmap` `slurp` `spit` — str (path)
`feach` `ffold` `fsplit` — str (path) + fn
`fget` `fatoi` `fatof` `fsget` `fbyte` — field_idx
`fcount` `fmatch` `femit` — str (path)
`bfs` `dfs` `wfind` — node + fn
`addto` `faddto` — dict
`json` — str
`unjson` — any value
`iter` `next` `collect` `imap` `ifilter` — iterator
`try` `retry` `spawn` — fn addr

### Control flow, directives, time, bloom, shell

`if` `ifelse` `while` `for` `break` `cont` — structural
`call` `ret` — structural
`weave` `task` `endt` `wrun` — structural
`now` `time` `timef` — scalar
`bloom` `badd` `btest` — scalar/handle
`sh` `shp` — str
`exec` — list
`hasargs` `argi` `sortkeys` `topn` `rangefold` — scalar/handle
