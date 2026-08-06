# µFlux Specification v10 (this repo)

Based on the µFlux Whitepaper v7.0 (`µFlux_Whitepaper_v7.0.pdf`), with the v8
encoding rework, the v9 feature set (library modules, op-native
datastructures, multiple translation units, reflection, wove-style dataflow
concurrency, and a second, human-readable text encoding), and the v10 revision
(uniform container protocol, real garbage collection, structured control flow,
weave fanout, iterators, JSON, large-data and data-analytics opcodes) merged
into one document. This document is normative for `comp/` (the `uf` compiler).
The `trans/` transpiler targets the **text encoding** (see the final section).

**v10 is intentionally not backward compatible with v9** — the project is
undeployed and developmental, so this revision optimizes the language itself
rather than the migration path.

## Purpose

µFlux is a language for **LLM-authored one-off scripts that efficiently
process data**. Design consequences:

1. **Small orthogonal core.** An LLM should be able to hold the whole opcode
   set in context. Few ops, no aliases, no near-duplicates, one obvious way
   per task.
2. **Uniform container protocol.** `get`/`set`/`del`/`has`/`keys`/`len` work
   on every container by tag dispatch. No `DGET`/`IDX`/`FIELDS` zoo to
   misremember.
3. **Algorithmic efficiency by default.** Typed 64-aligned arrays with
   vectorized whole-array ops (autovectorized C loops), open-addressing hash
   maps, dense bitmap masks, Timsort, streaming channels. The fast thing and
   the short thing are the same thing.
4. **Structured control flow.** LLMs generate `if/while/for` far more
   reliably than raw jump labels. Raw jumps remain available but are no
   longer the only way.
5. **Self-contained scripts.** File I/O, argv, shell-out, regex, and time
   are in the core opcode set — no manifest needed for the common cases.
6. **No memory management.** A real garbage collector reclaims everything
   the runtime allocates. Scripts never call `free` on managed objects
   (manual `malloc`/`free` remains for raw FFI buffers only).

Concrete decisions carried over:

1. **Source files use the extension `.uf`** (dense encoding) or `.uft` (text
   encoding, conventional only).
2. **Dense tokens are single Egyptian-hieroglyph codepoints**, one glyph per
   token. Whitespace is insignificant except where it separates two glyph runs
   that would otherwise fold into one. `;` starts a comment to end of line. A
   `.uf` file carries **one header comment block only**; the body is dense
   glyph source.
