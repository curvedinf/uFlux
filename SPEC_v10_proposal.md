# µFlux Specification v10 (proposal, v2)

Status: **proposal**, normative-in-waiting for `comp/`. Supersedes both SPEC.md
(v9) and the `uflux_v10_191_spec.md` draft. **v10 is intentionally not
backward compatible with v9** — the project is undeployed and developmental,
so this revision optimizes the language itself rather than the migration path.

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

## Encoding

Unchanged from v9: dense hieroglyph encoding and whitespace-delimited text
encoding, auto-detected (any char ≥ U+13000 → dense), round-trip emitters,
l-space numbers, v-space names, the `-` lookback rule (dense only), ASCII
operator tokens. `.uf`/`.uft` conventions unchanged. The deprecated v7
sequential glyph aliases (U+13000..U+13037) are **removed**.

## Type tags (renumbered)

0 int, 1 float, 2 ptr, 3 byte, 4 void, 5 arr, 6 tensor, 7 list, 8 dict,
9 str, 10 chan, 11 atom, 12 buf, 13 obj, 14 bitmap, 15 time, 16 dur,
17 bloom.

Changes from v9: the DYN/MAP/RING names are gone (they are `list`/`dict`/
`chan`); str/chan/atom are first-class tags; bitmap/time/dur/bloom are new.

## The uniform container protocol

Six ops cover all element access, dispatched on the handle's tag:

| op | stack | dict | list/arr/tensor | str | obj |
|----|-------|------|-----------------|-----|-----|
| `get`   | h k → v        | value (missing key: dies) | elem at idx (OOB: dies) | byte at idx | field by name or offset |
| `get?`  | h k → v_or_0   | 0 on miss | 0 on OOB | 0 on OOB | 0 on missing field |
| `set`   | h k v →        | put | idx set (OOB: dies) | byte set | field set (missing: dies) |
| `del`   | h k →          | remove key (tombstone) | — | — | — |
| `has`   | h k → 0/1      | key present | idx in bounds | substring found | field exists |
| `keys`  | h → list       | keys | — | — | field names |

`len` (generalized, as v9) and `typeof` complete the protocol. A null (0)
handle returns 0 from `get?`/`has`, dies elsewhere. This removes v9's IDX,
SETI, DGET, DPUT, DDEL, DCOUNT, DKEYS, and FIELDS — eight opcodes collapse
into behavior of the protocol.

## Complete opcode table (normative)

Surviving v9 opcodes keep their index and glyph. Removed indices are
retired, not reused. New opcodes are 104..163; their codepoints come from
the free blocks U+1332A..U+1333F and U+13400..U+1342F (assignments are 1:1
and final for tooling, with glyph *visuals* subject to one mnemonics review
pass — every op has a unique codepoint from day one, so dense source is
never blocked).

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

### Removed from v9 (indices retired)

- **22 SEND, 77 METHOD** — dynamic dispatch removed. Structs + the container
  protocol cover scripts; vtables were the most machinery per use in v9.
- **24 IDX, 25 SETI, 57–61 DGET/DPUT/DDEL/DCOUNT/DKEYS, 76 FIELDS** —
  subsumed by the container protocol (`get`/`set`/`del`/`len`/`keys`).
- **30 VEC, 31 PIN, 32 UNPIN, 39 BUFPTR, 50 GC** — runtime no-ops. The
  vector ops below make VEC's intent real; the rest were placeholders.
- **86 SHX, 87 SHL** — merged into the new `sh` (`split` covers SHL).
- The v7 glyph aliases U+13000..U+13037.

### New opcodes (104..163)

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
| 119 | U+13339 | `get?` | h k → v_or_0 | never dies on absence; wrong container kind dies |
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
| 132 | U+13406 | `vadd2` | arr arr → arr' (length mismatch: dies) |
| 133 | U+13407 | `vsub2` | arr arr → arr' |
| 134 | U+13408 | `vmul2` | arr arr → arr' |
| 135 | U+13409 | `vdiv2` | arr arr → arr' (any 0 divisor: dies) |
| 136 | U+1340A | `vmax2` | arr arr → arr' |
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
| 151 | U+1341D | `vslice` | arr start stop → arr' (Python semantics, clamped) |
| 152 | U+1341E | `vconcat` | arr arr → arr' |
| 153 | U+1341F | `vmap` | arr fn_addr → arr' | elementwise fn (elem → elem); covers every unary math fn via `USE"m"` |
| 154 | U+13420 | `vfold` | arr init fn_addr → acc | generic reduction; fn (acc elem → acc) |

`vmap`/`vfold` are what let this family stay at 27 ops instead of 51:
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

## Totals

- 164 opcode slots (0..163); 18 v9 indices retired; **146 live opcodes**.
- 60 new; 86 kept unchanged; 5 kept with renames/semantic changes.
- Every live opcode has a unique dense glyph codepoint (tables above).

## Deferred (explicit non-goals for v10)

Real GC (still the top candidate for v11), B-tree/trie/skiplist/R-tree/
zone-map/suffix-array index structures (need an iterator protocol RFC
first; library candidates), dataframe ops (pivot/partition/columnar —
need record semantics), Roaring bitmaps (dense words until profiling
objects), async I/O, object-file linking, pkg-config probing, hand-written
SIMD intrinsics (autovectorization first).

## Open questions

1. Glyph visuals for U+1332A.. and U+13400.. assignments (one review pass).
2. Should `sort` on dict keys / `keys` ordering guarantee anything? Current
   answer: no ordering guarantee on `keys`.
3. Whether `while`'s cond-quotation contract (leaves exactly one value)
   gets compile-time stack verification in straight-line cases.
