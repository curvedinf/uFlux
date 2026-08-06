// ufc — reference µFlux compiler (SPEC.md, whitepaper v7.0)
// Pipeline: hieroglyph lexer -> parser (labels/macros/structs/imports) -> C codegen -> cc.
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
                eprintln!("usage: uf input.uf... ['inline source'|-]        compile+run (cached in TMPDIR)");
                eprintln!("       uf -c input.uf... [-o output] [--emit-c|--emit-text|--emit-dense]");
                eprintln!("       uf --to-text prog.uf | --to-dense prog.uft   convert encodings (writes prog.uft/.uf)");
                return;
            }
            s if s.starts_with('-') && s.is_ascii() && s != "-" => panic!("unknown option {}", s),
            s => inputs.push(s.to_string()),
        }
        i += 1;
    }
    if inputs.is_empty() {
        panic!("no input (usage: uf input.uf... ['inline source'|-] [-c] [-o output])");
    }
    // Each input is one translation unit; the FIRST input is the main TU.
    // Non-final inputs must be files; the final input may also be inline
    // source or "-" (stdin).
    let mut structs: StructMap = HashMap::new();
    let mut tus: Vec<Parsed> = Vec::new();
    let mut mods: Vec<String> = Vec::new();
    let mut emit_toks: Vec<Tok> = Vec::new();
    let mut hash_src = String::from("codegen-rev: slim16-bbvtab-forinl\n");
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
    let parsed = merge_tus(tus, mods);
    let csrc = gen(&parsed, &structs);

    // effective link line: pthread always (weave/chan/atom substrate), -l per USE
    let mut links: Vec<String> = vec!["-lpthread".to_string()];
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
        let status = Command::new("cc")
            .args(["-O2", "-w", "-o", &out, &tmpc])
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
        let status = Command::new("cc")
            .args(["-O2", "-w", "-o", &bins])
            .arg(&tmpc)
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
