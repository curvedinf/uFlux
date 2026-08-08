# µFlux Specification v11

Normative for `comp/` (the `uf` compiler). The `trans/` transpiler targets the
text encoding (see final section).

**v11 is not backward compatible with v10**: `x!`/`x@` are now call-local
variables. Globals use `^x!`/`^x@`. The project is undeployed/developmental.

µFlux is a stack-based language compiled to C then native via `cc`, designed
for LLM-authored one-off scripts (low token count, fast, reliable). Every value
sits on a shared data stack; every opcode consumes from the top and pushes
results back. Programs are postfix (execution order), no expression syntax.

Design pillars: small orthogonal opcode set; uniform container protocol by tag
dispatch; algorithmic efficiency by default (typed arrays, autovectorized C
loops, open-addressing hashes, Timsort, streaming channels); structured control
flow; self-contained scripts (file I/O, argv, shell-out, regex, JSON in core);
garbage-collected (scripts never `free` managed objects).

## Source encodings

Two encodings, identical semantics, auto-detected:

- **Dense** (`.uf`) — single-token emoji glyphs, one glyph per op. Optimized
  for the Qwen3-0.6B tokenizer (every opcode glyph, v-space atom, and l-space
  atom is a single token).
- **Text** (`.uft`) — lowercase ASCII mnemonics, whitespace-delimited.

**Detection:** any char ≥ U+13000 → dense; else text. `.uft` extension is
conventional, not required. Inline source and stdin use the same detection.
MTU files may mix encodings freely (per-TU auto-detection).

### Dense Unicode spaces

| space | range | contents |
|-------|-------|----------|
| opcodes | U+1F300+ (emoji) | 207 slots (0..206), 187 live |
| v-space | U+1F941+ interleaved with other emoji | variable/label name atoms, runs fold |
| l-space | U+1F130+ (enclosed alphanumerics) | base-64 digit atoms (self-evaluating numbers) |
| delimiters | U+13100..U+13108 | chat-template delimiters, stripped pre-compilation |
| type glyphs | U+13110..U+13117 | int, float, ptr, byte, void, handle, str, bool |

Glyph assignments are in `comp/src/lex.rs` (`OP_GLYPHS`, `V_SPACE`, `L_SPACE`).
The deprecated v7 sequential glyph aliases (U+13000..U+13037) are removed.

## Comments and strings

`;` starts a line comment in both encodings. `"..."` is a string literal with
escapes limited to `\n \t \r \0 \\ \"` (any other `\x` copies x literally).
A bare `"..."` self-evaluates (pushes a string handle).

## Numbers (l-space grammar)

l-space atoms are base-64 digits: atom U+133A4+i has digit value i (0..63).

```
number := ['-'] lrun ['.' lrun] ['e' ['-'] lrun]
lrun   := one or more l-space atoms, big-endian
```

- Bare `lrun`: self-evaluating big-endian base-64 unsigned int (becomes int
  cell; overflow past u64 is a compile error).
- `lrun '.' lrun`: fixed-point (`d1/64 + d2/4096 + …`), becomes f64 cell.
- `'e' ['-'] lrun`: multiplies by 10^exp (decimal scientific). Any number with
  `.` or `e` is a float cell.
- Leading `-`: negates (only valid in sign position per the lookback rule).
- `LIT` also accepts ASCII decimal/hex (`0x..`)/float literals (including
  negatives) and type keywords, and also accepts an l-run number after it.
- Two adjacent l-runs fold into one number; generators must put whitespace
  between distinct numeric literals.

A stray `.` or `-` (not SUB by lookback, not a sign before an l-run) is a
lexer error. `e` begins an exponent only immediately after an l-run; elsewhere
it begins an ASCII identifier.

## The `-` lookback rule (dense only)

`-` is a **sign** iff the previous token did NOT push a value; otherwise SUB.

Value-pushing tokens: number, string, GETV (`@`), type-id push, ADDR (`'`),
and value-leaving opcodes (DUP OVR PICK ADD SUB MUL AND SHR INC DEC GET CLONE
CAST ARR TENSOR OBJ CAT FMT BUF MALLOC LOADX SIZEOF OFFSET PRINT SCAN CALL SYS
and all result-producing v10+ ops).

Non-pushing: program start, label definitions, LIT/STR themselves, SETV/SET/
STOREX/BUFCOPY/FREE/DROP and other consumers, SWP, directives.

This rule is dense-only: in text mode `-` alone is SUB and `-5` is a number
(tokens are space-delimited).

## Names, labels, and variables (v-space)

A run of v-space atoms folds into one name. Whitespace must separate two
adjacent v-runs meant to be distinct.

v11 variable semantics:
- `<name>!` — pop into **local** variable (call-scoped, fresh frame per CALL).
- `<name>@` — push **local** variable.
- `^<name>!` / `^<name>@` — pop/push **global** variable (static, persists
  across calls, shared across threads/TUs).