3. **Operand orders:** all container element access goes through the uniform
   container protocol (`get` is `handle key → value`, `set` is
   `handle key value →`). Object field SET keeps the v9 order
   `handle offset value →` (mirrors the protocol's `set`).

## Encodings and detection

µFlux source exists in two encodings with identical syntax and semantics:

- **Dense** — the hieroglyph token stream described above.
- **Text** — lowercase ASCII mnemonics, whitespace-delimited (see "Text
  encoding" below).

**Detection:** any char ≥ U+13000 in the input → dense; else text. `.uft` is a
conventional extension, not required. Inline source and `-` (stdin) use the
same detection; MTU files may mix encodings freely (per-TU auto-detection).

The deprecated v7 sequential glyph aliases (U+13000..U+13037) are **removed**
in v10.

## Unicode spaces (dense)

| space | range | contents |
|-------|-------|----------|
| opcodes | hand-picked glyphs in U+13000–U+1342F (tables below) | 191 slots, 173 live opcodes |
| v-space | U+13362..U+133A3 | variable/label name atoms, runs fold |
| l-space | U+133A4..U+133E3 | base-64 digit atoms (self-evaluating numbers) |
| delimiters | U+13100..U+13108 | chat-template delimiters, stripped pre-compilation |
| type glyphs | U+13110..U+13117 | int, float, ptr, byte, void, handle, str, bool |

The opcode glyphs are disjoint from v-space, l-space, the delimiters and the
type glyphs. Every live opcode has a unique dense glyph codepoint (tables
below); assignments are 1:1 and final for tooling.

## Numbers (l-space grammar)

l-space atoms are base-64 digits: atom U+133A4+i has digit value i (0..63).

```
number := ['-'] lrun ['.' lrun] ['e' ['-'] lrun]
lrun   := one or more l-space atoms, big-endian
```

- A bare `lrun` is a self-evaluating big-endian base-64 **unsigned integer**;
  overflow past u64 is a compile error. It becomes an int cell.
- `lrun '.' lrun` is fixed-point: `d1/64 + d2/4096 + ...`, becoming an f64 cell
  (digits beyond f64 resolution are consumed and ignored).
- `'e' ['-'] lrun` multiplies by 10^exp (decimal scientific); any number with
  `.` or `e` is a float cell.
- A leading `-` makes the number negative; it is only valid in sign position
  per the lookback rule.
- `LIT` still accepts ASCII decimal/hex (`0x..`)/float literals (including
  negatives) and type keywords, and also accepts an l-run number after it.
- Because two adjacent l-runs fold into one big-endian number, generators must
  put whitespace between two numeric literals that are meant to be distinct.

A stray `.` or a stray `-` (one that is neither SUB by the lookback rule nor a
sign followed by an l-run) is a lexer error. `e` begins an exponent **only**
immediately after an l-run; elsewhere `e` begins an ASCII identifier.

## The `-` lookback rule (dense only)

`-` is a **sign** iff the previous token did NOT push a value; otherwise it is
SUB.

Value-pushing tokens: a number, a string, GETV (`@` or glyph), a type-id push
(LIT/type glyph), ADDR (`'`), and every value-leaving opcode — DUP OVR PICK ADD
SUB MUL AND SHR INC DEC GET CLONE CAST ARR TENSOR OBJ CAT FMT BUF MALLOC
LOADX SIZEOF OFFSET PRINT SCAN CALL SYS.

Non-pushing tokens: program start, label definitions, LIT/STR themselves (their
operand follows), jumps (JMP/JZ/JE/`=`), SETV/SET/STOREX/BUFCOPY/FREE/DROP
and other consumers, SWP, and all directives (MACRO/STRUCT/IMPORT/EXPORT/EXTERN).

Examples: `𓆨𓆤 𓆨𓆤 -` → 128 128 SUB → 0. At program start `-𓆩` → -5.
After a label or SETV, `-` begins a negative number.

This rule is a dense-only artifact: it does not exist in the text encoding
(see below).

## Names, labels and variables (v-space, dense)

A run of v-space atoms folds into one name (`𓍢𓍣` is a single name distinct
from `𓍢`). Whitespace must separate two adjacent v-runs that are meant to be
distinct names — including a label definition followed by a variable
reference.

- `<v-run>!` or `<v-run><SETV glyph>`: pop into variable.
- `<v-run>@` or `<v-run><GETV glyph>`: push variable.
- A v-run immediately after JMP/JZ/JE/`=`/CALL/ADDR/`'` is a label
  **reference**.
- Any other bare v-run is a label **definition** (no colon needed).
- ASCII `name:` still defines a label; ASCII names still work as jump targets.
- IMPORT/EXPORT/EXTERN/MACRO/STRUCT names remain ASCII.

## Type glyphs and immediates

Type glyphs U+13110..U+13117 = int(0) float(1) ptr(2) byte(3) void(4)
handle(5→2) str(6→2) bool(7→3) — handle/str are ptr aliases, bool a byte
alias; void is 4 (SIZEOF void = 8, not useful).

- After `LIT`: pushes the type id (like `LIT int`).
- Directly after TENSOR/ARR/OBJ/CAST/SIZEOF: type **immediate** — pushes the
  id with no LIT. For ARR/TENSOR (stack `[ty, len]`, len on top) the compiler
  emits the id followed by SWP so the length pushed earlier ends up on top.
- Elsewhere a bare type glyph pushes its id (expression position).

ASCII type keywords (`int float ptr byte handle str bool void`) keep working
in all the same positions. Reflection contexts (CAST type names) additionally
accept container type names mapping to the v10 tags: `arr` 5, `tensor` 6,
`list` 7, `dict` 8, `str` 9, `chan` 10, `atom` 11, `buf` 12, `obj` 13,
`bitmap` 14, `time` 15, `dur` 16, `bloom` 17, `iter` 18.

## Type tags (renumbered)

0 int, 1 float, 2 ptr, 3 byte, 4 void, 5 arr, 6 tensor, 7 list, 8 dict,
9 str, 10 chan, 11 atom, 12 buf, 13 obj, 14 bitmap, 15 time, 16 dur,
17 bloom, 18 iter.

Changes from v9: the DYN/MAP/RING names are gone (they are `list`/`dict`/
`chan`); str/chan/atom are first-class tags; bitmap/time/dur/bloom/iter
are new.

## The uniform container protocol

Six ops cover all element access, dispatched on the handle's tag:

| op | stack | dict | list/arr/tensor | str | obj |
|----|-------|------|-----------------|-----|-----|
| `get`   | h k → v        | value (missing key: dies) | elem at idx (OOB: dies) | byte at idx | field by name or offset |
| `getq`  | h k → v_or_0   | 0 on miss | 0 on OOB | 0 on OOB | 0 on missing field |
| `set`   | h k v →        | put | idx set (OOB: dies) | byte set | field set (missing: dies) |
| `del`   | h k →          | remove key (tombstone) | — | — | — |
| `has`   | h k → 0/1      | key present | idx in bounds | substring found | field exists |
| `keys`  | h → list       | keys | — | — | field names |

`len` (generalized, as v9) and `typeof` complete the protocol. A null (0)
handle returns 0 from `getq`/`has`, dies elsewhere. This removes v9's IDX,
SETI, DGET, DPUT, DDEL, DCOUNT, DKEYS, and FIELDS — eight opcodes collapse
into behavior of the protocol.

## Complete opcode table (normative)

Surviving v9 opcodes keep their index and glyph. Removed indices are
retired, not reused. New opcodes are 104..190; their codepoints come from
the free blocks U+1332A..U+13358 and U+13400..U+1342F (assignments are 1:1
and final for tooling).

### Kept from v9, unchanged semantics

| idx | glyph | cp | mnemonic | stack effect |
|-----|-------|----|----------|--------------|
| 0  | 𓍀 | U+13340 | `lit` | → v (immediate follows) |
| 1  | 𓁐 | U+13050 | `dup` | a → a a |
| 2  | 𓁑 | U+13051 | `ovr` | a b → a b a |
| 3  | 𓂹 | U+130B9 | `drop` | a → |
| 4  | 𓅡 | U+13161 | `swp` | a b → b a |
| 5  | 𓂩 | U+130A9 | `pick` | … n → elem |
| 6  | 𓂝 | U+1309D | `add` (`+`) | a b → a+b |
| 7  | 𓂞 | U+1309E | `sub` (`-`) | a b → a−b |
| 8  | 𓂡 | U+130A1 | `mul` (`*`) | a b → a*b |
| 9  | 𓂢 | U+130A2 | `and` (`&`) | a b → a&b |
| 10 | 𓃗 | U+130D7 | `shr` | a → a>>1 |
| 11 | 𓂥 | U+130A5 | `inc` | a → a+1 |
| 12 | 𓂦 | U+130A6 | `dec` | a → a−1 |
| 13 | 𓂻 | U+130BB | `jmp` | (label operand) |
| 14 | 𓂼 | U+130BC | `jz` | cond → |
| 15 | 𓂽 | U+130BD | `je` (`=`) | a b → |
| 16 | 𓂾 | U+130BE | `for` | count addr → (pushes k per iteration) |
| 17 | 𓃀 | U+130C0 | `call` | (label operand) |
| 18 | 𓂿 | U+130BF | `ret` | → |
| 19 | 𓉐 | U+13250 | `obj` | → h (type immediate) |
| 20 | 𓁷 | U+13077 | `get` | h k → v (**now polymorphic**, see protocol) |
| 21 | 𓁸 | U+13078 | `set` | h k v → (**now polymorphic**) |
| 23 | 𓉑 | U+13251 | `arr` | len → h (type immediate; 64-aligned) |
| 26 | 𓄎 | U+1310E | `clone` | h → h' |
| 27 | 𓄪 | U+1312A | `cast` | h type → h (checked) |
| 28 | 𓄳 | U+13133 | `macro` | (directive) |
| 29 | 𓉓 | U+13253 | `tensor` | len → h (type immediate) |
| 33 | 𓂤 | U+130A4 | `setv` (`<v>!`) | value → |
| 34 | 𓁻 | U+1307B | `getv` (`<v>@`) | → value |
| 35 | 𓂋 | U+1308B | `str` | → h (bare `"…"` preferred) |
| 36 | 𓂌 | U+1308C | `cat` | a b → h |
| 37 | 𓂍 | U+1308D | `fmt` | args… fmt → h |
| 38 | 𓉖 | U+13256 | `buf` | size → ptr |
| 40 | 𓉘 | U+13258 | `bufcopy` | dst src n → |
| 41 | 𓁼 | U+1307C | `addr` (`'`) | → code address |
| 42 | 𓁹 | U+13079 | `loadx` | addr → value |
| 43 | 𓁽 | U+1307D | `storex` | value addr → |
| 44 | 𓁾 | U+1307E | `sizeof` | type → n |
| 45 | 𓁿 | U+1307F | `offset` | → n (compile-time `Struct.field`) |
| 46 | 𓉙 | U+13259 | `struct` | (directive) |
| 47 | 𓉛 | U+1325B | `malloc` | size → ptr |
| 48 | 𓉜 | U+1325C | `free` | ptr → |
| 49 | 𓉠 | U+13260 | `sys` | args… num → ret |
| 51 | 𓉞 | U+1325E | `import` | (directive) |
| 52 | 𓉟 | U+1325F | `export` | (directive) |
| 53 | 𓉢 | U+13262 | `extern` | → address of global symbol |
| 54 | 𓂎 | U+1308E | `print` | args… fmt → n |
| 55 | 𓂀 | U+13080 | `scan` | fmt → values… count |
| 56 | 𓊵 | U+132B5 | `dict` | → h |
| 62 | 𓊶 | U+132B6 | `list` | → h |
| 63 | 𓂧 | U+130A7 | `push` (was `append`) | h v → h' |
| 64 | 𓂨 | U+130A8 | `pop` | h → v (empty: dies) |
| 65 | 𓊷 | U+132B7 | `chan` | cap → h |
| 66 | 𓂺 | U+130BA | `enq` | h v → |
| 67 | 𓃁 | U+130C1 | `deq` | h → v |
| 68 | 𓉣 | U+13263 | `close` | h → |
| 69 | 𓋰 | U+132F0 | `atom` | v → h |
| 70 | 𓂆 | U+13086 | `aget` | h → v |
| 71 | 𓂇 | U+13087 | `aset` | h v → |
| 72 | 𓂟 | U+1309F | `aadd` | h n → old |
| 73 | 𓂠 | U+130A0 | `cas` | h old new → 0/1 |
| 74 | 𓄫 | U+1312B | `typeof` | h → tag (new numbering) |
| 75 | 𓄬 | U+1312C | `len` | h → n (generalized; bitmap → bit count, str → bytes) |
| 78 | 𓋹 | U+132F9 | `use` | (directive) |
| 79 | 𓋸 | U+132F8 | `mod` | (directive) |
| 80 | 𓋷 | U+132F7 | `pub` | (directive) |
| 81 | 𓐍 | U+1340D | `weave` | (begin task scope) |
| 82 | 𓐎 | U+1340E | `task` | (begin task body) |
| 83 | 𓐏 | U+1340F | `endt` | (end task body) |
| 84 | 𓐐 | U+13410 | `wrun` | → result |
| 88 | 𓈗 | U+13217 | `shp` | cmd → chan |
| 89 | 𓆣 | U+131A3 | `exec` | list → status |
| 90 | 𓌜 | U+1331C | `match` (was `rx`) | str pat → list found |
| 91 | 𓌝 | U+1331D | `replace` (was `rxsub`) | str pat repl → str' |
| 92 | 𓌞 | U+1331E | `rsplit` (was `rxsplit`) | str pat → list |
| 93 | 𓌟 | U+1331F | `glob` | str pat → 0/1 |
| 94 | 𓌠 | U+13320 | `split` | str sep → list (literal) |
| 95 | 𓌡 | U+13321 | `join` | list sep → str |
| 96 | 𓌢 | U+13322 | `slice` | str a b → str' |
| 97 | 𓌣 | U+13323 | `find` | str sub → idx |
| 98 | 𓌤 | U+13324 | `repl` | str old new → str' |
| 99 | 𓌥 | U+13325 | `trim` | str → str' |
| 100 | 𓌦 | U+13326 | `up` | str → str' |
| 101 | 𓌧 | U+13327 | `down` | str → str' |
| 102 | 𓌨 | U+13328 | `starts` | str affix → 0/1 |
| 103 | 𓌩 | U+13329 | `ends` | str affix → 0/1 |

### Changed from v9

| idx | glyph | cp | mnemonic | stack effect | change |
|-----|-------|----|----------|--------------|--------|
| 20/21 | 𓁷/𓁸 | — | `get`/`set` | — | polymorphic (protocol above) |
| 63 | 𓂧 | — | `push` | — | renamed from `append` |
| 85 | 𓆉 | U+13189 | `sh` | cmd → out err status | **merged SH+SHX**: always captures stdout and stderr as fresh strings, status on top (−1 spawn failure, 128+signal). For fire-and-forget, drop the strings. |
| 90–92 | — | — | `match`/`replace`/`rsplit` | — | renamed from `rx`/`rxsub`/`rxsplit` (semantics unchanged, including `\1..` backrefs) |
| 74 | 𓄫 | — | `typeof` | — | new tag numbering |
| 50 | 𓉝 | U+1325D | `gc` | → | **now real**: forces a full mark-sweep collection (see "Garbage collection" below); was a no-op in v9 |
| 36 | 𓂌 | — | `cat` | a b → h' | tag-dispatched: str concat, arr concat, list concat (element-type mismatch on arr: dies). Covers the dropped `vconcat` with identical token count |
| 96 | 𓌢 | — | `slice` | seq a b → seq' | tag-dispatched: str/arr/list, Python slice semantics. Covers the dropped `vslice` with identical token count |

### Removed from v9 (indices retired)

- **22 SEND, 77 METHOD** — dynamic dispatch removed. Structs + the container
  protocol cover scripts; vtables were the most machinery per use in v9.
- **24 IDX, 25 SETI, 57–61 DGET/DPUT/DDEL/DCOUNT/DKEYS, 76 FIELDS** —
  subsumed by the container protocol (`get`/`set`/`del`/`len`/`keys`).
- **30 VEC, 31 PIN, 32 UNPIN, 39 BUFPTR** — runtime no-ops. The
  vector ops below make VEC's intent real; the rest were placeholders.
- **86 SHX, 87 SHL** — merged into the new `sh` (`split` covers SHL).
- The v7 glyph aliases U+13000..U+13037.
- `vslice`/`vconcat` (draft ops) — subsumed by tag-dispatched `slice`/`cat`.

### New opcodes (104..190)

**Arithmetic & logic.** int ops die on float/pointer operands (use CAST or
`uf_f`-aware ops); comparisons follow EQ/LT/GT rules below.

| idx | glyph cp | mnemonic | stack effect | notes |
|-----|----------|----------|--------------|-------|
| 104 | U+1332A | `div` | a b → a/b | int truncates toward zero; float f64. b=0: dies. |
| 105 | U+1332B | `rem` | a b → a%b | C remainder (sign follows dividend). b=0: dies. |
| 106 | U+1332C | `eq` | a b → 0/1 | ints by value; floats by bit pattern; strings by content; handles by identity; mixed int/float numeric. |
| 107 | U+1332D | `lt` | a b → 0/1 | numeric, or string lexicographic; other handles: dies. |
| 108 | U+1332E | `gt` | a b → 0/1 | as `lt`. |
| 109 | U+1332F | `not` | a → 0/1 | 1 if a==0 (JZ truthiness); null handle → 1. |
| 110 | U+13330 | `or` | a b → a\|b | ints only. |
| 111 | U+13331 | `xor` | a b → a^b | ints only. |
| 112 | U+13332 | `shl` | a b → a<<b | ints only; b<0 or b≥64: dies. |
| 113 | U+13333 | `bnot` | a → ~a | ints only. |

**Structured control flow** (compiler-resolved; quotation addresses via
`'<label>`). The compiler maintains a per-function loop stack; `break`/
`cont` outside a loop are **compile errors**.

| idx | glyph cp | mnemonic | stack effect | notes |
|-----|----------|----------|--------------|-------|
| 114 | U+13334 | `if` | cond body_addr → | CALL body if cond nonzero |
| 115 | U+13335 | `ifelse` | cond then_addr else_addr → | one op, no parser pairing state |
| 116 | U+13336 | `while` | cond_addr body_addr → | CALL cond (leaves one value); exit on 0, else CALL body, repeat |
| 117 | U+13337 | `break` | → | to end of nearest enclosing `while`/`for` |
| 118 | U+13338 | `cont` | → | to next iteration of nearest enclosing loop |

**Container protocol additions & sequences.**

| idx | glyph cp | mnemonic | stack effect | notes |
|-----|----------|----------|--------------|-------|
| 119 | U+13339 | `getq` | h k → v_or_0 | never dies on absence; wrong container kind dies |
| 120 | U+1333A | `has` | h k → 0/1 | membership (protocol table) |
| 121 | U+1333B | `orelse` | a b → c | a if truthy else b; both already evaluated |
| 122 | U+1333C | `keys` | h → list | dict keys / obj field names |
| 123 | U+1333D | `range` | start stop → list | ints [start, stop); empty if start≥stop |
| 124 | U+1333E | `sort` | seq → seq' | fresh; Timsort (stable); works on list and arr by tag; incomparable element types: dies |
| 125 | U+1333F | `filter` | list pred_addr → list' | keep elems where pred (elem → 0/1) truthy |
| 126 | U+13400 | `some` | list pred_addr → 0/1 | short-circuits; empty → 0 |
| 127 | U+13401 | `every` | list pred_addr → 0/1 | short-circuits; empty → 1 |

**Vector ops** (128..154). Operate on arr/tensor of numeric element type;
plain C loops autovectorized by cc -O2; results freshly allocated; die on
non-arr input. A **bitmap** (tag 14) is a dense LSB-first u64-word array —
the mask currency of the `v*` family.

| idx | glyph cp | mnemonic | stack effect |
|-----|----------|----------|--------------|
| 128 | U+13402 | `vadd` | arr scalar → arr' |
| 129 | U+13403 | `vsub` | arr scalar → arr' |
| 130 | U+13404 | `vmul` | arr scalar → arr' |
| 131 | U+13405 | `vdiv` | arr scalar → arr' (scalar 0: dies) |
| 132 | U+13406 | `veadd` | arr arr → arr' (length mismatch: dies) |
| 133 | U+13407 | `vesub` | arr arr → arr' |
| 134 | U+13408 | `vemul` | arr arr → arr' |
| 135 | U+13409 | `vediv` | arr arr → arr' (any 0 divisor: dies) |
| 136 | U+1340A | `vemax` | arr arr → arr' (elementwise max) |
| 190 | U+13358 | `vemin` | arr arr → arr' (elementwise min) |
| 137 | U+1340B | `veq` | arr scalar → bitmap |
| 138 | U+1340C | `vlt` | arr scalar → bitmap |
| 139 | U+13411 | `vgt` | arr scalar → bitmap |
| 140 | U+13412 | `vge` | arr scalar → bitmap |
| 141 | U+13413 | `vle` | arr scalar → bitmap |
| 142 | U+13414 | `vand` | bm bm → bm' |
| 143 | U+13415 | `vor` | bm bm → bm' |
| 144 | U+13416 | `vnot` | bm → bm' |
| 145 | U+13417 | `vcount` | bm → n (popcount) |
| 146 | U+13418 | `vgather` | arr bm → arr' (keep set-bit elements) |
| 147 | U+13419 | `vsum` | arr → scalar (empty → 0) |
| 148 | U+1341A | `vmean` | arr → f64 (empty: dies) |
| 149 | U+1341B | `vmin` | arr → scalar (empty: dies) |
| 150 | U+1341C | `vmax` | arr → scalar (empty: dies) |
| 153 | U+1341F | `vmap` | arr fn_addr → arr' (elementwise fn (elem → elem); covers every unary math fn via `USE"m"`) |
| 154 | U+13420 | `vfold` | arr init fn_addr → acc (generic reduction; fn (acc elem → acc)) |

`vmap`/`vfold` are what let this family stay at 26 ops instead of 51:
`vsqrt`..`vceil` are `vmap` over an IMPORTed libm fn; windowed/grouped/
cumulative reductions are `vfold` or library loops.

**Time** (scalar cells: time = tag 15, dur = tag 16, both i64 nanos).

| idx | glyph cp | mnemonic | stack effect | notes |
|-----|----------|----------|--------------|-------|
| 155 | U+13421 | `now` | → t | CLOCK_REALTIME nanos |
| 156 | U+13422 | `time` | str fmt → t | fmt `"unix"` (float s) or strptime(3); unparseable: dies |
| 157 | U+13423 | `timef` | t fmt → str | fmt `"unix"` or strftime(3); honors process `TZ` via libc — no thread-local tz context ops |

All calendar arithmetic, durations (`dur`, `durparse`), truncation, and
time-series joins are library code over the i64 (shipped under `mods/`).

**Bloom filter** (tag 17; double-hashed FNV-1a, sized for 1% FP at n).

| idx | glyph cp | mnemonic | stack effect |
|-----|----------|----------|--------------|
| 158 | U+13424 | `bloom` | n → h (n<1: dies) |
| 159 | U+13425 | `badd` | h v → (ints by value, strings by content) |
| 160 | U+13426 | `btest` | h v → 0/1 (1 = maybe, 0 = definitely not) |

**Script I/O** — the missing piece for one-off data scripts.

| idx | glyph cp | mnemonic | stack effect | notes |
|-----|----------|----------|--------------|-------|
| 161 | U+13427 | `slurp` | path → str | whole file as string; not found/unreadable: dies |
| 162 | U+13428 | `spit` | path str → | write (create/truncate); error: dies |
| 163 | U+13429 | `argv` | → list | program argv as list of strings (replaces the `extern"uf_argc"` dance, which still works) |

**Additional data ops (164..171)** — grouping, dedup, and the sort-adjacent
lookups. Each is small (a dict loop or a binary search), composes with the
existing protocol, and needs no iterator protocol.

| idx | glyph cp | mnemonic | stack effect | notes |
|-----|----------|----------|--------------|-------|
| 164 | U+1342A | `group` | list fn_addr → dict | hash group: fn (elem → key); dict maps key → list of elems, insertion-ordered keys |
| 165 | U+1342B | `agg` | dict fn_addr → dict' | map each group's value-list through fn (list → value); fresh dict |
| 166 | U+1342C | `unique` | list → list' | dedup preserving first-occurrence order, O(n) via dict; elems must be dict-hashable (int/str/ptr identity); nested containers by identity |
| 167 | U+1342D | `flat` | list → list' | flatten one level; non-list elements pass through |
| 168 | U+1342E | `chunk` | seq size → list | split list/arr into size-element pieces (last may be short); size<1: dies |
| 169 | U+1342F | `vargsort` | arr → idx_arr | indices that would stably sort the array |
| 170 | U+13343 | `vsearchsorted` | sorted_arr val → idx | binary-search insertion point; unsorted input: undefined |
| 171 | U+13344 | `vwhere` | arr arr bm → arr' | blend: bm bit set → first arr, else second; length mismatch: dies |

Typical script shape: `data 'key group 'summarize agg` — grouping and
aggregation without any SQL-ish machinery.

**Large-data shortcuts (172..178)** — the three primitives that make multi-GB
and graph-scale processing expressible, each wrapping an algorithm that cannot
be written efficiently in µFlux itself. No file-handle object type: every op
is self-contained.

| idx | glyph cp | mnemonic | stack effect | notes |
|-----|----------|----------|--------------|-------|
| 172 | U+13346 | `mmap` | path → str | whole file as a **read-only zero-copy string handle** (tag str, marked non-owned). All string ops work on it: `find`, `match`, `slice`, `split`, `has`. Multi-GB files cost no heap. Write attempts (`set`): dies. Not found/unreadable: dies. The mapping is registered with the GC and `munmap`ed when the handle is swept (or at process exit). |
| 173 | U+13347 | `feach` | path fn_addr → | call fn (line → cont) per line, streamed; stops early when fn returns 0 |
| 174 | U+13348 | `ffold` | path init fn_addr → acc | streaming reduce over lines; fn (acc line → acc) |
| 175 | U+13349 | `fmatch` | path pat → chan | spawn a producer thread streaming regex-matching lines into a fresh chan (cap 64), closed at EOF; composes with `weave` consumers and `deq` loops |
| 176 | U+1334A | `bfs` | start fn_addr → list | breadth-first visit-order list; fn (node → list of neighbors); visited tracked via dict |
| 177 | U+1334B | `dfs` | start fn_addr → list | depth-first pre-order list; same fn contract |
| 178 | U+1334C | `wfind` | start fn_addr pred_addr → v_or_0 | BFS with early exit: first node where pred (node → 0/1) is truthy, else 0 |

The multi-GB log query is then one line — `"app.log" "ERROR.*timeout" fmatch`
plus a `deq` loop — with constant memory, regex matching off the hot path's
producer thread, and the chan giving backpressure for free. `ffold` covers
counts/sums/histograms (`0 'countfold ffold`); `mmap` + `find` covers "find
first/last offset then `slice`" without parsing the file at all.

The graph itself needs no new type: adjacency is a `dict` of node →
neighbor-list, or an implicit fn over any data source (DB rows, `fmatch`
results, object graphs via `keys`/`getq`). For graphs too large for an
exact visited dict, the documented pattern is a `bloom` visited set
(approximate — may skip some re-visits, never loops). "Find conditions on
connected data" = `wfind` with a pred, or `bfs` + `filter`/`some`/`every`.

**JSON (179..180)** — the wire format for curl-gathered data, mapped onto
native containers so the uniform container protocol applies to parsed
documents directly.

| idx | glyph cp | mnemonic | stack effect | notes |
|-----|----------|----------|--------------|-------|
| 179 | U+1334D | `json` | str → v | parse: object → dict (string keys), array → list, string → str, number → int (integral, else float), true/false → 1/0, null → 0. Malformed: dies. |
| 180 | U+1334E | `unjson` | v → str | serialize any native structure; dict keys must be strings; atom/chan/iter/bitmap/bloom: dies |

GB-scale JSON is NDJSON-style streaming, not one giant parse:
`feach`/`ffold` + `json` per line, or `fmatch` → `iter` → `imap 'json`.
HTTP stays out of core: `USE"curl"` via the existing manifest system.

**Iterators — one protocol for every collection (181..185).** An **iter**
(tag 18) is a single-use, mutable cursor. Every collection is *iterable*;
this is the generalization that lets all data sources and sinks compose.

| idx | glyph cp | mnemonic | stack effect | notes |
|-----|----------|----------|--------------|-------|
| 181 | U+1334F | `iter` | h → it | cursor over: list/arr/tensor (elements), dict (keys), str/mmap (bytes as ints), chan (items until close), bitmap (set-bit indices) |
| 182 | U+13350 | `next` | it → v more | next value + flag; exhausted/closed → `0 0`. Non-iter: dies |
| 183 | U+13351 | `collect` | it → list | drain into a fresh list |
| 184 | U+13352 | `imap` | it fn_addr → it' | **lazy** map; fn (v → v') runs per `next` |
| 185 | U+13353 | `ifilter` | it pred_addr → it' | **lazy** filter; pred (v → 0/1) |

Rules: iterators are consumed by use (no `clone` — dies); a chan iterator
ends when the chan is closed and drained; dict iterators yield keys (values
via `getq`). `imap`/`ifilter` compose — `source iter 'clean ifilter 'parse
imap collect` never materializes the intermediate.

**Streaming sinks (186).**

| idx | glyph cp | mnemonic | stack effect | notes |
|-----|----------|----------|--------------|-------|
| 186 | U+13354 | `femit` | path it → n | stream any iterable to a file, one item per line (ints/floats in decimal, strings as-is, everything else `unjson`); returns count written; error: dies |

**Error containment (187..188)** — containment as ops, usable anywhere —
inside weave bodies, around `json` on dirty lines, around curl IMPORT calls —
instead of only at DAG declaration.

| idx | glyph cp | mnemonic | stack effect | notes |
|-----|----------|----------|--------------|-------|
| 187 | U+13355 | `try` | body_addr → result ok | CALL body under die containment, with a stack-depth checkpoint. Body contract: leaves exactly one value. Success → that value + 1. On any `die`: stack restored to entry depth, push `0 0`. |
| 188 | U+13356 | `retry` | n body_addr → result ok | `try` up to n+1 times, stopping at first success |

- Implementation: `die` unwinds to the nearest `setjmp` checkpoint pushed
  by `try`/`retry` (they nest); with no checkpoint, `die` is fatal as
  before. Workers in a fanout task use them in the body — a "tolerant"
  task is simply a body that ends in `try`; nothing is special-cased in
  the DAG.
- No backoff, jitter, or exception typing — count and containment are the
  whole policy. Scripts that need backoff sleep via `sys` nanosleep in the
  body. Distinguishing failure *kinds* is `getq`-style sentinel inspection,
  not typed exceptions.

Example — scrape 10k URLs, 16 workers, 2 retries, skip the hopeless:

```
weave
  urls 16 task fetch 2 'curlget retry drop … endt   ; failed page → 0
  fetch 'ok ifilter … endt                          ; downstream drops 0s
wrun
```

**Detached threads (189).**

| idx | glyph cp | mnemonic | stack effect | notes |
|-----|----------|----------|--------------|-------|
| 189 | U+13357 | `spawn` | body_addr → chan | run body on a detached thread with a fresh `Ctx`; returns a cap-1 chan immediately. At body end, its top-of-stack is enqueued and the chan closed — `deq` on it is a **join**. A body with no result should push a dummy value last. |

Rules: spawned threads are outside the weave DAG and outlive `wrun`
(persistent side channels are the point); coordination is exclusively via
chans, atoms, and the GC-rooted heap — a running spawn's `Ctx` stacks are
GC roots for its lifetime; an uncontained `die` in a spawned thread kills
the process (wrap flaky work in `try`); no cancellation — close the chans
it consumes and let it exit, or structure it around a `deq` loop that ends
on close.

Canonical patterns: a tailer (`'tailf spawn` feeding a chan consumed by a
weave fanout input), a progress reporter (atom counter + spawn loop on
`sys` nanosleep), a slow writer (spawn running `femit` over a chan so the
main line never blocks on disk).

## ASCII tokens (dense)

Eight operators have ASCII spellings; the glyphs above remain accepted for
them. (Historical note: these tokens were introduced in v8 and are unchanged.)

| token | meaning |
|-------|---------|
| `+` | ADD |
| `-` | SUB or number sign, by the lookback rule (above) |
| `*` | MUL |
| `&` | AND |
| `=<label>` | JE to label |
| `<v-run>@` | GETV (push variable) |
| `<v-run>!` | SETV (pop into variable) |
| `'<label>` | ADDR (push code address of label) |

A bare `"..."` string self-evaluates (equivalent to `STR "..."`). A bare `@`
or `!` not directly after a v-run is a lexer error.

## Datastructures

DICT, LIST, CHAN and ATOM are tagged, managed handles (same GC heap as ARR).
Complexities:

- **DICT** — open-addressing hash map, FNV-1a, tombstone deletes, grow at 70%
  load (amortized O(1)). Keys are cells: ints compare by value, pointers
  compare as C strings (pointer keys must be NUL-terminated). Access goes
  through the container protocol: `dict → h`; `h k v set`; `h k get` (dies on
  miss) / `h k getq` (0 on miss); `h k del`; `h len` (count); `h keys → list`.
- **LIST** — growable cell vector, ×2 growth (amortized O(1) append).
  `list → h`; `h v push → h'` (returns the — possibly reallocated — handle);
  `h pop → v` (empty: dies). `get`/`set`/`has`/`len` work on lists.
- **CHAN** — bounded MPSC ring buffer, blocking semantics (mutex + condvars).
  `cap chan → h`; `h v enq` (blocks while full; dies on a closed chan);
  `h deq → v` (blocks while empty; a closed+empty chan yields sentinel `0`);
  `h close →`.
- **ATOM** — atomic i64 cell. `v atom → h`; `h aget → v`; `h v aset`;
  `h n aadd → old`; `h old new cas → 0/1`.

**LEN** is generalized: ARR/TENSOR length, LIST length, DICT count, CHAN
count, ATOM 1, bitmap bit count, str byte count. **TYPEOF h → tag** with the
v10 tag numbering (above). LEN/TYPEOF require a tagged handle (raw BUF/MALLOC
pointers are not tagged; calling LEN/TYPEOF on them is an error).

## Modules: USE and binding manifests

`USE"name"` (𓋹 / `use`): links `-l<name>` into the cc invocation and loads
the binding manifest `<name>.ufm`, searched in `./mods`, `~/.uflux/mods`, then
each dir of `$UFMODPATH`. A manifest is a plain µFlux file (either encoding)
containing IMPORT/EXTERN/STRUCT lines; it is compiled as part of the TU that
USEs it (so manifest STRUCTs are visible there). Ships: `m.ufm`, `c.ufm`,
`pthread.ufm`, `curl.ufm`, `sdl2.ufm`, `ssl.ufm`. Multiple USEs accumulate;
`--emit-c` prints the effective link line as a comment. `-lpthread` is always
linked (weave/chan/atom substrate). pkg-config probing is a documented future
refinement; the compiler always emits `-l<name>`.

## Multiple translation units (MTU)

`uf -c main.uf lib.uf ... -o app`; the final input may also be inline source or
`-` (stdin). The **first** input is the main TU (execution starts at its pc 0;
a TU's top-level flow never falls into the next TU). Per-TU:

- Optional `MOD"name"` header; default is the filename stem (`main` for
  inline/stdin). Glyph v-names and ASCII labels are file-local, variables are
  always file-local, macros are file-local.
- `PUB` before a label def (glyph or ASCII) exports the label to the global
  namespace; CALL/`'` resolve PUB names across TUs. Duplicate PUB = compile
  error. STRUCTs and IMPORT/EXTERN/USE are global (deduped).
- Encodings may be mixed freely (per-TU auto-detection).
- Implementation: per-TU lex+parse, one merged codegen, one C file, one cc
  run. Object-file linking is deferred (see Non-goals).

## Reflection

- `typeof h → tag` (v10 tag table above), `len h → n` (generalized),
  `keys h → list`: dict keys, or an obj's interned field-name strings
  (STRUCT layouts are registered in a runtime table at compile time).
- `OBJ StructName` objects carry a tag header (tag OBJ, struct id); GET/SET
  offsets are relative to the object data (after the header). SET operand
  order is `handle offset value →`; GET is `handle offset → value`. Field
  access by name or by offset both go through the container protocol.
- **CAST is checked**: `cast Type` compares the handle's tag (struct id for
  objects) and dies on mismatch; the handle passes through on success.
- Dynamic dispatch (v9's SEND/METHOD) is **removed**; structs + the container
  protocol cover the same ground with no vtable machinery.

## Garbage collection

v10 replaces v9's no-op arena with a **precise, non-moving, stop-the-world
mark-sweep collector**. Chosen for implementation simplicity and handle
stability (a handle is never invalidated by a collection, which keeps the
FFI, chan buffers, and weave task results trivially safe).

- **Heap objects.** Every tagged object (arr, tensor, list, dict, str, chan,
  atom, obj, bitmap, bloom) is allocated with a GC header (`gc_next` link,
  mark bit) and linked into a global allocation list. Object bodies that
  hold cells (list/dict/arr data, chan rings, obj fields) are scanned for
  children during marking; str/bitmap/bloom bodies are leaf bytes and are
  skipped.
- **Roots** (all precise — cells are tagged, so no conservative scanning):
  each `Ctx`'s data and call stacks (cells with heap tags), all variables,
  weave task results, and chan queue contents. Registers/C-stack values are
  never roots: the interpreter only holds handles inside cells, which is
  already the invariant the v9 `Ctx` refactor established.
- **Untagged pointers** (`malloc`, `buf`) are never traced and never freed
  by the collector — they remain manual (`free`) or process-lifetime. This
  is the documented leak boundary, unchanged from v9.
- **Trigger.** Collection runs when the number of bytes allocated since the
  last collection exceeds a threshold (default: max(1 MiB, 2× live bytes at
  last collection)), and on the explicit `gc` op (50). Threshold is
  adjustable via `UF_GC_THRESHOLD` env var.
- **Concurrency.** Stop-the-world: collection takes a global GC mutex and
  every weave worker parks at its next allocation safepoint. Collections
  also never start mid-`wrun` join. Pause cost is bounded by live-set size;
  scripts that never exceed the threshold never collect.
- **Non-goals:** compaction, generations, incremental/concurrent marking —
  all deferred; non-moving mark-sweep is the permanent floor.

## Concurrency — weave with fanout

```
weave                      ; 𓐍 begin task scope
  task a ... endt          ; 𓐎𓍣 ... 𓐏 — task a, no inputs
  task b ... endt
  a b task c ... endt      ; task c declares inputs a, b (inputs before task, name after)
  pages 8 task fetch ... endt   ; fanout: task fetch, one input, 8 workers
wrun                       ; 𓐐 schedule the DAG, wait, publish results
```

- Static DAG: a task's inputs are edges from tasks of those names, in the same
  weave block. A task may declare **any number of inputs** — arbitrary DAG
  topologies are expressible (the v9 implementation's single-input
  restriction and the old max-8-inputs cap are gone). Unknown input or
  cycle = compile error. Task bodies are
  self-contained: their labels are task-local (two tasks may reuse v-names);
  variables are shared.
- Runtime: worker pool of `min(total declared workers, ncpu)` pthreads plus
  the calling thread. Each task runs with its own fresh data + call stacks;
  inputs are copied in as the task's initial stack in declared order;
  the task's top-of-stack at `endt` is its result.
- After `wrun`, every task's result is readable as `<name>@` (write-once
  publish into the enclosing variable scope), and the **final** task's result
  is also pushed on the stack.
- CHAN/ENQ/DEQ cover streaming; ATOM covers shared counters. No cancellation
  or remote executors (documented non-goals); retry/containment is provided
  by the `try`/`retry` ops inside bodies, not by the DAG.

### Weave fanout — static degree, dynamic distribution

Grammar extension to the v9 task block (no new opcodes):

```
taskblock := <input-name>* [<count>] TASK <name> token* ENDT
```

`<count>` is a numeric literal (l-run or decimal) in the input-name
position, lexically distinguishable from names. `pages 8 task fetch …
endt` = task `fetch`, one input `pages`, **8 workers**.

Semantics:

- A fanout task (count > 1) must have **at least one input**. Only the
  **first** input drives the fanout: it must be iterable at runtime
  (list/arr/tensor/chan/iter per the iterable rule); anything else dies.
  Any **additional inputs are broadcast**: their edge values are copied
  unchanged into every worker's initial stack (see below). Non-fanout
  tasks (no count, or count 1) keep v9 semantics unchanged.
- Distribution is **dynamic**: items of the first input flow into an
  internal bounded chan that the task's workers pull from — cheap workers
  never idle behind slow ones. A chan input is drained until close; other
  iterables until exhaustion.
- Each worker runs the body with its own fresh `Ctx`; its initial stack
  is the item in the first input's position plus the broadcast values of
  the remaining inputs, in declared order (deepest first, as in v9).
  Top-of-stack at `endt` is that item's result.
- The task's published result (`<name>@`, and the stack push if it is the
  final task) is a **list of per-item results in completion order**.
  Downstream that needs input order sorts on a carried index; fanout is
  unordered by design (sequence tagging costs a lock per item — deferred
  until a profile demands it).
