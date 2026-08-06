use crate::ast::*;
use crate::lex::*;
use std::collections::HashMap;

// ---------------- round-trip emitters (v9) ----------------
pub fn escape_str(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            '\r' => o.push_str("\\r"),
            '\0' => o.push_str("\\0"),
            c => o.push(c),
        }
    }
    o
}

// glyph codepoint for an opcode index (v10 table)
fn glyph_of(idx: usize) -> char {
    let cp = CUSTOM_OPS.iter().find(|&&(_, i)| i == idx).map(|&(g, _)| g).unwrap();
    char::from_u32(cp).unwrap()
}

// dense -> text: mnemonics; glyph names "v57" are already valid text idents
pub fn emit_text(toks: &[Tok]) -> String {
    let mut o = String::new();
    let mut in_weave = false;
    for t in toks {
        match t {
            Tok::Op(name) => o.push_str(&format!("{} ", text_mnemonic(op_index(name).expect("op")))),
            Tok::PushI(v) => o.push_str(&format!("{} ", v)),
            Tok::PushF(v) => o.push_str(&format!("{:?} ", v)),
            Tok::PushS(s) => o.push_str(&format!("\"{}\" ", escape_str(s))),
            Tok::Jump(op, l) => o.push_str(&format!("{} {} ", text_mnemonic(op_index(op).expect("jump")), l)),
            Tok::SetV(n) => o.push_str(&format!("{}! ", n)),
            Tok::GetV(n) => o.push_str(&format!("{}@ ", n)),
            Tok::Import(im) => o.push_str(&format!("import c\"{}\"({})->{} ", im.name, im.params.join(","), im.ret)),
            Tok::Export(n) => o.push_str(&format!("export \"{}\" ", escape_str(n))),
            Tok::Extern(n) => o.push_str(&format!("extern \"{}\" ", escape_str(n))),
            Tok::Use(n) => o.push_str(&format!("use \"{}\" ", escape_str(n))),
            Tok::Mod(n) => o.push_str(&format!("mod \"{}\" ", escape_str(n))),
            Tok::Pub => o.push_str("pub "),
            Tok::Method(n) => o.push_str(&format!("method {}: ", n)),
            Tok::MacroDef(n, body) => o.push_str(&format!("macro {} {{ {} }} ", n, emit_text(body))),
            Tok::StructDef(n, fields) => {
                let fs: Vec<String> = fields.iter().map(|(f, t)| format!("{}:{}", f, t)).collect();
                o.push_str(&format!("struct {} {{ {} }} ", n, fs.join(", ")));
            }
            Tok::Sys(n) => o.push_str(&format!("sys {} ", n)),
            Tok::Ident(n) => o.push_str(&format!("{} ", n)),
            Tok::LabelDef(n) => o.push_str(&format!("{}: ", n)),
            Tok::Entry => o.push_str("entry: "),
            Tok::Task { name, inputs, count, body } => {
                if !in_weave {
                    o.push_str("weave\n  ");
                    in_weave = true;
                }
                for i in inputs {
                    o.push_str(&format!("{} ", i));
                }
                if let Some(c) = count {
                    o.push_str(&format!("{} ", c));
                }
                o.push_str(&format!("task {} {} endt\n  ", name, emit_text(body)));
            }
            Tok::TaskEnd(_) => {}
            Tok::Entry => {}
            Tok::Wrun => {
                o.push_str("wrun\n");
                in_weave = false;
            }
        }
    }
    o.push('\n');
    o
}