- A v-run after CALL/ADDR/`'` is a label **reference**.
- Any other bare v-run is a label **definition** (no colon needed).
- ASCII `name:` still defines a label; ASCII names work as jump targets.
- IMPORT/EXPORT/EXTERN/MACRO/STRUCT names remain ASCII.

Local variables exist from first assignment until the nearest enclosing RET.
if/while/for body labels are continuations that share the caller's frame.

## Type glyphs

U+13110..U+13117 = int(0) float(1) ptr(2) byte(3) void(4) handle(5→2)
str(6→2) bool(7→3) — handle/str are ptr aliases, bool a byte alias; void is 4
(SIZEOF void = 8, not useful).

- After `LIT`: pushes the type id.
- Directly after TENSOR/ARR/OBJ/CAST/SIZEOF: type **immediate** — pushes the
  id with no LIT. For ARR/TENSOR (stack `[ty, len]`, len on top) the compiler
  emits id then SWP so the length ends up on top.
- Elsewhere a bare type glyph pushes its id (expression position).
- ASCII type keywords (`int float ptr byte handle str bool void`) work in all
  the same positions.

## Type tags

0 int, 1 float, 2 ptr, 3 byte, 4 void, 5 arr, 6 tensor, 7 list, 8 dict,
9 str, 10 chan, 11 atom, 12 buf, 13 obj, 14 bitmap, 15 time, 16 dur,
17 bloom, 18 iter.

## Runtime cell

`typedef struct { int tag; int64_t i; } Cell;` (16 bytes with alignment).
FLOAT stores the double **bit pattern** in `i`; `uf_f()` converts tag-aware.
Pointer payloads live in `i` (cast at use sites). Tagged objects (arr/tensor/
list/dict/str/chan/atom/buf/obj/bitmap/bloom) have a `{tag, len, esz}` header
preceded by a GC header. Execution state (data stack, call stack, locals) lives
in a `Ctx` so weave tasks and spawned threads each get a fresh one.

## Uniform container protocol

Six ops cover all element access, dispatched on the handle's tag:

| op | dict | list/arr/tensor | str | obj |
|----|------|-----------------|-----|-----|
| `get` | value (missing: dies) | elem at idx (OOB: dies) | byte at idx | field by name or offset |
| `getq` | 0 on miss | 0 on OOB | 0 on OOB | 0 on missing field |
| `set` | put | idx set (OOB: dies) | byte set | field set |
| `del` | remove key (tombstone) | — | — | — |
| `has` | key present | idx in bounds | substring found | field exists |
| `keys` | keys | — | — | field names |

`len` and `typeof` complete the protocol. A null (0) handle returns 0 from
`getq`/`has`, dies elsewhere.

## Opcode reference

207 slots (0..206); 20 retired (indices 13–15, 22, 24–25, 30–32, 39, 57–61,
76–77, 86–87, 151). Retired indices never reuse. Each live opcode has a unique
dense glyph; the text mnemonic is the lowercased name. Glyph assignments are
1:1 and final in `comp/src/lex.rs`.

### Core stack, memory, and I/O

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 0 | 🌀 | `_lit` | → v | immediate follows (number/type glyph/keyword) |
| 1 | 😀 | `dup` | a → a a | |
| 2 | 🚀 | `ovr` | a b → a b a | |
| 3 | 🤍 | `drop` | a → | |
| 4 | 🌁 | `swp` | a b → b a | |
| 5 | 😁 | `pick` | … n → elem | copy nth-from-top (0-indexed) |
| 6 | 🚂 | `add` | a b → a+b | `+` |
| 7 | 🤐 | `sub` | a b → a−b | `-` (dense: lookback rule) |
| 8 | 🌂 | `mul` | a b → a*b | `*` |
| 9 | 😂 | `and` | a b → a&b | `&` |
| 10 | 🚃 | `shr` | a → a>>1 | |
| 11 | 🤑 | `inc` | a → a+1 | |
| 12 | 🌃 | `dec` | a → a−1 | |
| 16 | 😃 | `for` | count addr → | pushes k per iteration |
| 17 | 🚄 | `_call` | (label operand) | |
| 18 | 🤒 | `ret` | → | |
| 19 | 🌄 | `_obj` | → h | type immediate (struct id) |
| 20 | 😄 | `get` | h k → v | polymorphic (protocol above) |
| 21 | 🚅 | `set` | h k v → | polymorphic |
| 23 | 🤓 | `_arr` | len → h | type immediate; 64-aligned typed array |
| 26 | 🌅 | `clone` | h → h' | deep copy |
| 27 | 😅 | `_cast` | h type → h | checked downcast (struct id); dies on mismatch |
| 28 | 🚆 | `macro` | (directive) | `macro name { body }` |
| 29 | 🤔 | `_tensor` | len → h | type immediate |
| 33 | 🌆 | `setv` | value → | `<v>!` local, `^<v>!` global (v11) |
| 34 | 😆 | `getv` | → value | `<v>@` local, `^<v>@` global (v11) |
| 35 | 🚇 | `_str` | → h | bare `"…"` preferred |
| 36 | 🤕 | `cat` | a b → h | tag-dispatched: str/arr/list concat |
| 37 | 🌇 | `fmt` | args… fmt → h | |
| 38 | 😇 | `buf` | size → ptr | raw (untracked) buffer |
| 40 | 🚉 | `bufcopy` | dst src n → | |
| 41 | 🤖 | `_addr` | → code address | `'label` |
| 42 | 🌈 | `loadx` | addr → value | raw memory read |
| 43 | 😈 | `storex` | value addr → | raw memory write |
| 44 | 🚊 | `_sizeof` | type → n | |
| 45 | 🤗 | `_offset` | → n | compile-time `Struct.field` |
| 46 | 🌉 | `struct` | (directive) | `struct Name { field:type, … }` |
| 47 | 😉 | `malloc` | size → ptr | raw (untracked) |
| 48 | 🚌 | `free` | ptr → | raw only — never on GC handles |
| 49 | 🤘 | `_sys` | args… num → ret | syscall by number |
| 50 | 🌊 | `gc` | → | forces a full mark-sweep collection |
| 51 | 😊 | `import` | (directive) | `import c"fn"(types)->ret` |
| 52 | 🚍 | `export` | (directive) | `export "name"` before a label |
| 53 | 🤙 | `extern` | → address | `extern "symbol"` — global C symbol via `__asm__` |
| 54 | 🌋 | `print` | args… fmt → n | fmt on top; one arg per `%` conversion |
| 55 | 😋 | `scan` | fmt → values… count | fscanf semantics; any error aborts |
| 206 | 🛎 | `entry` | → | marks program entry; implicit jump from pc 0 |

