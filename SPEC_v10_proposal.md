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
6. **No memory management.** A real garbage collector reclaims everything
   the runtime allocates. Scripts never call `free` on managed objects
   (manual `malloc`/`free` remains for raw FFI buffers only).

## Encoding

Unchanged from v9: dense hieroglyph encoding and whitespace-delimited text
encoding, auto-detected (any char ≥ U+13000 → dense), round-trip emitters,
l-space numbers, v-space names, the `-` lookback rule (dense only), ASCII
operator tokens. `.uf`/`.uft` conventions unchanged. The deprecated v7
sequential glyph aliases (U+13000..U+13037) are **removed**.

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
  (GC (50) stays — it is now a real collector, see below.)
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
| 153 | U+1341F | `vmap` | arr fn_addr → arr' | elementwise fn (elem → elem); covers every unary math fn via `USE"m"` |
| 154 | U+13420 | `vfold` | arr init fn_addr → acc | generic reduction; fn (acc elem → acc) |

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

## Additional data ops (164..171)

The highest-value ops pulled back from the deferred list — grouping,
dedup, and the sort-adjacent lookups. Each is small (a dict loop or a
binary search), composes with the existing protocol, and needs no iterator
protocol. The rest of the deferred list (index structures, pivot/partition
dataframe ops, streaming file ops) stays deferred.

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

## Large-data shortcuts (172..178)

The three primitives that make multi-GB and graph-scale processing
expressible — each wraps an algorithm that cannot be written efficiently in
µFlux itself. No file-handle object type: every op is self-contained.

**Zero-copy file access.** `mmap` resolves the lifetime question via the
GC: the mapping is registered with the collector and `munmap`ed when the
handle is swept (or at process exit).

| idx | glyph cp | mnemonic | stack effect | notes |
|-----|----------|----------|--------------|-------|
| 172 | U+13346 | `mmap` | path → str | whole file as a **read-only zero-copy string handle** (tag str, marked non-owned). All string ops work on it: `find`, `match`, `slice`, `split`, `has`. Multi-GB files cost no heap. Write attempts (`set`): dies. Not found/unreadable: dies. |

**Streaming** — buffered, chunked at line boundaries, early-exit capable.

| idx | glyph cp | mnemonic | stack effect | notes |
|-----|----------|----------|--------------|-------|
| 173 | U+13347 | `feach` | path fn_addr → | call fn (line → cont) per line, streamed; stops early when fn returns 0 |
| 174 | U+13348 | `ffold` | path init fn_addr → acc | streaming reduce over lines; fn (acc line → acc) |
| 175 | U+13349 | `fmatch` | path pat → chan | spawn a producer thread streaming regex-matching lines into a fresh chan (cap 64), closed at EOF; composes with `weave` consumers and `deq` loops |

The multi-GB log query is then one line —
`"app.log" "ERROR.*timeout" fmatch` plus a `deq` loop — with constant
memory, regex matching off the hot path's producer thread, and the chan
giving backpressure for free. `ffold` covers counts/sums/histograms
(`0 'countfold ffold`); `mmap` + `find` covers "find first/last offset
then `slice`" without parsing the file at all.

