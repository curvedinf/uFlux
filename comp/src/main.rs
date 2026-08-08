// ufc — reference µFlux compiler (SPEC.md, whitepaper v7.0)
// Pipeline: hieroglyph lexer -> parser (labels/macros/structs/imports/v11 locals) -> C codegen -> cc.
// Std-only. Modules: ast (types), lex (dense+text lexers), parse, prelude (C runtime), gen (codegen), emit (v9 emitters).

use std::collections::HashMap;
use std::env;
use std::fs;
use std::process::Command;

mod ast;
mod emit;
mod gen;
mod lex;
mod parse;
mod prelude;

use ast::*;
use emit::*;
use gen::*;
use lex::*;
use parse::*;

// ---------------- directory discovery ----------------

/// Collect all .uf/.uft files in dir, sorted for deterministic compilation order.
fn collect_uf_files(dir: &std::path::Path) -> Vec<String> {
    let mut files: Vec<String> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read dir {}: {}", dir.display(), e))
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let p = e.path();
            if p.is_file() {
                if let Some(ext) = p.extension() {
                    if ext == "uf" || ext == "uft" {
                        return Some(p.to_string_lossy().to_string());
                    }
                }
            }
            None
        })
        .collect();
    files.sort();
    files
}

/// Check if dir contains init.uf or init.uft
fn has_init(dir: &std::path::Path) -> bool {
    dir.join("init.uf").exists() || dir.join("init.uft").exists()
}

/// Recursively collect files from an init-directory: init.uf first, then other
/// .uf/.uft, then recurse into nested init subdirs.
fn collect_init_dir(
    dir: &std::path::Path,
    files: &mut Vec<String>,
    init_flags: &mut Vec<bool>,
) {
    // init.uf (or init.uft) first — it's the thread entry point
    let init_file = if dir.join("init.uf").exists() {
        dir.join("init.uf").to_string_lossy().to_string()
    } else {
        dir.join("init.uft").to_string_lossy().to_string()
    };
    files.push(init_file);
    init_flags.push(true);

    // other .uf/.uft in this dir (not init)
    let mut others = collect_uf_files(dir);
    others.retain(|f| {
        let p = std::path::Path::new(f);
        p.file_name().map(|n| n != "init.uf" && n != "init.uft").unwrap_or(true)
    });
    for f in others {
        files.push(f);
        init_flags.push(false);
    }

    // recurse into nested init subdirs
    let subdirs: Vec<std::path::PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read dir {}: {}", dir.display(), e))
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    for sd in subdirs {
        if has_init(&sd) {
            collect_init_dir(&sd, files, init_flags);
        }
    }
}

/// Discover source files from a directory for directory mode.
/// Returns (file_paths, init_flags).
fn discover_directory(root: &str) -> (Vec<String>, Vec<bool>) {
    let rootpath = std::path::Path::new(root);
    let mut files: Vec<String> = Vec::new();
    let mut init_flags: Vec<bool> = Vec::new();

    // main.uf (or main.uft) is the entry point — must exist
    let main_file = if rootpath.join("main.uf").exists() {
        rootpath.join("main.uf").to_string_lossy().to_string()
    } else if rootpath.join("main.uft").exists() {
        rootpath.join("main.uft").to_string_lossy().to_string()
    } else {
        panic!("uf: no main.uf found in {}", root);
    };
    files.push(main_file);
    init_flags.push(false);

    // other .uf/.uft in root (not main)
    let mut others = collect_uf_files(rootpath);
    others.retain(|f| {
        let p = std::path::Path::new(f);
        p.file_name().map(|n| n != "main.uf" && n != "main.uft").unwrap_or(true)
    });
    for f in others {
        files.push(f);
        init_flags.push(false);
    }

    // subdirs with init.uf
    let subdirs: Vec<std::path::PathBuf> = fs::read_dir(rootpath)
        .unwrap_or_else(|e| panic!("cannot read dir {}: {}", rootpath.display(), e))
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    for sd in subdirs {
        if has_init(&sd) {
            collect_init_dir(&sd, &mut files, &mut init_flags);
        }
    }

    (files, init_flags)
}