### Containers

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 56 | 🚐 | `dict` | → h | open-addressing hash map (FNV-1a) |
| 62 | 🤚 | `list` | → h | growable cell vector |
| 63 | 🌌 | `push` | h v → h' | append (returns possibly-realloced handle) |
| 64 | 😌 | `pop` | h → v | empty: dies |
| 65 | 🚑 | `chan` | cap → h | bounded MPSC ring (blocking) |
| 66 | 🤛 | `enq` | h v → | blocks while full; dies on closed chan |
| 67 | 🌍 | `deq` | h → v | blocks while empty; closed+empty → 0 |
| 68 | 😍 | `close` | h → | |
| 69 | 🚒 | `atom` | v → h | atomic i64 cell |
| 70 | 🤜 | `aget` | h → v | atomic load |
| 71 | 🌎 | `aset` | h v → | atomic store |
| 72 | 😎 | `aadd` | h n → old | atomic fetch-add |
| 73 | 🚓 | `cas` | h old new → 0/1 | compare-and-swap |
| 74 | 🤝 | `typeof` | h → tag | v10+ tag numbering |
| 75 | 🌏 | `len` | h → n | generalized (arr/tensor/list/dict/chan/bitmap/str) |
| 119 | 🌜 | `getq` | h k → v_or_0 | never dies on absence |
| 120 | 😙 | `has` | h k → 0/1 | membership |
| 121 | 🚣 | `orelse` | a b → c | a if truthy else b |
| 122 | 🤨 | `keys` | h → list | dict keys / obj field names |
| 152 | 🌦 | `del` | h k → | remove (dict: tombstone) |

### Arithmetic & logic

int ops die on float/pointer operands (use CAST or `uf_f`-aware ops).

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 104 | 😕 | `div` | a b → a/b | int truncates toward zero; float f64. b=0: dies |
| 105 | 🚚 | `rem` | a b → a%b | C remainder (sign follows dividend). b=0: dies |
| 106 | 🤤 | `eq` | a b → 0/1 | ints by value; floats by bit pattern; strings by content; mixed int/float numeric |
| 107 | 🌙 | `lt` | a b → 0/1 | numeric or string lexicographic |
| 108 | 😖 | `gt` | a b → 0/1 | as lt |
| 109 | 🚛 | `not` | a → 0/1 | 1 if a==0 |
| 110 | 🤥 | `or` | a b → a\|b | ints only |
| 111 | 🌚 | `xor` | a b → a^b | ints only |
| 112 | 😗 | `shl` | a b → a<<b | ints only; b<0 or b≥64: dies |
| 113 | 🚜 | `bnot` | a → ~a | ints only |

### Structured control flow

Compiler-resolved; quotation addresses via `'<label>`. `break`/`cont` outside a
loop are compile errors.

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 114 | 🤦 | `if` | cond body_addr → | CALL body if cond nonzero |
| 115 | 🌛 | `ifelse` | cond then_addr else_addr → | |
| 116 | 😘 | `while` | cond_addr body_addr → | exit on 0, else CALL body, repeat |
| 117 | 🚢 | `break` | → | to end of nearest enclosing while/for |
| 118 | 🤧 | `cont` | → | to next iteration of nearest enclosing loop |