**Graph traversal** — visited-set dedup is the part you can't write
efficiently in script code (it needs the dict's probe loop in C).

| idx | glyph cp | mnemonic | stack effect | notes |
|-----|----------|----------|--------------|-------|
| 176 | U+1334A | `bfs` | start fn_addr → list | breadth-first visit-order list; fn (node → list of neighbors); visited tracked via dict |
| 177 | U+1334B | `dfs` | start fn_addr → list | depth-first pre-order list; same fn contract |
| 178 | U+1334C | `wfind` | start fn_addr pred_addr → v_or_0 | BFS with early exit: first node where pred (node → 0/1) is truthy, else 0 |

The graph itself needs no new type: adjacency is a `dict` of node →
neighbor-list, or an implicit fn over any data source (DB rows, `fmatch`
results, object graphs via `keys`/`getq`). For graphs too large for an
exact visited dict, the documented pattern is a `bloom` visited set
(approximate — may skip some re-visits, never loops). "Find conditions on
connected data" = `wfind` with a pred, or `bfs` + `filter`/`some`/`every`.

## JSON (179..180)

The wire format for curl-gathered data, mapped onto native containers so
the uniform container protocol applies to parsed documents directly.

| idx | glyph cp | mnemonic | stack effect | notes |
|-----|----------|----------|--------------|-------|
| 179 | U+1334D | `json` | str → v | parse: object → dict (string keys), array → list, string → str, number → int (integral, else float), true/false → 1/0, null → 0. Malformed: dies. |
| 180 | U+1334E | `unjson` | v → str | serialize any native structure; dict keys must be strings; atom/chan/iter/bitmap/bloom: dies |

GB-scale JSON is NDJSON-style streaming, not one giant parse:
`feach`/`ffold` + `json` per line, or `fmatch` → `iter` → `imap 'json`.
HTTP stays out of core: `USE"curl"` via the existing manifest system.

## Iterators — one protocol for every collection (181..185)

An **iter** (tag 18) is a single-use, mutable cursor. Every collection is
*iterable*; this is the generalization that lets all data sources and
sinks compose.

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

## Streaming sinks (186)

| idx | glyph cp | mnemonic | stack effect | notes |
|-----|----------|----------|--------------|-------|
| 186 | U+13354 | `femit` | path it → n | stream any iterable to a file, one item per line (ints/floats in decimal, strings as-is, everything else `unjson`); returns count written; error: dies |

## Detached threads (189)

Persistent side-channel threads, generalizing the internal workers of
`shp`/`fmatch` into a script-visible op.

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

## Composability contract (normative)

v10's ops are designed around four structural commonalities; these rules
are part of the spec, not conventions:

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

## Weave fanout — static degree, dynamic distribution

Grammar extension to the v9 task block (no new opcodes):

```
taskblock := <input-name>* [<count>] TASK <name> token* ENDT
```

`<count>` is a numeric literal (l-run or decimal) in the input-name
position, lexically distinguishable from names. `pages 8 task fetch …
endt` = task `fetch`, one input `pages`, **8 workers**.

Semantics:

- **Multiple inputs for every task.** Any task — fanout or not — may
  declare any number of inputs: edges from that many upstream tasks,
  copied in as the task's initial stack in declared order. The v9
  implementation's single-input restriction (and the spec's old
  max-8-inputs cap) is gone; this is what makes arbitrary DAG topologies
  expressible. Cycles and unknown inputs remain compile errors; the DAG
  itself stays fully static.
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

### Error containment ops (187..188)

Generalized from wove's retry/tolerance task policy: containment as ops,
usable anywhere — inside weave bodies, around `json` on dirty lines, around
curl IMPORT calls — instead of only at DAG declaration.

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

### Timing introspection

With `UF_WEAVE_DEBUG` set, `wrun` prints one line per task to stderr:
name, wall time, declared workers, items processed (fanout), retries used,
tolerated failures. Compile-time flag-free — the runtime always tracks
counts, the env var only gates printing. (Wove's single most quoted
feature is its timing table; this is the one-stderr-line-per-task version.)

### Wove ideas explicitly not taken

Inheritable/reusable weave templates (reusability machinery — one-off
scripts copy-paste), remote/network executors and backend adapters
(documented v9 non-goal, unchanged), background detachment of whole weaves
(scripts run to completion; `shp`/`fmatch` already detach producers),
async/sync mixing (µFlux is uniformly threads), helper-function library
(lives in `mods/`).

### Fanout integration points (normative)

Where per-item collection-wide multitasking connects to the rest of v10:

1. **Sources** — the fanout-driving (first) input is any iterable (the
   iterable rule):
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

## Totals

- 191 opcode slots (0..190); 18 indices retired (16 v9 + `vslice`/`vconcat`,
  subsumed by tag-dispatched `slice`/`cat`); **173 live opcodes**.
- 85 new; 80 kept unchanged; 8 kept with renames/semantic changes
  (get/set, push, sh, match/replace/rsplit, typeof, gc, cat, slice).
- Every live opcode has a unique dense glyph codepoint (tables above).

## Deferred (explicit non-goals for v10)

B-tree/trie/skiplist/R-tree/zone-map/suffix-array index structures (the
iterator protocol above removes the design blocker; they remain library
candidates in `mods/`, promoted to opcodes only if profiling demands),
dataframe ops (pivot/partition/columnar — need record semantics), HTTP
clients in core (`USE"curl"` by design), Roaring bitmaps (dense words
until profiling objects), generational/incremental GC, async I/O,
object-file linking, pkg-config probing, hand-written SIMD intrinsics
(autovectorization first; `-march=native` for the runtime cache is a
codegen-note decision).

## Open questions

1. Glyph visuals for U+1332A.. and U+13400.. assignments (one review pass).
2. Should `sort` on dict keys / `keys` ordering guarantee anything? Current
   answer: no ordering guarantee on `keys`.
3. Whether `while`'s cond-quotation contract (leaves exactly one value)
   gets compile-time stack verification in straight-line cases.