// locate mods/<name>.ufm in CWD, ~/.uflux/mods, then each $UFMODPATH dir
fn find_manifest(name: &str) -> Option<String> {
    let mut candidates: Vec<std::path::PathBuf> = vec![std::path::PathBuf::from(format!("mods/{}.ufm", name))];
    if let Ok(home) = env::var("HOME") {
        candidates.push(std::path::PathBuf::from(format!("{}/.uflux/mods/{}.ufm", home, name)));
    }
    if let Ok(paths) = env::var("UFMODPATH") {
        for dir in paths.split(':').filter(|d| !d.is_empty()) {
            candidates.push(std::path::PathBuf::from(format!("{}/{}.ufm", dir, name)));
        }
    }
    for c in candidates {
        if let Ok(s) = fs::read_to_string(&c) {
            return Some(s);
        }
    }
    None
}

/// Generate the agent SKILL.md template with the live opcode list.
fn skill_template() -> String {
    // Pair each live opcode with its dense glyph and usage
    let op_entries: Vec<String> = OP_NAMES
        .iter()
        .enumerate()
        .filter(|(_, n)| !n.starts_with('~'))
        .map(|(_, n)| {
            let glyph = char::from_u32(OP_GLYPHS[op_index(n).unwrap()]).unwrap();
            let usage = op_usage(n);
            if usage.is_empty() {
                format!("- {} `{}`", glyph, n.to_lowercase())
            } else {
                format!("- {} `{}` — {}", glyph, n.to_lowercase(), usage)
            }
        })
        .collect();
    let count = op_entries.len();
    let op_list = op_entries.join("\n");

    format!(
r#"---
name: uflux
description: |
  Use µFlux (`uf`) for all inline custom tools — counting lines, transforming data, inspecting files, quick scripts. µFlux is a stack-based language compiled to C then native via `cc`, designed for LLM-authored scripts (low token count, fast, reliable). Use dense mode (glyph, `.uf`) for one-off tools; use text mode (mnemonic, `.uft`) for saved programs. Invoke inline with `uf "<code here>"` (last positional arg is treated as source if no file matches), or pipe stdin with `uf -`.
---

# µFlux (uf) Agent Skill

µFlux is a dynamically typed, weakly typed, stack-based language compiled to C then native via `cc`.
Programs are terse (low token count), fast, and reliable — designed for LLM-authored scripts.
Cells are untyped 64-bit values at runtime — ints, floats, and pointers freely interconvert.
Type inference happens at compile time where possible, but types are not enforced at the language level.

**Do not model µFlux after Forth.** Despite being stack-based, µFlux has no raw jumps, no stack manipulation puzzles, and no `:`/`;` word definitions. It uses structured control flow (`if`/`ifelse`/`while`/`for` with quoted label addresses), named labels with `call`, and high-level ops like `filter`, `sort`, `vmap`, `ffold`. Think of it as a terse scripting language that happens to use a stack, not a Forth dialect.

> **Maintenance:** When `uf` is updated, regenerate this file with `uf --skill` to refresh the opcode list.

## Inline usage

Run code inline without saving a file:
```
uf '"hello\n" print drop'
uf '1 2 add "%d\n" print drop'
```
Or pipe via stdin: `echo '...' | uf -`

## Encoding

µFlux has two encodings:

- **Dense** (glyph mode, `.uf`): single-token emoji glyphs, optimized for LLM token efficiency.
  Use for **one-off tools** and inline scripts where token count matters.
- **Text** (mnemonic mode, `.uft`): human-readable ASCII mnemonics like `dup`, `add`, `if`.
  Use for **saved programs** that humans will read, edit, and maintain.

The compiler auto-detects the encoding per file: any character at or above U+13000 = dense.

## Quick reference

- Stack-based: everything operates on the data stack. `1 2 add` pushes 1, pushes 2, pops both, pushes 3.
- Comments: `;` to end of line.
- Numbers: self-evaluating (pushed onto stack). Negative numbers allowed in text mode.
- Strings: `"hello\n"` — escapes: \n \t \r \0 \\ \"
- Variables: `x!` (store), `x@` (fetch) for locals; `^x!` / `^x@` for globals; `x++` / `x+=` increment/accumulate.
- Labels: `name:` defines; `'name` pushes address; `call name` calls.
- Control flow: `if` (cond `'label`), `ifelse` (cond `'then_label 'else_label`), `while` (`'cond_label 'body_label`), `for` (count `'body_label`); `break`/`cont` in loop bodies only.
- Entry point: `entry:` marks where execution starts (implicit jump from pc 0).
- Printing: `print` with a printf-style format string on top of stack. Use `drop` to consume the return value.
- Shell: `sh` takes a command string, pushes **3 values**: stdout, stderr, exit-status. Consume all three (e.g. `drop drop drop` to keep only stdout, or `drop drop` to keep stdout + check status).
- Modules: `use "name"` links -l<name> and loads mods/<name>.ufm manifest.
- FFI: `import c"fn"(arg_types)->ret` declares C functions.

## Reserved words

**Every opcode mnemonic is a reserved identifier.** You cannot use any of them as label names or variable names. The full list is in the opcode list below (191 names). For example, `iter:` is illegal because `iter` is an opcode — use a different name like `walk:` or `each:`.

## Removed opcodes (do not use)

`jmp`, `jz`, and `je` are **deleted from the language**. Using them produces a compile error. Use structured control flow instead:
- `jmp label` → use `call label` or `entry:`
- `jz` (jump if zero) → use `if` with a `'label`
- `je` (jump if equal) → use `eq` then `if` with a `'label`

The `=` token also triggers this error — use `eq` to compare.

## Common mistakes

**Mnemonics are abbreviated, not standard Forth names.** Do not assume Forth names work. Check the opcode list. Common mismatches:
- `swap` → use `swp`
- `drop` → use `drp`
- `over` → use `ovr`
- `@`/`!` (Forth fetch/store) → use `x@`/`x!` (locals) or `^x@`/`^x!` (globals)

**There are NO inline block keywords.** `else` and `then` are NOT opcodes. You cannot write Forth-style `if ... else ... then`. You MUST define separate labels and pass their addresses:
```
cond 'yes if                         ; one-branch
cond 'then_label 'else_label ifelse  ; two-branch
```
Where `yes:`, `then_label:`, `else_label:` are labels ending with `ret`.

**`eq` compares ints and floats by value, pointers by identity.** String comparison with `eq` is unreliable — use `match`, `starts`, `ends`, or `find` for string equality.

**`split` on text with a trailing separator creates an empty trailing element.** For example `"a\nb\n" "\n" split` produces `["a", "b", ""]`. Filter empties: `lines 'nonempty filter` where `nonempty: len 0 gt ret`.

**`rsplit` (regex split) may produce unexpected results.** Prefer `split` (literal separator) which is reliable.

**`addto` argument order is `dict key amount`.** Not amount-first: `d@ "x" 5 addto`.

**`spit` argument order is `path str`** — path is FIRST, content is SECOND (unlike shell redirection).

**`sh` idiom to keep only stdout:** `"cmd" sh drop drop` (drops stderr and status, leaves stdout).

**`print` returns the char count — always `drop` after it** unless you need the count.

**For complex operations on handles (lists, dicts, strings), prefer storing to a variable and re-fetching** rather than `dup`-ing the handle. `dup` works for simple cases like `dup len` but can cause issues in multi-step operations.

**Use globals (`^x!`/`^x@`) for state shared across labels** called via `if`/`for`/`filter`/`ffold` callbacks. Locals (`x!`/`x@`) are scoped to the calling function and may not be visible inside callback labels.

## Container construction

There is no `{{}}` syntax. Create containers with opcodes:

| Construct | Syntax | Notes |
|-----------|--------|-------|
| Empty list | `list` | push then: `list 42 push` |
| Empty dict | `dict` | put then: `dict "key" val set` |
| Typed array | `type len arr` | type = type id: int=0, float=1, ptr=2, byte=3 |
| Tensor | `type len tensor` | same as arr |
| String | `"hello\n"` or `str` | bare quoted strings are preferred |

`push` returns a (possibly reallocated) handle — always keep the result: `lst 42 push lst!`

## Key opcode signatures

Common ops with non-obvious stack signatures:

| Op | Stack effect | Notes |
|----|-------------|-------|
| `sh` | `cmd → stdout stderr status` | 3 return values; `/bin/sh -c` |
| `print` | `args... fmt → n` | fmt on top; one arg per `%`; `n` = chars written, usually `drop`ped |
| `split` | `str sep → list` | literal separator (empty sep: dies) |
| `join` | `list sep → str` | |
| `slurp` | `path → str` | whole file |
| `spit` | `path str →` | create/truncate |
| `arr` | `type len → h` | type: int=0 float=1 ptr=2 byte=3 |
| `iter` | `h → it` | create cursor from any collection |
| `next` | `it → v more` | `more`=0 means exhausted (`v` is 0) |
| `collect` | `it → list` | drain iterator into a list |
| `ffold` | `path init fn_addr → acc` | streaming reduce over file lines; fn is `(acc line → acc)` |
| `feach` | `path fn_addr →` | call fn per line; fn is `(line → )`, stops early if fn returns 0 |
| `for` | `count body_addr →` | pushes loop index k per iteration |
| `filter` | `list pred_addr → list'` | pred is `(elem → 0/1)` |
| `vmap` | `arr fn_addr → arr'` | fn is `(elem → elem')` |
| `vfold` | `arr init fn_addr → acc` | fn is `(acc elem → acc)` |

## Worked examples

### Count lines in all .rs files (inline)
```
uf '"find . -name "*.rs" | sort" sh drop drop "\n" split 0 "LOC: %d\n" print drop'
```

### ffold to count lines in a file
```
"data.txt" 0 'step ffold "lines: %d\n" print drop
step:
  ; stack: acc line — drop line, increment acc
  drop inc ret
```

### for loop
```
5 'body for "done\n" print drop ret
body: "%d\n" print drop ret
```

## Forth vs µFlux — use high-level ops, not manual stack manipulation

µFlux is stack-based but is **not Forth**. Avoid manual loop-and-pop patterns.
Use the built-in high-level ops instead. Below: the Forth-style way (wrong) vs
the µFlux way (right).

### Sum a list of numbers

**Forth-style (wrong):** manual loop with iter/next, stack juggling
```
0 acc!
list 10 push 20 push 30 push iter
walk:
  next
  if
    acc@ add acc!
    call walk
  else
    drop
    acc@ "sum: %d\n" print drop
    ret
  then
```

**µFlux (right):** one op does the work
```
list 10 push 20 push 30 push
0 'addup vfold "sum: %d\n" print drop
addup:
  add ret
```

### Double every element in an array

**Forth-style (wrong):** loop index, vget, multiply, vset
```
0 5 arr int nums!
0 i!
loop:
  i@ 5 lt if
    nums@ i@ vget 2 mul
    nums@ i@ swp vset
    i@ inc i!
    call loop
  else
    drop
  then
```

**µFlux (right):** vmap with a function
```
0 5 arr int
'dbl vmap
"first: %d\n" 0 swp vget print drop
dbl:
  2 mul ret
```

### Keep elements greater than 3

**Forth-style (wrong):** manual filter with a loop and conditional append
```
0 10 range
0 i!
keep!
walk:
  i@ 10 lt if
    i@ 3 gt if
      keep@ i@ push keep!
    then
    i@ inc i!
    call walk
  else
    drop
    keep@ len "kept: %d\n" print drop
    ret
  then
```

**µFlux (right):** filter with a predicate
```
0 10 range 'big? filter len "kept: %d\n" print drop
big?:
  3 gt ret
```

### Count lines matching a pattern in a file

**Forth-style (wrong):** slurp, split, iter/next loop with match per line
```
0 count!
"log.txt" slurp "\n" split iter
walk:
  next if
    "ERROR" match drop if
      count@ inc count!
    then
    call walk
  else
    drop
    count@ "errors: %d\n" print drop
    ret
  then
```

**µFlux (right):** feach does streaming, no slurp needed
```
"log.txt" 'check feach
0 count!
"errors: %d\n" count@ print drop ret
check:
  "ERROR" match drop if count@ inc count! then
  1 ret
```

### Transform a list of strings to uppercase

**Forth-style (wrong):** manual loop with push/pop
```
list "hello" push "world" push
0 i!
out!
walk:
  i@ 2 lt if
    dup i@ get up
    out@ swp push out!
    i@ inc i!
    call walk
  else
    drop
    out@ "\n" join "%s\n" print drop
    ret
  then
```

**µFlux (right):** vmap on strings (or just use `up` directly)
```
list "hello" push "world" push
'\n' split   ; not needed — use collect from iter
'ucase imap collect "\n" join "%s\n" print drop
ucase:
  up ret
```

## Best practices

- Keep programs minimal — the stack is your state; avoid unnecessary variables.
- Use locals (`x!`/`x@`) for function-scoped state, globals (`^x!`/`^x@`) for shared state.
- Prefer structured ops (`filter`, `sort`, `vmap`, `vfold`) over manual loops.
- Use `slurp`/`spit` for file I/O, `sh` for shell commands, `json`/`unjson` for structured data.
- Dense mode for inline/one-shot scripts; text mode for anything you'll save or share.
- Test with: `uf program.uft` (compiles, caches, and runs in one step).

## Opcode list ({count} live opcodes)

{op_list}
"#,
        count = count,
        op_list = op_list
    )
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut inputs: Vec<String> = Vec::new();
    let mut output: Option<String> = None;
    let mut emit_c = false;
    let mut emit_text_f = false;
    let mut emit_dense_f = false;
    let mut compile_only = false;
    let mut convert = false;
    let mut run_args: Vec<String> = Vec::new();
    let mut gc_threshold: Option<u64> = None;
    let mut gc_off = false;
    let mut force_mt = false;
    let mut debug = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--" => {
                // everything after `--` is the program's argv, not ufc input
                i += 1;
                run_args = args[i..].to_vec();
                break;
            }
            "-o" => {
                i += 1;
                output = Some(args.get(i).unwrap_or_else(|| panic!("-o needs an argument")).clone());
            }
            "-c" => compile_only = true,
            "--gc-threshold" => {
                i += 1;
                gc_threshold = Some(args.get(i).unwrap_or_else(|| panic!("--gc-threshold needs a byte value (e.g. 1000000)")).parse::<u64>()
                    .unwrap_or_else(|e| panic!("--gc-threshold: {}", e)));
            }
            "--gc-off" => gc_off = true,
            "--mt" => force_mt = true,
            "--debug" | "-D" => debug = true,
            "--emit-c" => emit_c = true,
            "--emit-text" => emit_text_f = true,
            "--emit-dense" => emit_dense_f = true,
            "--to-text" => {
                emit_text_f = true;
                convert = true;
            }
            "--to-dense" => {
                emit_dense_f = true;
                convert = true;
            }
            "-h" | "--help" => {
                eprintln!("usage: uf [directory]                   compile+run directory (auto-discover)");
                eprintln!("       uf input.uf... ['inline source'|-]        compile+run (cached in TMPDIR)");
                eprintln!("       uf -c input.uf... [-o output] [--emit-c|--emit-text|--emit-dense]");
                eprintln!("       uf --to-text prog.uf | --to-dense prog.uft   convert encodings (writes prog.uft/.uf)");
                eprintln!("       uf -s | --skill                    print agent SKILL.md template");
                eprintln!("");
                eprintln!("  runtime flags (baked into compiled binary):");
                eprintln!("       --gc-threshold N   GC collection threshold in bytes (default: 1MB)");
                eprintln!("       --gc-off            disable garbage collector entirely");
                eprintln!("       --mt                force multi-threaded allocator (use mutex even if single-threaded)");
                eprintln!("       --debug, -D         enable crash dump (stack trace + local/global var dump on runtime errors)");
                eprintln!("       --                  pass remaining args to the program");
                return;
            }
            "-s" | "--skill" => {
                print!("{}", skill_template());
                return;
            }
            s if s.starts_with('-') && s.is_ascii() && s != "-" => panic!("unknown option {}", s),
            s => inputs.push(s.to_string()),
        }
        i += 1;
    }
    // Directory mode: bare `uf` or `uf somedir/` discovers files automatically.
    let init_flags: Vec<bool>;
    if inputs.is_empty() {
        let (files, flags) = discover_directory(".");
        inputs = files;
        init_flags = flags;
    } else if inputs.len() == 1 && std::path::Path::new(&inputs[0]).is_dir() {
        let dir = inputs[0].clone();
        let (files, flags) = discover_directory(&dir);
        inputs = files;
        init_flags = flags;
    } else {
        init_flags = vec![false; inputs.len()];
    }
    // Each input is one translation unit; the FIRST input is the main TU.
    // Non-final inputs must be files; the final input may also be inline
    // source or "-" (stdin).
    let mut structs: StructMap = HashMap::new();
    let mut tus: Vec<Parsed> = Vec::new();
    let mut mods: Vec<String> = Vec::new();
    let mut emit_toks: Vec<Tok> = Vec::new();
    // the cache key must change whenever the codegen/runtime changes, so fold
    // in the compiler executable's own mtime (rebuild => new cache entries)
    let mut hash_src = String::from("codegen-rev: v11-locals-postfix\n");
    if debug { hash_src.push_str("debug-mode\n"); }
    if let Ok(exe) = env::current_exe() {
        if let Ok(md) = fs::metadata(&exe) {
            if let Ok(mt) = md.modified() {
                hash_src.push_str(&format!("{:?}\n", mt));
            }
        }
    }
    let emitting = emit_text_f || emit_dense_f;
    let n_in = inputs.len();
    for (k, input) in inputs.iter().enumerate() {
        let last = k == n_in - 1;
        let (src, defmod) = if input == "-" {
            let mut s = String::new();
            use std::io::Read as _;
            std::io::stdin().read_to_string(&mut s).unwrap_or_else(|e| panic!("cannot read stdin: {}", e));
            (s, "main".to_string())
        } else {
            match fs::read_to_string(input) {
                Ok(s) => {
                    let stem = std::path::Path::new(input)
                        .file_stem()
                        .map(|x| x.to_string_lossy().to_string())
                        .unwrap_or_else(|| "main".to_string());
                    let clean: String = stem
                        .chars()
                        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
                        .collect();
                    (s, clean)
                }
                Err(_) if last && !input.ends_with(".uf") && !input.ends_with(".uft") => {
                    (input.clone(), "main".to_string())
                }
                Err(e) => panic!("cannot read {}: {}", input, e),
            }
        };
        let mut toks = lex_source(&src);
        hash_src.push('\u{1}');
        hash_src.push_str(&src);
        if emitting {
            emit_toks.append(&mut toks);
            continue;
        }
        // USE"name" manifests prepend to the TU that asked for them
        let uses: Vec<String> = toks
            .iter()
            .filter_map(|t| match t {
                Tok::Use(n) => Some(n.clone()),
                _ => None,
            })
            .collect();
        let mut manifest_toks = Vec::new();
        for u in &uses {
            let msrc = find_manifest(u).unwrap_or_else(|| panic!("USE\"{}\": no mods/{}.ufm found (searched CWD/mods, ~/.uflux/mods, UFMODPATH)", u, u));
            hash_src.push_str(&msrc);
            let mut mt = lex_source(&msrc);
            manifest_toks.append(&mut mt);
        }
        manifest_toks.append(&mut toks);
        tus.push(parse(manifest_toks, &mut structs));
        mods.push(defmod);
    }
    if emitting {
        let s = if emit_text_f { emit_text(&emit_toks) } else { emit_dense(&emit_toks) };
        let derived = if convert && output.is_none() {
            if inputs.len() != 1 {
                panic!("--to-text/--to-dense take exactly one input file (or use -o)");
            }
            let inp = &inputs[0];
            if inp == "-" || !std::path::Path::new(inp).exists() {
                panic!("--to-text/--to-dense need a file input (or use -o with inline source)");
            }
            let stem = inp.strip_suffix(".uf").or_else(|| inp.strip_suffix(".uft")).unwrap_or(inp);
            Some(format!("{}.{}", stem, if emit_text_f { "uft" } else { "uf" }))
        } else {
            None
        };
        match output.as_ref().or(derived.as_ref()) {
            Some(o) => {
                fs::write(o, &s).unwrap_or_else(|e| panic!("cannot write {}: {}", o, e));
                eprintln!("wrote {}", o);
            }
            None => print!("{}", s),
        }
        return;
    }
    let parsed = merge_tus(tus, mods, &init_flags);
    let csrc = gen(&parsed, &structs, debug);
    // bake runtime config into the generated binary
    let mut config_lines = String::new();
    if let Some(t) = gc_threshold {
        config_lines.push_str(&format!("  setenv(\"UF_GC_THRESHOLD\",\"{}\",1);\n", t));
    }
    if gc_off {
        config_lines.push_str("  uf_gc_on=0;\n");
    }
    if force_mt {
        config_lines.push_str("  atomic_store(&uf_gc_mt,1);\n");
    }
    let csrc = if config_lines.is_empty() {
        csrc
    } else {
        csrc.replace("uf_gc_init();", &format!("{}uf_gc_init();", config_lines))
    };

    // effective link line: pthread always (weave/chan/atom substrate), -l per USE
    let mut links: Vec<String> = vec!["-lpthread".to_string(), "-lm".to_string()];
    for u in &parsed.uses {
        links.push(format!("-l{}", u));
    }
    if emit_c {
        let csrc = format!("// link: cc <this-file>.c {}\n{}", links.join(" "), csrc);
        match &output {
            Some(o) => {
                let path = format!("{}.c", o);
                fs::write(&path, &csrc).unwrap_or_else(|e| panic!("cannot write {}: {}", path, e));
                eprintln!("wrote {}", path);
            }
            None => print!("{}", csrc),
        }
        return;
    }
    if compile_only {
        let out = output.unwrap_or_else(|| "a.out".to_string());
        let tmpc = format!("{}.ufc.c", out);
        fs::write(&tmpc, &csrc).unwrap_or_else(|e| panic!("cannot write {}: {}", tmpc, e));
        let mut cc_args: Vec<String> = if debug { vec!["-O0".into(), "-g".into()] } else { vec!["-O2".into()] };
        cc_args.extend(["-w".into(), "-o".into(), out, tmpc.clone()]);
        let status = Command::new("cc")
            .args(&cc_args)
            .args(&links)
            .status()
            .unwrap_or_else(|e| panic!("failed to run cc: {}", e));
        if !status.success() {
            eprintln!("cc failed; C source kept at {}", tmpc);
            std::process::exit(1);
        }
        let _ = fs::remove_file(&tmpc);
        return;
    }
    // Default mode: compile to a cached binary in the OS temp dir and run it.
    // Cache key = FNV-1a of all TU sources + manifests + compiler version + links.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in hash_src.as_bytes().iter().chain(env!("CARGO_PKG_VERSION").as_bytes()).chain(links.join(" ").as_bytes()) {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let dir = env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    let cdir = std::path::Path::new(&dir).join("uflux-cache");
    fs::create_dir_all(&cdir).unwrap_or_else(|e| panic!("cannot create {}: {}", cdir.display(), e));
    let bin = cdir.join(format!("{:016x}", h));
    let bins = bin.to_string_lossy().to_string();
    if !bin.exists() {
        let tmpc = cdir.join(format!("{:016x}.c", h));
        fs::write(&tmpc, &csrc).unwrap_or_else(|e| panic!("cannot write {}: {}", tmpc.display(), e));
        let mut cc_args: Vec<String> = if debug { vec!["-O0".into(), "-g".into()] } else { vec!["-O2".into()] };
        cc_args.extend(["-w".into(), "-o".into(), bins.clone()]);
        cc_args.push(tmpc.to_string_lossy().to_string());
        let status = Command::new("cc")
            .args(&cc_args)
            .args(&links)
            .status()
            .unwrap_or_else(|e| panic!("failed to run cc: {}", e));
        let _ = fs::remove_file(&tmpc);
        if !status.success() {
            std::process::exit(1);
        }
    }
    let status = Command::new(&bins)
        .args(&run_args)
        .status()
        .unwrap_or_else(|e| panic!("failed to run {}: {}", bins, e));
    std::process::exit(status.code().unwrap_or(1));
}