### Sequences

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 123 | 🌝 | `range` | start stop → list | ints [start, stop) |
| 124 | 😚 | `sort` | seq → seq' | Timsort (stable); list and arr |
| 125 | 🚦 | `filter` | list pred_addr → list' | keep elems where pred truthy |
| 126 | 🤩 | `some` | list pred_addr → 0/1 | short-circuits; empty → 0 |
| 127 | 🌞 | `every` | list pred_addr → 0/1 | short-circuits; empty → 1 |
| 164 | 🌩 | `group` | list fn_addr → dict | fn (elem → key); dict maps key → list |
| 165 | 😤 | `agg` | dict fn_addr → dict' | map each group's value-list through fn |
| 166 | 🚶 | `unique` | list → list' | dedup, first-occurrence order, O(n) via dict |
| 167 | 🤳 | `flat` | list → list' | flatten one level |
| 168 | 🌪 | `chunk` | seq size → list | split into size-element pieces (last may be short); size<1: dies |

### Vector ops (128–154, plus 169–171, 185–186)

Operate on arr/tensor of numeric element type; autovectorized C loops; results
freshly allocated; die on non-arr input. A **bitmap** (tag 14) is a dense
LSB-first u64-word array — the mask currency of the `v*` family.

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 128 | 😛 | `vadd` | arr scalar → arr' | |
| 129 | 🚧 | `vsub` | arr scalar → arr' | |
| 130 | 🤪 | `vmul` | arr scalar → arr' | |
| 131 | 🌟 | `vdiv` | arr scalar → arr' | scalar 0: dies |
| 132 | 😜 | `veadd` | arr arr → arr' | length mismatch: dies |
| 133 | 🚨 | `vesub` | arr arr → arr' | |
| 134 | 🤫 | `vemul` | arr arr → arr' | |
| 135 | 🌠 | `vediv` | arr arr → arr' | any 0 divisor: dies |
| 136 | 😝 | `vemax` | arr arr → arr' | elementwise max |
| 201 | 😭 | `vemin` | arr arr → arr' | elementwise min |
| 137 | 🚩 | `veq` | arr scalar → bitmap | |
| 138 | 🤬 | `vlt` | arr scalar → bitmap | |
| 139 | 🌡 | `vgt` | arr scalar → bitmap | |
| 140 | 😞 | `vge` | arr scalar → bitmap | |
| 141 | 🚪 | `vle` | arr scalar → bitmap | |
| 142 | 🤭 | `vand` | bm bm → bm' | |
| 143 | 🌤 | `vor` | bm bm → bm' | |
| 144 | 😟 | `vnot` | bm → bm' | |
| 145 | 🚫 | `vcount` | bm → n | popcount |
| 146 | 🤮 | `vgather` | arr bm → arr' | keep set-bit elements |
| 147 | 🌥 | `vsum` | arr → scalar | empty → 0 |
| 148 | 😠 | `vmean` | arr → f64 | empty: dies |
| 149 | 🚬 | `vmin` | arr → scalar | empty: dies |
| 150 | 🤯 | `vmax` | arr → scalar | empty: dies |
| 153 | 😡 | `vmap` | arr fn_addr → arr' | elementwise fn (elem → elem) |
| 154 | 🚲 | `vfold` | arr init fn_addr → acc | generic reduction fn (acc elem → acc) |
| 169 | 😥 | `vargsort` | arr → idx_arr | indices that would stably sort |
| 170 | 🚹 | `vsearchsorted` | sorted_arr val → idx | binary-search insertion point |
| 171 | 🤴 | `vwhere` | arr arr bm → arr' | blend: bit set → first arr, else second |
| 185 | 😩 | `vget` | h idx → v | direct typed array read (no handle validation) |
| 186 | 🛀 | `vset` | h idx v → | direct typed array write |

`vmap`/`vfold` let the family stay compact: `vsqrt`..`vceil` are `vmap` over an
IMPORTed libm fn; windowed/grouped/cumulative reductions are `vfold`.

### Time (scalar cells: time tag 15, dur tag 16, both i64 nanos)

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 155 | 🤰 | `now` | → t | CLOCK_REALTIME nanos |
| 156 | 🌧 | `time` | str fmt → t | `"unix"` (float s) or strptime(3) |
| 157 | 😢 | `timef` | t fmt → str | `"unix"` or strftime(3); honors process TZ |

Calendar arithmetic, durations, truncation, time-series joins are library code
(`mods/`).

### Bloom filter (tag 17; double-hashed FNV-1a, 1% FP at n)

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 158 | 🚴 | `bloom` | n → h | n<1: dies |
| 159 | 🤱 | `badd` | h v → | ints by value, strings by content |
| 160 | 🌨 | `btest` | h v → 0/1 | 1 = maybe, 0 = definitely not |

### Script I/O

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 161 | 😣 | `slurp` | path → str | whole file; not found: dies |
| 162 | 🚵 | `spit` | path str → | create/truncate; error: dies |
| 163 | 🤲 | `argv` | → list | program argv as list of strings |