// text -> dense: custom glyphs; labels/vars become v-space slots, assigned
// deterministically by first use (names already of the form v<N> keep slot N)
pub fn emit_dense(toks: &[Tok]) -> String {
    fn is_slot(n: &str) -> Option<u32> {
        n.strip_prefix('v').and_then(|d| d.parse::<u32>().ok()).filter(|&i| i < 64)
    }
    fn slot_glyphs(i: u32) -> String {
        char::from_u32(VAR_BASE + i).unwrap().to_string()
    }
    fn lrun(mut v: u64) -> String {
        let mut ds = vec![char::from_u32(LIT_BASE + (v % 64) as u32).unwrap()];
        v /= 64;
        while v > 0 {
            ds.push(char::from_u32(LIT_BASE + (v % 64) as u32).unwrap());
            v /= 64;
        }
        ds.iter().rev().collect()
    }
    fn nm(n: &str, names: &mut HashMap<String, u32>, next: &mut u32) -> String {
        if let Some(i) = is_slot(n) {
            return slot_glyphs(i);
        }
        if let Some(&i) = names.get(n) {
            return slot_glyphs(i);
        }
        let i = *next;
        *next += 1;
        if i >= 64 {
            panic!("emit-dense: more than 64 distinct label/var names");
        }
        names.insert(n.to_string(), i);
        slot_glyphs(i)
    }
    fn rec(toks: &[Tok], names: &mut HashMap<String, u32>, next: &mut u32, weave: &mut bool, o: &mut String) {
        for t in toks {
            match t {
                Tok::Op(name) => {
                    let idx = op_index(name).expect("op");
                    o.push(glyph_of(idx));
                    o.push(' ');
                }
                Tok::PushI(v) => {
                    if *v >= 0 {
                        o.push_str(&lrun(*v as u64));
                        o.push(' ');
                    } else {
                        o.push(glyph_of(0)); // LIT
                        o.push_str(&format!("{} ", v));
                    }
                }
                Tok::PushF(v) => {
                    o.push(glyph_of(0)); // LIT
                    o.push_str(&format!("{:?} ", v));
                }
                Tok::PushS(s) => o.push_str(&format!("\"{}\" ", escape_str(s))),
                Tok::Jump(op, l) => {
                    let idx = op_index(op).expect("jump");
                    if *op == "ADDR" {
                        o.push('\'');
                        o.push_str(&nm(l, names, next));
                        o.push(' ');
                    } else if *op == "JE" {
                        o.push_str("= ");
                        o.push_str(&nm(l, names, next));
                        o.push(' ');
                    } else {
                        o.push(glyph_of(idx));
                        o.push_str(&nm(l, names, next));
                        o.push(' ');
                    }
                }
                Tok::SetV(n) => o.push_str(&format!("{}! ", nm(n, names, next))),
                Tok::GetV(n) => o.push_str(&format!("{}@ ", nm(n, names, next))),
                Tok::Import(im) => o.push_str(&format!("{}c\"{}\"({})->{} ", glyph_of(51), im.name, im.params.join(","), im.ret)),
                Tok::Export(n) => o.push_str(&format!("{}\"{}\" ", glyph_of(52), escape_str(n))),
                Tok::Extern(n) => o.push_str(&format!("{}\"{}\" ", glyph_of(53), escape_str(n))),
                Tok::Use(n) => o.push_str(&format!("{}\"{}\" ", glyph_of(78), escape_str(n))),
                Tok::Mod(n) => o.push_str(&format!("{}\"{}\" ", glyph_of(79), escape_str(n))),
                Tok::Pub => o.push_str(&format!("{} ", glyph_of(80))),
                Tok::Method(_) => panic!("emit-dense: METHOD was removed in v10"),
                Tok::MacroDef(n, body) => {
                    o.push_str(&format!("{}{} {{ ", glyph_of(28), n));
                    rec(body, names, next, weave, o);
                    o.push_str("} ");
                }
                Tok::StructDef(n, fields) => {
                    let fs: Vec<String> = fields.iter().map(|(f, t)| format!("{}:{}", f, t)).collect();
                    o.push_str(&format!("{}{} {{ {} }} ", glyph_of(46), n, fs.join(", ")));
                }
                Tok::Sys(n) => o.push_str(&format!("{}{} ", glyph_of(49), n)),
                Tok::Ident(n) => o.push_str(&format!("{} ", n)),
                Tok::LabelDef(n) => o.push_str(&format!("{}\n", nm(n, names, next))),
                Tok::Entry => o.push_str(&format!("{}\n", glyph_of(195))),
                Tok::Task { name, inputs, count, body } => {
                    if !*weave {
                        o.push_str(&format!("{}\n", glyph_of(81)));
                        *weave = true;
                    }
                    for i in inputs {
                        o.push_str(&format!("{} ", nm(i, names, next)));
                    }
                    if let Some(c) = count {
                        o.push_str(&lrun(*c as u64));
                        o.push(' ');
                    }
                    o.push_str(&format!("{}{} ", glyph_of(82), nm(name, names, next)));
                    rec(body, names, next, weave, o);
                    o.push_str(&format!("{}\n", glyph_of(83)));
                }
                Tok::TaskEnd(_) => {}
                Tok::Entry => {}
                Tok::Wrun => {
                    o.push_str(&format!("{}\n", glyph_of(84)));
                    *weave = false;
                }
            }
        }
    }
    let mut o = String::new();
    let mut names: HashMap<String, u32> = HashMap::new();
    let mut next = 0u32;
    let mut weave = false;
    rec(toks, &mut names, &mut next, &mut weave, &mut o);
    o.push('\n');
    o
}