- Empty input → empty list. A worker dying kills the program (consistent
  with the v9/v10 error model; no partial results).
- Compile-time validation unchanged otherwise: count is a literal 1..64,
  unknown input or cycle is still a compile error, and the DAG stays
  static — only edge *multiplicity* is dynamic, never topology.
- Scheduling: the pool is `min(total declared workers, ncpu)` threads plus
  the calling thread; fanout workers share it with ordinary tasks.

This subsumes the `pmap` op idea: fanout is expressed in the DAG where it
belongs, sources are any iterable (`fmatch` chan, `mmap` chunks, `bfs`
frontier), and reduction is an ordinary downstream task or `ffold`/`vfold`.

### Fanout integration points (normative)

Where per-item collection-wide multitasking connects to the rest of v10:

1. **Sources** — the fanout-driving (first) input is any iterable (the iterable rule):
   `fmatch`/`shp` chans (live streaming), `mmap`+`chunk` slices (file
   parallelism without parsing), `slurp`+`split`/`rsplit` lists, `range`,
   `bfs` frontiers, `keys` of a dict, drained `imap`/`ifilter` chains, and
   NDJSON lines via `feach`-built lists. Nothing source-side is special.
2. **Bodies** — everything composes inside a worker: lazy iterator chains
   (`'clean ifilter 'parse imap`), `v*`/`ve*` bulk numerics per item,
   `try`/`retry` around flaky operations, nested `group`/`agg`. A worker
   body is ordinary µFlux with one item on the stack.