### Shell

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 85 | 🌑 | `sh` | cmd → out err status | `/bin/sh -c`; -1 spawn failure, 128+signal |
| 88 | 😑 | `shp` | cmd → chan | detached thread feeds stdout line-by-line (cap 64) |
| 89 | 🚖 | `exec` | list → status | no shell; list is argv (elem 0 = program) |

### Strings & regex

All results freshly allocated. Indices are **byte** indices. Embedded
backtracking regex engine (no dependencies).

Regex syntax: literals; `\` escape; `.` any char except EOS; `*` `+` `?` greedy
(with backtracking); `[...]` char classes (ranges, negation `[^...]`, `]`
first is literal); `^` start anchor, `$` end anchor; `|` alternation; `(...)`
capture groups (max 9, group 0 = whole match). Malformed pattern = runtime die.

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 90 | 🤠 | `match` | str pat → list found | first match; group strings 0..n; found 0/1 on top |
| 91 | 🌓 | `replace` | str pat repl → str' | replace ALL; `\1`..`\9` backrefs |
| 92 | 😒 | `rsplit` | str pat → list | pieces between matches; empty matches skipped |
| 93 | 🚗 | `glob` | str pat → 0/1 | fnmatch-style |
| 94 | 🤡 | `split` | str sep → list | literal separator; empty sep: dies |
| 95 | 🌔 | `join` | list sep → str | |
| 96 | 😓 | `slice` | seq a b → seq' | tag-dispatched (str/arr/list); Python slice semantics |
| 97 | 🚘 | `find` | str sub → idx | first occurrence, −1 on miss |
| 98 | 🤢 | `repl` | str old new → str' | literal replace all; empty old: dies |
| 99 | 🌕 | `trim` | str → str' | strips isspace both ends |
| 100 | 😔 | `up` | str → str' | ASCII uppercase |
| 101 | 🚙 | `down` | str → str' | ASCII lowercase |
| 102 | 🤣 | `starts` | str affix → 0/1 | |
| 103 | 🌘 | `ends` | str affix → 0/1 | |

### Large-data & graph ops

No file-handle object type; every op is self-contained.

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 172 | 🌫 | `mmap` | path → str | read-only zero-copy string; GC-unmapped on sweep. All string ops work |
| 173 | 😦 | `feach` | path fn_addr → | call fn (line → cont) per line; stops early when fn returns 0 |
| 174 | 🚼 | `ffold` | path init fn_addr → acc | streaming reduce; fn (acc line → acc) |
| 175 | 🤵 | `fsplit` | path sep init fn_addr → acc | streaming split; fields via `fget`/`fatoi`/`fatof`/`fsget`/`fbyte` |
| 176 | 🌬 | `fget` | field_idx → str | zero-copy field view (current fsplit line) |
| 177 | 😧 | `fatoi` | field_idx → int | parse field directly, no alloc |
| 178 | 🚾 | `fatof` | field_idx → float | parse field directly, no alloc |
| 179 | 🤶 | `fsget` | field_idx off len → str | zero-alloc field substring |
| 180 | 🌮 | `fbyte` | field_idx off → int | single byte from field, no alloc |
| 181 | 😨 | `fmatch` | path pat → chan | spawn producer streaming regex-matching lines (cap 64); closed at EOF |
| 182 | 🚿 | `bfs` | start fn_addr → list | breadth-first visit-order; fn (node → neighbors) |
| 183 | 🤷 | `dfs` | start fn_addr → list | depth-first pre-order; same fn contract |
| 184 | 🌯 | `wfind` | start fn_addr pred_addr → v_or_0 | BFS with early exit: first match or 0 |
| 187 | 🤸 | `addto` | dict key amount → | dict[key] += amount; missing starts at 0 |
| 188 | 🌰 | `faddto` | dict field_idx amount → | dict[field] += amount; no Str alloc |
| 189 | 😪 | `finc` | dict field_idx → | dict[field] += 1; no Str alloc |

### JSON

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 190 | 🛁 | `json` | str → v | object → dict, array → list, number → int/float, true/false → 1/0, null → 0 |
| 191 | 🤹 | `unjson` | v → str | dict keys must be strings; atom/chan/iter/bitmap/bloom: dies |

### Iterators (tag 18)

Single-use, mutable cursors. Every collection is iterable.

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 192 | 🌱 | `iter` | h → it | list/arr/tensor (elems), dict (keys), str (bytes), chan (until close), bitmap (set-bit indices) |
| 193 | 😫 | `next` | it → v more | exhausted → `0 0`; non-iter: dies |
| 194 | 🛋 | `collect` | it → list | drain into fresh list |
| 195 | 🤽 | `imap` | it fn_addr → it' | lazy map; fn (v → v') |
| 196 | 🌲 | `ifilter` | it pred_addr → it' | lazy filter; pred (v → 0/1) |
| 197 | 😬 | `femit` | path it → n | stream any iterable to file, one item per line; returns count |

### Error containment & threads

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 198 | 🛌 | `try` | body_addr → result ok | CALL under die containment; success → value + 1, die → `0 0` |
| 199 | 🤾 | `retry` | n body_addr → result ok | try up to n+1 times |
| 200 | 🌳 | `spawn` | body_addr → chan | detached thread; TOS enqueued + chan closed at body end; `deq` = join |

`die` unwinds to the nearest `setjmp` checkpoint pushed by `try`/`retry` (they
nest); with no checkpoint, `die` is fatal. No backoff, jitter, or exception
typing — count and containment are the whole policy.

### Conversion

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 202 | 🛍 | `atoi` | str → int | strtoll base 10 |
| 203 | 🥀 | `atof` | str → float | strtod |
| 204 | 🌴 | `itoa` | int → str | |
| 205 | 😮 | `ftoa` | float → str | |

### Script convenience (207–210)

| idx | | mn | stack | notes |
|----|---|----|----|-------|
| 207 | 🥛 | `hasargs` | → 0/1 | 1 if `argv` has more than one element; replaces `argv len 1 gt` |
| 208 | 🥜 | `argi` | idx → int | `argv[idx]` parsed as integer via `strtoll`; out of bounds: dies |
| 209 | 🥝 | `sortkeys` | dict → key_list | `keys` + `sort` fused; returns dict keys sorted ascending |
| 210 | 🥞 | `topn` | dict n → list | top-n `[key value]` pairs by value descending, ties by key ascending; selection sort |
| 211 | 🥘 | `rangefold` | count init label → scalar | fold over range 0..count; label `(acc i → acc)` called per iteration |

### Modules & directives

| idx | | mn | notes |
|----|---|----|-------|
| 78 | 😏 | `use` | `use"name"` — link `-l<name>`, load `mods/<name>.ufm` |
| 79 | 🚔 | `mod` | `mod"name"` — translation-unit name |
| 80 | 🤞 | `pub` | export next label to global namespace |
| 81 | 🌐 | `weave` | begin task scope |
| 82 | 😐 | `task` | begin task body (inside weave) |
| 83 | 🚕 | `endt` | end task body |
| 84 | 🤟 | `wrun` | schedule the DAG, wait, publish results |

## Reflection

`typeof h → tag`, `len h → n` (generalized), `keys h → list` (dict keys or obj
field names). OBJ objects carry a tag header (tag obj, struct id); field access
by name or offset goes through the container protocol. CAST is checked (compares
struct id, dies on mismatch). Dynamic dispatch (v9 SEND/METHOD) is removed.

## Modules: USE and binding manifests

`use"name"`: links `-l<name>` and loads the binding manifest `<name>.ufm`,
searched in `./mods`, `~/.uflux/mods`, then `$UFMODPATH` dirs. A manifest is a
µFlux file containing IMPORT/EXTERN/STRUCT lines; compiled as part of the
USEing TU. Ships: `m.ufm`, `c.ufm`, `pthread.ufm`, `curl.ufm`, `sdl2.ufm`,
`ssl.ufm`. `-lpthread -lm` always linked; additional `-l<name>` per USE.

### FFI details

- `import c"fn"(types)->ret` — declares an unprototyped C function; args cast
  at call site. Generated C aliases the symbol (`__asm__`), so libc names
  already prototyped by the prelude are safe to import. Varargs: declare fixed
  params then `...` (e.g. `import c"printf"(ptr,...)->int`); format string is
  deepest, varargs above. `->int` is C `int` (32-bit), sign-extended into the
  64-bit cell.
- `extern "symbol"` — pushes the address of a global C symbol for use with
  `loadx`/`storex`. Runtime exposes `uf_argc` and `uf_argv` this way (though
  `argv` op 163 is preferred).

## Multiple translation units (MTU)

`uf main.uf lib.uf ...`: first input is the main TU (execution starts at its pc
0; a TU's top-level flow never falls into the next TU). Per-TU:

- Optional `MOD"name"` header; default is filename stem. Glyph v-names, ASCII
  labels, global variables (`^name`), and macros are file-local. Local
  variables are call-scoped (v11).
- `PUB` before a label exports it to the global namespace; CALL/`'` resolve PUB
  names across TUs. Duplicate PUB = compile error. STRUCTs, IMPORT/EXTERN/USE
  are global (deduped).
