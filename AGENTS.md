# AGENTS.md — µFlux

µFlux (Micro Flux): a stack-based language compiled to C then native via `cc`. Designed for LLM-authored scripts (low token count, fast, reliable). Current revision is **v11** — **not backward compatible** with v10 (local vs. global variable semantics changed). Full language spec in `SPEC.md`.

## Repository Layout

```
comp/      Rust compiler (→ C → native). 7 std-only modules in src/, no external crates.
trans/     C→µFlux transpiler, self-hosted in µFlux (text encoding).
examples/  Sample programs in both encodings.
mods/      FFI binding manifests (.ufm).
bench/     Cross-language benchmark suite.
SPEC.md    Normative language spec (~1200 lines, ~170+ ops).
README.md  Quickstart and project intro.
SPEC_V11_PROPOSAL.md, WEAVE_SPEC_PROPOSAL.md  Design proposals.
```

Compiler source map: `main.rs` (CLI/cache/cc), `lex.rs` (lexers + glyph/mnemonic tables), `parse.rs` (parser, label resolution, WEAVE DAG, v11 locals), `ast.rs` (types), `gen.rs` (C codegen + optimizations), `emit.rs` (encoding conversion), `prelude.rs` (embedded C runtime: GC, containers, opcodes, threading).

## Build

```sh
cd comp && cargo build --release      # binary at comp/target/release/uf
cargo install --path comp             # install uf to ~/.cargo/bin (on PATH via rustup)
```

No external Rust crates. Edition 2021, release profile `opt-level = 2`. Debug build: `cargo build` → `comp/target/debug/uf`.

## Running Programs

`uf --help` has the full CLI. Common forms: `uf prog.uf` (compile+run), `uf -c prog.uf -o bin`, `uf --emit-c prog.uf`, `uf --to-text`/`--to-dense` (encoding conversion), directory mode (auto-discovers `main.uf` + `init.uf` subdirs), inline source, multi-TU (multiple files in one invocation). See also `README.md`.

Runtime flags: `--gc-threshold N`, `--gc-off`, `--mt`.

## Testing

**No automated test runner, no Rust unit tests.** All tests are manual — compile and run, verify output by eye.

- **Integration tests**: `comp/tests/t01_basic.uft` … `t09_weave.uft`. One per feature area. Run: `uf comp/tests/tNN_*.uft`
- **Transpiler tests**: `trans/tests/*.c` — round-trip C→µFlux, compare against system binaries. See `trans/README.md`.
- **Benchmarks**: `cd bench && python3 run.py` (needs `.bench-venv/` with `transformers`; data in `bench/data/` is gitignored).

## Language & FFI Reference

All opcode semantics, control flow, variable scoping (`x!`/`x@` locals, `^x!`/`^x@` globals), structured concurrency (`spawn`/`chan`/`weave`), container protocol, string escapes, module system (`USE`/`import`/`extern`/`MOD`/`PUB`), and directory mode are in **`SPEC.md`**. Opcode→glyph/mnemonic tables are in `comp/src/lex.rs` (`OP_NAMES`, `OP_GLYPHS`, `text_mnemonic`).

Key gotchas not to re-derive: raw jumps (`jmp`/`jz`/`je`) are removed — compile errors. String escapes limited to `\n \t \r \0 \\ \"`. Linking is always `-lpthread -lm` plus `-l<name>` per `USE`.

## Development Notes

- No CI/CD, no formatter/linter.
- Experimental, under active development.
- Dense glyphs optimized for Qwen3-0.6B tokenizer (single-token per glyph).
- Transpiler is self-bootstrapped (`gen_trans.py` → `trans.uf` → `trans_bin.c`).
- Compiler cache key includes its own binary mtime — rebuilding auto-invalidates old cached outputs.
- **Any change to language semantics, opcodes, encodings, or behavior must be documented in `SPEC.md` in the same changeset.**
