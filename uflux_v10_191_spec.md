# µFlux v10 Complete Specification
# ================================
# Total: 191 ops
# Core: 72 | Data: 51 | Search/Index: 38 | Search Algorithms: 15 | Time: 15

---

## CORE OPS (72)

### Stack (5)

| Op | Glyph | Stack | Description | Edge Cases |
|----|-------|-------|-------------|------------|
| `drop` | 𓂹 | `a →` | Discard top value | |
| `dup` | 𓁐 | `a → a a` | Duplicate top value | |
| `ovr` | 𓁑 | `a b → a b a` | Copy second-from-top to top | |
| `pick` | 𓂩 | `… n → elem` | Copy nth-deep element to top | n < 0 or n >= depth: dies |
| `swp` | 𓅡 | `a b → b a` | Swap top two values | |

### Math (8)

| Op | Glyph | Stack | Description | Edge Cases |
|----|-------|-------|-------------|------------|
| `add` | 𓂝 | `a b → a+b` | Addition | |
| `sub` | 𓂞 | `a b → a-b` | Subtraction | |
| `mul` | 𓂡 | `a b → a*b` | Multiplication | |
| `div` | — | `a b → a/b` | Division; int truncates toward zero, float f64 div | div by zero: dies |
| `mod` | — | `a b → a%b` | Modulo; sign follows dividend (C semantics) | mod by zero: dies |
| `pow` | — | `a b → a**b` | Power; `pow(2,3)=8`, `pow(2,-1)=0.5` | negative base^non-integer: dies |
| `shr` | 𓃗 | `a → a>>1` | Logical shift right by 1 | |
| `and` | 𓂢 | `a b → a&b` | Bitwise AND | float/pointer: dies |

### Compare & Logic (4)

| Op | Glyph | Stack | Description | Edge Cases |
|----|-------|-------|-------------|------------|
| `eq` | — | `a b → 0/1` | Equality; scalars by value, strings by strcmp, pointers by identity | floats: exact bit equality |
| `lt` | — | `a b → 0/1` | Less than; numeric or string lexical | pointers: dies |
| `gt` | — | `a b → 0/1` | Greater than; numeric or string lexical | pointers: dies |
| `not` | — | `a → 0/1` | Logical negation; 1 if a==0, else 0 | handles: 1 if null (0), else 0 |

### Bitwise (4)

| Op | Glyph | Stack | Description | Edge Cases |
|----|-------|-------|-------------|------------|
| `or` | — | `a b → a|b` | Bitwise OR | float/pointer: dies |
| `shl` | — | `a b → a<<b` | Shift left; logical | b<0 or b>=64: dies; float/pointer: dies |
| `xor` | — | `a b → a^b` | Bitwise XOR | float/pointer: dies |
| `bnot` | — | `a → ~a` | Bitwise NOT (ones complement) | float/pointer: dies |

### Control (9)

| Op | Glyph | Stack | Description | Edge Cases |
|----|-------|-------|-------------|------------|
| `if` | — | `cond body_addr →` | Structured if; compiler emits jz. Execute body if cond nonzero | |
| `else` | — | `body_addr →` | Paired with if; else branch | |
| `while` | — | `cond_addr body_addr →` | Structured while; compiler emits labels. Loop while cond nonzero | |
| `for` | 𓂾 | `count body_addr →` | Counted loop; pushes index k (0..count-1) per iteration | count < 0: dies |
| `break` | — | `→` | Jump to nearest loop end | outside loop: dies |
| `continue` | — | `→` | Jump to loop start | outside loop: dies |
| `jmp` | 𓂻 | `addr →` | Raw unconditional jump; macro/compiler internal | |
| `ret` | 𓂿 | `value →` | Return to call site; value on stack is return value | |
| `call` | 𓃀 | `args… addr → result` | Call function at address | |

### Variables (2)

| Op | Glyph | Stack | Description | Edge Cases |
|----|-------|-------|-------------|------------|
| `!` (SETV) | 𓂤 | `value →` | Pop into named variable (global/module-level) | |
| `@` (GETV) | 𓁻 | `→ value` | Push named variable value | undefined name: dies |

### Containers (7)