- Encodings may be mixed (per-TU auto-detection).
- Implementation: per-TU lex+parse, one merged codegen, one C file, one cc run.

## Directory mode and init threads

Bare `uf` (or `uf somedir/`) discovers source files:

- **Root**: `main.uf`/`main.uft` is the entry point (first TU, pc 0). Error if
  not found. Other `*.uf`/`*.uft` in root are additional TUs.
- **Subdirectory with `init.uf`/`init.uft`**: compiled as TUs; the init file is
  flagged as an init TU — its top-level code runs in a separate thread,
  automatically spawned before main starts. Recurses into nested init subdirs.
- **Subdirectory without init**: ignored. `mods/` is never scanned.

Init threads are detached pthreads; each gets its own `Ctx`. Global variables
(`^name`) are shared across all threads. Coordination via chans and PUB/CALL.

Explicit-file mode is unchanged — no discovery, no init threads.

## Garbage collection

Precise, non-moving, stop-the-world mark-sweep. Handle stability (a handle is
never invalidated by a collection) keeps FFI, chan buffers, and weave task
results trivially safe.

- **Heap objects**: every tagged object allocated with a GC header, linked into
  a global list. Bodies holding cells (list/dict/arr/chan/obj) are scanned for
  children during marking; str/bitmap/bloom are leaf bytes.
