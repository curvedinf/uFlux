# µFlux

µFlux (Micro Flux) is a programming language designed for **LLM-authored one-off scripts**. 
It is optimized for low token count, fast data processing, and inherent reliability. µFlux 
is used like a scripting language inline in your existing CLI, but every script is actually 
a C compiled binary so it is near maximally fast. Many powerful features are integrated as 
core language operations, enabling immediate optimal results from LLM tool use.

### Where do these advantages come from?

```
$ uf '"Hello, dense uFlux!\n"𓂎'
Hello, dense uFlux!
$ uf '"Hello, text uFlux!\n" print'
Hello, text uFlux!
```

The advantages of µFlux come from sacrificing usability concerns of 
human author friendliness. µFlux is practically indecipherable when dense encoded.
Dense encoding is a mode which encodes all features of the language into a set of 
Egyptian hieroglyphs. The hieroglyphs act as *placeholders* for reserved LLM vocabulary 
tokens of a given model's choice. In this way, glyph encoding maximally compresses the 
functions of a modern language into minimal tokens. Note: The hieroglyphs are not 
designed to be the actual tokens used for a given LLM, but instead are consistent 
representational placeholders.

µFlux also provides a more decipherable text encoding with English-based mnemonics. 
Ultimately LLMs can encode mnemonic op codes in an identical amount of tokens, 
depending on the choices of their vocabulary, so English mnemonics are  
usually preferred. Glyphs are provided as a capability for LLM creator's flexibility so as to
separate µFlux model representations from standard language at the vocab level. English-based 
mnemonics may overlap existing model representations and cause ambiguity where it isn't desired.

### Reading µFlux

µFlux is a stack machine. Every value sits on a shared data stack, and every opcode
consumes values from the top and pushes results back. There is no expression syntax —
the program is written in the order it executes.

Here is a complete program (text encoding) that runs a shell command, splits the
output into lines, and prints the count and first line:

```
"printf 'alpha\\nbeta\\ngamma\\n'" sh drop drop trim "\n" split lines!
lines@ len "%d lines\n" print drop
lines@ 0 get "first: %s\n" print drop
```

Reading the first line left to right:

- `"printf 'alpha\\nbeta\\ngamma\\n'"` — push a string onto the stack.
- `sh` — execute it as a shell command; pushes exit status, stderr, and stdout.
- `drop drop` — discard status and stderr, leaving stdout on the stack.
- `trim` — strip surrounding whitespace from the string.
- `"\n"` — push a separator string.
- `split` — cut the string into a list at each separator.
- `lines!` — store the list into a variable named `lines`.

The `!` suffix stores the top of the stack into a named variable. The `@` suffix
loads a variable back onto the stack — `lines@` is used in the next two lines.

That walkthrough covers the execution model. The rest of the language is knowing
which opcodes exist. There are about 170, each doing one thing. A sample:

| | |
|---|---|
| **Containers** | `list`, `arr` (typed array), `dict`, `obj` — all accessed through `get` / `set` / `len` / `keys` / `has` / `del` |
| **Strings** | `cat`, `split`, `find`, `repl`, `trim`, `match` (regex), `fmt`, `starts` |
| **Shell / OS** | `sh` (capture), `shp` (streaming), `exec` (argv list, no shell) |
| **Concurrency** | `chan`, `enq`, `deq`, `spawn` |
| **I/O** | `print`, `scan`, file read / write |
| **FFI** | `import c"fn"(types)->ret` |

Control flow uses labels — `name:` defines a jump target, `'name` references it.
`if` / `while` / `for` take label arguments rather than blocks:

```
n@ 0 gt 'report 'skip ifelse     ; if n > 0, jump to 'report, else 'skip
128 'fill for                     ; run 'fill 128 times (index on stack)
```

Raw jumps (`jmp`, `jz`, `je`) are available when structured forms don't fit.

The full opcode reference is `SPEC.md`.

### Running

```sh
cd comp && cargo build --release
./comp/target/release/uf examples/hello.uf                         # compile + run
./comp/target/release/uf -c examples/hello.uf -o hello && ./hello   # produce a binary
```

The compiler translates source to C and invokes `cc`. Programs are compiled, not
interpreted. A garbage collector handles all runtime memory — scripts never call
`free` on managed objects (raw `malloc` / `free` exist only for FFI buffers).

### Examples

| File | |
|------|-|
| `examples/hello.uf` | Print a string |
| `examples/fib.uf` | Iterative Fibonacci (dense) |
| `examples/shelltest.uf` | Shell commands, streaming pipelines, exec |
| `examples/dot.uf` | Typed arrays, vectorized dot product |
| `examples/maptest.uf` | Dict operations |
| `examples/ring.uf` | Channels and `spawn` |
| `examples/text/matmul.uft` | Matrix multiply (text encoding) |
| `examples/text/chat.uft` | Multi-user HTTP chat server: concurrency, FFI, SSE streaming |

### Repository

```
comp/      uf compiler (Rust → C → native)
trans/     C-to-µFlux transpiler, written in µFlux
examples/  Programs in both encodings
mods/      FFI binding manifests (.ufm)
SPEC.md    Language specification
```

µFlux is experimental and under active development. The current revision is v10.