| Op | Glyph | Stack | Description | Edge Cases |
|----|-------|-------|-------------|------------|
| `obj` | 𓉐 | `→ h` | Create struct instance; type immediate follows | |
| `arr` | 𓉑 | `len → h` | Create fixed-size typed array; 64-byte aligned; type immediate follows | len < 0: dies |
| `list` | 𓊶 | `→ h` | Create empty growable vector | |
| `dict` | 𓊵 | `→ h` | Create empty hash map (FNV-1a, open addressing, 70% grow) | |
| `str` | 𓂋 | `→ h` | Create empty string; bare `"…"` self-evaluates | |
| `buf` | 𓉖 | `size → ptr` | Create raw arena buffer (untagged) | |
| `chan` | 𓊷 | `cap → h` | Create bounded MPSC ring buffer (mutex + condvars) | cap < 1: dies |

### Access (7)

| Op | Glyph | Stack | Description | Edge Cases |
|----|-------|-------|-------------|------------|
| `get` | 𓁷 | `container key → value` | Polymorphic: dict key lookup, list index, object field name | type mismatch: dies |
| `set` | 𓁸 | `container key value →` | Polymorphic: dict put, list index set, object field set | OOB list index: dies; missing obj field: dies |
| `len` | 𓄬 | `h → n` | Generalized length: arr/list/dict count/chan count/str bytes/atom 1 | untagged handle: dies |
| `keys` | — | `h → list` | Dict keys (list of strings), object field names (list of strings) | untagged handle: dies |
| `push` | 𓂧 | `h v → h'` | Append to list; returns possibly reallocated handle | non-list: dies |
| `pop` | 𓂨 | `h → v` | Remove and return last element from list | empty list: dies; non-list: dies |
| `del` | 𓂃 | `h key →` | Delete key from dict | non-dict: dies |

### New Core Container (5)

| Op | Glyph | Stack | Description | Edge Cases |
|----|-------|-------|-------------|------------|
| `unpack` | — | `list n → v1…vn rest_list` | Destructure first n elements; push remainder as new list | n > len(list): dies; n must be compile-time literal |
| `get?` | — | `container key → value_or_0` | Optional get; dict missing key→0, list OOB→0, obj missing field→0 | null handle (0) container: returns 0; string container: dies |
| `orelse` | — | `a b → a_or_b` | Coalescing; returns a if truthy (nonzero/non-null), else b | NOT short-circuit; both evaluated |
| `has` | — | `container key → 0/1` | Membership test; dict key exists, list index in bounds, string substring found, obj field exists | null container: 0 |
| `find` | — | `list 'pred → value_or_0` | First element where pred returns truthy; pred contract: `elem → 0/1` | empty list: 0; no match: 0 |

### New Core Sequence (6)

| Op | Glyph | Stack | Description | Edge Cases |
|----|-------|-------|-------------|------------|
| `some` | — | `list 'pred → 0/1` | Any element matches; short-circuits on first match | empty list: 0 |
| `every` | — | `list 'pred → 0/1` | All elements match; short-circuits on first non-match | empty list: 1 (vacuous truth) |
| `flat` | — | `list → list` | Flatten one level; non-list elements preserved | `[[[1]]]` → `[[1]]` |
| `unique` | — | `list → list` | Deduplicate preserving first-occurrence order; O(n) via dict | elements must be hashable (int/string/ptr); float: dies; nested containers: identity hash |
| `range` | — | `start stop → list` | Integer sequence [start, stop); step=1; empty if start>=stop | float inputs: truncated toward zero |
| `sort` | — | `list → list` | Return new sorted list; Timsort (stable); elements must be comparable | incomparable types: dies |

### String (15)

