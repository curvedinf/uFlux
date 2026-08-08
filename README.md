# µFlux

A scripting language that gives you **Python convenience at near-native speed**.

When an LLM writes Python for a data task, it's slow. When it writes C++,
it spends 3× the tokens on boilerplate. µFlux splits the difference: fewer
tokens than C++/Rust, 3–4× faster than Python, built-in ops for everything.

## Language Attributes

| Attribute | µFlux |
|---|---|
| Execution | Compiled (µFlux → C → native binary) |
| Typing | Dynamic and weak |
| Memory | Garbage collected |
| Primitives | int, float, str, ptr |
| Data structures | list, dict, arr, tensor, chan, atom, obj, bitmap, bloom, iter |
| C Imports | Zero-overhead, no glue |
| Concurrency | Channels, threads, dataflow DAGs with fanout |
| Built-ins | Regex, JSON, shell, I/O, streaming |

### Tokens to write (fewer = cheaper LLM calls)

| | µFlux | C++ | Rust | Python | Node.js |
|---|---|---|---|---|---|
| logextract | 332 | 846 | 667 | 329 | 437 |
| analytics | 348 | 793 | 756 | 366 | 471 |
| mandelbrot | 175 | 189 | 216 | 163 | 165 |
| spectralnorm | 442 | 350 | 487 | 258 | 390 |
| **total** | **1297** | **2178** | **2126** | **1116** | **1463** |

**33% fewer tokens than C++, 32% fewer than Rust** — the languages that match
its speed. More than Python, but Python is 3–4× slower.

Token counts use the **Qwen3-0.6B** tokenizer (vocab = 151,643). µFlux dense glyphs are each a single token in this vocab.

### Speed (seconds, lower = faster)

| | µFlux | C++ | Rust | Python | Node.js |
|---|---|---|---|---|---|
| logextract (510 MB) | **1.05** | 0.42 | 0.72 | 3.82 | 3.25 |
| analytics (512 MB) | **1.19** | 1.05 | 1.82 | 4.09 | 3.66 |
| mandelbrot | **0.10** | 0.05 | 0.05 | 3.98 | 0.07 |
| spectralnorm | **1.14** | 1.12 | 1.10 | 136.4 | 1.64 |
| **total** | **3.48** | **2.64** | **3.69** | **148.3** | **8.62** |

**3.7× faster than Python on data tasks**, within 1.1–2.5× of hand-tuned C++.

## Quick start

```sh
cd comp && cargo build --release
./comp/target/release/uf '"Hello!\n" print'
cargo install --path comp       # optional: install uf to PATH
```

## Usage

```sh
uf '"Hello µFlux!" print'      # run an inline program (cached)
uf prog.uf                     # compile + run (cached)
uf -c prog.uf -o hello         # compile to standalone binary
uf --emit-c prog.uf            # dump the generated C
uf --to-text prog.uf           # convert dense → text
uf somedir/                    # directory mode (auto-discovers main + init threads)
```

## Example

128×128 matrix multiply, text encoding — no headers, no memory management,
no imports, no declarations:

```
fi:  row! 128 'fe for ret
fe:  col! row@ 128 * col@ + ix!
     ^A@ ix@ row@ col@ + set
     ^B@ ix@ row@ col@ - set ret
cr:  row! 128 'dc for ret
dc:  col! 0 acc! 128 'ij for
     ^C@ row@ 128 * col@ + acc@ set ret
ij:  j! ^A@ row@ 128 * j@ + get
     ^B@ j@ 128 * col@ + get
     * acc@ + acc! ret
entry:
  16384 arr int ^A! 16384 arr int ^B! 16384 arr int ^C!
  128 'fi for
  128 'cr for
  ...
```

## Examples

| File | What it shows |
|------|---------------|
| `examples/hello.uf` | Minimal print |
| `examples/fib.uf` | Iterative Fibonacci (dense encoding) |
| `examples/dot.uf` | Typed arrays, vectorized dot product |
| `examples/maptest.uf` | Dict operations |
| `examples/ring.uf` | Channels and `spawn` |
| `examples/shelltest.uf` | Shell, streaming pipelines, `exec` |
| `examples/text/matmul.uft` | Matrix multiply (text encoding) |
| `examples/text/chat.uft` | Multi-user HTTP chat server (concurrency, FFI, SSE) |

## Documentation

Full language spec (every opcode, semantics, encoding rules) in
[`SPEC.md`](SPEC.md). Benchmark details in [`bench/SPEC.md`](bench/SPEC.md).

## Status

Experimental, under active development. Current revision: **v11**.