- **Roots**: each `Ctx`'s data and call stacks, all global variables (`^name`),
  active local-variable frames, weave task results, chan queue contents.
  Registers/C-stack are never roots (interpreter holds handles only inside cells).
- **Untagged pointers** (`malloc`, `buf`): never traced, never freed by GC.
- **Trigger**: bytes allocated since last collection exceeds threshold (default:
  max(1 MiB, 2× live bytes)), and explicit `gc` op (50). Adjustable via
  `UF_GC_THRESHOLD` env var or `--gc-threshold` runtime flag.
- **Concurrency**: stop-the-world via global GC mutex; weave workers park at
  allocation safepoints. Collections never start mid-`wrun` join.
- **Non-goals**: compaction, generations, incremental/concurrent marking.

## Concurrency — weave with fanout

```
weave
  task a ... endt              ; task a, no inputs
  a task b ... endt            ; task b depends on a
  pages 8 task fetch ... endt  ; fanout: 8 workers, one input (pages)
wrun
```

Grammar: `<input>* [<count>] task <name> token* endt` repeated until `wrun`.
`<count>` is a numeric literal 1..64 (fanout degree; default 1).

- **Static DAG**: task inputs name tasks in the same weave block. Unknown input
  or cycle = compile error. Task bodies are self-contained (labels task-local;
  two tasks may reuse v-names). Global variables (`^name`) cross task
  boundaries; local variables are per-task.
- **Runtime**: worker pool of `min(total declared workers, ncpu)` pthreads plus
  the calling thread. Each task runs with fresh data + call stacks; inputs
  copied in as initial stack in declared order; TOS at `endt` is the result.
- **After `wrun`**: every task's result is readable as `<name>@`; the final
  task's result is also pushed on the stack.
- **Fanout** (count > 1): must have ≥1 input. Only the **first** input drives
  fanout (must be iterable: list/arr/tensor/chan/iter). Additional inputs are
  broadcast (copied unchanged into every worker). Distribution is dynamic (items
  flow into an internal bounded chan; workers pull — cheap workers never idle
  behind slow ones). A chan input drains until close; other iterables until
  exhaustion. Published result is a **list of per-item results in completion
  order**. Empty input → empty list. Count is compile-time 1..64.
- **Timing**: `UF_WEAVE_DEBUG` env var prints per-task wall time, declared
  workers, items processed, retries, tolerated failures to stderr.

`spawn` (200): run body on a detached thread with a fresh `Ctx`; returns a
cap-1 chan immediately. At body end, TOS is enqueued and chan closed — `deq`
on it is a join. An uncontained `die` in a spawned thread kills the process.

## Text encoding

Lowercase ASCII mnemonics, whitespace-delimited. Same Tok AST as dense.

- Tokens split on whitespace; `;` comments; `"..."` strings may contain spaces.
  `-` alone is SUB, `-5` is a number (no lookback rule).
- Bare decimal/hex/float literals self-evaluate; `_lit` stays for type ids.
- Names: label def `name:`, refs `'name`, variables `name!`/`name@` (any
  identifier). Opcode mnemonics are reserved words.
- `--emit-text` / `--emit-dense` round-trip between encodings. `--to-text` /
  `--to-dense` convert (writes `<stem>.uft` / `<stem>.uf`, `-o` overrides).

## PRINT and SCAN