| Op | Glyph | Stack | Description | Edge Cases |
|----|-------|-------|-------------|------------|
| `cat` | 𓂌 | `a b → handle` | String concatenation | |
| `fmt` | 𓂍 | `args… fmt → handle` | Format string; fmt on top, args below (deepest first) | |
| `print` | 𓂎 | `args… fmt → n` | Print to stdout; returns printf return value | |
| `scan` | 𓂀 | `fmt → values… count` | Read from stdin; fscanf semantics; values pushed in conversion order, count on top | input error: aborts |
| `match` | — | `str pat → list found` | Regex match; list holds capture groups 0..n; found flag 0/1 on top | no match: empty list, 0 |
| `replace` | — | `str pat repl → str'` | Regex replace all; `\1`..`\9` backrefs; `\` for literal backslash | malformed pattern: dies |
| `split` | — | `str pat → list` | Auto-detect: metachars in pat → regex split, else literal; empty matches skipped | empty separator: dies |
| `join` | — | `list sep → str` | Join list of strings with separator | non-string elements: dies |
| `slice` | — | `str a b → str'` | Python slice: neg indices count from end, clamped to [0,len], b<a → "" | |
| `find` | — | `str sub → idx` | Byte index of first substring occurrence; -1 on miss | |
| `trim` | — | `str → str'` | Strip isspace from both ends | |
| `up` | — | `str → str'` | ASCII uppercase | |
| `down` | — | `str → str'` | ASCII lowercase | |
| `starts` | — | `str affix → 0/1` | Prefix test | |
| `ends` | — | `str affix → 0/1` | Suffix test | |

### Shell (3)

| Op | Glyph | Stack | Description | Edge Cases |
|----|-------|-------|-------------|------------|
| `sh` | 𓆉 | `cmd → str` | Run command via /bin/sh -c; return stdout as string; stderr inherited | spawn failure: returns "" |
| `shp` | 𓈗 | `cmd → chan` | Shell to channel; detached worker feeds stdout line-by-line; closes chan at exit | |
| `exec` | 𓆣 | `list → status` | No shell execution; list is argv (element 0 = program); fork+execvp+waitpid | empty list: dies |

### Concurrency (12)

| Op | Glyph | Stack | Description | Edge Cases |
|----|-------|-------|-------------|------------|
| `weave` | 𓐍 | `→` | Begin task scope; static DAG validation | |
| `task` | 𓐎 | `input_names… name →` | Begin named task body; inputs are edges from prior tasks in same weave | unknown input or cycle: compile error |
| `endt` | 𓐏 | `→` | End task body; top-of-stack is task result | |
| `wrun` | 𓐐 | `→ result` | Schedule DAG, wait, publish results; final task result pushed | |
| `enq` | 𓂺 | `h v →` | Enqueue value; blocks while full; dies on closed chan | non-chan: dies |
| `deq` | 𓃁 | `h → v` | Dequeue value; blocks while empty; closed+empty → sentinel 0 | non-chan: dies |
| `close` | 𓉣 | `h →` | Close channel | |
| `atom` | 𓋰 | `v → h` | Create atomic i64 cell with initial value v | |
| `aget` | 𓂆 | `h → v` | Atomic read | non-atom: dies |
| `aset` | 𓂇 | `h v →` | Atomic write | non-atom: dies |
| `aadd` | 𓂟 | `h n → old` | Atomic add; returns old value | non-atom: dies |
| `cas` | 𓂠 | `h old new → 0/1` | Compare-and-swap; returns 1 if swapped, 0 if not | non-atom: dies |

### Type (2)

| Op | Glyph | Stack | Description | Edge Cases |
|----|-------|-------|-------------|------------|
| `typeof` | 𓄫 | `h → tag` | Return type tag: 0 int, 1 float, 2 ptr, 3 byte, 4 void, 5 arr, 6 list, 7 dict, 8 chan, 9 atom, 10 str, 11 buf, 12 obj, 13 time, 14 dur, 15 bitmap, 16 tree, 17 trie, 18 bloom, 19 rtree, 20 skiplist, 21 ttree, 22 sparse, 23 zonemap, 24 suffix | untagged handle: dies |
| `sizeof` | 𓁾 | `type → n` | Size in bytes: 1 for byte, 8 for others; void=8 | |

### Module (4)

| Op | Glyph | Stack | Description | Edge Cases |
|----|-------|-------|-------------|------------|
| `use` | 𓋹 | `→` | Load binding manifest; links -l<name>; searches ./mods, ~/.uflux/mods, $UFMODPATH | manifest not found: compile error |
| `mod` | 𓋸 | `→` | Name translation unit; optional header | |
| `pub` | 𓋷 | `→` | Export following label to global namespace | duplicate pub: compile error |
| `extern` | 𓉢 | `sym → fn_ptr` | Push C function pointer; symbol operand | symbol not found: link error |