3. **Sinks** — the published result list feeds ordinary downstream ops:
   `ffold`/`vfold` reduction, `group`/`agg` analytics, `sort`, or `iter
   'rowfmt imap "out.tsv" femit` streaming to file.
4. **Nesting** — a task body may contain its own `weave…wrun` block
   (bodies are self-contained; the inner weave gets its own DAG and shares
   the pool, never oversubscribing beyond `ncpu`). Variables remain shared
   per v9 rules — nested weaves writing shared variables are the script's
   own problem, documented not prevented.
5. **Streaming handoff** — DAG edges are materialized handoffs: a
   downstream task starts when upstream `endt`s. For overlap (producer
   still generating while fanout consumes), the producer must be a
   **detached** op (`fmatch`, `shp`, or a `1 task` that spawns one) whose
   chan is created before the weave and passed as a variable or published
   result. Mid-task early publishing of edges is explicitly not provided —
   detached producers are the one pattern, and the fanout input drains
   their chans until close.
6. **GC interplay** — fanout workers park at allocation safepoints as
   specified in the GC section; item results are rooted by the internal
   result list until `wrun` publishes them.
7. **Introspection** — `UF_WEAVE_DEBUG` lines report declared workers and
   items processed per fanout task, so degree tuning is a feedback loop,
   not a guess.