- **PRINT** (54): `args… fmt → n`. Format string on top (deepest arg first —
  reverse of C's printf convention). Pops fmt, pops one arg per `%` conversion,
  pushes printf return value. `%%` prints `%`.
- **SCAN** (55): `fmt → values… count`. Each conversion reads stdin via fscanf:
  `%d/%i/%u/%x/%o` → i64, `%f/%e/%g` → f64, `%s` → fresh string handle. Values
  pushed in conversion order, then count. Input error aborts.

## Concrete grammar

Whitespace/comments skipped between tokens. Chat delimiters U+13100..13108
stripped anywhere.

```
program      := token*
token        := number | string | varset | varget | labeldef | jump | op | directive
number       := ['-'] lrun ['.' lrun] ['e' ['-'] lrun]
string       := '"' ... '"'
name         := [a-zA-Z][a-zA-Z0-9_]*    ; must NOT start with '_' (reserved for _-prefixed ops)
varset       := ['^'] name ('!' | setv-glyph)
varget       := ['^'] name ('@' | getv-glyph)
labeldef     := name (':')?
jump         := (_call | "'") name
op           := opcode-glyph | '+' | '-' | '*' | '&'
             ; text mode: immediate-operand ops are _-prefixed — see section below
directive    := import c"name"(params)->ret | export "name" | extern "sym"
             | macro name { token* } | struct name { field:type, … }
             | use "name" | mod "name" | pub <labeldef>
             | weave taskblock* wrun
taskblock    := <input>* [<count>] task <name> token* endt
```

Quotation-taking ops (if/ifelse/while/for, try/retry, filter/some/every,
vmap/vfold, imap/ifilter, feach/ffold, bfs/dfs/wfind, spawn, group/agg, fanout
bodies) take label addresses on the stack — written `'<label>` — resolved by
the compiler. `break`/`cont` valid only inside a lexically enclosing
`while`/`for` body in the same function (compile error otherwise).

## Immediate-operand opcodes (_-prefixed, text mode only)

Opcodes starting with `_` take a **compile-time immediate**: the next source
token is consumed as a label name, type name, or numeric operand at compile
time. The immediate operand never touches the runtime stack — the value it
denotes is baked into the generated code or resolved to an address by the
compiler.

All other opcodes operate purely on the runtime stack.

This makes the distinction visible at a glance: `_call foo` consumes the
token `foo` as a label reference, whereas `+`, `dup`, `swap` read their
inputs from the stack at run time.

User-defined identifiers (variables, labels) may **not** start with `_` —
the prefix is reserved for these opcodes.

Dense/glyph mode is unaffected: glyphs dispatch by codepoint, not by name,
so no prefix is needed (or possible) there.

| mnemonic | opcode | immediate operand | stack effect |
|----------|--------|-------------------|--------------|
| `_lit` | LIT | number / type glyph / type keyword | → v |
| `_call` | CALL | label name | call (see notes) |
| `_addr` | ADDR | label name (`'label`) | → code address |
| `_sys` | SYS | syscall number | args… num → ret |
| `_str` | STR | string literal | → h |
| `_sizeof` | SIZEOF | type name | type → n |
| `_offset` | OFFSET | `Struct.field` | → n |
| `_obj` | OBJ | type name (struct id) | → h |
| `_cast` | CAST | type name (struct id) | h type → h |
| `_arr` | ARR | type name | len → h |
| `_tensor` | TENSOR | type name | len → h |

## Codegen notes

Pipeline: tokens → parser (labels/macros/structs/imports/v11 locals) → C with
computed-goto threaded interpreter → `cc -O2 -w`.

CLI modes: `uf prog.uf` (compile + run, cached binary in `$TMPDIR/uflux-cache/`);
`uf -c prog.uf -o bin` (compile only); `uf --emit-c prog.uf` (dump C); `--emit-text`/
`--emit-dense` (encoding conversion); `--to-text`/`--to-dense` (convert). First
positional arg is a file if it exists, otherwise inline source. Everything after
`--` is forwarded as program argv.

**`--debug` / `-D`**: compiles in debug mode (`cc -O0 -g`). Disables local-variable
register caching so locals are always memory-resident and accurate. On any fatal
runtime error (`die()`), prints a crash dump to stderr with: call stack (label
names + PCs), local variables per frame (names + values), and global variables
(names + values). In normal mode, no metadata tables are emitted and `die()`
behaves as before. Debug binaries are cached separately.

Always-on (non-debug) error messages include the operation name and stack depth:
e.g. `stack underflow in op_print (sp=0)`, `stack overflow in op_push (sp=1048576)`.

Codegen optimizations (`comp/src/gen.rs`):
- **Basic-block stack virtualization**: within straight-line blocks, ds push/pop
  become C locals; spilled to real ds at jump targets and control-flow edges.
- **Type specialization**: when both arithmetic operands have known types,
  emits raw C arithmetic (no tag checks), enabling SSE/autovectorization.
- **FOR inlining**: a FOR preceded by ADDR of a compile-time-known label, whose
  body is structurally inlinable, becomes a direct C `for` loop over a renamed
  inline copy. Depth cap 8.
- **Deferred variable stores**: SETV writes a temp cache within a block; dirty
  temps flushed at block boundaries.

Linking: always `-lpthread -lm` plus `-l<name>` per USE.

## Totals

207 opcode slots (0..206); 20 retired indices (never reused); **187 live
opcodes**. Every live opcode has a unique dense glyph codepoint.

## Non-goals

B-tree/trie/skiplist/R-tree/suffix-array index structures (library candidates in
`mods/`); dataframe ops; HTTP clients in core (`use"curl"`); Roaring bitmaps;
generational/incremental/compacting GC; async I/O; object-file linking;
pkg-config probing; hand-written SIMD (autovectorization first); remote weave
executors; MSP/mobile targets.

## C → µFlux transpiler (trans/)

`trans/trans.uf` is a C-subset → µFlux transpiler, self-hosted in µFlux (text
encoding). Bootstrapped via `gen_trans.py`. Supported subset, libc IMPORT
preamble, and coreutils test adaptations (`true`, `false`, `echo`, `yes`, `wc`)
are documented in `trans/README.md`.