### Misc (6)

| Op | Glyph | Stack | Description | Edge Cases |
|----|-------|-------|-------------|------------|
| `clone` | 𓄎 | `h → h'` | Deep copy handle | |
| `cast` | 𓄪 | `h type → h` | Checked cast; compare tag, die on mismatch; handle passes through on success | |
| `addr` | 𓁼 | `→ code_addr` | Push label address; `'<label>` preferred ASCII | |
| `loadx` | 𓁹 | `addr → value` | Load from code address | |
| `storex` | 𓁽 | `value addr →` | Store to code address | |
| `struct` | 𓉙 | `→` | Compile-time directive; define struct layout with field:type pairs | |

---

## DATA MANIPULATION OPS (51)

### Vectorized Broadcast Math (15)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `vadd` | `arr scalar → arr` | SIMD broadcast add: each element + scalar | type mismatch: dies; non-arr: dies |
| `vsub` | `arr scalar → arr` | SIMD broadcast sub | type mismatch: dies; non-arr: dies |
| `vmul` | `arr scalar → arr` | SIMD broadcast mul | type mismatch: dies; non-arr: dies |
| `vdiv` | `arr scalar → arr` | SIMD broadcast div | div by zero in any element: dies; type mismatch: dies; non-arr: dies |
| `vabs` | `arr → arr` | SIMD absolute value | non-arr: dies |
| `vneg` | `arr → arr` | SIMD negation | non-arr: dies |
| `vsqrt` | `arr → arr` | SIMD square root | negative element: dies; non-arr: dies |
| `vlog` | `arr → arr` | SIMD natural log | non-positive element: dies; non-arr: dies |
| `vexp` | `arr → arr` | SIMD e^x | overflow: inf; non-arr: dies |
| `vsin` | `arr → arr` | SIMD sine | non-arr: dies |
| `vcos` | `arr → arr` | SIMD cosine | non-arr: dies |
| `vround` | `arr → arr` | SIMD round to nearest even | non-arr: dies |
| `vfloor` | `arr → arr` | SIMD floor | non-arr: dies |
| `vceil` | `arr → arr` | SIMD ceiling | non-arr: dies |
| `vclip` | `arr low high → arr` | SIMD clamp: min(max(elem, low), high) | low > high: dies; non-arr: dies |

### Vectorized Element-Wise Math (5)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `vadd2` | `arr arr → arr` | Element-wise add | length mismatch: dies; type mismatch: dies |
| `vsub2` | `arr arr → arr` | Element-wise sub | length mismatch: dies; non-arr: dies |
| `vmul2` | `arr arr → arr` | Element-wise mul | length mismatch: dies; non-arr: dies |
| `vdiv2` | `arr arr → arr` | Element-wise div | length mismatch: dies; div by zero: dies; non-arr: dies |
| `vmax2` | `arr arr → arr` | Element-wise max | length mismatch: dies; non-arr: dies |

### Vectorized Compare → Bitmap (5)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `veq` | `arr scalar → bitmap` | Element == scalar; returns compressed bitmap | non-arr: dies |
| `vgt` | `arr scalar → bitmap` | Element > scalar | non-arr: dies |
| `vlt` | `arr scalar → bitmap` | Element < scalar | non-arr: dies |
| `vge` | `arr scalar → bitmap` | Element >= scalar | non-arr: dies |
| `vle` | `arr scalar → bitmap` | Element <= scalar | non-arr: dies |

### Bitmap Operations (5)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `vand` | `bitmap bitmap → bitmap` | Bitwise AND of two bitmaps | length mismatch: dies |
| `vor` | `bitmap bitmap → bitmap` | Bitwise OR | length mismatch: dies |
| `vnot` | `bitmap → bitmap` | Bitwise NOT | non-bitmap: dies |
| `vcount` | `bitmap → n` | Population count (number of set bits) | non-bitmap: dies |
| `vgather` | `arr bitmap → arr` | Compress: keep elements where bitmap bit is set | length mismatch: dies; non-arr/non-bitmap: dies |