### Timing introspection

With `UF_WEAVE_DEBUG` set, `wrun` prints one line per task to stderr:
name, wall time, declared workers, items processed (fanout), retries used,
tolerated failures. Compile-time flag-free — the runtime always tracks
counts, the env var only gates printing.

### Wove ideas explicitly not taken

Inheritable/reusable weave templates (reusability machinery — one-off
scripts copy-paste), remote/network executors and backend adapters
(documented non-goal, unchanged), background detachment of whole weaves
(scripts run to completion; `shp`/`fmatch` already detach producers),
async/sync mixing (µFlux is uniformly threads), helper-function library
(lives in `mods/`).

## Shell ops

All take a command string handle (a plain C string pointer, as pushed by a
bare `"..."`). Status is the process exit status (`-1` on spawn failure,
`128+signal` if killed). Platform shell: `/bin/sh -c` on POSIX, `cmd /C` via
`_popen`/`_spawnvp` on Windows (`_WIN32` paths are provided but only POSIX is
exercised by the test suite). v9's SHX and SHL are retired: SH now always
captures, and `split` covers line-splitting.

- **SH** (85): `cmd → out err status`. Runs the command through the platform
  shell, capturing stdout and stderr as two fresh strings (three cells,
  status on top). POSIX: pipe + fork + dup2 + `execle("/bin/sh","-c")` with
  stderr drained through a `tmpfile` (no deadlock). Windows: `_popen` with
  `2><tmpfile>` appended. For fire-and-forget, drop the strings.
