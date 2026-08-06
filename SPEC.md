# µFlux Specification v9 (this repo)

Based on the µFlux Whitepaper v7.0 (`µFlux_Whitepaper_v7.0.pdf`), with the v8
encoding rework and the v9 feature set (library modules, op-native
datastructures, multiple translation units, reflection, wove-style dataflow
concurrency, and a second, human-readable text encoding) merged into one
document. This document is normative for `comp/` (the `uf` compiler). The
`trans/` transpiler targets the **v9 text encoding** (see the final section).

Concrete decisions carried over:

1. **Source files use the extension `.uf`** (dense encoding) or `.uft` (text
   encoding, conventional only).
2. **Dense tokens are single Egyptian-hieroglyph codepoints**, one glyph per
   token. Whitespace is insignificant except where it separates two glyph runs
   that would otherwise fold into one. `;` starts a comment to end of line. A
   `.uf` file carries **one header comment block only**; the body is dense
   glyph source.
3. **Operand orders:** `SETI` is `handle idx value →` (breaking change from
   v7's `value idx handle`; `IDX` is unchanged at `handle idx → elem`). `SET`
   is `handle offset value →` (mirrors SETI; v7's SET order was broken and
   unused).

## Encodings and detection

µFlux source exists in two encodings with identical syntax and semantics:

- **Dense** — the hieroglyph token stream described above.
- **Text** — lowercase ASCII mnemonics, whitespace-delimited (see "Text
  encoding" below).

**Detection:** any char ≥ U+13000 in the input → dense; else text. `.uft` is a
conventional extension, not required. Inline source and `-` (stdin) use the
same detection; MTU files may mix encodings freely (per-TU auto-detection).

## Unicode spaces (dense)

| space | range | contents |
|-------|-------|----------|
| opcodes | hand-picked glyphs in U+13000–U+1342F (table below) | 104 opcodes |
| v-space | U+13362..U+133A3 | variable/label name atoms, runs fold |
| l-space | U+133A4..U+133E3 | base-64 digit atoms (self-evaluating numbers) |
| delimiters | U+13100..U+13108 | chat-template delimiters, stripped pre-compilation |
| type glyphs | U+13110..U+13117 | int, float, ptr, byte, void, handle, str, bool |

The opcode glyphs are disjoint from v-space, l-space, the delimiters and the
type glyphs. The v7 sequential assignment (U+13000 + index) is retained as
**deprecated aliases** for the first 56 opcodes (U+13000..U+13037); new code
uses the custom glyphs.

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
SUB MUL AND SHR INC DEC GET IDX CLONE CAST ARR TENSOR OBJ CAT FMT BUF MALLOC
LOADX SIZEOF OFFSET PRINT SCAN CALL SYS.

Non-pushing tokens: program start, label definitions, LIT/STR themselves (their
operand follows), jumps (JMP/JZ/JE/`=`), SETV/SET/SETI/SEND/STOREX/BUFCOPY/FREE/DRP
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
in all the same positions. Reflection contexts (CAST/METHOD/SEND type names)
additionally accept container type names mapping to tags: `arr` 5, `tensor` 6,
`dyn`/`list` 7, `map`/`dict` 8, `ring`/`chan` 9, `atom` 10, `str` 11, `obj` 13.

## Opcode table (normative, indices 0..103)

Glyphs were chosen for mnemonic value; the rationale column records the
intended association. The text mnemonic is the reserved word used by the text
encoding and the `--emit-text` emitter.

| idx | glyph | cp | name | mnemonic | stack effect | rationale |
|-----|-------|----|------|----------|--------------|-----------|
| 0  | 𓍀 | U+13340 | LIT | `lit` | → v (immediate follows) | papyrus scroll — a written constant |
| 1  | 𓁐 | U+13050 | DUP | `dup` | a → a a | pair — duplicate |
| 2  | 𓁑 | U+13051 | OVR | `ovr` | a b → a b a | second of a pair |
| 3  | 𓂹 | U+130B9 | DRP | `drop` | a → | falling — drop |
| 4  | 𓅡 | U+13161 | SWP | `swap` | a b → b a | exchange |
| 5  | 𓂩 | U+130A9 | PICK | `pick` | … n → elem (copies nth-deep) | hand — pick |
| 6  | 𓂝 | U+1309D | ADD | `add` | a b → a+b | arm (D36); ASCII `+` preferred |
| 7  | 𓂞 | U+1309E | SUB | `sub` | a b → a−b | arm variant (D37); ASCII `-` preferred |
| 8  | 𓂡 | U+130A1 | MUL | `mul` | a b → a*b | arms; ASCII `*` preferred |
| 9  | 𓂢 | U+130A2 | AND | `and` | a b → a&b | arms joined; ASCII `&` preferred |
| 10 | 𓃗 | U+130D7 | SHR | `shr` | a → a>>1 (logical) | shift right |
| 11 | 𓂥 | U+130A5 | INC | `inc` | a → a+1 | arm raised |
| 12 | 𓂦 | U+130A6 | DEC | `dec` | a → a−1 | arm lowered |
| 13 | 𓂻 | U+130BB | JMP | `jmp` | (label operand) | legs (D54) |
| 14 | 𓂼 | U+130BC | JZ | `jz` | cond → (jump if 0) | legs variant |
| 15 | 𓂽 | U+130BD | JE | `je` | a b → (jump if equal) | legs variant; ASCII `=` preferred |
| 16 | 𓂾 | U+130BE | FOR | `for` | count addr → (pushes k per iteration) | legs in a loop |
| 17 | 𓃀 | U+130C0 | CALL | `call` | (label operand; args/ret by convention) | foot (D58) |
| 18 | 𓂿 | U+130BF | RET | `ret` | (returns to call site) | legs returning |
| 19 | 𓉐 | U+13250 | OBJ | `obj` | → h (type/struct immediate) | house (O1) |
| 20 | 𓁷 | U+13077 | GET | `get` | h offset → value | eye (D6) |
| 21 | 𓁸 | U+13078 | SET | `set` | h offset value → | eye variant |
| 22 | 𓂐 | U+13090 | SEND | `send` | args… recv method_id → result | mouth (D26) |
| 23 | 𓉑 | U+13251 | ARR | `arr` | len → h (type immediate) | house row |
| 24 | 𓁶 | U+13076 | IDX | `idx` | h idx → elem | eye (D5) |
| 25 | 𓁺 | U+1307A | SETI | `seti` | h idx value → | eye storing |
| 26 | 𓄎 | U+1310E | CLONE | `clone` | h → h' | copy |
| 27 | 𓄪 | U+1312A | CAST | `cast` | h type → h (dies on mismatch) | mold |
| 28 | 𓄳 | U+13133 | MACRO | `macro` | (compile-time directive) | — |
| 29 | 𓉓 | U+13253 | TENSOR | `tensor` | len → h (type immediate; 64-aligned) | house grid |
| 30 | 𓄜 | U+1311C | VEC | `vec` | (no-op at runtime) | fast |
| 31 | 𓉔 | U+13254 | PIN | `pin` | (no-op at runtime) | — |
| 32 | 𓉕 | U+13255 | UNPIN | `unpin` | (no-op at runtime) | — |
| 33 | 𓂤 | U+130A4 | SETV | `setv` | value → (into variable) | hand storing; ASCII `<v-run>!` preferred |
| 34 | 𓁻 | U+1307B | GETV | `getv` | → value (from variable) | eye fetching; ASCII `<v-run>@` preferred |
| 35 | 𓂋 | U+1308B | STR | `str` | → handle (bare `"..."` preferred) | mouth (D21) |
| 36 | 𓂌 | U+1308C | CAT | `cat` | a b → handle (concat) | mouth joined |
| 37 | 𓂍 | U+1308D | FMT | `fmt` | args… fmt → handle | mouth shaping |
| 38 | 𓉖 | U+13256 | BUF | `buf` | size → ptr | house store |
| 39 | 𓉗 | U+13257 | BUFPTR | `bufptr` | (no-op at runtime) | — |
| 40 | 𓉘 | U+13258 | BUFCOPY | `bufcopy` | dst src n → | — |
| 41 | 𓁼 | U+1307C | ADDR | `addr` | → code address (label operand) | pointing; ASCII `'<label>` preferred |
| 42 | 𓁹 | U+13079 | LOADX | `loadx` | addr → value | eye loading |
| 43 | 𓁽 | U+1307D | STOREX | `storex` | value addr → | — |
| 44 | 𓁾 | U+1307E | SIZEOF | `sizeof` | type → n (1 for byte/bool, else 8) | measure |
| 45 | 𓁿 | U+1307F | OFFSET | `offset` | → n (compile-time `Struct.field`) | — |
| 46 | 𓉙 | U+13259 | STRUCT | `struct` | (compile-time directive) | house plan |
| 47 | 𓉛 | U+1325B | MALLOC | `malloc` | size → ptr (raw, untagged) | house build |
| 48 | 𓉜 | U+1325C | FREE | `free` | ptr → | broken house |
| 49 | 𓉠 | U+13260 | SYS | `sys` | args… num → ret (arity immediate) | gate |
| 50 | 𓉝 | U+1325D | GC | `gc` | (no-op; arena collector is future work) | sweeping |
| 51 | 𓉞 | U+1325E | IMPORT | `import` | (compile-time directive) | door in |
| 52 | 𓉟 | U+1325F | EXPORT | `export` | (compile-time directive) | door out |
| 53 | 𓉢 | U+13262 | EXTERN | `extern` | → address of global symbol (operand follows) | — |
| 54 | 𓂎 | U+1308E | PRINT | `print` | args… fmt → n | mouth printing |
| 55 | 𓂀 | U+13080 | SCAN | `scan` | fmt → values… count | eye reading (D4) |
| 56 | 𓊵 | U+132B5 | DICT | `dict` | → h | basket — keyed container |
| 57 | 𓂁 | U+13081 | DGET | `dget` | h k → v found | eye into the basket |
| 58 | 𓂂 | U+13082 | DPUT | `dput` | h k v → | storing into the basket |
| 59 | 𓂃 | U+13083 | DDEL | `ddel` | h k → | removing from the basket |
| 60 | 𓂄 | U+13084 | DCOUNT | `dcount` | h → n | counting the basket |
| 61 | 𓂅 | U+13085 | DKEYS | `dkeys` | h → list | listing the basket |
| 62 | 𓊶 | U+132B6 | LIST | `list` | → h | basket row — growable vector |
| 63 | 𓂧 | U+130A7 | APPEND | `append` | h v → h' | arm adding |
| 64 | 𓂨 | U+130A8 | POP | `pop` | h → v | hand taking |
| 65 | 𓊷 | U+132B7 | CHAN | `chan` | cap → h | vessel — channel/ring |
| 66 | 𓂺 | U+130BA | ENQ | `enq` | h v → (blocks while full) | legs entering |
| 67 | 𓃁 | U+130C1 | DEQ | `deq` | h → v (blocks while empty) | foot leaving |
| 68 | 𓉣 | U+13263 | CLOSE | `close` | h → | door shut |
| 69 | 𓋰 | U+132F0 | ATOM | `atom` | v → h | the indivisible |
| 70 | 𓂆 | U+13086 | AGET | `aget` | h → v | eye reading the atom |
| 71 | 𓂇 | U+13087 | ASET | `aset` | h v → | eye writing the atom |
| 72 | 𓂟 | U+1309F | AADD | `aadd` | h n → old | arm adding to the atom |
| 73 | 𓂠 | U+130A0 | CAS | `cas` | h old new → 0/1 | arms exchanging |
| 74 | 𓄫 | U+1312B | TYPEOF | `typeof` | h → tag | identify the mold |
| 75 | 𓄬 | U+1312C | LEN | `len` | h → n | measure length |
| 76 | 𓉚 | U+1325A | FIELDS | `fields` | obj → list | house fields |
| 77 | 𓂑 | U+13091 | METHOD | `method` | (compile-time method registration) | mouth declaring |
| 78 | 𓋹 | U+132F9 | USE | `use` | (compile-time directive) | bringing in a library |
| 79 | 𓋸 | U+132F8 | MOD | `mod` | (compile-time directive) | naming the unit |
| 80 | 𓋷 | U+132F7 | PUB | `pub` | (compile-time directive) | making public |
| 81 | 𓐍 | U+1340D | WEAVE | `weave` | (begin task scope) | interlacing threads |
| 82 | 𓐎 | U+1340E | TASK | `task` | (begin task body) | one thread of work |
| 83 | 𓐏 | U+1340F | ENDT | `endt` | (end task body) | thread end |
| 84 | 𓐐 | U+13410 | WRUN | `wrun` | → result (final task's) | threads run |
| 85 | 𓆉 | U+13189 | SH | `sh` | cmd → status | turtle — carries a shell |
| 86 | 𓆊 | U+1318A | SHX | `shx` | cmd → out err status | turtle variant — shell with capture |
| 87 | 𓆋 | U+1318B | SHL | `shl` | cmd → list | turtle variant — shell to list |
| 88 | 𓈗 | U+13217 | SHP | `shp` | cmd → chan | water — streaming shell |
| 89 | 𓆣 | U+131A3 | EXEC | `exec` | list → status | scarab — direct exec |
| 90 | 𓌜 | U+1331C | RX | `rx` | str pat → list found | knife — regex cut |
| 91 | 𓌝 | U+1331D | RXSUB | `rxsub` | str pat repl → str' | knife — regex replace |
| 92 | 𓌞 | U+1331E | RXSPLIT | `rxsplit` | str pat → list | knife — regex split |
| 93 | 𓌟 | U+1331F | GLOB | `glob` | str pat → 0/1 | blade — filename match |
| 94 | 𓌠 | U+13320 | SPLIT | `split` | str sep → list | blade — cut apart |
| 95 | 𓌡 | U+13321 | JOIN | `join` | list sep → str | blades tied — join together |
| 96 | 𓌢 | U+13322 | SLICE | `slice` | str a b → str' | knife slice — substring |
| 97 | 𓌣 | U+13323 | FIND | `find` | str sub → idx | tool — seek substring |
| 98 | 𓌤 | U+13324 | REPL | `repl` | str old new → str' | tool — replace |
| 99 | 𓌥 | U+13325 | TRIM | `trim` | str → str' | knife — shave ends |
| 100 | 𓌦 | U+13326 | UP | `up` | str → str' | raised tool — uppercase |
| 101 | 𓌧 | U+13327 | DOWN | `down` | str → str' | lowered tool — lowercase |
| 102 | 𓌨 | U+13328 | STARTS | `starts` | str affix → 0/1 | head tool — prefix test |
| 103 | 𓌩 | U+13329 | ENDS | `ends` | str affix → 0/1 | tail tool — suffix test |

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

DICT, LIST, CHAN and ATOM are tagged, managed handles (same arena as ARR).
Complexities:

- **DICT** — open-addressing hash map, FNV-1a, tombstone deletes, grow at 70%
  load (amortized O(1)). Keys are cells: ints compare by value, pointers
  compare as C strings (pointer keys must be NUL-terminated).
  `DICT → h`; `DPUT h k v →`; `DGET h k → v found` (two cells, found flag on
  top, `0 0` on miss); `DDEL h k →`; `DCOUNT h → n`; `DKEYS h → list`.
- **LIST** — growable cell vector, ×2 growth (amortized O(1) append).
  `LIST → h`; `APPEND h v → h'` (returns the — possibly reallocated — handle);
  `POP h → v`. IDX/SETI/LEN work on lists.
- **CHAN** — bounded MPSC ring buffer, blocking semantics (mutex + condvars).
  `CHAN cap → h`; `ENQ h v →` (blocks while full; dies on a closed chan);
  `DEQ h → v` (blocks while empty; a closed+empty chan yields sentinel `0`);
  `CLOSE h →`.
- **ATOM** — atomic i64 cell. `ATOM v → h`; `AGET h → v`; `ASET h v →`;
  `AADD h n → old`; `CAS h old new → 0/1`.

**LEN** is generalized: ARR/TENSOR length, LIST length, DICT count, CHAN
count, ATOM 1. **TYPEOF h → tag** with tags: 5 ARR, 6 TENSOR, 7 DYN, 8 MAP,
9 RING, 10 ATOM, 11 STR, 12 BUF, 13 OBJ (0..4 remain the scalar type ids).
LEN/TYPEOF require a tagged handle (raw BUF/MALLOC pointers are not tagged;
calling LEN/TYPEOF on them is an error).

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
  run. Object-file linking is a possible v10 step.

## Reflection

- `TYPEOF h → tag` (tag table above), `LEN h → n` (generalized),
  `FIELDS obj → list` of interned field-name strings (STRUCT layouts are
  registered in a runtime table at compile time). `DKEYS` doubles as map
  reflection.
- `OBJ StructName` objects carry a tag header (tag OBJ, struct id); GET/SET
  offsets are relative to the object data (after the header). SET operand
  order is `handle offset value →` (mirrors SETI; v7's order was broken and
  unused). GET is `handle offset → value`.
- **SEND is real**: `METHOD TypeName:` on a label registers it as method
  `<label>` for the type (struct name → struct id, or a container/scalar type
  name). `SEND args… recv method_id → result` pops the method id and the
  receiver, pushes the receiver back as self, and dispatches through the
  method table; unknown method = runtime error. `SEND "name"` / `SEND name`
  pushes the interned (FNV-1a) method hash as an immediate; otherwise the id
  comes from the stack. The method name is the label name. Method-table type
  keys are `1000 + struct id` for structs and the HT_* tag for containers.
- **CAST is checked**: `CAST Type` compares the handle's tag (struct id for
  objects) and dies on mismatch; the handle passes through on success.

## Concurrency — simplified wove

```
weave                      ; 𓐍 begin task scope
  a task ... endt          ; 𓍣 𓐎 ... 𓐏 — task a, no inputs
  b task ... endt
  a b c task ... endt      ; task c declares inputs a, b (names before task)
wrun                       ; 𓐐 schedule the DAG, wait, publish results
```

- Static DAG: a task's inputs are edges from tasks of those names, in the same
  weave block. Unknown input or cycle = compile error. Task bodies are
  self-contained: their labels are task-local (two tasks may reuse v-names);
  variables are shared.
- Runtime: worker pool of `min(ntasks, ncpu)` pthreads plus the calling
  thread. Each task runs with its own fresh data + call stacks; inputs are
  copied in as the task's initial stack in declared order (max 8 inputs); the
  task's top-of-stack at `endt` is its result.
- After `wrun`, every task's result is readable as `<name>@` (write-once
  publish into the enclosing variable scope), and the **final** task's result
  is also pushed on the stack.
- RING/ENQ/DEQ cover streaming; ATOM covers shared counters. No cancellation,
  retries, timeouts, or remote executors (documented future work).

## Shell ops

All take a command string handle (a plain C string pointer, as pushed by a
bare `"..."`). Status is the process exit status (`-1` on spawn failure,
`128+signal` if killed). Platform shell: `/bin/sh -c` on POSIX, `cmd /C` via
`_popen`/`_spawnvp` on Windows (`_WIN32` paths are provided but only POSIX is
exercised by the test suite).

- **SH** (85): `cmd → status`. Runs the command through the platform shell
  with stdio inherited.
- **SHX** (86): `cmd → out err status`. Captures stdout and stderr as two
  fresh strings (three cells, status on top). POSIX: pipe + fork + dup2 +
  `execle("/bin/sh","-c")` with stderr drained through a `tmpfile` (no
  deadlock). Windows: `_popen` with `2><tmpfile>` appended.
- **SHL** (87): `cmd → list`. Captures stdout, split into a LIST of line
  strings (no trailing newlines; `\r\n` tolerated). Dies on spawn failure,
  not on nonzero exit.
- **SHP** (88): `cmd → chan`. Returns a fresh CHAN (cap 64) immediately; a
  detached worker thread feeds stdout line-by-line via ENQ and CLOSEs the
  chan at process exit, so a `DEQ` loop drains it and terminates on the
  closed-chan sentinel `0`. Composes with weave tasks.
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

- **RX** (90): `str pat → list found`. First match; the list holds group
  strings 0..n (group 0 = whole match); `found` 0/1 on top. On no match the
  list is empty.
- **RXSUB** (91): `str pat repl → str'`. Replaces ALL matches. `repl`
  supports `\1`..`\9` backrefs (written `\\1`..`\\9` in source strings) and
  `\\` for a literal backslash. In RXSUB/RXSPLIT, `^` anchors at the current
  scan position.
- **RXSPLIT** (92): `str pat → list`. Pieces between matches, tail included;
  empty matches are skipped (advancing one char).
- **GLOB** (93): `str pat → 0/1`. fnmatch-style: `*`, `?`, `[...]` (POSIX
  `fnmatch(3)`; a small equivalent matcher is compiled on `_WIN32`, with `!`
  negation).
- **SPLIT** (94): `str sep → list`. Literal separator, tail included; empty
  separator is a runtime error.
- **JOIN** (95): `list sep → str`. Joins a list of strings.
- **SLICE** (96): `str a b → str'`. Python slice semantics: negative indices
  count from the end, both clamped to `[0, len]`, `b < a` yields "".
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
- Mnemonic table (normative): the mnemonic column of the opcode table above
  (all names lowercased, with `drop` for DRP). ASCII ops unchanged:
  `+ - * & = @ ! '`.
- **Round-trip emitters**: `uf --emit-text` (dense → text) and
  `uf --emit-dense` (text → dense), from the shared token AST. The dense
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
op           := opcode glyph (custom or legacy alias) | '+' | '-' | '*' | '&'
directive    := IMPORT c"name"(params)->ret | EXPORT "name" | EXTERN "sym"
             | MACRO name { token* } | STRUCT name { field:type, ... }
             | USE "name" | MOD "name" | PUB <labeldef>
             | WEAVE taskblock* WRUN | METHOD TypeName: <labeldef>
LIT          := LIT (ASCII number | type keyword | type glyph | l-number)
immediate    := (OBJ|CAST|ARR|TENSOR|SIZEOF) (type glyph | type keyword)?
FOR/SYS      := address/arity stack operands and immediates as in v7
taskblock    := <input-name>* TASK <name> token* ENDT
```

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
Tagged objects (ARR/TENSOR/DYN/MAP/RING/ATOM/STR/BUF/OBJ) have a
`{tag, len, esz}` header (byte arrays esz=1, else esz=8; OBJ len = struct
id). SETI stores with the `handle idx value` order. Execution state (data
stack, call stack) lives in a `Ctx` so weave tasks each get a fresh one.

Dispatch performance work (semantics-preserving):

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
  FOR bodies via PUSHADDR, weave task entries, SEND methods, exports) appear
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
  `extern"uf_argv" loadx <i*8> + loadx` is argv[i].

## Non-goals (documented)

Real GC (collector still a no-op arena — the top v10 candidate), async
I/O/netpoller, panic/defer error containment, object-file linking, remote
weave executors, weave retries/timeouts, MSP/mobile targets, pkg-config
probing for USE.

## History / changes

- **v7 → v8** (encoding rework):
  - Opcode glyphs hand-picked (table above); U+13000+i remain deprecated
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
  - SET operand order fixed to `handle offset value` (v7's was broken and
    unused); SEND is real (vtable dispatch); CAST is checked.
  - Multiple translation units: one `uf` invocation compiles and merges
    several files; `MOD`/`PUB` control naming and export.
  - USE binding manifests (`mods/*.ufm`) link C libraries.
  - Second source encoding (text) with round-trip emitters `--emit-text` /
    `--emit-dense`.
  - Runtime refactor: execution state in `Ctx`; object headers carry a tag.

## C → µFlux transpiler (trans/)

`trans/trans.uf` is a working C-subset → µFlux transpiler, written in µFlux
(v9 text encoding) itself. It lexes and parses a C source file (regex-based
lexer, recursive-descent parser) and prints a v9 text-encoding µFlux program
to stdout, which `uf` then compiles. The supported subset, the fixed libc
IMPORT preamble it emits, and the `tests/` adaptations of GNU coreutils
programs (`true`, `false`, `echo`, `yes`, `wc`) are documented in
`trans/README.md` and `trans/tests/README.md`.