### Vectorized Aggregate (4)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `vsum` | `arr → scalar` | SIMD tree reduction sum | empty arr: 0; non-arr: dies |
| `vmean` | `arr → scalar` | Sum / len | empty arr: dies (NaN); non-arr: dies |
| `vmin` | `arr → scalar` | Minimum element | empty arr: dies; non-arr: dies |
| `vmax` | `arr → scalar` | Maximum element | empty arr: dies; non-arr: dies |

### Data Movement (5)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `vslice` | `arr start stop → arr` | Subarray copy; Python slice semantics | bounds clamped to [0, len]; non-arr: dies |
| `vconcat` | `arr arr → arr` | Concatenate two arrays | type mismatch: dies |
| `vreshape` | `arr shape_list → arr` | Reinterpret dimensions; product must match len | product mismatch: dies; non-arr: dies |
| `vflatten` | `arr → arr` | Collapse to 1D | non-arr: dies |
| `vindex` | `arr idx_arr → arr` | Gather by index: result[i] = arr[idx_arr[i]] | OOB idx: dies; non-arr: dies |

### Sort/Search (3)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `vsort` | `arr → arr` | Return new sorted array; Timsort (stable) | incomparable types: dies; non-arr: dies |
| `vargsort` | `arr → arr` | Indices that would sort the array | non-arr: dies |
| `vsearchsorted` | `sorted_arr val → idx` | Binary search: insertion point to maintain order | unsorted input: undefined behavior; non-arr: dies |

### Group/Window (2)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `vgroup` | `arr 'key_fn → dict_of_arrs` | Hash/radix group by key function; returns dict mapping key→subarray | non-arr: dies |
| `vwindow` | `arr size → arr2d` | Sliding window; each row is a window of size elements; stride=1 | size > len: dies; size < 1: dies; non-arr: dies |

### Additional Data Movement (12)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `vscatter` | `src_arr dst_arr bitmap → arr` | Masked write: dst[i] = src[i] where bitmap[i] set | length mismatch: dies |
| `vassign` | `arr idx_arr val_arr → arr` | Scatter by index: arr[idx[i]] = val[i] | OOB idx: dies; length mismatch: dies |
| `vcompress` | `arr bitmap → arr` | Alias for vgather; compress by bitmap | length mismatch: dies |
| `vwhere` | `arr1 arr2 mask_arr → arr` | Blend: result[i] = mask[i] ? arr1[i] : arr2[i] | length mismatch: dies |
| `vselect` | `arr1 arr2 mask_arr → arr` | Alias for vwhere | length mismatch: dies |
| `vdiff` | `arr → arr` | Adjacent difference: result[i] = arr[i+1] - arr[i]; len-1 elements | empty arr: empty; non-arr: dies |
| `vinterp` | `arr xp_arr fp_arr → arr` | Linear interpolation: lookup x in xp, interpolate fp | xp not sorted: undefined; non-arr: dies |
| `vtile` | `arr reps → arr` | Repeat entire array reps times | reps < 0: dies |
| `vrepeat` | `arr reps → arr` | Repeat each element reps times | reps < 0: dies |
| `vroll` | `arr shift → arr` | Circular shift; positive=right, negative=left | non-arr: dies |
| `vexpand` | `arr1 arr2 → arr2d` | Cross product: result[i,j] = (arr1[i], arr2[j]) | |
| `vlookup` | `keys_arr sorted_table_arr → idx_arr` | Binary search lookup: for each key, find index in sorted table | unsorted table: undefined; non-arr: dies |

### Join/Group Aggregate (5)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `vjoin` | `arr1 arr2 → arr` | Sort-merge join on two sorted arrays; returns array of pairs | unsorted input: undefined; non-arr: dies |
| `vgroup_agg` | `arr 'agg_fn → arr` | Group by key (implied by sorted order) and aggregate; agg_fn: `group_arr → scalar` | non-arr: dies |
| `vwindow_agg` | `arr size 'agg_fn → arr` | Rolling window aggregate; agg_fn applied to each window | size > len: dies; non-arr: dies |
| `vreduce_stream` | `file_handle init 'fn → scalar` | Streaming reduce over file; fn: `acc elem → new_acc` | |
| `vscan` | `file_handle 'fn →` | Stream apply fn to each element; no accumulation | |