- **SHP** (88): `cmd → chan`. Returns a fresh CHAN (cap 64) immediately; a
  detached worker thread feeds stdout line-by-line via ENQ and CLOSEs the
  chan at process exit, so a `DEQ` loop drains it and terminates on the
  closed-chan sentinel `0`. Composes with weave tasks and fanout inputs.
- **EXEC** (89): `list → status`. No shell: the list is the argv (list of
  strings, element 0 = program). POSIX: fork + execvp + waitpid; Windows:
  `_spawnvp(_P_WAIT)`. Dies on an empty list.

## String ops

All results are freshly allocated managed strings or LIST handles. Indices
are **byte** indices. A compact backtracking regex engine (no dependencies)
is embedded in the runtime.

Regex syntax subset:

- literals; `\` escapes the next char (`\\` matches a backslash)
- `.` any char except end-of-string
- `*` `+` `?` greedy quantifiers (with backtracking; a quantified atom never
  loops on an empty match)
- `[...]` char classes: ranges (`a-z`), negation (`[^...]`), `]` first is
  literal, `\` escapes
- `^` anchor at alternative start, `$` anchor at alternative end
- `|` alternation (top level and inside groups)
- `(...)` capture groups, max 9; group 0 is the whole match. Captures under
  a repetition reflect the last iteration. Unmatched groups yield "".

A malformed pattern (unbalanced `(`/`[`, trailing `\`, >9 groups) is a clean
runtime error (`die`), not a crash.

- **MATCH** (90): `str pat → list found`. First match; the list holds group
  strings 0..n (group 0 = whole match); `found` 0/1 on top. On no match the
  list is empty. (Renamed from v9's RX.)
- **REPLACE** (91): `str pat repl → str'`. Replaces ALL matches. `repl`
  supports `\1`..`\9` backrefs (written `\\1`..`\\9` in source strings) and
  `\\` for a literal backslash. In REPLACE/RSPLIT, `^` anchors at the current
  scan position. (Renamed from v9's RXSUB.)
- **RSPLIT** (92): `str pat → list`. Pieces between matches, tail included;
  empty matches are skipped (advancing one char). (Renamed from v9's RXSPLIT.)
- **GLOB** (93): `str pat → 0/1`. fnmatch-style: `*`, `?`, `[...]` (POSIX
  `fnmatch(3)`; a small equivalent matcher is compiled on `_WIN32`, with `!`
  negation).
- **SPLIT** (94): `str sep → list`. Literal separator, tail included; empty
  separator is a runtime error.
- **JOIN** (95): `list sep → str`. Joins a list of strings.
- **SLICE** (96): `seq a b → seq'`. Tag-dispatched (str/arr/list). Python
  slice semantics: negative indices count from the end, both clamped to
  `[0, len]`, `b < a` yields the empty sequence.
- **FIND** (97): `str sub → idx`. Byte index of first occurrence, −1 on miss.
- **REPL** (98): `str old new → str'`. Literal, replace all; empty `old` is
  a runtime error.
- **TRIM** (99): `str → str'`. Strips `isspace` from both ends.
- **UP** (100) / **DOWN** (101): `str → str'`. ASCII case mapping.
- **STARTS** (102) / **ENDS** (103): `str affix → 0/1`.

## Text encoding

Identical syntax and semantics with lowercase ASCII mnemonics,
whitespace-delimited. Detection is described above (any char ≥ U+13000 →
dense).

- Tokens split on whitespace; `;` comments unchanged; `"..."` strings may
  contain spaces. Because tokens are space-delimited, `-` alone is SUB and
  `-5` is a number: **the dense lookback rule does not exist in text mode**.
  Bare decimal/hex/float literals self-evaluate; `lit` stays for type ids and
  explicitness. Type words: `int float ptr byte void handle str bool`.
- Names: label def `name:`, refs `jmp name` / `'name` (or `addr name`),
  variables `name!` / `name@` — any identifier, not just v-slots. Opcode
  mnemonics are reserved words; using one as a label/variable = compile error.
- Mnemonic table (normative): the mnemonic column of the opcode tables above
  (all names lowercased, with `drop` for DRP and `swp` for SWP). ASCII ops
  unchanged: `+ - * & = @ ! '`.
- **Round-trip emitters**: `uf --emit-text` (dense → text) and
  `uf --emit-dense` (text → dense), from the shared token AST. Conversion
  mode: `uf --to-text prog.uf` writes `prog.uft`, `uf --to-dense prog.uft`
  writes `prog.uf` (same emitters, extension-derived output name; `-o`
  overrides). The dense
  emitter assigns fresh v-slots to text identifiers deterministically by first
  use (names of the form `v<N>` keep slot N); the text emitter spells glyph
  names as `v<N>`. Lossless modulo identifier spelling. Example: the matmul
  line `𓆦𓆤'𓎠𓂾` ↔ `128 'fi for`.

## PRINT and SCAN

- **PRINT** (54): `args… fmt → n`. The format string handle is **on top** of
  the stack with the args below it (deepest arg first — the reverse of the
  CALL-printf convention where the fmt is deepest). Pops the fmt, pops one arg
  per `%` conversion, prints, pushes the printf return value. `%%` prints `%`.
- **SCAN** (55): `fmt → values… count`. Pops the fmt; for each conversion
  reads stdin with fscanf semantics: `%d/%i/%u/%x/%o` → i64 (read via `%lld`),
  `%f/%e/%g` → f64 (always read via `%lf` into a double), `%s` → a freshly
  allocated string handle. Values are pushed in conversion order, then the
  count is pushed. Any input error aborts the program. Literal (non-`%`,
  non-space) text in the format is unsupported and is a runtime error.

## Grammar (concrete)

Whitespace/comments are skipped between tokens. Chat delimiters U+13100..13108
are stripped anywhere.

```
program      := token*
token        := number | string | varset | varget | labeldef | jump | op | directive
number       := l-number (grammar above)            // self-evaluating
string       := '"' ... '"'                          // self-evaluating (PushS)
varset       := v-run ('!' | SETV-glyph)
varget       := v-run ('@' | GETV glyph)
labeldef     := v-run | ASCII-name ':'
jump         := (JMP|JZ|JE|CALL glyph | '=' | "'") (v-run | ASCII-name)
op           := opcode glyph | '+' | '-' | '*' | '&'
directive    := IMPORT c"name"(params)->ret | EXPORT "name" | EXTERN "sym"
             | MACRO name { token* } | STRUCT name { field:type, ... }
             | USE "name" | MOD "name" | PUB <labeldef>
             | WEAVE taskblock* WRUN
LIT          := LIT (ASCII number | type keyword | type glyph | l-number)
immediate    := (OBJ|CAST|ARR|TENSOR|SIZEOF) (type glyph | type keyword)?
FOR/SYS      := address/arity stack operands and immediates as in v7
taskblock    := <input-name>* [<count>] TASK <name> token* ENDT
count        := numeric literal 1..64 (fanout degree; >1 ⇒ first input fans out, rest broadcast)
```

Structured control flow (IF/IFELSE/WHILE) and the quotation-taking ops
(TRY/RETRY, FILTER/SOME/EVERY, VMAP/VFOLD, IMAP/IFILTER, FEACH/FFOLD,
BFS/DFS/WFIND, SPAWN, GROUP/AGG, fanout bodies) take label **addresses** on
the stack — written `'<label>` (ADDR) in source — and are resolved by the
compiler like any other label reference. `break`/`cont` are valid only
inside a lexically enclosing `while`/`for` body in the same function
(compile error otherwise).

## Codegen notes (uf)

Pipeline: tokens → parser (labels/macros/structs/imports) → C with a
computed-goto threaded interpreter → cc. CLI modes: **default is compile and
run** — `uf prog.uf` or `uf 'SOURCE'` compiles to a cached binary under
`${TMPDIR:-/tmp}/uflux-cache/<fnv64-of-sources+version+links>` (created by the
OS's temp-dir policy) and executes it; a cache hit skips compilation entirely.
**`-c` is compile-only** (the old ufc behavior): `uf -c prog.uf -o out`.
Emitters work in either mode: `--emit-c` dumps the C (including the effective
link line as a comment), `--emit-text` / `--emit-dense` run the encoding
emitters. The first positional argument is a file if it exists, otherwise it
is compiled as inline source. `-lpthread` is always linked. Everything after
a `--` separator is forwarded as the program's argv (run mode only;
`uf prog.uft -- arg1 arg2`).

Runtime cells are **16 bytes**: `{tag, i64}` (was `{tag, i64, f64, ptr}`).
Pointer payloads live in `i` (cast at use sites); FLOAT cells store the
double **bit pattern** in `i` — `uf_f()` converts tag-aware for mixed
int/float arithmetic, and JZ keeps its historical truthiness
(`(int64_t)value == 0`). Behavior-visible edge: `%d`-printing or int-casting
a FLOAT cell now reads the bit pattern instead of a synchronised truncation.
Tagged objects (ARR/TENSOR/LIST/DICT/STR/CHAN/ATOM/BUF/OBJ/BITMAP/BLOOM)
have a `{tag, len, esz}` header (byte arrays esz=1, else esz=8; OBJ len =
struct id), preceded by the GC header described in "Garbage collection"
(GC header → object header → data). `set` stores with the
`handle key value` order. Execution state (data
stack, call stack) lives in a `Ctx` so weave tasks and spawned threads each
get a fresh one.

v10 codegen changes:

- **GC safepoints**: allocations go through a GC-aware allocator that may
  trigger collection; weave/fanout workers park at allocation safepoints
  (see "Garbage collection"). Generated code keeps handles only inside
  cells — never in unrooted C locals across an allocation.
- **Structured control flow is compiler-resolved**: IF/IFELSE/WHILE take
  quotation addresses like FOR; the compiler emits direct static control
  flow (gotos/direct C loops) for them wherever the address is a
  compile-time known label, so they cost the same as hand-written jumps.
  `break`/`cont` compile to jumps to the enclosing loop's end/continue
  edge via the per-function loop stack.
- **Container protocol dispatch**: `get`/`set`/`del`/`has`/`keys` compile
  to a tag switch in the runtime; no per-type opcodes remain.
- Vector ops (`v*`/`ve*`) are plain C loops over 64-aligned arr data,
  autovectorized by cc -O2; `-march=native` for the runtime cache is a
  build-time decision, not a spec requirement.

Dispatch performance work (semantics-preserving, carried over from v9):

- **Basic-block stack virtualization**: within a straight-line block (between
  jump targets and control-flow instructions), ds pushes/pops become C
  locals; the virtual stack is spilled to the real ds, in order, before any
  instruction that is a jump target, transfers control, or needs the real
  stack — so the ds state at every block boundary is exactly the naive one.
  Fused ops use the same `uf_c*` helpers as the `op_*` functions.
- **Deferred variable stores**: within a block, SETV writes a temp cache and
  GETV reads from it; dirty temps are stored back to their globals at the
  same flush points (distinct var globals cannot alias, so this is
  unobservable inside the block).
- **Sparse labtab**: only labels that can be entered dynamically (initial pc,
  FOR/IF/WHILE bodies via PUSHADDR, weave task entries, spawned bodies,
  exports) appear
  in the computed-goto table; all other labels are reached by direct gotos
  only, so cc -O2 optimizes across static control flow. Together with the
  slim cell this closes most of the interpreter overhead (fib-class loops
  run at ~0.5–2× of equivalent -O2 C).
- **FOR inlining**: a FOR immediately preceded by ADDR of a compile-time
  known label, whose body is structurally inlinable (ends at its first RET,
  no early RETs, no internal jump leaving the body range), is emitted as a
  direct C `for` loop over a *renamed inline copy* of the body instructions
  (labels and K_ continuations get an `F<site>_` prefix so internal jumps
  stay local; CALL targets, which are out-of-range subroutine labels, stay
  unprefixed). Nested FORs inline recursively (depth cap 8, then fallback).
  The iteration index is still pushed on the real data stack per iteration,
  so stack behavior matches the subroutine path exactly; the ADDR feeding
  the FOR is elided (unless control can jump onto it). Any other FOR —
  computed address, unusual layout — keeps the subroutine machinery.

Foreign-interface details:

- IMPORT declares an unprototyped C function; args are cast at the call site
  (i64/double/ptr/char by the declared param types). The generated C aliases
  the symbol (`__asm__`), so importing libc names already prototyped by the
  prelude's includes (e.g. `printf`) is safe. Varargs: declare the fixed
  params then `...` (e.g. `import c"printf"(ptr,...)->int`); the call
  convention is **format string deepest, varargs above it** (the runtime
  locates the fmt by its `%` count — `%*` consumes two args, matching C —
  max 8 varargs).
- IMPORT return types map to the true C ABI type: `->int` is a C `int`
  (32-bit) and the result is **sign-extended** into the 64-bit cell, so
  libc functions returning negative `int`s (e.g. `fgetc`'s `EOF`,
  `strcmp`) compare correctly.
- EXTERN pushes the address of a global symbol (via `__asm__` alias), for
  use with LOADX/STOREX. The runtime exposes two such globals:
  `int64_t uf_argc` and `void* uf_argv` (the process `char**argv`), so
  `extern"uf_argc" loadx` is argc and
  `extern"uf_argv" loadx <i*8> + loadx` is argv[i]. (The `argv` op (163) is
  the preferred v10 spelling; the extern dance still works.)

## Composability contract (normative)

v10's ops are designed around structural commonalities; these rules are part
of the spec, not conventions:

1. **The iterable rule.** Everywhere a "seq" operand appears (`sort`,
   `filter`, `some`, `every`, `group`, `chunk`, `unique`, `flat`, `femit`),
   any iterable collection *or* an iter is accepted; iters are drained.
   `collect` bridges into the materialized world when a random-access
   container is needed.
2. **The container-protocol rule.** Anything parsed (`json`), built
   (`dict`, `obj`), or gathered (`group` results) is accessed exclusively
   through `get`/`getq`/`set`/`del`/`has`/`keys`/`len` — no per-type
   accessors ever.
3. **The typed-fast-path rule.** Bulk numeric work happens on arr/tensor
   through the `v*`/`ve*` ops; bitmaps are the mask currency between them
   (`veq`…`vgather`, `vwhere`, `vand`/`vor`/`vnot`). `collect` + `arr`
   conversion moves data from the iterable world to the typed world
   (`collect 'int arr` is the documented pattern — ARR takes a length, so
   `collect len arr bufcopy`-style library glue, shipped in `mods/`).
4. **The chan bridge rule.** Every producer that outlives its caller
   (`fmatch`, `shp`) emits a chan; every consumer composes via `iter` over
   that chan; `weave` tasks exchange chans and publish results through
   `wrun` variables, and fanout tasks drain any iterable — chans included —
   across their workers. Backpressure is always the chan's bound, never a
   buffer the script manages.
5. **The string interchange rule.** Strings, mmap'd files, and `unjson`
   output are all tag str — one set of ops (`find`/`match`/`slice`/`split`)
   serves files, memory, and wire data alike.

The canonical v10 pipeline, end to end:

```
USE"curl"
… gather …                       ; curl IMPORTs, pages of NDJSON
"raw.nd" spit                    ; or straight into processing
"raw.nd" ffold                   ; streaming fold: json per line,
                                 ; group/agg in the accumulator
… 'key group 'summ agg …         ; relational analytics
sort iter 'rowfmt imap           ; lazy render
"out.tsv" femit                  ; stream to file
```

## Totals

- 191 opcode slots (0..190); 18 indices retired (16 v9 + `vslice`/`vconcat`,
  subsumed by tag-dispatched `slice`/`cat`); **173 live opcodes**.
- 85 new; 80 kept unchanged; 8 kept with renames/semantic changes
  (get/set, push, sh, match/replace/rsplit, typeof, gc, cat, slice).
- Every live opcode has a unique dense glyph codepoint (tables above).

## Non-goals (documented, deferred from v10)

B-tree/trie/skiplist/R-tree/zone-map/suffix-array index structures (the
iterator protocol removes the design blocker; they remain library
candidates in `mods/`, promoted to opcodes only if profiling demands),
dataframe ops (pivot/partition/columnar — need record semantics), HTTP
clients in core (`USE"curl"` by design), Roaring bitmaps (dense words
until profiling objects), generational/incremental/compacting GC,
async I/O, object-file linking, pkg-config probing, hand-written SIMD
intrinsics (autovectorization first; `-march=native` for the runtime cache
is a codegen-note decision), remote weave executors, fanout sequence
tagging (completion-order results are by design until a profile demands
input order), MSP/mobile targets.

## History / changes

- **v7 → v8** (encoding rework):
  - Opcode glyphs hand-picked; U+13000+i remain deprecated
    aliases for the first 56.
  - v-space moved to U+13362.. (was U+13080.. slot space, reclaimed for
    opcodes).
  - l-space (U+133A4..) numbers are self-evaluating; LIT no longer required.
  - 8 ASCII operator tokens added; bare strings self-evaluate.
  - Label definitions need no colon; `'` is ADDR, `=` is JE, `@`/`!` are
    GETV/SETV.
  - `-` is SUB or sign by the (dense-only) lookback rule.
  - SETI operand order changed to `handle idx value`.
  - New opcodes PRINT (54) and SCAN (55).
  - Type glyphs double as immediates after type-expecting opcodes.
- **v8 → v9** (features):
  - 29 new opcodes (56..84): DICT/LIST/CHAN/ATOM datastructure families,
    TYPEOF/LEN/FIELDS/METHOD reflection, USE/MOD/PUB modules, WEAVE/TASK/
    ENDT/WRUN dataflow concurrency.
  - SET operand order fixed to `handle offset value`; SEND is real
    (vtable dispatch); CAST is checked.
  - Multiple translation units: one `uf` invocation compiles and merges
    several files; `MOD`/`PUB` control naming and export.
  - USE binding manifests (`mods/*.ufm`) link C libraries.
  - Second source encoding (text) with round-trip emitters `--emit-text` /
    `--emit-dense`.
  - Runtime refactor: execution state in `Ctx`; object headers carry a tag.
- **v9 → v10** (uniform protocol, real GC, data-processing core).
  **Not backward compatible with v9.**
  - **Uniform container protocol**: `get`/`set`/`del`/`has`/`keys` (+ `getq`,
    `len`, `typeof`) dispatch on the handle's tag and replace the per-type
    accessor ops — IDX/SETI, DGET/DPUT/DDEL/DCOUNT/DKEYS, and FIELDS are
    removed (indices retired).
  - **173 live opcodes in 191 slots (0..190)**; 18 indices retired and never
    reused.
  - **Real garbage collection**: precise, non-moving, stop-the-world
    mark-sweep replaces the v9 no-op arena; `gc` (50) now forces a
    collection. Scripts never `free` managed objects.
  - **Structured control flow**: `if`/`ifelse`/`while`/`break`/`cont` as
    compiler-resolved ops; raw jumps remain.
  - **Weave fanout**: `pages 8 task fetch … endt` — static worker count,
    dynamic distribution over any iterable input, per-item results in
    completion order; `UF_WEAVE_DEBUG` timing introspection; `try`/`retry`
    error containment ops; `spawn` detached threads.
  - **Iterators**: `iter`/`next`/`collect`/`imap`/`ifilter` — one lazy
    protocol over every collection, chans and mmap'd files included.
  - **JSON**: `json`/`unjson` mapped onto native dict/list containers.
  - **New data ops**: `group`/`agg`/`unique`/`flat`/`chunk`, `sort`/`filter`/
    `some`/`every`, `vargsort`/`vsearchsorted`/`vwhere`, the `v*`/`ve*`
    vector family with bitmap masks, `bloom`, `now`/`time`/`timef`,
    `slurp`/`spit`/`argv`, `mmap`/`feach`/`ffold`/`fmatch` streaming file
    ops, `bfs`/`dfs`/`wfind` graph traversal, `femit` streaming sink,
    arithmetic/logic ops (`div`/`rem`/`eq`/`lt`/`gt`/`not`/`or`/`xor`/`shl`/
    `bnot`).
  - **Renames/changes**: `append` → `push`; `rx`/`rxsub`/`rxsplit` →
    `match`/`replace`/`rsplit`; `swap` mnemonic → `swp`; `sh` merged with
    SHX (always captures; SHX/SHL retired); `cat`/`slice` are
    tag-dispatched; `typeof` uses the renumbered tags (list/dict/chan/str
    first-class; bitmap/time/dur/bloom/iter new).
  - **Removed**: SEND/METHOD dynamic dispatch; VEC/PIN/UNPIN/BUFPTR no-op
    placeholders; the deprecated v7 glyph aliases U+13000..U+13037.

## C → µFlux transpiler (trans/)

`trans/trans.uf` is a working C-subset → µFlux transpiler, written in µFlux
(text encoding) itself. It lexes and parses a C source file (regex-based
lexer, recursive-descent parser) and prints a text-encoding µFlux program
to stdout, which `uf` then compiles. The supported subset, the fixed libc
IMPORT preamble it emits, and the `tests/` adaptations of GNU coreutils
programs (`true`, `false`, `echo`, `yes`, `wc`) are documented in
`trans/README.md` and `trans/tests/README.md`.