---

## SEARCH & INDEX OPS (38)

### Columnar Extraction (2)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `column` | `list_of_obj field_name → arr` | Extract column from list of objects as contiguous typed array | field not found in all objects: dies; type mismatch: dies |
| `project` | `list_of_obj field_names… n → obj` | Multi-column extraction; returns columnar object with n fields | field not found: dies |

### B-Tree / Ordered Index (6)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `tree` | `→ h` | Create empty B-tree (ordered index) | |
| `tput` | `h key value →` | B-tree insert or update | non-tree: dies |
| `tget` | `h key → value found` | B-tree search; returns value and found flag (0/1 on top) | non-tree: dies |
| `tlower` | `h key → iter` | Lower bound: first key >= given key; returns iterator handle | non-tree: dies |
| `tupper` | `h key → iter` | Upper bound: first key > given key | non-tree: dies |
| `trange` | `h low_key high_key → iter` | Range scan iterator: all keys in [low, high) | non-tree: dies; low > high: empty iter |

### Bitmap Index (8)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `bitmap` | `n → h` | Create n-bit bitmap (Roaring), all zero | n < 0: dies |
| `bset` | `h idx →` | Set bit at index | idx < 0 or >= n: dies; non-bitmap: dies |
| `bget` | `h idx → 0/1` | Get bit at index | idx OOB: dies; non-bitmap: dies |
| `bcount` | `h → n` | Population count (set bits) | non-bitmap: dies |
| `band` | `h h → h` | Bitmap AND | length mismatch: dies; non-bitmap: dies |
| `bor` | `h h → h` | Bitmap OR | length mismatch: dies; non-bitmap: dies |
| `bnot` | `h → h` | Bitmap NOT | non-bitmap: dies |
| `biter` | `h → iter` | Iterator over set bit indices | non-bitmap: dies |

### Inverted Index (2)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `index` | `list_of_str → h` | Build inverted index: dict of term → bitmap of document IDs | |
| `search` | `h terms… n → bitmap` | Search inverted index: AND of n terms; returns bitmap of matching docs | term not found: empty bitmap |

### Composite Keys (1)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `key` | `values… n → composite_key` | Create hashable composite key from n values; concatenates cell bytes with type tags | n < 1: dies |

### Zone Maps (2)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `zonemap` | `arr → h` | Build zone map: array of {min, max, count} per block | non-arr: dies |
| `zprune` | `h pred_val → bitmap` | Prune blocks: returns bitmap of blocks that might contain pred_val | non-zonemap: dies |

### Trie / Prefix Index (2)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `trie` | `→ h` | Create empty trie (prefix tree) | |
| `tsearch` | `h prefix → list` | Trie prefix search: all strings with given prefix | non-trie: dies |

### Bloom Filter (3)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `bloom` | `n → h` | Create Bloom filter for n expected elements; auto-sized bit array | n < 1: dies |
| `badd` | `h value →` | Add element to Bloom filter | non-bloom: dies |
| `btest` | `h value → 0/1` | Test membership; 1 = probably in set, 0 = definitely not | non-bloom: dies |

### R-Tree / Spatial (2)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `rtree` | `→ h` | Create empty R-tree (spatial index for 2D/3D) | |
| `rsearch` | `h region → list` | R-tree search: all entries whose bounding box intersects query region | non-rtree: dies |

### Skip List (3)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `skiplist` | `→ h` | Create empty skip list (probabilistic ordered index) | |
| `sput` | `h key value →` | Skip list insert | non-skiplist: dies |
| `sget` | `h key → value found` | Skip list search; returns value and found flag | non-skiplist: dies |

### Join / Partition (5)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `partition` | `list n 'key_fn → list_of_lists` | Hash partition into n buckets by key function | n < 1: dies |
| `repartition` | `list_of_lists n 'key_fn → list_of_lists` | Reshuffle across n buckets to balance | n < 1: dies |
| `coalesce` | `list_of_lists n → list_of_lists` | Reduce number of partitions to n by merging | n > len: dies |
| `join` | `list1 list2 'left_key 'right_key → list_of_pairs` | Sort-merge join; sorts both lists by key, merges | non-list: dies |
| `vlookup` | `keys_arr sorted_table_arr → idx_arr` | Binary search lookup; alias for data module vlookup | unsorted table: undefined |

### Group / Aggregate (2)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `group` | `list 'key_fn → dict_of_lists` | Hash group by key function; returns dict mapping key→sublist | non-list: dies |
| `agg` | `dict_of_lists 'agg_fn → dict_of_values` | Aggregate each group; agg_fn: `group_list → scalar` | non-dict: dies |

### Window / Frame (3)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `window` | `arr size → list_of_arrs` | Sliding window (overlapping); stride=1 | size > len: dies; size < 1: dies |
| `tumble` | `arr size → list_of_arrs` | Tumbling window (non-overlapping); drops remainder | size > len: dies |
| `session` | `arr 'gap_fn → list_of_arrs` | Sessionize by gap predicate; gap_fn: `a b → 0/1` (1 = new session) | non-arr: dies |

### Pivot (2)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `pivot` | `list row_key col_key value_key → arr2d` | Pivot table: 2D array from records; row_labels×col_labels | missing keys: 0 fill |
| `unpivot` | `arr2d row_labels col_labels → list` | Unpivot: records from matrix; each cell becomes a record | shape mismatch: dies |

### Streaming (3)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `chunk` | `arr size → list_of_arrs` | Split array into chunks of size elements; last chunk may be smaller | size < 1: dies |
| `mmap` | `str → arr` | Memory-map file as array; zero-copy access | file not found: dies |
| `vscan` | `file_handle 'fn →` | Stream apply fn to each element; no accumulation | |

### Flatten / Explode (3)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `flatmap` | `list 'fn → list` | Map fn over list, then flatten one level; fn: `elem → list` | non-list: dies |
| `explode` | `list_of_lists → list` | Flatten one level; alias for core `flat` | |
| `explode_with` | `list_of_lists 'fn → list` | Apply fn to each sublist, then flatten | non-list: dies |

---

## SEARCH ALGORITHMS (15)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `zonemap` | `arr → h` | Build zone map statistics per block | non-arr: dies |
| `zprune` | `h pred_val → bitmap` | Prune blocks using zone map | non-zonemap: dies |
| `bloom` | `n → h` | Create Bloom filter | n < 1: dies |
| `badd` | `h value →` | Add to Bloom filter | non-bloom: dies |
| `btest` | `h value → 0/1` | Test Bloom filter | non-bloom: dies |
| `rtree` | `→ h` | Create R-tree spatial index | |
| `rsearch` | `h region → list` | Spatial search | non-rtree: dies |
| `skiplist` | `→ h` | Create skip list | |
| `sput` | `h key value →` | Skip list insert | non-skiplist: dies |
| `sget` | `h key → value found` | Skip list search | non-skiplist: dies |
| `srange` | `h low high → iter` | Skip list range iterator | non-skiplist: dies |
| `trie` | `→ h` | Create trie | |
| `tsearch` | `h prefix → list` | Trie prefix search | non-trie: dies |
| `suffix` | `str → h` | Build suffix array for substring search | |
| `ssearch` | `h substr → list` | Suffix array substring search; returns list of indices | non-suffix-array: dies |

---

## TIME DOMAIN OPS (15)

### Parse/Format (3)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `time` | `str fmt → t` | Parse string to time (i64 nanos since epoch). fmt: "unix" (float seconds), "iso" (ISO 8601), "rfc3339", "rfc2822", or strftime format. | unparseable: dies |
| `timef` | `t fmt → str` | Format time to string. Same fmt options as `time`. | |
| `now` | `→ t` | Current time (nanos since epoch) | |

### Convert (2)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `unix` | `t → f` | Convert to Unix time (float seconds) | |
| `unixi` | `t → n` | Convert to Unix time (int seconds, truncates) | |

### Duration (3)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `dur` | `n unit → d` | Create duration from scalar + unit. Units: `ns`, `us`, `ms`, `s`, `m`, `h`, `d`, `w`, `mo` (30d), `y` (365d). | negative n: valid (past duration) |
| `durparse` | `str → d` | Parse duration string: "1h30m", "2d", "500ms", "1.5h" | unparseable: dies |
| `durf` | `d → str` | Format duration: "1h30m45s", "2d", "500ms" | |

### Interval (1)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `tin` | `t start stop → 0/1` | Time in half-open interval [start, stop) | |

### Timezone (4)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `tzset` | `tz_str →` | Set thread-local timezone context for subsequent `timef`/`time`/`tzconv` ops. "UTC", "America/New_York", "+05:30". | invalid tz: dies |
| `tzget` | `→ str` | Get current timezone context | |
| `tzconv` | `t tz_str → t` | Convert time to different timezone (same instant, different wall clock). Returns same i64, sets context for formatting. | |
| `tzlist` | `→ list` | List available timezone names | system-dependent |

### Vectorized (2)

| Op | Stack | Description | Edge Cases |
|----|-------|-------------|------------|
| `vtime` | `str_arr fmt → t_arr` | Vectorized parse string array to time array | unparseable elements: 0 (epoch) |
| `vtimef` | `t_arr fmt → str_arr` | Vectorized format time array to string array | |

---

## SUMMARY

| Category | Count |
|----------|-------|
| Core | 72 |
| Data Manipulation | 51 |
| Search & Index | 38 |
| Search Algorithms | 15 |
| Time Domain | 15 |
| **Total** | **191** |

## Type Tags

| Tag | Type | Notes |
|-----|------|-------|
| 0 | int | |
| 1 | float | |
| 2 | ptr | base pointer |
| 3 | byte | |
| 4 | void | sizeof=8 |
| 5 | arr | fixed-size typed array |
| 6 | list | growable vector |
| 7 | dict | hash map |
| 8 | chan | bounded MPSC ring |
| 9 | atom | atomic i64 |
| 10 | str | string (ptr alias) |
| 11 | buf | raw buffer |
| 12 | obj | struct instance |
| 13 | time | i64 nanos since epoch |
| 14 | dur | i64 nanos (duration) |
| 15 | bitmap | Roaring bitmap |
| 16 | tree | B-tree index |
| 17 | trie | prefix tree |
| 18 | bloom | Bloom filter |
| 19 | rtree | R-tree spatial index |
| 20 | skiplist | skip list index |
| 21 | ttree | time-ordered B-tree |
| 22 | sparse | sparse matrix |
| 23 | zonemap | zone map statistics |
| 24 | suffix | suffix array |

## Library Functions (Cut from Native)

The following operations are available as µFlux library code (via `USE "time"` or inline):

| Function | Description | Implementation |
|----------|-------------|----------------|
| `tadd`, `tsub`, `tdiff` | Time arithmetic | `add`, `sub` on i64 |
| `tlt`, `tgt`, `teq` | Time comparison | `lt`, `gt`, `eq` on i64 |
| `dadd`, `dsub`, `dmul`, `ddiv`, `dlt`, `dabs` | Duration arithmetic | `add`, `sub`, `mul`, `div`, `lt`, `abs` on i64 |
| `year`, `month`, `day`, `yday`, `wday`, `hour`, `min`, `sec`, `nano`, `week`, `isoweek`, `quarter` | Calendar extraction | Division/modulo with lookup tables |
| `trunc`, `round`, `ceil` (time) | Time truncation | Division by unit constants |
| `trange`, `tslide`, `tgroup` | Time generation | `range` + `mul` + `add` on i64 |
| `dhours`, `dmins`, `dsecs`, `dms`, `dbus`, `dns`, `dtotal` | Duration decomposition | Division by constants |
| `vtadd`, `vtdiff`, `vttrunc` | Vectorized time math | `vadd`, `vsub` on i64 arrays |
| `vyear`, `vhour`, `vweekday` | Vectorized calendar extract | Library loop over array |
| `tasof`, `tmerge`, `talign` | Time series join | Generic `join` + time key |
| `tgroup_agg`, `twindow_agg`, `tewma` | Time aggregation | `vgroup` + `trunc` + calendar logic |
| `ttree`, `ttput`, `ttget`, `ttrange`, `tttrunc`, `ttasof`, `ttnearest` | Time index | Generic `tree` with time keys |
