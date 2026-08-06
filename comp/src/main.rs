// ufc — reference µFlux compiler (SPEC.md, whitepaper v7.0)
// Pipeline: hieroglyph lexer -> parser (labels/macros/structs/imports) -> C codegen -> cc.
// Std-only, single file.

use std::collections::{HashMap, VecDeque};
use std::env;
use std::fs;
use std::process::Command;

// ---------------- glyph tables ----------------
const OP_BASE: u32 = 0x13000; // legacy alias range U+13000+i (deprecated, still accepted)
const VAR_BASE: u32 = 0x13362; // v-space: variable/label name atoms, runs fold
const LIT_BASE: u32 = 0x133A4; // l-space: base-64 digit atoms for self-evaluating numbers
const DELIM_BASE: u32 = 0x13100;
const TYPE_BASE: u32 = 0x13110;

// v8 custom opcode glyphs, hand-picked for mnemonic value (see SPEC.md).
// Disjoint from v-space, l-space, delimiters (U+13100..13108) and type
// glyphs (U+13110..13117). Index = opcode index in OP_NAMES.
const CUSTOM_OPS: [(u32, usize); 104] = [
    (0x13340, 0),  // LIT    𓍀 papyrus scroll — a written constant
    (0x13050, 1),  // DUP    𓁐 pair — duplicate
    (0x13051, 2),  // OVR    𓁑 pair variant — second copy
    (0x130B9, 3),  // DRP    𓂹 falling — drop
    (0x13161, 4),  // SWP    𓅡 exchange — swap
    (0x130A9, 5),  // PICK   𓂩 hand — pick
    (0x1309D, 6),  // ADD    𓂝 arm (D36)
    (0x1309E, 7),  // SUB    𓂞 arm (D37)
    (0x130A1, 8),  // MUL    𓂡 arms — multiply
    (0x130A2, 9),  // AND    𓂢 arms joined
    (0x130D7, 10), // SHR    𓃗 shift right
    (0x130A5, 11), // INC    𓂥 arm raised
    (0x130A6, 12), // DEC    𓂦 arm lowered
    (0x130BB, 13), // JMP    𓂻 legs (D54)
    (0x130BC, 14), // JZ     𓂼 legs variant
    (0x130BD, 15), // JE     𓂽 legs variant
    (0x130BE, 16), // FOR    𓂾 legs in a loop
    (0x130C0, 17), // CALL   𓃀 foot (D58)
    (0x130BF, 18), // RET    𓂿 legs returning
    (0x13250, 19), // OBJ    𓉐 house (O1)
    (0x13077, 20), // GET    𓁷 eye (D6)
    (0x13078, 21), // SET    𓁸 eye variant
    (0x13090, 22), // SEND   𓂐 mouth (D26)
    (0x13251, 23), // ARR    𓉑 house row
    (0x13076, 24), // IDX    𓁶 eye (D5)
    (0x1307A, 25), // SETI   𓁺 eye storing
    (0x1310E, 26), // CLONE  𓄎 copy
    (0x1312A, 27), // CAST   𓄪 mold
    (0x13133, 28), // MACRO  𓄳
    (0x13253, 29), // TENSOR 𓉓 house grid
    (0x1311C, 30), // VEC    𓄜 fast
    (0x13254, 31), // PIN    𓉔
    (0x13255, 32), // UNPIN  𓉕
    (0x130A4, 33), // SETV   𓂤 hand storing
    (0x1307B, 34), // GETV   𓁻 eye fetching
    (0x1308B, 35), // STR    𓂋 mouth (D21)
    (0x1308C, 36), // CAT    𓂌 mouth joined
    (0x1308D, 37), // FMT    𓂍 mouth shaping
    (0x13256, 38), // BUF    𓉖
    (0x13257, 39), // BUFPTR 𓉗
    (0x13258, 40), // BUFCOPY 𓉘
    (0x1307C, 41), // ADDR   𓁼 pointing
    (0x13079, 42), // LOADX  𓁹 eye loading
    (0x1307D, 43), // STOREX 𓁽
    (0x1307E, 44), // SIZEOF 𓁾 measure
    (0x1307F, 45), // OFFSET 𓁿
    (0x13259, 46), // STRUCT 𓉙 house plan
    (0x1325B, 47), // MALLOC 𓉛
    (0x1325C, 48), // FREE   𓉜 broken
    (0x13260, 49), // SYS    𓉠 gate
    (0x1325D, 50), // GC     𓉝 sweeping
    (0x1325E, 51), // IMPORT 𓉞 door in
    (0x1325F, 52), // EXPORT 𓉟 door out
    (0x13262, 53), // EXTERN 𓉢
    (0x1308E, 54), // PRINT  𓂎 mouth printing
    (0x13080, 55), // SCAN   𓂀 eye reading (D4)
    // ---- v9 ----
    (0x132B5, 56), // DICT   𓊵 basket — keyed container
    (0x13081, 57), // DGET   𓂁 eye into basket
    (0x13082, 58), // DPUT   𓂂 eye storing into basket
    (0x13083, 59), // DDEL   𓂃 eye removing
    (0x13084, 60), // DCOUNT 𓂄 eye counting
    (0x13085, 61), // DKEYS  𓂅 eye listing
    (0x132B6, 62), // LIST   𓊶 basket row — growable vector
    (0x130A7, 63), // APPEND 𓂧 arm adding
    (0x130A8, 64), // POP    𓂨 hand taking
    (0x132B7, 65), // CHAN   𓊷 vessel — channel/ring
    (0x130BA, 66), // ENQ    𓂺 legs entering
    (0x130C1, 67), // DEQ    𓃁 foot leaving
    (0x13263, 68), // CLOSE  𓉣 door shut
    (0x132F0, 69), // ATOM   𓋰 indivisible
    (0x13086, 70), // AGET   𓂆 eye reading atom
    (0x13087, 71), // ASET   𓂇 eye writing atom
    (0x1309F, 72), // AADD   𓂟 arm adding to atom
    (0x130A0, 73), // CAS    𓂠 arms exchanging
    (0x1312B, 74), // TYPEOF 𓄫 identify mold
    (0x1312C, 75), // LEN    𓄬 measure length
    (0x1325A, 76), // FIELDS 𓉚 house fields
    (0x13091, 77), // METHOD 𓂑 mouth declaring
    (0x132F9, 78), // USE    𓋹 bringing in a library
    (0x132F8, 79), // MOD    𓋸 naming the unit
    (0x132F7, 80), // PUB    𓋷 making public
    (0x1340D, 81), // WEAVE  𓐍 interlacing threads
    (0x1340E, 82), // TASK   𓐎 one thread of work
    (0x1340F, 83), // ENDT   𓐏 thread end
    (0x13410, 84), // WRUN   𓐐 threads run
    // ---- shell ----
    (0x13189, 85), // SH     𓆉 turtle — carries a shell
    (0x1318A, 86), // SHX    𓆊 turtle variant — shell with capture
    (0x1318B, 87), // SHL    𓆋 turtle variant — shell to list
    (0x13217, 88), // SHP    𓈗 water — streaming shell
    (0x131A3, 89), // EXEC   𓆣 scarab — direct exec
    // ---- strings ----
    (0x1331C, 90), // RX     𓌜 knife — regex cut
    (0x1331D, 91), // RXSUB  𓌝 knife — regex replace
    (0x1331E, 92), // RXSPLIT 𓌞 knife — regex split
    (0x1331F, 93), // GLOB   𓌟 blade — filename match
    (0x13320, 94), // SPLIT  𓌠 blade — cut apart
    (0x13321, 95), // JOIN   𓌡 blade joined — tie together
    (0x13322, 96), // SLICE  𓌢 knife slice — substring
    (0x13323, 97), // FIND   𓌣 tool — seek substring
    (0x13324, 98), // REPL   𓌤 tool — replace
    (0x13325, 99), // TRIM   𓌥 knife — shave ends
    (0x13326, 100), // UP    𓌦 raised tool — uppercase
    (0x13327, 101), // DOWN  𓌧 lowered tool — lowercase
    (0x13328, 102), // STARTS 𓌨 head tool — prefix test
    (0x13329, 103), // ENDS  𓌩 tail tool — suffix test
];

// custom glyph table first, then the deprecated sequential aliases U+13000+i
fn opcode_index(c: char) -> Option<usize> {
    let cp = c as u32;
    if let Some(&(_, idx)) = CUSTOM_OPS.iter().find(|&&(g, _)| g == cp) {
        return Some(idx);
    }
    if cp >= OP_BASE && cp < OP_BASE + 56 {
        return Some((cp - OP_BASE) as usize);
    }
    None
}

// U+13110..U+13117 = int float ptr byte void handle str bool.
// Ids match type_id(): int 0, float 1, ptr 2, byte 3; handle/str are ptr
// aliases, bool a byte alias, void is 4 (SIZEOF void = 8, not useful).
fn glyph_type_id(c: char) -> Option<i64> {
    let cp = c as u32;
    if cp >= TYPE_BASE && cp <= TYPE_BASE + 7 {
        Some(match cp - TYPE_BASE {
            0 => 0,
            1 => 1,
            2 => 2,
            3 => 3,
            4 => 4,
            5 => 2,
            6 => 2,
            _ => 3,
        })
    } else {
        None
    }
}

const OP_NAMES: [&str; 104] = [
    "LIT", "DUP", "OVR", "DRP", "SWP", "PICK", "ADD", "SUB", "MUL", "AND", "SHR", "INC", "DEC",
    "JMP", "JZ", "JE", "FOR", "CALL", "RET", "OBJ", "GET", "SET", "SEND", "ARR", "IDX", "SETI",
    "CLONE", "CAST", "MACRO", "TENSOR", "VEC", "PIN", "UNPIN", "SETV", "GETV", "STR", "CAT", "FMT",
    "BUF", "BUFPTR", "BUFCOPY", "ADDR", "LOADX", "STOREX", "SIZEOF", "OFFSET", "STRUCT", "MALLOC",
    "FREE", "SYS", "GC", "IMPORT", "EXPORT", "EXTERN", "PRINT", "SCAN",
    // v9
    "DICT", "DGET", "DPUT", "DDEL", "DCOUNT", "DKEYS", "LIST", "APPEND", "POP", "CHAN",
    "ENQ", "DEQ", "CLOSE", "ATOM", "AGET", "ASET", "AADD", "CAS", "TYPEOF", "LEN", "FIELDS",
    "METHOD", "USE", "MOD", "PUB", "WEAVE", "TASK", "ENDT", "WRUN",
    // v9 shell + strings
    "SH", "SHX", "SHL", "SHP", "EXEC",
    "RX", "RXSUB", "RXSPLIT", "GLOB", "SPLIT", "JOIN", "SLICE", "FIND", "REPL",
    "TRIM", "UP", "DOWN", "STARTS", "ENDS",
];

fn op_index(name: &str) -> Option<usize> {
    OP_NAMES.iter().position(|n| *n == name)
}

fn type_id(kw: &str) -> Option<i64> {
    match kw {
        "int" => Some(0),
        "float" => Some(1),
        "ptr" | "handle" => Some(2),
        "byte" => Some(3),
        _ => None,
    }
}

// ---------------- tokens ----------------
#[derive(Clone, Debug)]
struct Import {
    name: String,
    params: Vec<String>, // may contain "..."
    ret: String,
}

#[derive(Clone, Debug)]
enum Tok {
    Op(&'static str),          // simple opcode with no immediate
    PushI(i64),
    PushF(f64),
    PushS(String),             // STR literal
    Jump(&'static str, String), // JMP/JZ/JE/FOR/CALL/ADDR + label
    SetV(String),
    GetV(String),
    Import(Import),
    Export(String),
    Extern(String),
    Use(String),                 // USE"name" — link -lname + load mods/<name>.ufm
    Mod(String),                 // MOD"name" — translation-unit name
    Pub,                         // PUB — export the next label def globally
    Method(String),              // METHOD TypeName: — register the next label as a method
    Task { name: String, inputs: Vec<String>, body: Vec<Tok> }, // inside WEAVE..WRUN
    TaskEnd(String),             // internal: end of task body, holds skip label
    Wrun,                        // WRUN — schedule the collected tasks
    MacroDef(String, Vec<Tok>),
    StructDef(String, Vec<(String, String)>), // field name, type name
    Sys(usize),                // arity immediate (default 0)
    Ident(String),             // macro invocation site
    LabelDef(String),
}

// ---------------- lexer ----------------
struct Lexer {
    chars: Vec<char>,
    pos: usize,
    // lookback state for the '-' sign/SUB disambiguation rule: true iff the
    // previous token pushed a value (number, string, GETV/@, type id,
    // ADDR/', or the value-leaving ops IDX/LOADX)
    pushed: bool,
}

impl Lexer {
    fn new(src: &str) -> Self {
        Lexer { chars: src.chars().collect(), pos: 0, pushed: false }
    }
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }
    fn next(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }
    fn skip_ws(&mut self) {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => {
                    self.pos += 1;
                }
                Some(';') => {
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                _ => break,
            }
        }
    }
    fn err(&self, msg: &str) -> ! {
        panic!("lex error at char {}: {}", self.pos, msg)
    }
    fn lex_ident(&mut self) -> String {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' {
                s.push(c);
                self.pos += 1;
            } else {
                break;
            }
        }
        s
    }
    fn lex_string(&mut self) -> String {
        // assumes current char is '"'
        self.pos += 1;
        let mut s = String::new();
        loop {
            match self.next() {
                None => self.err("unterminated string"),
                Some('"') => break,
                Some('\\') => match self.next() {
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    Some('r') => s.push('\r'),
                    Some('0') => s.push('\0'),
                    Some('\\') => s.push('\\'),
                    Some('"') => s.push('"'),
                    Some(c) => s.push(c),
                    None => self.err("unterminated escape"),
                },
                Some(c) => s.push(c),
            }
        }
        s
    }
    // fold a run of slot glyphs (U+13080..U+130BF) into one name "v0_1_..."
    fn fold_slots(&mut self) -> String {
        let mut name = String::from("v");
        let mut first = true;
        while let Some(c) = self.peek() {
            let cp = c as u32;
            if cp >= VAR_BASE && cp < VAR_BASE + 64 {
                if !first {
                    name.push('_');
                }
                name.push_str(&(cp - VAR_BASE).to_string());
                first = false;
                self.pos += 1;
            } else {
                break;
            }
        }
        name
    }
    // read a run of l-space digit atoms (U+133A4..U+133E3), big-endian base-64.
    // Returns the digit values; empty if the next char is not an l-glyph.
    fn lrun(&mut self) -> Vec<u32> {
        let mut ds = Vec::new();
        while let Some(c) = self.peek() {
            let cp = c as u32;
            if cp >= LIT_BASE && cp < LIT_BASE + 64 {
                ds.push(cp - LIT_BASE);
                self.pos += 1;
            } else {
                break;
            }
        }
        ds
    }
    fn lrun_u64(&mut self) -> u64 {
        let ds = self.lrun();
        let mut v: u64 = 0;
        for d in ds {
            v = v
                .checked_mul(64)
                .and_then(|x| x.checked_add(d as u64))
                .unwrap_or_else(|| self.err("base-64 literal overflow (> u64)"));
        }
        v
    }
    // full l-space number grammar (SPEC.md):
    //   number := ['-'] lrun ['.' lrun] ['e' ['-'] lrun]
    // bare lrun = big-endian base-64 unsigned int; '.' = base-64 fixed point
    // (d1/64 + d2/4096 + ...); 'e' = decimal scientific (x10^exp); '-' = sign
    // (only reachable under the lookback rule; neg=true comes from lex_into).
    fn lex_lnumber(&mut self, neg: bool) -> Tok {
        let v = self.lrun_u64();
        let mut is_float = false;
        let mut frac = 0.0f64;
        if self.peek() == Some('.') {
            self.pos += 1;
            let ds = self.lrun();
            if ds.is_empty() {
                self.err("'.' in a number must be followed by an l-run");
            }
            is_float = true;
            let mut scale = 64.0f64;
            for d in ds {
                frac += d as f64 / scale;
                scale *= 64.0;
                if !scale.is_finite() {
                    break; // further digits are below f64 resolution
                }
            }
        }
        let mut exp: i64 = 0;
        if self.peek() == Some('e') {
            self.pos += 1;
            let eneg = if self.peek() == Some('-') {
                self.pos += 1;
                true
            } else {
                false
            };
            let ds = self.lrun();
            if ds.is_empty() {
                self.err("'e' in a number must be followed by an l-run exponent");
            }
            let mut e: u64 = 0;
            for d in ds {
                e = e.saturating_mul(64).saturating_add(d as u64);
            }
            exp = if eneg { -(e.min(400) as i64) } else { e.min(400) as i64 };
            is_float = true;
        }
        if !is_float {
            let i = v as i64;
            return Tok::PushI(if neg { i.wrapping_neg() } else { i });
        }
        let mut f = (v as f64 + frac) * 10f64.powi(exp.clamp(-400, 400) as i32);
        if neg {
            f = -f;
        }
        Tok::PushF(f)
    }
    // lex the whole source
    fn lex(&mut self) -> Vec<Tok> {
        let mut out = Vec::new();
        self.lex_into(&mut out, 0);
        out
    }
    // lex until EOF (stop=0), until matching '}' (1), or until the ENDT glyph (2)
    fn lex_into(&mut self, out: &mut Vec<Tok>, stop: u8) {
        loop {
            self.skip_ws();
            let c = match self.peek() {
                None => {
                    if stop != 0 {
                        self.err("unterminated block");
                    }
                    return;
                }
                Some(c) => c,
            };
            let cp = c as u32;
            if cp >= DELIM_BASE && cp <= DELIM_BASE + 8 {
                self.pos += 1;
                continue; // delimiters are invisible; lookback state unchanged
            }
            if stop == 1 && c == '}' {
                self.pos += 1;
                return;
            }
            if stop == 2 && opcode_index(c) == Some(83) {
                // ENDT ends a task body
                self.pos += 1;
                return;
            }
            // ---- l-space: self-evaluating base-64 number ----
            if cp >= LIT_BASE && cp < LIT_BASE + 64 {
                out.push(self.lex_lnumber(false));
                self.pushed = true;
                continue;
            }
            // ---- v-space: variable use or label definition ----
            if cp >= VAR_BASE && cp < VAR_BASE + 64 {
                // fold a run of v-glyphs into one name
                let name = self.fold_slots();
                self.skip_ws();
                match self.peek() {
                    // SETV/GETV: variable use ('!'/'@' ASCII tokens or glyphs)
                    Some('!') => {
                        self.pos += 1;
                        out.push(Tok::SetV(name));
                        self.pushed = false;
                    }
                    Some('@') => {
                        self.pos += 1;
                        out.push(Tok::GetV(name));
                        self.pushed = true;
                    }
                    Some(g) if opcode_index(g) == Some(33) => {
                        self.pos += 1;
                        out.push(Tok::SetV(name));
                        self.pushed = false;
                    }
                    Some(g) if opcode_index(g) == Some(34) => {
                        self.pos += 1;
                        out.push(Tok::GetV(name));
                        self.pushed = true;
                    }
                    // otherwise: glyph label definition (no colon)
                    _ => {
                        out.push(Tok::LabelDef(name));
                        self.pushed = false;
                    }
                }
                continue;
            }
            // ---- ASCII operator tokens ----
            match c {
                '+' => {
                    self.pos += 1;
                    out.push(Tok::Op("ADD"));
                    self.pushed = true; // value-leaving op (matches the ADD glyph)
                    continue;
                }
                '*' => {
                    self.pos += 1;
                    out.push(Tok::Op("MUL"));
                    self.pushed = true;
                    continue;
                }
                '&' => {
                    self.pos += 1;
                    out.push(Tok::Op("AND"));
                    self.pushed = true;
                    continue;
                }
                '=' => {
                    self.pos += 1;
                    self.skip_ws();
                    let label = self.jump_label("JE");
                    out.push(Tok::Jump("JE", label));
                    self.pushed = false;
                    continue;
                }
                '\'' => {
                    // ADDR: '<label> (v-run or ASCII)
                    self.pos += 1;
                    self.skip_ws();
                    let label = self.jump_label("ADDR");
                    out.push(Tok::Jump("ADDR", label));
                    self.pushed = true;
                    continue;
                }
                '"' => {
                    // bare string self-evaluates (PushS)
                    out.push(Tok::PushS(self.lex_string()));
                    self.pushed = true;
                    continue;
                }
                '-' => {
                    self.pos += 1;
                    if self.pushed {
                        // previous token pushed a value: this is SUB
                        out.push(Tok::Op("SUB"));
                        self.pushed = false;
                    } else {
                        // sign position: only a negative l-number may follow
                        self.skip_ws();
                        match self.peek() {
                            Some(g) if (g as u32) >= LIT_BASE && (g as u32) < LIT_BASE + 64 => {
                                out.push(self.lex_lnumber(true));
                                self.pushed = true;
                            }
                            _ => self.err("stray '-' (SUB needs a value-pushing token before it; sign needs an l-run after it)"),
                        }
                    }
                    continue;
                }
                '.' => self.err("stray '.' (only valid between l-runs in a number)"),
                '@' | '!' => self.err("bare '@'/'!' must directly follow a v-run"),
                _ => {}
            }
            // ---- bare type glyph: pushes the type id ----
            if let Some(id) = glyph_type_id(c) {
                // (also valid after LIT and after type-expecting opcodes, handled in lex_op)
                self.pos += 1;
                out.push(Tok::PushI(id));
                self.pushed = true;
                continue;
            }
            // ---- opcode glyphs (custom table + legacy aliases) ----
            if let Some(idx) = opcode_index(c) {
                self.pos += 1;
                self.lex_op(idx, out);
                continue;
            }
            if c.is_ascii_alphabetic() || c == '_' {
                let name = self.lex_ident();
                if self.peek() == Some(':') {
                    self.pos += 1;
                    out.push(Tok::LabelDef(name));
                } else {
                    out.push(Tok::Ident(name));
                }
                self.pushed = false;
                continue;
            }
            if cp >= OP_BASE && cp < DELIM_BASE + 9 {
                self.err(&format!("glyph U+{:04X} is not assigned", cp));
            }
            self.err(&format!("unexpected character {:?}", c));
        }
    }

    // label operand for JMP/JZ/JE/CALL/ADDR/'='/'\'' sites: v-run or ASCII ident
    fn jump_label(&mut self, op: &str) -> String {
        if let Some(c) = self.peek() {
            let cp = c as u32;
            if cp >= VAR_BASE && cp < VAR_BASE + 64 {
                return self.fold_slots();
            }
        }
        let label = self.lex_ident();
        if label.is_empty() {
            self.err(&format!("{} needs a label", op));
        }
        label
    }

    fn lex_op(&mut self, idx: usize, out: &mut Vec<Tok>) {
        let name = OP_NAMES[idx];
        self.lex_op_inner(idx, out);
        // lookback update: literals/strings and every value-leaving opcode
        // count as "pushed a value"; purely consuming/structural ops do not
        self.pushed = matches!(
            name,
            "LIT" | "STR" | "DUP" | "OVR" | "PICK" | "ADD" | "SUB" | "MUL" | "AND" | "SHR"
                | "INC" | "DEC" | "GET" | "IDX" | "CLONE" | "CAST" | "ARR" | "TENSOR" | "OBJ"
                | "CAT" | "FMT" | "BUF" | "MALLOC" | "LOADX" | "SIZEOF" | "OFFSET" | "PRINT"
                | "SCAN" | "CALL" | "SYS" | "DICT" | "DGET" | "DCOUNT" | "DKEYS" | "LIST"
                | "APPEND" | "POP" | "CHAN" | "DEQ" | "ATOM" | "AGET" | "AADD" | "CAS"
                | "TYPEOF" | "LEN" | "FIELDS" | "SEND"
                | "SH" | "SHX" | "SHL" | "SHP" | "EXEC"
                | "RX" | "RXSUB" | "RXSPLIT" | "GLOB" | "SPLIT" | "JOIN" | "SLICE"
                | "FIND" | "REPL" | "TRIM" | "UP" | "DOWN" | "STARTS" | "ENDS"
        );
    }

    fn lex_op_inner(&mut self, idx: usize, out: &mut Vec<Tok>) {
        let name = OP_NAMES[idx];
        match name {
            "LIT" => {
                self.skip_ws();
                let c = self.peek().unwrap_or_else(|| self.err("LIT needs a literal"));
                let cp = c as u32;
                if let Some(id) = glyph_type_id(c) {
                    // type glyph after LIT: push the type id (like `LIT int`)
                    self.pos += 1;
                    out.push(Tok::PushI(id));
                } else if cp >= LIT_BASE && cp < LIT_BASE + 64 {
                    // l-run number after LIT (same grammar as a bare l-run)
                    out.push(self.lex_lnumber(false));
                } else if c.is_ascii_alphabetic() {
                    let kw = self.lex_ident();
                    match type_id(&kw) {
                        Some(id) => out.push(Tok::PushI(id)),
                        None => self.err(&format!("unknown type keyword {}", kw)),
                    }
                } else {
                    let mut s = String::new();
                    while let Some(c) = self.peek() {
                        if c.is_ascii_digit()
                            || matches!(c, 'x' | 'X' | 'a'..='f' | 'A'..='F' | '.' | '+' | '-' | '_')
                        {
                            s.push(c);
                            self.pos += 1;
                        } else {
                            break;
                        }
                    }
                    let s = s.replace('_', "");
                    if s.starts_with("0x") || s.starts_with("0X") {
                        out.push(Tok::PushI(
                            i64::from_str_radix(&s[2..], 16).unwrap_or_else(|_| self.err("bad hex literal")),
                        ));
                    } else if s.contains('.') || s.contains('e') || s.contains('E') {
                        out.push(Tok::PushF(s.parse().unwrap_or_else(|_| self.err("bad float literal"))));
                    } else {
                        out.push(Tok::PushI(s.parse().unwrap_or_else(|_| self.err("bad int literal"))));
                    }
                }
            }
            "STR" => {
                self.skip_ws();
                if self.peek() != Some('"') {
                    self.err("STR must be followed by a string literal");
                }
                out.push(Tok::PushS(self.lex_string()));
            }
            "JMP" | "JZ" | "JE" | "CALL" | "ADDR" => {
                self.skip_ws();
                let label = self.jump_label(name);
                out.push(Tok::Jump(OP_NAMES[idx], label));
            }
            "IMPORT" => {
                self.skip_ws();
                if self.next() != Some('c') {
                    self.err("IMPORT must be followed by c\"name\"(sig)->ret");
                }
                self.skip_ws();
                if self.peek() != Some('"') {
                    self.err("IMPORT: expected string");
                }
                let fname = self.lex_string();
                self.skip_ws();
                if self.next() != Some('(') {
                    self.err("IMPORT: expected '('");
                }
                let mut params = Vec::new();
                let mut cur = String::new();
                loop {
                    match self.next() {
                        Some(')') => {
                            if !cur.trim().is_empty() {
                                params.push(cur.trim().to_string());
                            }
                            break;
                        }
                        Some(',') => {
                            params.push(cur.trim().to_string());
                            cur.clear();
                        }
                        Some(c) => cur.push(c),
                        None => self.err("IMPORT: unterminated signature"),
                    }
                }
                self.skip_ws();
                // accept "->" or bare ">" (paper typo tolerated)
                if self.peek() == Some('-') {
                    self.pos += 1;
                }
                if self.next() != Some('>') {
                    self.err("IMPORT: expected '->'");
                }
                self.skip_ws();
                let ret = self.lex_ident();
                out.push(Tok::Import(Import { name: fname, params, ret }));
            }
            "EXPORT" => {
                self.skip_ws();
                if self.peek() != Some('"') {
                    self.err("EXPORT must be followed by \"name\"");
                }
                out.push(Tok::Export(self.lex_string()));
            }
            "USE" => {
                self.skip_ws();
                if self.peek() != Some('"') {
                    self.err("USE must be followed by \"name\"");
                }
                out.push(Tok::Use(self.lex_string()));
            }
            "MOD" => {
                self.skip_ws();
                if self.peek() != Some('"') {
                    self.err("MOD must be followed by \"name\"");
                }
                out.push(Tok::Mod(self.lex_string()));
            }
            "PUB" => {
                out.push(Tok::Pub);
            }
            "WEAVE" => {
                // compile-time task scope: (input v-runs)* <name v-run> TASK body ENDT
                // repeated until the WRUN glyph
                loop {
                    self.skip_ws();
                    match self.peek() {
                        Some(g) if opcode_index(g) == Some(84) => {
                            self.pos += 1;
                            out.push(Tok::Wrun);
                            break;
                        }
                        Some(g) if (g as u32) >= VAR_BASE && (g as u32) < VAR_BASE + 64 => {
                            let mut runs = Vec::new();
                            while let Some(g2) = self.peek() {
                                if (g2 as u32) >= VAR_BASE && (g2 as u32) < VAR_BASE + 64 {
                                    runs.push(self.fold_slots());
                                    self.skip_ws();
                                } else {
                                    break;
                                }
                            }
                            match self.peek() {
                                Some(g) if opcode_index(g) == Some(82) => self.pos += 1,
                                _ => self.err("WEAVE: expected TASK glyph after task name"),
                            }
                            let name = runs.pop().unwrap_or_else(|| self.err("WEAVE: task needs a name"));
                            let mut body = Vec::new();
                            self.lex_into(&mut body, 2);
                            out.push(Tok::Task { name, inputs: runs, body });
                        }
                        _ => self.err("WEAVE: expected task name or WRUN"),
                    }
                }
            }
            "TASK" | "ENDT" | "WRUN" => self.err("TASK/ENDT/WRUN are only valid inside WEAVE..WRUN"),
            "METHOD" => {
                self.skip_ws();
                let tname = self.lex_ident();
                self.skip_ws();
                if self.next() != Some(':') {
                    self.err("METHOD must be followed by TypeName:");
                }
                out.push(Tok::Method(tname));
            }
            "SEND" => {
                // optional method-name immediate: SEND "name" / SEND name pushes
                // the interned hash; otherwise the method id comes from the stack
                self.skip_ws();
                match self.peek() {
                    Some('"') => {
                        let m = self.lex_string();
                        out.push(Tok::PushI(fnv64(&m)));
                        out.push(Tok::Op("SEND"));
                    }
                    Some(c) if c.is_ascii_alphabetic() => {
                        let m = self.lex_ident();
                        out.push(Tok::PushI(fnv64(&m)));
                        out.push(Tok::Op("SEND"));
                    }
                    _ => out.push(Tok::Op("SEND")),
                }
            }
            "EXTERN" => {
                self.skip_ws();
                if self.peek() != Some('"') {
                    self.err("EXTERN must be followed by \"symbol\"");
                }
                out.push(Tok::Extern(self.lex_string()));
            }
            "MACRO" => {
                self.skip_ws();
                let mname = self.lex_ident();
                self.skip_ws();
                if self.next() != Some('{') {
                    self.err("MACRO: expected '{'");
                }
                let mut body = Vec::new();
                self.lex_into(&mut body, 1);
                out.push(Tok::MacroDef(mname, body));
            }
            "STRUCT" => {
                self.skip_ws();
                let sname = self.lex_ident();
                self.skip_ws();
                if self.next() != Some('{') {
                    self.err("STRUCT: expected '{'");
                }
                let mut fields = Vec::new();
                let mut cur = String::new();
                loop {
                    match self.next() {
                        Some('}') => {
                            if !cur.trim().is_empty() {
                                let f: Vec<&str> = cur.trim().splitn(2, ':').collect();
                                if f.len() != 2 {
                                    self.err("STRUCT field must be name:type");
                                }
                                fields.push((f[0].trim().to_string(), f[1].trim().to_string()));
                            }
                            break;
                        }
                        Some(',') => {
                            let f: Vec<&str> = cur.trim().splitn(2, ':').collect();
                            if f.len() != 2 {
                                self.err("STRUCT field must be name:type");
                            }
                            fields.push((f[0].trim().to_string(), f[1].trim().to_string()));
                            cur.clear();
                        }
                        Some(c) => cur.push(c),
                        None => self.err("STRUCT: unterminated body"),
                    }
                }
                out.push(Tok::StructDef(sname, fields));
            }
            "SIZEOF" => {
                self.skip_ws();
                if let Some(c) = self.peek() {
                    if let Some(id) = glyph_type_id(c) {
                        self.pos += 1;
                        out.push(Tok::PushI(id));
                        out.push(Tok::Op("SIZEOF"));
                        return;
                    }
                    if c.is_ascii_alphabetic() {
                        let sym = self.lex_ident();
                        out.push(Tok::Ident(format!("@sizeof:{}", sym)));
                        return;
                    }
                }
                out.push(Tok::Op("SIZEOF"));
            }
            "OFFSET" => {
                self.skip_ws();
                let sym = self.lex_ident();
                if self.peek() == Some('.') {
                    self.pos += 1;
                    let field = self.lex_ident();
                    out.push(Tok::Ident(format!("@offset:{}.{}", sym, field)));
                } else if !sym.is_empty() {
                    out.push(Tok::Ident(format!("@offset:{}", sym)));
                } else {
                    self.err("OFFSET needs Struct.field");
                }
            }
            "OBJ" | "CAST" | "ARR" | "TENSOR" => {
                self.skip_ws();
                // ARR/TENSOR take [ty, len] with len on top; a type immediate
                // follows the length in source, so it lands above len and must
                // be swapped underneath before the op runs.
                let needs_swp = name == "ARR" || name == "TENSOR";
                if let Some(c) = self.peek() {
                    if let Some(id) = glyph_type_id(c) {
                        // type-glyph immediate: push the id, then the op
                        self.pos += 1;
                        out.push(Tok::PushI(id));
                        if needs_swp {
                            out.push(Tok::Op("SWP"));
                        }
                        out.push(Tok::Op(OP_NAMES[idx]));
                        return;
                    }
                    if c.is_ascii_alphabetic() {
                        let kw = self.lex_ident();
                        if let Some(id) = type_id(&kw) {
                            out.push(Tok::PushI(id));
                            if needs_swp {
                                out.push(Tok::Op("SWP"));
                            }
                        } else if name == "OBJ" {
                            // struct name: resolved at parse time via @objsize
                            out.push(Tok::Ident(format!("@objsize:{}", kw)));
                        } else if name == "CAST" {
                            // struct name: checked downcast via @cast
                            out.push(Tok::Ident(format!("@cast:{}", kw)));
                        } else {
                            self.err(&format!("unknown type keyword {}", kw));
                        }
                        out.push(Tok::Op(OP_NAMES[idx]));
                        return;
                    }
                }
                out.push(Tok::Op(OP_NAMES[idx]));
            }
            "SYS" => {
                self.skip_ws();
                let mut n = 0usize;
                let mut got = false;
                while let Some(c) = self.peek() {
                    if c.is_ascii_digit() {
                        n = n * 10 + (c as usize - '0' as usize);
                        got = true;
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                out.push(Tok::Sys(if got { n } else { 0 }));
            }
            _ => {
                out.push(Tok::Op(OP_NAMES[idx]));
            }
        }
    }
}

// ---------------- parser ----------------
#[derive(Clone, Debug)]
enum Ins {
    PushI(i64),
    PushF(f64),
    PushS(usize),
    PushAddr(String),
    Simple(&'static str), // maps to a C helper op_*
    Jmp(String),
    Jz(String),
    Je(String),
    For,
    Call(String),
    CallExt(usize),
    Ret,
    SetV(String),
    GetV(String),
    Extern(String),
    Send,                        // runtime dispatch through the method table
    Weave(Vec<WeaveMeta>),       // schedule a static task DAG, publish results
    Sys(usize),
}

#[derive(Clone, Debug)]
struct WeaveMeta {
    name: String,      // task name (also the variable it publishes to)
    pc: String,        // entry label
    inputs: Vec<usize>, // task indices in the same weave
}

struct Parsed {
    ins: Vec<Ins>,
    labels: HashMap<String, usize>,
    imports: Vec<Import>,
    exports: Vec<(String, String)>, // C name, label
    externs: Vec<String>,
    strings: Vec<String>,
    vars: Vec<String>,
    uses: Vec<String>,
    modname: Option<String>,
    pubs: Vec<String>, // label names marked PUB (exported cross-TU)
    methods: Vec<(i64, i64, String)>, // (type key, method name hash, label)
}

// struct layouts: field name -> offset, total size, struct id. Shared across
// TUs and manifests (structs are global; labels/vars/macros are file-local).
type StructMap = HashMap<String, (Vec<(String, i64)>, i64, i64)>;

// FNV-1a 64 over bytes (matches uf_fnv in the C prelude)
fn fnv64(s: &str) -> i64 {
    let mut h: u64 = 1469598103934665603;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    h as i64
}

// method dispatch type key: struct names -> 1000+sid, container names -> HT_*,
// scalar type keywords -> 0..3
fn method_typekey(name: &str, structs: &StructMap) -> Option<i64> {
    if let Some((_, _, sid)) = structs.get(name) {
        return Some(1000 + sid);
    }
    match name {
        "int" => Some(0),
        "float" => Some(1),
        "ptr" | "handle" => Some(2),
        "byte" => Some(3),
        "arr" => Some(5),
        "tensor" => Some(6),
        "dyn" | "list" => Some(7),
        "map" | "dict" => Some(8),
        "ring" | "chan" => Some(9),
        "atom" => Some(10),
        "str" => Some(11),
        "obj" => Some(13),
        _ => None,
    }
}

fn parse(toks: Vec<Tok>, structs: &mut StructMap) -> Parsed {
    let mut macros: HashMap<String, Vec<Tok>> = HashMap::new();
    let mut imports: Vec<Import> = Vec::new();
    let mut p = Parsed {
        ins: Vec::new(),
        labels: HashMap::new(),
        imports: Vec::new(),
        exports: Vec::new(),
        externs: Vec::new(),
        strings: Vec::new(),
        vars: Vec::new(),
        uses: Vec::new(),
        modname: None,
        pubs: Vec::new(),
        methods: Vec::new(),
    };
    let mut q: VecDeque<Tok> = toks.into();
    let mut pending_export: Option<String> = None;
    let mut pending_pub = false;
    let mut pending_method: Option<String> = None;
    let mut weave_ctr = 0usize;
    let mut cur_weave: Vec<(String, Vec<String>)> = Vec::new();
    let mut expand_depth = 0usize;
    while let Some(t) = q.pop_front() {
        match t {
            Tok::Op("RET") => p.ins.push(Ins::Ret),
            Tok::Op("FOR") => p.ins.push(Ins::For),
            Tok::Op("SEND") => p.ins.push(Ins::Send),
            Tok::Op(name) => p.ins.push(simple_ins(name)),
            Tok::PushI(v) => p.ins.push(Ins::PushI(v)),
            Tok::PushF(v) => p.ins.push(Ins::PushF(v)),
            Tok::PushS(s) => {
                let idx = p.strings.len();
                p.strings.push(s);
                p.ins.push(Ins::PushS(idx));
            }
            Tok::Jump(op, label) => match op {
                "JMP" => p.ins.push(Ins::Jmp(label)),
                "JZ" => p.ins.push(Ins::Jz(label)),
                "JE" => p.ins.push(Ins::Je(label)),
                "ADDR" => p.ins.push(Ins::PushAddr(label)),
                "CALL" => {
                    if let Some(ii) = p.imports.iter().position(|im| im.name == label) {
                        p.ins.push(Ins::CallExt(ii));
                    } else {
                        p.ins.push(Ins::Call(label));
                    }
                }
                _ => unreachable!(),
            },
            Tok::SetV(n) => {
                if !p.vars.contains(&n) {
                    p.vars.push(n.clone());
                }
                p.ins.push(Ins::SetV(n));
            }
            Tok::GetV(n) => {
                if !p.vars.contains(&n) {
                    p.vars.push(n.clone());
                }
                p.ins.push(Ins::GetV(n));
            }
            Tok::Import(im) => {
                if !p.imports.iter().any(|x| x.name == im.name) {
                    p.imports.push(im);
                }
            }
            Tok::Export(n) => pending_export = Some(n),
            Tok::Use(n) => {
                if !p.uses.contains(&n) {
                    p.uses.push(n);
                }
            }
            Tok::Mod(n) => p.modname = Some(n),
            Tok::Pub => pending_pub = true,
            Tok::Extern(n) => {
                if !p.externs.contains(&n) {
                    p.externs.push(n.clone());
                }
                p.ins.push(Ins::Extern(n));
            }
            Tok::MacroDef(n, body) => {
                macros.insert(n, body);
            }
            Tok::StructDef(n, fields) => {
                let mut off = 0i64;
                let mut fs = Vec::new();
                for (fname, fty) in &fields {
                    let sz = match type_id(fty) {
                        Some(3) => 1,
                        Some(_) => 8,
                        None => match structs.get(fty) {
                            Some((_, tsz, _)) => *tsz,
                            None => panic!("STRUCT {}: unknown field type {}", n, fty),
                        },
                    };
                    // natural alignment up to 8
                    let align = sz.min(8);
                    off = (off + align - 1) / align * align;
                    fs.push((fname.clone(), off));
                    off += sz;
                }
                let sid = structs.len() as i64;
                structs.insert(n, (fs, off, sid));
            }
            Tok::Method(tname) => pending_method = Some(tname),
            Tok::Sys(n) => p.ins.push(Ins::Sys(n)),
            Tok::Task { name, inputs, body } => {
                // emit: JMP over the body; task entry label; body; RET (via TaskEnd)
                let skip = format!("__wskip{}", weave_ctr);
                weave_ctr += 1;
                p.ins.push(Ins::Jmp(skip.clone()));
                p.labels.insert(name.clone(), p.ins.len());
                // task bodies are self-contained: their labels are task-local,
                // so two tasks may reuse the same v-names
                let prefix = format!("{}/", name);
                let body = prefix_task_labels(body, &prefix);
                q.push_front(Tok::TaskEnd(skip));
                for t in body.into_iter().rev() {
                    q.push_front(t);
                }
                cur_weave.push((name, inputs));
            }
            Tok::TaskEnd(skip) => {
                p.ins.push(Ins::Ret);
                p.labels.insert(skip, p.ins.len());
            }
            Tok::Wrun => {
                // static DAG checks: inputs must name tasks in this weave; acyclic
                let names: Vec<&String> = cur_weave.iter().map(|(n, _)| n).collect();
                let mut metas: Vec<WeaveMeta> = Vec::new();
                for (n, ins) in &cur_weave {
                    let mut idxs = Vec::new();
                    for inp in ins {
                        match names.iter().position(|x| *x == inp) {
                            Some(i) => idxs.push(i),
                            None => panic!("WEAVE: task {} has unknown input {}", n, inp),
                        }
                    }
                    if idxs.len() > 8 {
                        panic!("WEAVE: task {} has more than 8 inputs", n);
                    }
                    metas.push(WeaveMeta { name: n.clone(), pc: n.clone(), inputs: idxs });
                }
                // Kahn topo: cycle check (execution order is the scheduler's job)
                let mut done = vec![false; metas.len()];
                for _ in 0..metas.len() {
                    let mut progressed = false;
                    for (i, m) in metas.iter().enumerate() {
                        if !done[i] && m.inputs.iter().all(|&j| done[j]) {
                            done[i] = true;
                            progressed = true;
                        }
                    }
                    if !progressed {
                        break;
                    }
                }
                if done.iter().any(|d| !d) {
                    panic!("WEAVE: cycle in task DAG");
                }
                if !metas.is_empty() {
                    for m in &metas {
                        if !p.vars.contains(&m.name) {
                            p.vars.push(m.name.clone());
                        }
                    }
                    p.ins.push(Ins::Weave(metas));
                }
                cur_weave.clear();
            }
            Tok::LabelDef(n) => {
                p.labels.insert(n.clone(), p.ins.len());
                if pending_pub {
                    p.pubs.push(n.clone());
                    pending_pub = false;
                }
                if let Some(tname) = pending_method.take() {
                    let tk = method_typekey(&tname, structs)
                        .unwrap_or_else(|| panic!("METHOD: unknown type {}", tname));
                    p.methods.push((tk, fnv64(&n), n.clone()));
                }
                if let Some(x) = pending_export.take() {
                    p.exports.push((x, n));
                }
            }
            Tok::Ident(n) => {
                if let Some(rest) = n.strip_prefix("@sizeof:") {
                    if let Some(id) = type_id(rest) {
                        p.ins.push(Ins::PushI(if id == 3 { 1 } else { 8 }));
                    } else if let Some((_, tsz, _)) = structs.get(rest) {
                        p.ins.push(Ins::PushI(*tsz));
                    } else {
                        panic!("SIZEOF: unknown type {}", rest);
                    }
                } else if let Some(rest) = n.strip_prefix("@offset:") {
                    let parts: Vec<&str> = rest.splitn(2, '.').collect();
                    if parts.len() != 2 {
                        panic!("OFFSET needs Struct.field, got {}", rest);
                    }
                    let (fs, _, _) = structs
                        .get(parts[0])
                        .unwrap_or_else(|| panic!("OFFSET: unknown struct {}", parts[0]));
                    let off = fs
                        .iter()
                        .find(|(f, _)| f == parts[1])
                        .unwrap_or_else(|| panic!("OFFSET: no field {} in {}", parts[1], parts[0]));
                    p.ins.push(Ins::PushI(off.1));
                } else if let Some(rest) = n.strip_prefix("@objsize:") {
                    let (_, tsz, sid) = structs
                        .get(rest)
                        .unwrap_or_else(|| panic!("OBJ: unknown struct {}", rest));
                    // size in low 32 bits, struct id above (op_obj splits them)
                    p.ins.push(Ins::PushI(tsz | (sid << 32)));
                } else if let Some(rest) = n.strip_prefix("@cast:") {
                    let (_, _, sid) = structs
                        .get(rest)
                        .unwrap_or_else(|| panic!("CAST: unknown struct {}", rest));
                    p.ins.push(Ins::PushI(1000 + sid));
                } else if let Some(body) = macros.get(&n).cloned() {
                    expand_depth += 1;
                    if expand_depth > 1024 {
                        panic!("macro expansion too deep (recursive macro?)");
                    }
                    for t2 in body.into_iter().rev() {
                        q.push_front(t2);
                    }
                } else {
                    panic!("unknown identifier {} (not a macro)", n);
                }
            }
        }
    }
    let _ = imports;
    p
}

// rewrite label definitions and references inside a task body to be task-local
fn prefix_task_labels(body: Vec<Tok>, prefix: &str) -> Vec<Tok> {
    body.into_iter()
        .map(|t| match t {
            Tok::LabelDef(n) => Tok::LabelDef(format!("{}{}", prefix, n)),
            Tok::Jump(op, l) => Tok::Jump(op, format!("{}{}", prefix, l)),
            Tok::MacroDef(n, mbody) => Tok::MacroDef(n, prefix_task_labels(mbody, prefix)),
            other => other,
        })
        .collect()
}

// Merge per-TU Parsed units into one image; tus[0] is the main TU (pc 0).
// Labels are mangled "<mod>:<name>"; jump/call operands defined in the same TU
// are rewritten to the mangled name, everything else stays a global (PUB) name.
// Variables are always file-local: "<mod>__<name>". IMPORT/EXTERN/USE dedupe.
fn merge_tus(tus: Vec<Parsed>, mods: Vec<String>) -> Parsed {
    let mut m = Parsed {
        ins: Vec::new(),
        labels: HashMap::new(),
        imports: Vec::new(),
        exports: Vec::new(),
        externs: Vec::new(),
        strings: Vec::new(),
        vars: Vec::new(),
        uses: Vec::new(),
        modname: None,
        pubs: Vec::new(),
        methods: Vec::new(),
    };
    for (tu, defmod) in tus.into_iter().zip(mods) {
        let modname = tu.modname.clone().unwrap_or(defmod);
        let off = m.ins.len();
        let str_base = m.strings.len();
        let mut imp_map: Vec<usize> = Vec::new();
        for im in &tu.imports {
            let ix = match m.imports.iter().position(|x| x.name == im.name) {
                Some(i) => i,
                None => {
                    m.imports.push(im.clone());
                    m.imports.len() - 1
                }
            };
            imp_map.push(ix);
        }
        let local = |l: &String| -> String {
            if tu.labels.contains_key(l) {
                format!("{}:{}", modname, l)
            } else {
                l.clone()
            }
        };
        for ins in &tu.ins {
            m.ins.push(match ins {
                Ins::PushI(v) => Ins::PushI(*v),
                Ins::PushF(v) => Ins::PushF(*v),
                Ins::PushS(i) => Ins::PushS(i + str_base),
                Ins::PushAddr(l) => Ins::PushAddr(local(l)),
                Ins::Simple(h) => Ins::Simple(h),
                Ins::Jmp(l) => Ins::Jmp(local(l)),
                Ins::Jz(l) => Ins::Jz(local(l)),
                Ins::Je(l) => Ins::Je(local(l)),
                Ins::For => Ins::For,
                Ins::Call(l) => Ins::Call(local(l)),
                Ins::CallExt(ii) => Ins::CallExt(imp_map[*ii]),
                Ins::Ret => Ins::Ret,
                Ins::SetV(v) => Ins::SetV(format!("{}__{}", modname, v)),
                Ins::GetV(v) => Ins::GetV(format!("{}__{}", modname, v)),
                Ins::Extern(n) => Ins::Extern(n.clone()),
                Ins::Send => Ins::Send,
                Ins::Weave(metas) => Ins::Weave(
                    metas
                        .iter()
                        .map(|t| WeaveMeta {
                            name: format!("{}__{}", modname, t.name),
                            pc: local(&t.pc),
                            inputs: t.inputs.clone(),
                        })
                        .collect(),
                ),
                Ins::Sys(a) => Ins::Sys(*a),
            });
        }
        for (name, idx) in &tu.labels {
            m.labels.insert(format!("{}:{}", modname, name), idx + off);
        }
        // a TU's top-level flow must not fall through into the next TU
        m.ins.push(Ins::Ret);
        for name in &tu.pubs {
            if m.labels.contains_key(name) {
                panic!("duplicate PUB label {}", name);
            }
            m.labels.insert(name.clone(), tu.labels[name] + off);
            m.pubs.push(name.clone());
        }
        for (tk, mh, l) in &tu.methods {
            m.methods.push((*tk, *mh, local(l)));
        }
        m.strings.extend(tu.strings.iter().cloned());
        for e in &tu.externs {
            if !m.externs.contains(e) {
                m.externs.push(e.clone());
            }
        }
        for (c, l) in &tu.exports {
            m.exports.push((c.clone(), local(l)));
        }
        m.vars.extend(tu.vars.iter().map(|v| format!("{}__{}", modname, v)));
        for u in &tu.uses {
            if !m.uses.contains(u) {
                m.uses.push(u.clone());
            }
        }
    }
    m
}

fn simple_ins(name: &'static str) -> Ins {
    // helper name in the C prelude
    let helper: &'static str = match name {
        "DUP" => "op_dup",
        "OVR" => "op_ovr",
        "DRP" => "op_drp",
        "SWP" => "op_swp",
        "PICK" => "op_pick",
        "ADD" => "op_add",
        "SUB" => "op_sub",
        "MUL" => "op_mul",
        "AND" => "op_and",
        "SHR" => "op_shr",
        "INC" => "op_inc",
        "DEC" => "op_dec",
        "OBJ" => "op_obj",
        "GET" => "op_get",
        "SET" => "op_set",
        "ARR" => "op_arr",
        "IDX" => "op_idx",
        "SETI" => "op_seti",
        "CLONE" => "op_clone",
        "CAST" => "op_cast",
        "TENSOR" => "op_tensor",
        "VEC" => "op_nop",
        "PIN" => "op_nop",
        "UNPIN" => "op_nop",
        "CAT" => "op_cat",
        "FMT" => "op_fmt",
        "BUF" => "op_buf",
        "BUFPTR" => "op_nop",
        "BUFCOPY" => "op_bufcopy",
        "LOADX" => "op_loadx",
        "STOREX" => "op_storex",
        "SIZEOF" => "op_sizeof",
        "MALLOC" => "op_malloc",
        "FREE" => "op_free",
        "GC" => "op_nop",
        "PRINT" => "op_print",
        "SCAN" => "op_scan",
        "DICT" => "op_dict",
        "DGET" => "op_dget",
        "DPUT" => "op_dput",
        "DDEL" => "op_ddel",
        "DCOUNT" => "op_dcount",
        "DKEYS" => "op_dkeys",
        "LIST" => "op_list",
        "APPEND" => "op_append",
        "POP" => "op_lpop",
        "CHAN" => "op_chan",
        "ENQ" => "op_enq",
        "DEQ" => "op_deq",
        "CLOSE" => "op_close",
        "ATOM" => "op_atom",
        "AGET" => "op_aget",
        "ASET" => "op_aset",
        "AADD" => "op_aadd",
        "CAS" => "op_cas",
        "TYPEOF" => "op_typeof",
        "LEN" => "op_len",
        "FIELDS" => "op_fields",
        "SH" => "op_sh",
        "SHX" => "op_shx",
        "SHL" => "op_shl",
        "SHP" => "op_shp",
        "EXEC" => "op_exec",
        "RX" => "op_rx",
        "RXSUB" => "op_rxsub",
        "RXSPLIT" => "op_rxsplit",
        "GLOB" => "op_glob",
        "SPLIT" => "op_split",
        "JOIN" => "op_join",
        "SLICE" => "op_slice",
        "FIND" => "op_find",
        "REPL" => "op_repl",
        "TRIM" => "op_trim",
        "UP" => "op_up",
        "DOWN" => "op_down",
        "STARTS" => "op_starts",
        "ENDS" => "op_ends",
        other => panic!("no helper for {}", other),
    };
    Ins::Simple(helper)
}

// ---------------- C prelude ----------------
const PRELUDE: &str = r#"
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <ctype.h>
#include <unistd.h>
#include <pthread.h>
#include <stdatomic.h>
#include <sched.h>
#include <sys/syscall.h>
#ifdef _WIN32
#include <process.h>
#else
#include <sys/wait.h>
#include <fnmatch.h>
#endif

enum { T_INT=0, T_FLOAT=1, T_PTR=2 };
/* Slim 16-byte cell: pointer payloads live in i (cast at use sites), float
   payloads are the double bit pattern in i. uf_f()/uf_fromf() convert.
   (This keeps a whole cell in two GP registers and lets cc -O2 scalarize
   fused basic blocks.) */
typedef struct { int tag; int64_t i; } Cell;
/* handle tags (TYPEOF results): 0..4 are the scalar type ids, handles start at 5 */
enum { HT_ARR=5, HT_TENSOR=6, HT_DYN=7, HT_MAP=8, HT_RING=9, HT_ATOM=10, HT_STR=11, HT_BUF=12, HT_OBJ=13 };
typedef struct { uint64_t tag; uint64_t len; uint64_t esz; char data[]; } Hdr;
/* container structs; only the {tag,len,esz} prefix is shared with Hdr, so any
   generic access to elements must go through uf_data() */
typedef struct { uint64_t tag; uint64_t len; uint64_t esz; uint64_t cap; Cell data[]; } Dyn;
typedef struct { uint64_t tag; uint64_t len; uint64_t cap; Cell* keys; Cell* vals; unsigned char* st; } Map;
typedef struct { uint64_t tag; uint64_t len; uint64_t cap; Cell* buf; uint64_t head; uint64_t tail; pthread_mutex_t mu; pthread_cond_t notfull; pthread_cond_t notempty; int closed; } Ring;
typedef struct { uint64_t tag; uint64_t len; _Atomic int64_t v; } Atom;
static char* uf_data(Hdr*a){ return a->tag==HT_DYN ? (char*)((Dyn*)a)->data : (char*)a->data; }

/* per-task execution context: each weave task runs with its own stacks */
typedef struct { Cell* ds; long sp; long dcap; const void** cs; long csp; long ccap; } Ctx;
static void die(const char*m){ fprintf(stderr,"uflux: %s\n",m); exit(1); }
static Ctx* ctx_new(long dcap,long ccap){ Ctx*c=(Ctx*)malloc(sizeof(Ctx)); c->ds=(Cell*)malloc(dcap*sizeof(Cell)); c->cs=(const void**)malloc(ccap*sizeof(void*)); c->sp=0; c->csp=0; c->dcap=dcap; c->ccap=ccap; return c; }
static Cell main_ds[1<<20]; static const void* main_cs[1<<16];
static Ctx main_cx_store = { main_ds, 0, 1<<20, main_cs, 0, 1<<16 };
static Ctx* main_cx = &main_cx_store;
int64_t uf_argc=0; void* uf_argv=0; /* program args, reachable via EXTERN "uf_argc"/"uf_argv" + LOADX */

static void pushc(Ctx*cx,Cell c){ if(cx->sp>=cx->dcap)die("stack overflow"); cx->ds[cx->sp++]=c; }
/* pure Cell constructors/arith: shared by the op helpers and by the fused
   basic-block codegen (which keeps values in C locals instead of the ds) */
static inline Cell uf_mki(int64_t v){ Cell c; c.tag=T_INT; c.i=v; return c; }
static inline Cell uf_mkp(void* v){ Cell c; c.tag=T_PTR; c.i=(int64_t)v; return c; }
static inline double uf_fbits(int64_t i){ union{int64_t i;double f;}u;u.i=i;return u.f; }
static inline int64_t uf_ibits(double f){ union{int64_t i;double f;}u;u.f=f;return u.i; }
/* numeric value of a cell as a double (tag-aware, for mixed int/float arith) */
static inline double uf_f(Cell c){ return c.tag==T_FLOAT?uf_fbits(c.i):(double)c.i; }
static inline Cell uf_mkf(double v){ Cell c; c.tag=T_FLOAT; c.i=uf_ibits(v); return c; }
static inline Cell uf_fromf(double v){ return uf_mkf(v); }
/* truthiness as JZ historically defined it: (int64_t)value == 0 */
static inline int uf_zero(Cell c){ return c.tag==T_FLOAT?(int64_t)uf_fbits(c.i)==0:c.i==0; }
static inline Cell uf_cadd(Cell a,Cell b){ if(a.tag==T_FLOAT||b.tag==T_FLOAT)return uf_fromf(uf_f(a)+uf_f(b)); return uf_mki(a.i+b.i); }
static inline Cell uf_csub(Cell a,Cell b){ if(a.tag==T_FLOAT||b.tag==T_FLOAT)return uf_fromf(uf_f(a)-uf_f(b)); return uf_mki(a.i-b.i); }
static inline Cell uf_cmul(Cell a,Cell b){ if(a.tag==T_FLOAT||b.tag==T_FLOAT)return uf_fromf(uf_f(a)*uf_f(b)); return uf_mki(a.i*b.i); }
static inline Cell uf_cand(Cell a,Cell b){ return uf_mki(a.i&b.i); }
static inline Cell uf_cshr(Cell a){ return uf_mki((int64_t)((uint64_t)a.i>>1)); }
static inline Cell uf_cinc(Cell a){ if(a.tag==T_FLOAT)return uf_fromf(uf_f(a)+1.0); return uf_mki(a.i+1); }
static inline Cell uf_cdec(Cell a){ if(a.tag==T_FLOAT)return uf_fromf(uf_f(a)-1.0); return uf_mki(a.i-1); }
static inline int uf_ceq(Cell a,Cell b){ return (a.tag==T_FLOAT||b.tag==T_FLOAT)?uf_f(a)==uf_f(b):a.i==b.i; }
static inline Cell uf_cidx(Cell h,int64_t ix){ Hdr*a=(Hdr*)h.i; if(ix<0||(uint64_t)ix>=a->len)die("IDX out of bounds"); char*dt=uf_data(a); if(a->esz==8)return uf_mki(((int64_t*)dt)[ix]); if(a->esz==sizeof(Cell))return ((Cell*)dt)[ix]; return uf_mki((int64_t)((uint8_t*)dt)[ix]); }
static inline void uf_cseti(Cell h,int64_t ix,Cell v){ Hdr*a=(Hdr*)h.i; if(ix<0||(uint64_t)ix>=a->len)die("SETI out of bounds"); char*dt=uf_data(a); if(a->esz==8)((int64_t*)dt)[ix]=v.i; else if(a->esz==sizeof(Cell))((Cell*)dt)[ix]=v; else ((uint8_t*)dt)[ix]=(uint8_t)v.i; }
static void pushi(Ctx*cx,int64_t v){ pushc(cx,uf_mki(v)); }
static void pushf(Ctx*cx,double v){ pushc(cx,uf_mkf(v)); }
static void pushp(Ctx*cx,void* v){ pushc(cx,uf_mkp(v)); }
static Cell pop(Ctx*cx){ if(cx->sp<=0) die("stack underflow"); return cx->ds[--cx->sp]; }
static void op_nop(Ctx*cx){ (void)cx; }

static void op_dup(Ctx*cx){ if(cx->sp<1)die("stack underflow"); pushc(cx,cx->ds[cx->sp-1]); }
static void op_ovr(Ctx*cx){ if(cx->sp<2)die("stack underflow"); pushc(cx,cx->ds[cx->sp-2]); }
static void op_drp(Ctx*cx){ (void)pop(cx); }
static void op_swp(Ctx*cx){ Cell t=cx->ds[cx->sp-1]; cx->ds[cx->sp-1]=cx->ds[cx->sp-2]; cx->ds[cx->sp-2]=t; }
static void op_pick(Ctx*cx){ int64_t n=pop(cx).i; if(n<0||n>=cx->sp)die("PICK out of range"); pushc(cx,cx->ds[cx->sp-1-n]); }

static void op_add(Ctx*cx){ Cell b=pop(cx),a=pop(cx); pushc(cx,uf_cadd(a,b)); }
static void op_sub(Ctx*cx){ Cell b=pop(cx),a=pop(cx); pushc(cx,uf_csub(a,b)); }
static void op_mul(Ctx*cx){ Cell b=pop(cx),a=pop(cx); pushc(cx,uf_cmul(a,b)); }
static void op_and(Ctx*cx){ Cell b=pop(cx),a=pop(cx); pushc(cx,uf_cand(a,b)); }
static void op_shr(Ctx*cx){ pushc(cx,uf_cshr(pop(cx))); }
static void op_inc(Ctx*cx){ pushc(cx,uf_cinc(pop(cx))); }
static void op_dec(Ctx*cx){ pushc(cx,uf_cdec(pop(cx))); }

static void* uf_alloc(size_t sz,int align){ void*p=NULL; if(align>0){ if(posix_memalign(&p,(size_t)align,sz?sz:1))die("alloc failed"); } else { p=malloc(sz?sz:1); } if(!p)die("out of memory"); return p; }
static void op_arrn(Ctx*cx,uint64_t tag,int align){ int64_t len=pop(cx).i, ty=pop(cx).i; int64_t esz=(ty==3)?1:8; if(len<0)die("negative length"); Hdr*h=(Hdr*)uf_alloc(sizeof(Hdr)+(size_t)len*(size_t)esz,align); h->tag=tag; h->len=(uint64_t)len; h->esz=(uint64_t)esz; memset(h->data,0,(size_t)len*(size_t)esz); pushp(cx,h); }
static void op_arr(Ctx*cx){ op_arrn(cx,HT_ARR,0); }
static void op_tensor(Ctx*cx){ op_arrn(cx,HT_TENSOR,64); }
static void op_idx(Ctx*cx){ int64_t ix=pop(cx).i; Cell h=pop(cx); pushc(cx,uf_cidx(h,ix)); }
static void op_seti(Ctx*cx){ Cell v=pop(cx); int64_t ix=pop(cx).i; Cell h=pop(cx); uf_cseti(h,ix,v); }
static void op_clone(Ctx*cx){ Cell h=pop(cx); Hdr*a=(Hdr*)((void*)h.i); size_t sz=sizeof(Hdr)+(size_t)a->len*a->esz; void*n=uf_alloc(sz,0); memcpy(n,a,sz); memcpy(uf_data((Hdr*)n),uf_data(a),(size_t)a->len*a->esz); pushp(cx,n); }
static void op_cast(Ctx*cx){ Cell id=pop(cx); Cell h=pop(cx); Hdr*a=(Hdr*)((void*)h.i); int64_t tk=(a->tag==HT_OBJ)?1000+(int64_t)a->len:(int64_t)a->tag; if(tk!=id.i)die("CAST: type mismatch"); pushc(cx,h); }

/* OBJ: size in low 32 bits of the operand, struct id above; the object carries
   a Hdr (tag HT_OBJ, len=struct id) so TYPEOF/FIELDS/SEND/CAST can see it.
   GET/SET offsets are relative to the object data (after the header). */
static void op_obj(Ctx*cx){ int64_t v=pop(cx).i; int64_t sz=v&0xffffffffLL; int64_t sid=v>>32; if(sz<=0)sz=8; Hdr*h=(Hdr*)uf_alloc(sizeof(Hdr)+(size_t)sz,0); h->tag=HT_OBJ; h->len=(uint64_t)sid; h->esz=8; memset(h->data,0,(size_t)sz); pushp(cx,h); }
static void op_get(Ctx*cx){ int64_t o=pop(cx).i; Cell h=pop(cx); Hdr*a=(Hdr*)((void*)h.i); pushi(cx,*(int64_t*)(a->data+o)); }
/* SET: handle offset value ->  (same convention as SETI; GET: handle offset -> v) */
static void op_set(Ctx*cx){ Cell v=pop(cx); int64_t o=pop(cx).i; Cell h=pop(cx); Hdr*a=(Hdr*)((void*)h.i); *(int64_t*)(a->data+o)=v.i; }

static void op_buf(Ctx*cx){ int64_t sz=pop(cx).i; if(sz<0)die("negative BUF size"); void*p=uf_alloc((size_t)sz,0); memset(p,0,(size_t)sz); pushp(cx,p); }
static void op_bufcopy(Ctx*cx){ int64_t n=pop(cx).i; Cell s=pop(cx),d=pop(cx); if(n>0)memmove(((void*)d.i),((void*)s.i),(size_t)n); }
static void op_loadx(Ctx*cx){ Cell a=pop(cx); pushi(cx,*(int64_t*)((void*)a.i)); }
static void op_storex(Ctx*cx){ Cell a=pop(cx); Cell v=pop(cx); *(int64_t*)((void*)a.i)=v.i; }
static void op_malloc(Ctx*cx){ int64_t sz=pop(cx).i; if(sz<0)die("negative MALLOC size"); void*p=malloc((size_t)sz?sz:1); if(!p)die("out of memory"); pushp(cx,p); }
static void op_free(Ctx*cx){ Cell p=pop(cx); free(((void*)p.i)); }
static void op_sizeof(Ctx*cx){ int64_t ty=pop(cx).i; pushi(cx,ty==3?1:8); }

static void op_cat(Ctx*cx){ Cell b=pop(cx),a=pop(cx); size_t la=strlen((char*)((void*)a.i)),lb=strlen((char*)((void*)b.i)); char*r=(char*)uf_alloc(la+lb+1,0); memcpy(r,((void*)a.i),la); memcpy(r+la,((void*)b.i),lb+1); pushp(cx,r); }
static int uf_count(const char*f){ int c=0; for(;f&&*f;f++){ if(*f=='%'){ if(f[1]=='%'){ f++; } else { c++; if(f[1]=='*')c++; } } } return c; }
static char* uf_fmt(const char*f,Cell*a,int n){
  size_t cap=256,bi=0; char*buf=(char*)uf_alloc(cap,0); int ai=0;
  for(const char*p=f;*p;){
    if(*p!='%'){ if(bi+2>cap){cap*=2;buf=(char*)realloc(buf,cap);} buf[bi++]=*p++; continue; }
    if(p[1]=='%'){ if(bi+2>cap){cap*=2;buf=(char*)realloc(buf,cap);} buf[bi++]='%'; p+=2; continue; }
    char d[32]; int di=0; d[di++]='%'; p++;
    while(*p&&strchr("-+ #0",*p)) d[di++]=*p++;
    while(*p&&(isdigit((unsigned char)*p)||*p=='.')) d[di++]=*p++;
    while(*p&&strchr("hlLjzt",*p)) p++;
    char conv=*p?*p++:'d';
    if(ai>=n) die("FMT: not enough args");
    Cell ar=a[ai++];
    char tmp[128]; int tl=0;
    d[di]=0;
    switch(conv){
      case 'd': case 'i': case 'u': case 'x': case 'X': case 'o': {
        { size_t l=strlen(d); d[l]='l'; d[l+1]='l'; d[l+2]=conv; d[l+3]=0; }
        tl=snprintf(tmp,sizeof(tmp),d,(unsigned long long)ar.i); break; }
      case 'c': tl=snprintf(tmp,sizeof(tmp),d,(int)ar.i); break;
      case 'f': case 'F': case 'e': case 'E': case 'g': case 'G': {
        size_t l=strlen(d); d[l]=conv; d[l+1]=0;
        tl=snprintf(tmp,sizeof(tmp),d,uf_f(ar)); break; }
      case 's': { size_t l=strlen(d); d[l]=conv; d[l+1]=0; tl=snprintf(tmp,sizeof(tmp),d,(char*)((void*)ar.i)); break; }
      case 'p': { size_t l=strlen(d); d[l]=conv; d[l+1]=0; tl=snprintf(tmp,sizeof(tmp),d,((void*)ar.i)); break; }
      default: die("FMT: unsupported directive");
    }
    if(tl<0) die("FMT failed");
    while(bi+(size_t)tl+1>cap){ cap*=2; buf=(char*)realloc(buf,cap); }
    memcpy(buf+bi,tmp,(size_t)tl); bi+=(size_t)tl;
  }
  buf[bi]=0; return buf;
}
static void op_fmt(Ctx*cx){ Cell f=pop(cx); int n=uf_count((char*)((void*)f.i)); Cell args[16]; if(n>16)die("FMT: too many args"); for(int k=n-1;k>=0;k--) args[k]=pop(cx); pushp(cx,uf_fmt((char*)((void*)f.i),args,n)); }
/* PRINT: fmt args.. -> n ; fmt is ON TOP with args below it (deepest first) */
static void op_print(Ctx*cx){ Cell f=pop(cx); int n=uf_count((char*)((void*)f.i)); Cell args[16]; if(n>16)die("PRINT: too many args"); for(int k=n-1;k>=0;k--) args[k]=pop(cx); char*s=uf_fmt((char*)((void*)f.i),args,n); int r=printf("%s",s); pushi(cx,(int64_t)r); }
/* SCAN: fmt -> values.. count ; per conversion fscanf(stdin,..):
   %d/%i -> i64 (via "%lld"), %f/%e/%g -> f64 (via "%lf" into double),
   %s -> freshly allocated string handle. Literal text in fmt unsupported. */
static void op_scan(Ctx*cx){
  Cell f=pop(cx); const char*p=(const char*)((void*)f.i); int n=0;
  for(;*p;p++){
    if(*p=='%'){
      if(p[1]=='%'){ p++; continue; }
      p++;
      while(*p&&strchr("-+ #0",*p)) p++;
      while(*p&&(isdigit((unsigned char)*p)||*p=='.')) p++;
      while(*p&&strchr("hlLjzt",*p)) p++;
      char conv=*p?*p:'\0';
      switch(conv){
        case 'd': case 'i': case 'u': case 'x': case 'X': case 'o': {
          long long v; char d[8]; d[0]='%'; d[1]='l'; d[2]='l'; d[3]=conv; d[4]=0;
          if(fscanf(stdin,d,&v)!=1) die("SCAN: input error"); pushi(cx,(int64_t)v); n++; break; }
        case 'f': case 'F': case 'e': case 'E': case 'g': case 'G': {
          double v; char d[8]; d[0]='%'; d[1]='l'; d[2]='f'; d[3]=0;
          if(fscanf(stdin,d,&v)!=1) die("SCAN: input error"); pushf(cx,v); n++; break; }
        case 's': {
          char*b=(char*)uf_alloc(1<<16,0);
          if(fscanf(stdin,"%65535s",b)!=1) die("SCAN: input error"); pushp(cx,b); n++; break; }
        default: die("SCAN: unsupported directive");
      }
    } else if(isspace((unsigned char)*p)) {
      continue; /* fmt whitespace matches optional input whitespace implicitly */
    } else {
      die("SCAN: literal text in format unsupported");
    }
  }
  pushi(cx,(int64_t)n);
}
static int uf_vargc(Ctx*cx){ for(int t=0;t<cx->sp;t++){ Cell fc=cx->ds[cx->sp-1-t]; if(fc.tag==T_PTR&&((void*)fc.i)&&uf_count((char*)((void*)fc.i))==t) return t; } die("vararg call: format string not found"); return 0; }

/* ---- v9: op-native datastructures ---- */
/* DYN: growable Cell vector; IDX/SETI/LEN work via the shared header prefix */
static void op_list(Ctx*cx){ Dyn*d=(Dyn*)uf_alloc(sizeof(Dyn)+8*sizeof(Cell),0); d->tag=HT_DYN; d->len=0; d->esz=sizeof(Cell); d->cap=8; pushp(cx,d); }
static void op_append(Ctx*cx){ Cell v=pop(cx),h=pop(cx); Dyn*d=(Dyn*)((void*)h.i); if(d->tag!=HT_DYN)die("APPEND: not a list"); if(d->len>=d->cap){ d->cap*=2; d=(Dyn*)realloc(d,sizeof(Dyn)+d->cap*sizeof(Cell)); if(!d)die("out of memory"); } d->data[d->len++]=v; pushp(cx,d); }
static void op_lpop(Ctx*cx){ Cell h=pop(cx); Dyn*d=(Dyn*)((void*)h.i); if(d->tag!=HT_DYN)die("POP: not a list"); if(d->len==0)die("POP: empty list"); pushc(cx,d->data[--d->len]); }

/* MAP: open addressing, FNV-1a, tombstones, grow at 70% load.
   Keys are cells: ints compare by value, pointers compare as C strings. */
static uint64_t uf_fnv(const void*p,size_t n){ const unsigned char*s=(const unsigned char*)p; uint64_t h=1469598103934665603ULL; for(size_t i=0;i<n;i++){ h^=s[i]; h*=1099511628211ULL; } return h; }
static uint64_t map_hash(Cell k){ if(k.tag==T_PTR&&((void*)k.i)) return uf_fnv(((void*)k.i),strlen((char*)((void*)k.i))); return uf_fnv(&k.i,8); }
static int map_keyeq(Cell a,Cell b){ if(a.tag==T_PTR&&b.tag==T_PTR&&((void*)a.i)&&((void*)b.i)) return strcmp((char*)((void*)a.i),(char*)((void*)b.i))==0; return a.i==b.i; }
static void map_put_raw(Map*m,Cell k,Cell v){
  uint64_t i=map_hash(k)%m->cap;
  for(;;){ if(m->st[i]!=1){ m->st[i]=1; m->keys[i]=k; m->vals[i]=v; m->len++; return; } if(map_keyeq(m->keys[i],k)){ m->vals[i]=v; return; } i=(i+1)%m->cap; }
}
static void map_grow(Map*m){
  uint64_t ncap=m->cap*2; Cell*ok=m->keys,*ov=m->vals; unsigned char*os=m->st; uint64_t ocap=m->cap;
  m->cap=ncap; m->keys=(Cell*)uf_alloc(ncap*sizeof(Cell),0); m->vals=(Cell*)uf_alloc(ncap*sizeof(Cell),0); m->st=(unsigned char*)calloc(ncap,1); m->len=0;
  for(uint64_t i=0;i<ocap;i++) if(os[i]==1) map_put_raw(m,ok[i],ov[i]);
  free(ok); free(ov); free(os);
}
static void op_dict(Ctx*cx){ Map*m=(Map*)uf_alloc(sizeof(Map),0); m->tag=HT_MAP; m->len=0; m->cap=16; m->keys=(Cell*)uf_alloc(16*sizeof(Cell),0); m->vals=(Cell*)uf_alloc(16*sizeof(Cell),0); m->st=(unsigned char*)calloc(16,1); pushp(cx,m); }
/* DPUT: h k v ->  (v on top) */
static void op_dput(Ctx*cx){ Cell v=pop(cx),k=pop(cx),h=pop(cx); Map*m=(Map*)((void*)h.i); if(m->tag!=HT_MAP)die("DPUT: not a dict"); if((m->len+1)*10>=m->cap*7) map_grow(m); map_put_raw(m,k,v); }
/* DGET: h k -> v found (two cells; found flag on top) */
static void op_dget(Ctx*cx){ Cell k=pop(cx),h=pop(cx); Map*m=(Map*)((void*)h.i); if(m->tag!=HT_MAP)die("DGET: not a dict"); uint64_t i=map_hash(k)%m->cap; for(;;){ if(m->st[i]==0){ pushi(cx,0); pushi(cx,0); return; } if(m->st[i]==1&&map_keyeq(m->keys[i],k)){ pushc(cx,m->vals[i]); pushi(cx,1); return; } i=(i+1)%m->cap; } }
static void op_ddel(Ctx*cx){ Cell k=pop(cx),h=pop(cx); Map*m=(Map*)((void*)h.i); if(m->tag!=HT_MAP)die("DDEL: not a dict"); uint64_t i=map_hash(k)%m->cap; for(;;){ if(m->st[i]==0) return; if(m->st[i]==1&&map_keyeq(m->keys[i],k)){ m->st[i]=2; m->len--; return; } i=(i+1)%m->cap; } }
static void op_dcount(Ctx*cx){ Cell h=pop(cx); Map*m=(Map*)((void*)h.i); if(m->tag!=HT_MAP)die("DCOUNT: not a dict"); pushi(cx,(int64_t)m->len); }
static void op_dkeys(Ctx*cx){ Cell h=pop(cx); Map*m=(Map*)((void*)h.i); if(m->tag!=HT_MAP)die("DKEYS: not a dict"); Dyn*d=(Dyn*)uf_alloc(sizeof(Dyn)+(m->len?m->len:1)*sizeof(Cell),0); d->tag=HT_DYN; d->len=0; d->esz=sizeof(Cell); d->cap=m->len?m->len:1; for(uint64_t i=0;i<m->cap;i++) if(m->st[i]==1) d->data[d->len++]=m->keys[i]; pushp(cx,d); }

/* RING/CHAN: bounded MPSC ring buffer with blocking ENQ/DEQ */
static void ring_enq(Ring*r,Cell v){ pthread_mutex_lock(&r->mu); while(r->len>=r->cap&&!r->closed) pthread_cond_wait(&r->notfull,&r->mu); if(r->closed){ pthread_mutex_unlock(&r->mu); die("ENQ: chan closed"); } r->buf[r->tail]=v; r->tail=(r->tail+1)%r->cap; r->len++; pthread_cond_signal(&r->notempty); pthread_mutex_unlock(&r->mu); }
static void ring_close(Ring*r){ pthread_mutex_lock(&r->mu); r->closed=1; pthread_cond_broadcast(&r->notempty); pthread_cond_broadcast(&r->notfull); pthread_mutex_unlock(&r->mu); }
/* CHAN: cap -> h */
static void op_chan(Ctx*cx){ int64_t cap=pop(cx).i; if(cap<=0)cap=16; Ring*r=(Ring*)uf_alloc(sizeof(Ring),0); r->tag=HT_RING; r->len=0; r->cap=(uint64_t)cap; r->buf=(Cell*)uf_alloc((size_t)cap*sizeof(Cell),0); r->head=0; r->tail=0; r->closed=0; pthread_mutex_init(&r->mu,0); pthread_cond_init(&r->notfull,0); pthread_cond_init(&r->notempty,0); pushp(cx,r); }
/* ENQ: h v ->  (blocks while full) */
static void op_enq(Ctx*cx){ Cell v=pop(cx),h=pop(cx); Ring*r=(Ring*)((void*)h.i); if(r->tag!=HT_RING)die("ENQ: not a chan"); ring_enq(r,v); }
/* DEQ: h -> v  (blocks while empty; closed+empty yields sentinel 0) */
static void op_deq(Ctx*cx){ Cell h=pop(cx); Ring*r=(Ring*)((void*)h.i); if(r->tag!=HT_RING)die("DEQ: not a chan"); pthread_mutex_lock(&r->mu); while(r->len==0&&!r->closed) pthread_cond_wait(&r->notempty,&r->mu); Cell v; if(r->len==0){ v=uf_mki(0); } else { v=r->buf[r->head]; r->head=(r->head+1)%r->cap; r->len--; pthread_cond_signal(&r->notfull); } pthread_mutex_unlock(&r->mu); pushc(cx,v); }
static void op_close(Ctx*cx){ Cell h=pop(cx); Ring*r=(Ring*)((void*)h.i); if(r->tag!=HT_RING)die("CLOSE: not a chan"); ring_close(r); }

/* ATOM: atomic i64 cell */
static void op_atom(Ctx*cx){ Cell v=pop(cx); Atom*a=(Atom*)uf_alloc(sizeof(Atom),0); a->tag=HT_ATOM; a->len=1; atomic_store(&a->v,v.i); pushp(cx,a); }
static void op_aget(Ctx*cx){ Cell h=pop(cx); Atom*a=(Atom*)((void*)h.i); if(a->tag!=HT_ATOM)die("AGET: not an atom"); pushi(cx,atomic_load(&a->v)); }
static void op_aset(Ctx*cx){ Cell v=pop(cx),h=pop(cx); Atom*a=(Atom*)((void*)h.i); if(a->tag!=HT_ATOM)die("ASET: not an atom"); atomic_store(&a->v,v.i); }
static void op_aadd(Ctx*cx){ Cell n=pop(cx),h=pop(cx); Atom*a=(Atom*)((void*)h.i); if(a->tag!=HT_ATOM)die("AADD: not an atom"); pushi(cx,atomic_fetch_add(&a->v,n.i)); }
/* CAS: h old new -> 0/1 (new on top) */
static void op_cas(Ctx*cx){ Cell nw=pop(cx),old=pop(cx),h=pop(cx); Atom*a=(Atom*)((void*)h.i); if(a->tag!=HT_ATOM)die("CAS: not an atom"); int64_t e=old.i; pushi(cx,atomic_compare_exchange_strong(&a->v,&e,nw.i)?1:0); }

/* generalized LEN + TYPEOF over tagged handles */
static void op_len(Ctx*cx){ Cell h=pop(cx); Hdr*a=(Hdr*)((void*)h.i); switch(a->tag){ case HT_ARR: case HT_TENSOR: case HT_DYN: case HT_MAP: case HT_RING: pushi(cx,(int64_t)a->len); return; case HT_ATOM: pushi(cx,1); return; default: die("LEN: handle has no length"); } }
static void op_typeof(Ctx*cx){ Cell h=pop(cx); Hdr*a=(Hdr*)((void*)h.i); pushi(cx,(int64_t)a->tag); }

/* reflection tables, populated by generated uf_init_reflection() */
static long uf_st_n=0; static const int64_t* uf_st_sids=0; static const int64_t* uf_st_nf=0; static const char*** uf_st_fields=0;

/* ---- wove-style task DAG scheduler ---- */
typedef void(*UfRun)(Ctx*,long);
typedef struct { long pc; int ninputs; int inputs[8]; Cell result; _Atomic int state; } WeaveTask;
typedef struct { WeaveTask* ts; int n; UfRun run; } WeaveJob;
static void* uf_worker(void*arg){
  WeaveJob*j=(WeaveJob*)arg;
  for(;;){
    int pick=-1;
    for(int i=0;i<j->n;i++){
      if(atomic_load(&j->ts[i].state)!=0) continue;
      int ready=1;
      for(int k=0;k<j->ts[i].ninputs;k++) if(atomic_load(&j->ts[j->ts[i].inputs[k]].state)!=2){ready=0;break;}
      if(!ready) continue;
      int exp=0;
      if(atomic_compare_exchange_strong(&j->ts[i].state,&exp,1)){ pick=i; break; }
    }
    if(pick<0){
      int alldone=1; for(int i=0;i<j->n;i++) if(atomic_load(&j->ts[i].state)!=2){alldone=0;break;}
      if(alldone) return 0;
      sched_yield(); continue;
    }
    WeaveTask*t=&j->ts[pick];
    Ctx*c=ctx_new(1<<16,1<<12);
    for(int k=0;k<t->ninputs;k++) pushc(c,j->ts[t->inputs[k]].result);
    j->run(c,t->pc);
    t->result = c->sp>0 ? c->ds[c->sp-1] : (Cell){T_INT,0,0,0};
    atomic_store(&t->state,2);
    free(c->ds); free((void*)c->cs); free(c);
  }
}
static void uf_weave(Ctx*cx,WeaveTask*ts,int n,UfRun run){
  (void)cx;
  long ncpu=sysconf(_SC_NPROCESSORS_ONLN);
  int nw=n; if(ncpu>0&&(long)nw>ncpu)nw=(int)ncpu; if(nw<1)nw=1; if(nw>64)nw=64;
  WeaveJob j={ts,n,run};
  if(nw<=1){ uf_worker(&j); return; }
  pthread_t th[64];
  for(int i=0;i<nw-1;i++) pthread_create(&th[i],0,uf_worker,&j);
  uf_worker(&j);
  for(int i=0;i<nw-1;i++) pthread_join(th[i],0);
}
/* FIELDS: obj -> dyn of interned field-name strings */
static void op_fields(Ctx*cx){
  Cell h=pop(cx); Hdr*a=(Hdr*)((void*)h.i); if(a->tag!=HT_OBJ)die("FIELDS: not an object");
  int64_t sid=(int64_t)a->len; const char**fs=0; int64_t nf=0;
  for(long q=0;q<uf_st_n;q++) if(uf_st_sids[q]==sid){ fs=uf_st_fields[q]; nf=uf_st_nf[q]; break; }
  if(!fs&&nf==0) die("FIELDS: unknown struct id");
  Dyn*d=(Dyn*)uf_alloc(sizeof(Dyn)+(nf?nf:1)*sizeof(Cell),0); d->tag=HT_DYN; d->len=0; d->esz=sizeof(Cell); d->cap=nf?nf:1;
  for(int64_t q=0;q<nf;q++){ Cell c=uf_mkp((void*)fs[q]); d->data[d->len++]=c; }
  pushp(cx,d);
}

/* ================= shared string/list helpers ================= */
static char* uf_str_dup_n(const char*s,size_t n){ char*r=(char*)uf_alloc(n+1,0); memcpy(r,s,n); r[n]=0; return r; }
static Dyn* uf_dyn_new(uint64_t cap){ if(!cap)cap=1; Dyn*d=(Dyn*)uf_alloc(sizeof(Dyn)+cap*sizeof(Cell),0); d->tag=HT_DYN; d->len=0; d->esz=sizeof(Cell); d->cap=cap; return d; }
static void uf_dyn_push(Dyn**pd,Cell c){ Dyn*d=*pd; if(d->len>=d->cap){ d->cap*=2; d=(Dyn*)realloc(d,sizeof(Dyn)+d->cap*sizeof(Cell)); if(!d)die("out of memory"); *pd=d; } d->data[d->len++]=c; }
static void uf_dyn_push_str(Dyn**pd,const char*s,size_t n){ char*p=uf_str_dup_n(s,n); Cell c=uf_mkp((void*)p); uf_dyn_push(pd,c); }
static char* uf_read_all(FILE*f){
  size_t cap=4096,n=0; char*b=(char*)uf_alloc(cap,0); size_t m;
  while((m=fread(b+n,1,cap-1-n,f))>0){ n+=m; if(cap-1-n==0){ cap*=2; b=(char*)realloc(b,cap); if(!b)die("out of memory"); } }
  b[n]=0; return b;
}
static int uf_wait_status(int r){
#ifdef _WIN32
  return r;
#else
  if(r==-1)return -1;
  if(WIFEXITED(r))return WEXITSTATUS(r);
  if(WIFSIGNALED(r))return 128+WTERMSIG(r);
  return r;
#endif
}

/* ================= shell ops ================= */
/* SH: cmd -> status (stdio inherited, platform shell) */
static void op_sh(Ctx*cx){ Cell c=pop(cx); pushi(cx,uf_wait_status(system((char*)((void*)c.i)))); }
/* SHX: cmd -> out err status (capture stdout+stderr; status on top) */
static void op_shx(Ctx*cx){
  Cell c=pop(cx); char*cmd=(char*)((void*)c.i);
#ifdef _WIN32
  char tmp[256]; tmpnam(tmp);
  char*full=(char*)uf_alloc(strlen(cmd)+strlen(tmp)+8,0);
  sprintf(full,"%s 2>%s",cmd,tmp);
  FILE* f=_popen(full,"r"); if(!f)die("SHX: spawn failed");
  char*out=uf_read_all(f); int st=uf_wait_status(_pclose(f));
  FILE* ef=fopen(tmp,"r"); char*err;
  if(ef){ err=uf_read_all(ef); fclose(ef); remove(tmp); } else err=uf_str_dup_n("",0);
  pushp(cx,out); pushp(cx,err); pushi(cx,st);
#else
  int pfd[2]; if(pipe(pfd))die("SHX: pipe");
  FILE* ef=tmpfile(); if(!ef)die("SHX: tmpfile");
  pid_t pid=fork();
  if(pid<0)die("SHX: fork");
  if(pid==0){
    close(pfd[0]);
    if(dup2(pfd[1],1)<0)_exit(127);
    if(dup2(fileno(ef),2)<0)_exit(127);
    execl("/bin/sh","sh","-c",cmd,(char*)0);
    _exit(127);
  }
  close(pfd[1]);
  FILE* f=fdopen(pfd[0],"r"); if(!f)die("SHX: fdopen");
  char*out=uf_read_all(f); fclose(f);
  int rs=0; waitpid(pid,&rs,0);
  int st=uf_wait_status(rs);
  rewind(ef); char*err=uf_read_all(ef); fclose(ef);
  pushp(cx,out); pushp(cx,err); pushi(cx,st);
#endif
}
/* SHL: cmd -> list (stdout split into lines; dies on spawn failure only) */
static void op_shl(Ctx*cx){
  Cell c=pop(cx);
#ifdef _WIN32
  FILE* f=_popen((char*)((void*)c.i),"r");
  if(!f)die("SHL: spawn failed");
  Dyn*d=uf_dyn_new(8); char line[16384];
  while(fgets(line,sizeof(line),f)){ size_t m=strlen(line); while(m>0&&(line[m-1]=='\n'||line[m-1]=='\r'))line[--m]=0; uf_dyn_push_str(&d,line,m); }
  _pclose(f);
#else
  FILE* f=popen((char*)((void*)c.i),"r");
  if(!f)die("SHL: spawn failed");
  Dyn*d=uf_dyn_new(8);
  char*line=0; size_t ncap=0; ssize_t m;
  while((m=getline(&line,&ncap,f))>=0){ while(m>0&&(line[m-1]=='\n'||line[m-1]=='\r'))line[--m]=0; uf_dyn_push_str(&d,line,(size_t)m); }
  free(line); pclose(f);
#endif
  pushp(cx,d);
}
/* SHP: cmd -> chan (worker thread streams stdout lines, closes chan at exit) */
typedef struct { Ring* r; char* cmd; } UfShp;
static void* uf_shp_worker(void*arg){
  UfShp* g=(UfShp*)arg;
#ifdef _WIN32
  FILE* f=_popen(g->cmd,"r");
#else
  FILE* f=popen(g->cmd,"r");
#endif
  if(f){
#ifdef _WIN32
    char line[16384];
    while(fgets(line,sizeof(line),f)){ size_t m=strlen(line); while(m>0&&(line[m-1]=='\n'||line[m-1]=='\r'))line[--m]=0; char*p=uf_str_dup_n(line,m); Cell v=uf_mkp((void*)p); ring_enq(g->r,v); }
    _pclose(f);
#else
    char*line=0; size_t ncap=0; ssize_t m;
    while((m=getline(&line,&ncap,f))>=0){ while(m>0&&(line[m-1]=='\n'||line[m-1]=='\r'))line[--m]=0; char*p=uf_str_dup_n(line,(size_t)m); Cell v=uf_mkp((void*)p); ring_enq(g->r,v); }
    free(line); pclose(f);
#endif
  }
  ring_close(g->r);
  free(g);
  return 0;
}
static void op_shp(Ctx*cx){
  Cell c=pop(cx);
  Ring*r=(Ring*)uf_alloc(sizeof(Ring),0); r->tag=HT_RING; r->len=0; r->cap=64; r->buf=(Cell*)uf_alloc(64*sizeof(Cell),0); r->head=0; r->tail=0; r->closed=0; pthread_mutex_init(&r->mu,0); pthread_cond_init(&r->notfull,0); pthread_cond_init(&r->notempty,0);
  UfShp* g=(UfShp*)malloc(sizeof(UfShp)); if(!g)die("out of memory"); g->r=r; g->cmd=(char*)((void*)c.i);
  pthread_t th; if(pthread_create(&th,0,uf_shp_worker,g)){ ring_close(r); die("SHP: thread"); }
  pthread_detach(th);
  pushp(cx,r);
}
/* EXEC: list -> status (argv list, no shell; fork+execvp/waitpid or _spawnvp) */
static void op_exec(Ctx*cx){
  Cell h=pop(cx); Dyn*d=(Dyn*)((void*)h.i); if(d->tag!=HT_DYN)die("EXEC: not a list");
  if(d->len==0)die("EXEC: empty argv");
  char**argv=(char**)malloc((d->len+1)*sizeof(char*)); if(!argv)die("out of memory");
  for(uint64_t i=0;i<d->len;i++)argv[i]=(char*)((void*)d->data[i].i);
  argv[d->len]=0;
#ifdef _WIN32
  intptr_t r=_spawnvp(_P_WAIT,argv[0],(const char* const*)argv);
  int st=(r==-1)?-1:(int)r;
#else
  pid_t pid=fork();
  if(pid<0)die("EXEC: fork");
  if(pid==0){ execvp(argv[0],argv); _exit(127); }
  int rs=0; waitpid(pid,&rs,0); int st=uf_wait_status(rs);
#endif
  free(argv); pushi(cx,st);
}

/* ================= embedded regex =================
   Small backtracking engine (no deps). Syntax: literals, '.', '*', '+', '?',
   '[...]' (ranges, '^' negation, escape via '\'), '^' at alternative start,
   '$' at alternative end, '|' alternation, '(' ... ')' capture groups (<=9).
   Greedy matching with backtracking; a quantified atom never loops on an
   empty match. */
typedef struct { const char* s; const char* e; } RxCap;
enum { RXA_LIT=0, RXA_DOT=1, RXA_CLS=2, RXA_GRP=3 };
typedef struct { int type; char ch; const char* cs; const char* ce; const char* gs; const char* ge; int cap; } RxAtom;

/* p just after '[': find the closing ']' (']' first is literal, '\' escapes) */
static int rx_cls_find(const char* p, const char** close){
  if(*p=='^')p++;
  if(*p==']')p++;
  while(*p){
    if(*p=='\\'&&p[1]){ p+=2; continue; }
    if(*p==']'){ *close=p; return 1; }
    p++;
  }
  return 0;
}
/* class body [cs,ce): does it contain c? */
static int rx_cls_in(const char* cs, const char* ce, char c){
  int neg=0; const char* p=cs;
  if(p<ce&&*p=='^'){ neg=1; p++; }
  int ok=0; int first=1;
  while(p<ce){
    char lo;
    if(*p=='\\'&&p+1<ce){ lo=p[1]; p+=2; } else lo=*p++;
    if(first&&lo==']'){ if(c==']')ok=1; first=0; continue; }
    first=0;
    if(p<ce&&*p=='-'&&p+1<ce){
      p++; char hi;
      if(*p=='\\'&&p+1<ce){ hi=p[1]; p+=2; } else hi=*p++;
      if((unsigned char)c>=(unsigned char)lo&&(unsigned char)c<=(unsigned char)hi)ok=1;
    } else if(c==lo)ok=1;
  }
  return neg?!ok:ok;
}
/* number of '(' in [pat0,p), skipping classes and escapes */
static int rx_group_index(const char* pat0, const char* p){
  int n=0; const char* q=pat0;
  while(q<p){
    if(*q=='\\'&&q[1]){ q+=2; continue; }
    if(*q=='['){ const char* cl; if(rx_cls_find(q+1,&cl)){ q=cl+1; continue; } }
    if(*q=='(')n++;
    q++;
  }
  return n;
}
static const char* rx_parse_atom(const char* p, RxAtom* a, const char* pat0){
  memset(a,0,sizeof(*a));
  char c=*p;
  if(c=='\\'){ if(!p[1])die("RX: trailing backslash"); a->type=RXA_LIT; a->ch=p[1]; return p+2; }
  if(c=='.'){ a->type=RXA_DOT; return p+1; }
  if(c=='['){ const char* cl; if(!rx_cls_find(p+1,&cl))die("RX: unbalanced ["); a->type=RXA_CLS; a->cs=p+1; a->ce=cl; return cl+1; }
  if(c=='('){
    int depth=1; const char* q=p+1;
    while(*q&&depth){
      if(*q=='\\'&&q[1]){ q+=2; continue; }
      if(*q=='['){ const char* cl; if(rx_cls_find(q+1,&cl)){ q=cl+1; continue; } q++; continue; }
      if(*q=='(')depth++;
      else if(*q==')')depth--;
      q++;
    }
    if(depth)die("RX: unbalanced (");
    a->type=RXA_GRP; a->gs=p+1; a->ge=q-1; a->cap=rx_group_index(pat0,p)+1;
    if(a->cap>9)die("RX: more than 9 groups");
    return q;
  }
  a->type=RXA_LIT; a->ch=c; return p+1;
}
static const char* rx_seq(const char* p, const char* pend, const char* s, RxCap* caps, const char* pat0);
/* match one occurrence of atom a at s; NULL on no match */
static const char* rx_atom1(RxAtom* a, const char* s, RxCap* caps, const char* pat0){
  switch(a->type){
  case RXA_LIT: return (*s&&*s==a->ch)?s+1:0;
  case RXA_DOT: return *s?s+1:0;
  case RXA_CLS: return (*s&&rx_cls_in(a->cs,a->ce,*s))?s+1:0;
  case RXA_GRP: {
    const char* alt=a->gs;
    for(;;){
      const char* ae=alt; int depth=0;
      while(ae<a->ge){
        if(*ae=='\\'&&ae+1<a->ge){ ae+=2; continue; }
        if(*ae=='['){ const char* cl; if(rx_cls_find(ae+1,&cl)&&cl<a->ge){ ae=cl+1; continue; } }
        if(*ae=='(')depth++;
        else if(*ae==')')depth--;
        else if(*ae=='|'&&depth==0)break;
        ae++;
      }
      const char* r=rx_seq(alt,ae,s,caps,pat0);
      if(r){ caps[a->cap].s=s; caps[a->cap].e=r; return r; }
      if(ae>=a->ge)return 0;
      alt=ae+1;
    }
  }
  }
  return 0;
}
/* match pattern segment [p,pend) at s; end pointer on success, NULL on fail */
static const char* rx_seq(const char* p, const char* pend, const char* s, RxCap* caps, const char* pat0){
  if(p>=pend)return s;
  if(*p=='$'&&p+1==pend)return *s==0?s:0;
  RxAtom a; const char* q=rx_parse_atom(p,&a,pat0);
  long min=1,max=1;
  if(q<pend&&(*q=='*'||*q=='+'||*q=='?')){
    char t=*q; q++;
    if(t=='*'){min=0;max=1<<30;} else if(t=='+'){min=1;max=1<<30;} else {min=0;max=1;}
  }
  if(max==1){
    const char* r=rx_atom1(&a,s,caps,pat0);
    if(r){ const char* e=rx_seq(q,pend,r,caps,pat0); if(e)return e; }
    if(min==0)return rx_seq(q,pend,s,caps,pat0);
    return 0;
  }
  /* greedy repetition with backtracking; never loops on an empty match */
  size_t capn=16,n=0; const char**v=(const char**)malloc(capn*sizeof(char*));
  if(!v)die("out of memory");
  v[n++]=s;
  while((long)n-1<max){
    const char* r=rx_atom1(&a,v[n-1],caps,pat0);
    if(!r||r==v[n-1])break;
    if(n==capn){ capn*=2; v=(const char**)realloc(v,capn*sizeof(char*)); if(!v)die("out of memory"); }
    v[n++]=r;
  }
  const char* ok=0;
  for(long k=(long)n-1;k>=min;k--){
    const char* e=rx_seq(q,pend,v[k],caps,pat0);
    if(e){ ok=e; break; }
  }
  free(v);
  return ok;
}
/* try pattern (with top-level alternation) at/after str; fills caps (0=whole) */
static int rx_exec(const char* pat, const char* str, RxCap* caps){
  const char* alt=pat;
  for(;;){
    const char* ae=alt; int depth=0;
    while(*ae){
      if(*ae=='\\'&&ae[1]){ ae+=2; continue; }
      if(*ae=='['){ const char* cl; if(rx_cls_find(ae+1,&cl)){ ae=cl+1; continue; } }
      if(*ae=='(')depth++;
      else if(*ae==')')depth--;
      else if(*ae=='|'&&depth==0)break;
      ae++;
    }
    const char* p0=alt; int anch=0;
    if(p0<ae&&*p0=='^'){ anch=1; p0++; }
    const char* pos=str;
    for(;;){
      for(int i=0;i<10;i++){ caps[i].s=0; caps[i].e=0; }
      const char* r=rx_seq(p0,ae,pos,caps,pat);
      if(r){ caps[0].s=pos; caps[0].e=r; return 1; }
      if(anch||!*pos)break;
      pos++;
    }
    if(!*ae)return 0;
    alt=ae+1;
  }
}

/* RX: str pat -> list found (group 0 = whole match; found on top) */
static void op_rx(Ctx*cx){
  Cell pat=pop(cx),st=pop(cx);
  const char* P=(char*)((void*)pat.i); const char* S=(char*)((void*)st.i);
  RxCap caps[10];
  int ntotal=rx_group_index(P,P+strlen(P));
  if(rx_exec(P,S,caps)){
    Dyn*d=uf_dyn_new((uint64_t)ntotal+1);
    for(int i=0;i<=ntotal;i++){
      if(caps[i].s) uf_dyn_push_str(&d,caps[i].s,(size_t)(caps[i].e-caps[i].s));
      else uf_dyn_push_str(&d,"",0);
    }
    pushp(cx,d); pushi(cx,1);
  } else {
    Dyn*d=uf_dyn_new(1); pushp(cx,d); pushi(cx,0);
  }
}
/* RXSUB: str pat repl -> str' (replace ALL matches; repl: \1..\9, \\) */
static void op_rxsub(Ctx*cx){
  Cell repl=pop(cx),pat=pop(cx),st=pop(cx);
  const char* R=(char*)((void*)repl.i); const char* P=(char*)((void*)pat.i); const char* S=(char*)((void*)st.i);
  size_t cap=256,n=0; char*out=(char*)uf_alloc(cap,0);
  RxCap caps[10];
  const char* cur=S;
#define UF_APP(src,L) do{ size_t _l=(size_t)(L); while(n+_l+1>cap){ cap*=2; out=(char*)realloc(out,cap); if(!out)die("out of memory"); } memcpy(out+n,(src),_l); n+=_l; }while(0)
  while(rx_exec(P,cur,caps)){
    UF_APP(cur,caps[0].s-cur);
    for(const char* r=R; *r; ){
      if(*r=='\\'&&r[1]){
        if(r[1]>='1'&&r[1]<='9'){ int g=r[1]-'0'; if(caps[g].s)UF_APP(caps[g].s,caps[g].e-caps[g].s); r+=2; }
        else { UF_APP(r+1,1); r+=2; }
      } else { UF_APP(r,1); r++; }
    }
    if(caps[0].e==caps[0].s){ if(!*cur)break; UF_APP(cur,1); cur++; }
    else cur=caps[0].e;
  }
  UF_APP(cur,strlen(cur));
#undef UF_APP
  out[n]=0; pushp(cx,out);
}
/* RXSPLIT: str pat -> list (pieces between matches; empty matches skipped) */
static void op_rxsplit(Ctx*cx){
  Cell pat=pop(cx),st=pop(cx);
  const char* P=(char*)((void*)pat.i); const char* cur=(char*)((void*)st.i);
  Dyn*d=uf_dyn_new(8); RxCap caps[10];
  while(rx_exec(P,cur,caps)){
    if(caps[0].e==caps[0].s){ if(!*cur)break; cur++; continue; }
    uf_dyn_push_str(&d,cur,(size_t)(caps[0].s-cur));
    cur=caps[0].e;
  }
  uf_dyn_push_str(&d,cur,strlen(cur));
  pushp(cx,d);
}

/* ================= string ops ================= */
#ifdef _WIN32
/* fnmatch fallback: '*', '?', '[...]' (ranges, '!' negation) */
static int uf_glob_match(const char* pat,const char* s){
  while(*pat){
    if(*pat=='*'){
      while(*pat=='*')pat++;
      if(!*pat)return 1;
      for(const char* t=s;;t++){ if(uf_glob_match(pat,t))return 1; if(!*t)break; }
      return 0;
    }
    if(*pat=='?'){ if(!*s)return 0; pat++; s++; continue; }
    if(*pat=='['){
      const char* cl; if(rx_cls_find(pat+1,&cl)){
        if(!*s)return 0;
        int neg=0; const char* p=pat+1;
        if(p<cl&&*p=='!'){ neg=1; p++; }
        int ok=0;
        while(p<cl){
          char lo=*p++;
          if(p<cl&&*p=='-'&&p+1<cl){ p++; char hi=*p++; if((unsigned char)*s>=(unsigned char)lo&&(unsigned char)*s<=(unsigned char)hi)ok=1; }
          else if(*s==lo)ok=1;
        }
        if(neg)ok=!ok;
        if(!ok)return 0;
        pat=cl+1; s++; continue;
      }
    }
    if(*pat=='\\'&&pat[1])pat++;
    if(*pat!=*s)return 0;
    if(!*s)return 0;
    pat++; s++;
  }
  return *s==0;
}
#endif
/* GLOB: str pat -> 0/1 (fnmatch-style: '*', '?', '[...]') */
static void op_glob(Ctx*cx){
  Cell pat=pop(cx),st=pop(cx);
#ifdef _WIN32
  pushi(cx,uf_glob_match((char*)((void*)pat.i),(char*)((void*)st.i))?1:0);
#else
  pushi(cx,fnmatch((char*)((void*)pat.i),(char*)((void*)st.i),0)==0?1:0);
#endif
}
/* SPLIT: str sep -> list (literal separator; pieces, tail included) */
static void op_split(Ctx*cx){
  Cell sep=pop(cx),st=pop(cx);
  const char* S=(char*)((void*)st.i); const char* E=(char*)((void*)sep.i);
  if(!*E)die("SPLIT: empty separator");
  size_t el=strlen(E);
  Dyn*d=uf_dyn_new(8);
  const char* cur=S;
  for(;;){
    const char* m=strstr(cur,E);
    if(!m)break;
    uf_dyn_push_str(&d,cur,(size_t)(m-cur));
    cur=m+el;
  }
  uf_dyn_push_str(&d,cur,strlen(cur));
  pushp(cx,d);
}
/* JOIN: list sep -> str (list of strings) */
static void op_join(Ctx*cx){
  Cell sep=pop(cx),h=pop(cx);
  Dyn*d=(Dyn*)((void*)h.i); if(d->tag!=HT_DYN)die("JOIN: not a list");
  const char* E=(char*)((void*)sep.i); size_t el=strlen(E);
  size_t cap=64; for(uint64_t i=0;i<d->len;i++)cap+=strlen((char*)((void*)d->data[i].i))+el;
  char*out=(char*)uf_alloc(cap,0); size_t n=0;
  for(uint64_t i=0;i<d->len;i++){
    if(i){ memcpy(out+n,E,el); n+=el; }
    size_t L=strlen((char*)((void*)d->data[i].i)); memcpy(out+n,(char*)((void*)d->data[i].i),L); n+=L;
  }
  out[n]=0; pushp(cx,out);
}
/* SLICE: str a b -> str' (Python slice: negatives from end, clamped; byte idx) */
static void op_slice(Ctx*cx){
  Cell b=pop(cx),a=pop(cx),st=pop(cx);
  const char* S=(char*)((void*)st.i); int64_t n=(int64_t)strlen(S);
  int64_t i=a.i,j=b.i;
  if(i<0)i+=n; if(j<0)j+=n;
  if(i<0)i=0; if(j<0)j=0;
  if(i>n)i=n; if(j>n)j=n;
  if(j<i)j=i;
  pushp(cx,uf_str_dup_n(S+i,(size_t)(j-i)));
}
/* FIND: str sub -> idx (-1 on miss; byte index) */
static void op_find(Ctx*cx){
  Cell sub=pop(cx),st=pop(cx);
  const char* m=strstr((char*)((void*)st.i),(char*)((void*)sub.i));
  pushi(cx,m?m-(char*)((void*)st.i):-1);
}
/* REPL: str old new -> str' (literal, replace all) */
static void op_repl(Ctx*cx){
  Cell nw=pop(cx),old=pop(cx),st=pop(cx);
  const char* S=(char*)((void*)st.i); const char* O=(char*)((void*)old.i); const char* N=(char*)((void*)nw.i);
  if(!*O)die("REPL: empty pattern");
  size_t ol=strlen(O),nl=strlen(N);
  size_t cap=strlen(S)+64,n=0; char*out=(char*)uf_alloc(cap,0);
  const char* cur=S;
  for(;;){
    const char* m=strstr(cur,O);
    if(!m)break;
    size_t pre=(size_t)(m-cur);
    while(n+pre+nl+1>cap){ cap*=2; out=(char*)realloc(out,cap); if(!out)die("out of memory"); }
    memcpy(out+n,cur,pre); n+=pre; memcpy(out+n,N,nl); n+=nl;
    cur=m+ol;
  }
  size_t tail=strlen(cur);
  while(n+tail+1>cap){ cap*=2; out=(char*)realloc(out,cap); if(!out)die("out of memory"); }
  memcpy(out+n,cur,tail); n+=tail;
  out[n]=0; pushp(cx,out);
}
/* TRIM: str -> str' (isspace, both ends) */
static void op_trim(Ctx*cx){
  Cell st=pop(cx);
  const char* s=(char*)((void*)st.i); size_t n=strlen(s);
  while(n>0&&isspace((unsigned char)s[0])){ s++; n--; }
  while(n>0&&isspace((unsigned char)s[n-1]))n--;
  pushp(cx,uf_str_dup_n(s,n));
}
/* UP/DOWN: str -> str' (ASCII case) */
static void op_up(Ctx*cx){ Cell st=pop(cx); char*s=(char*)((void*)st.i); size_t n=strlen(s); char*r=(char*)uf_alloc(n+1,0); for(size_t i=0;i<n;i++)r[i]=(s[i]>='a'&&s[i]<='z')?(char)(s[i]-32):s[i]; r[n]=0; pushp(cx,r); }
static void op_down(Ctx*cx){ Cell st=pop(cx); char*s=(char*)((void*)st.i); size_t n=strlen(s); char*r=(char*)uf_alloc(n+1,0); for(size_t i=0;i<n;i++)r[i]=(s[i]>='A'&&s[i]<='Z')?(char)(s[i]+32):s[i]; r[n]=0; pushp(cx,r); }
/* STARTS/ENDS: str affix -> 0/1 */
static void op_starts(Ctx*cx){ Cell af=pop(cx),st=pop(cx); const char*s=(char*)((void*)st.i); const char*a=(char*)((void*)af.i); pushi(cx,strncmp(s,a,strlen(a))==0?1:0); }
static void op_ends(Ctx*cx){ Cell af=pop(cx),st=pop(cx); const char*s=(char*)((void*)st.i); const char*a=(char*)((void*)af.i); size_t ls=strlen(s),la=strlen(a); pushi(cx,(la<=ls&&strcmp(s+ls-la,a)==0)?1:0); }
"#;

// ---------------- codegen ----------------
fn c_type(t: &str) -> &'static str {
    match t {
        "int" => "int64_t",
        "float" => "double",
        "ptr" | "handle" => "void*",
        "byte" => "char",
        "void" => "void",
        _ => panic!("unknown C type {}", t),
    }
}

// Return type used in the generated extern function-pointer declaration.
// µFlux cells are 64-bit, but a libc function declared `->int` returns a C
// int (32-bit) in eax; declaring the pointer with C `int` makes the call
// site sign-extend the result into the 64-bit cell (e.g. fgetc's EOF).
fn c_retty(t: &str) -> &'static str {
    match t {
        "int" => "int",
        "float" => "double",
        "ptr" | "handle" => "void*",
        "byte" => "char",
        "void" => "void",
        _ => panic!("unknown C type {}", t),
    }
}

fn c_escape(s: &str) -> String {
    let mut o = String::new();
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            '\r' => o.push_str("\\r"),
            '\0' => o.push_str("\\0"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\x{:02x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

// ---------------- basic-block stack virtualization (used by gen) ----------
// Within a straight-line block (between jump targets and control-flow
// instructions), stack pushes/pops become C locals instead of traffic on the
// runtime ds. The virtual stack is flushed (spilled to the real ds, in
// order) before any instruction that is a jump target, transfers control, or
// otherwise needs the real stack — so the observable ds state at every block
// boundary is exactly the naive one. All fused operations go through the
// same uf_c* helpers the op_* functions use, so tag/float semantics are
// unchanged.
fn vpop(e: &mut String, vs: &mut Vec<String>, n: &mut usize) -> String {
    if let Some(t) = vs.pop() {
        t
    } else {
        let t = format!("t{}", *n);
        *n += 1;
        e.push_str(&format!("Cell {}=pop(cx);", t));
        t
    }
}
fn vpush(e: &mut String, vs: &mut Vec<String>, n: &mut usize, init: String) -> String {
    let t = format!("t{}", *n);
    *n += 1;
    e.push_str(&format!("Cell {}={};", t, init));
    vs.push(t.clone());
    t
}
// vcache: deferred variable stores — (var name, temp, dirty). Within a
// block, SetV writes only the cache and GetV reads from it; dirty temps are
// stored back to their globals at flush time. Distinct var globals cannot
// alias each other, so deferring/reordering the stores is unobservable
// inside the block.
fn vflush(e: &mut String, vs: &mut Vec<String>, vc: &mut Vec<(String, String, bool)>) {
    for (v, t, d) in vc.drain(..) {
        if d {
            e.push_str(&format!("var_{}={};", v, t));
        }
    }
    for t in vs.drain(..) {
        e.push_str(&format!("pushc(cx,{});", t));
    }
}

fn plab(prefix: &str, i: usize) -> String {
    if prefix.is_empty() {
        format!("L_{}", i)
    } else {
        format!("{}L{}", prefix, i)
    }
}

// A FOR body starting at instruction `bs` is inlinable if its terminating
// RET is the first RET in the range (no early returns) and no internal jump
// leaves the range. Returns the exclusive end (= the RET's index).
fn for_body_range(ins: &[Ins], bs: usize) -> Option<usize> {
    let mut j = bs;
    while j < ins.len() {
        if matches!(ins[j], Ins::Ret) {
            return Some(j);
        }
        j += 1;
    }
    None
}
fn inlinable_for(p: &Parsed, bs: usize, be: usize) -> bool {
    let resolve = |name: &str| -> usize {
        *p.labels.get(name).unwrap_or_else(|| panic!("undefined label {}", name))
    };
    for (j, ins) in p.ins.iter().enumerate().take(be).skip(bs) {
        match ins {
            Ins::Ret => return false,
            Ins::Jmp(l) | Ins::Jz(l) | Ins::Je(l) => {
                let t = resolve(l);
                if t < bs || t >= be {
                    return false;
                }
            }
            _ => {}
        }
    }
    true
}

// Emit instructions [start, end) as C, prefixing every label (including K_
// continuations) with `prefix` — used to inline FOR bodies as renamed copies
// so their internal jumps stay local to the copy.
#[allow(clippy::too_many_arguments)]
fn emit_range(
    o: &mut String,
    p: &Parsed,
    targets: &std::collections::HashSet<usize>,
    inline_fors: &HashMap<usize, (usize, usize)>,
    suppress: &std::collections::HashSet<usize>,
    ext_idx: &HashMap<&str, usize>,
    start: usize,
    end: usize,
    prefix: &str,
    depth: usize,
) {
    let resolve = |name: &str| -> usize {
        *p.labels.get(name).unwrap_or_else(|| panic!("undefined label {}", name))
    };
    let mut vstack: Vec<String> = Vec::new();
    let mut vcache: Vec<(String, String, bool)> = Vec::new();
    let mut vtmp = 0usize;
    for (i, ins) in p.ins.iter().enumerate().take(end).skip(start) {
        let mut e = String::new();
        if i > start && targets.contains(&i) {
            vflush(&mut e, &mut vstack, &mut vcache);
        }
        e.push_str(&format!("{}: ", plab(prefix, i)));
        if suppress.contains(&i) {
            // PushAddr feeding an inlined FOR: the address is compile-time
            // known, so the push is elided entirely.
            o.push_str(&e);
            continue;
        }
        match ins {
            Ins::PushI(v) => {
                vpush(&mut e, &mut vstack, &mut vtmp, format!("uf_mki({}LL)", v));
            }
            Ins::PushF(v) => {
                vpush(&mut e, &mut vstack, &mut vtmp, format!("uf_mkf({:?})", v));
            }
            Ins::PushS(idx) => {
                vpush(&mut e, &mut vstack, &mut vtmp, format!("uf_mkp((void*)s{})", idx));
            }
            Ins::PushAddr(l) => {
                vflush(&mut e, &mut vstack, &mut vcache);
                // The target may live outside an inlined body range (e.g. a
                // nested FOR body); only in-range targets get the prefix.
                let t = resolve(l);
                let lab = if t >= start && t < end && !prefix.is_empty() {
                    plab(prefix, t)
                } else {
                    plab("", t)
                };
                e.push_str(&format!("pushp(cx,(void*)&&{});\n", lab))
            }
            Ins::Simple(h) => {
                let bin = match *h {
                    "op_add" => Some("uf_cadd"),
                    "op_sub" => Some("uf_csub"),
                    "op_mul" => Some("uf_cmul"),
                    "op_and" => Some("uf_cand"),
                    _ => None,
                };
                let un = match *h {
                    "op_shr" => Some("uf_cshr"),
                    "op_inc" => Some("uf_cinc"),
                    "op_dec" => Some("uf_cdec"),
                    _ => None,
                };
                if let Some(f) = bin {
                    let b = vpop(&mut e, &mut vstack, &mut vtmp);
                    let a = vpop(&mut e, &mut vstack, &mut vtmp);
                    vpush(&mut e, &mut vstack, &mut vtmp, format!("{}({},{})", f, a, b));
                } else if let Some(f) = un {
                    let a = vpop(&mut e, &mut vstack, &mut vtmp);
                    vpush(&mut e, &mut vstack, &mut vtmp, format!("{}({})", f, a));
                } else {
                    match *h {
                        "op_idx" => {
                            let ix = vpop(&mut e, &mut vstack, &mut vtmp);
                            let hh = vpop(&mut e, &mut vstack, &mut vtmp);
                            vpush(&mut e, &mut vstack, &mut vtmp, format!("uf_cidx({},({}).i)", hh, ix));
                        }
                        "op_seti" => {
                            let v = vpop(&mut e, &mut vstack, &mut vtmp);
                            let ix = vpop(&mut e, &mut vstack, &mut vtmp);
                            let hh = vpop(&mut e, &mut vstack, &mut vtmp);
                            e.push_str(&format!("uf_cseti({},({}).i,{});", hh, ix, v));
                        }
                        "op_dup" => {
                            if let Some(t) = vstack.last() {
                                let t = t.clone();
                                vstack.push(t);
                            } else {
                                e.push_str("op_dup(cx);");
                            }
                        }
                        "op_ovr" => {
                            if vstack.len() >= 2 {
                                let t = vstack[vstack.len() - 2].clone();
                                vstack.push(t);
                            } else {
                                vflush(&mut e, &mut vstack, &mut vcache);
                                e.push_str("op_ovr(cx);");
                            }
                        }
                        "op_drp" => {
                            if vstack.pop().is_none() {
                                e.push_str("op_drp(cx);");
                            }
                        }
                        "op_swp" => {
                            if vstack.len() >= 2 {
                                let n = vstack.len();
                                vstack.swap(n - 1, n - 2);
                            } else {
                                vflush(&mut e, &mut vstack, &mut vcache);
                                e.push_str("op_swp(cx);");
                            }
                        }
                        _ => {
                            vflush(&mut e, &mut vstack, &mut vcache);
                            e.push_str(&format!("{}(cx);\n", h));
                        }
                    }
                }
            }
            Ins::Jmp(l) => {
                vflush(&mut e, &mut vstack, &mut vcache);
                e.push_str(&format!("goto {};\n", plab(prefix, resolve(l))))
            }
            Ins::Jz(l) => {
                let t = vpop(&mut e, &mut vstack, &mut vtmp);
                vflush(&mut e, &mut vstack, &mut vcache);
                e.push_str(&format!("if(uf_zero({})) goto {};\n", t, plab(prefix, resolve(l))))
            }
            Ins::Je(l) => {
                let b = vpop(&mut e, &mut vstack, &mut vtmp);
                let a = vpop(&mut e, &mut vstack, &mut vtmp);
                vflush(&mut e, &mut vstack, &mut vcache);
                e.push_str(&format!("if(uf_ceq({},{})) goto {};\n", a, b, plab(prefix, resolve(l))))
            }
            Ins::For => {
                vflush(&mut e, &mut vstack, &mut vcache);
                let inl = if depth < 8 {
                    inline_fors.get(&i).copied()
                } else {
                    None
                };
                match inl {
                    Some((bs, be)) => {
                        // Compile-time-known body: direct C loop over a
                        // renamed inline copy; no call-stack or indirect
                        // jumps per iteration. The iteration index is pushed
                        // on the real ds per iteration, exactly as the
                        // subroutine path does.
                        e.push_str("{int64_t cnt=pop(cx).i;for(int64_t uf_k=0;uf_k<cnt;uf_k++){pushi(cx,uf_k);\n");
                        let inner = format!("{}F{}_", prefix, i);
                        emit_range(&mut e, p, targets, inline_fors, suppress, ext_idx, bs, be, &inner, depth + 1);
                        e.push_str("}}\n");
                    }
                    None => {
                        e.push_str(&format!(
                        "{{const void* t=((void*)pop(cx).i);int64_t cnt=pop(cx).i;for(int64_t k=0;k<cnt;k++){{pushi(cx,k);cx->cs[cx->csp++]=&&K_{}{};goto *t;K_{}{}:;}}}}\n",
                        prefix, i, prefix, i
                        ));
                    }
                }
            }
            Ins::Call(l) => {
                vflush(&mut e, &mut vstack, &mut vcache);
                // Call targets are top-level subroutine labels (outside any
                // inlined body range), so they are never prefixed; the K_
                // continuation is local to this copy and is prefixed.
                e.push_str(&format!(
                    "cx->cs[cx->csp++]=&&K_{}{};goto L_{};K_{}{}:;\n",
                    prefix,
                    i,
                    resolve(l),
                    prefix,
                    i
                ))
            }
            Ins::Ret => {
                vflush(&mut e, &mut vstack, &mut vcache);
                e.push_str("{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}\n")
            }
            Ins::SetV(v) => {
                let t = vpop(&mut e, &mut vstack, &mut vtmp);
                if let Some(ent) = vcache.iter_mut().find(|(n, _, _)| n == v) {
                    ent.1 = t;
                    ent.2 = true;
                } else {
                    vcache.push((v.clone(), t, true));
                }
            }
            Ins::GetV(v) => {
                if let Some((_, t, _)) = vcache.iter().find(|(n, _, _)| n == v) {
                    let t = t.clone();
                    vstack.push(t);
                } else {
                    let t = vpush(&mut e, &mut vstack, &mut vtmp, format!("var_{}", v));
                    vcache.push((v.clone(), t, false));
                }
            }
            Ins::Extern(name) => {
                vflush(&mut e, &mut vstack, &mut vcache);
                let xi = ext_idx[name.as_str()];
                e.push_str(&format!("pushp(cx,(void*)&uf_x{});\n", xi));
            }
            Ins::Sys(arity) => {
                vflush(&mut e, &mut vstack, &mut vcache);
                e.push_str("{int64_t num=pop(cx).i;");
                for k in (0..*arity).rev() {
                    e.push_str(&format!("int64_t sa{}=pop(cx).i;", k));
                }
                if *arity == 0 {
                    e.push_str("pushi(cx,syscall(num));");
                } else {
                    let args: Vec<String> = (0..*arity).map(|k| format!("sa{}", k)).collect();
                    e.push_str(&format!("pushi(cx,syscall(num,{}));", args.join(",")));
                }
                e.push_str("}\n");
            }
            Ins::CallExt(ii) => {
                vflush(&mut e, &mut vstack, &mut vcache);
                let im = &p.imports[*ii];
                e.push_str(&gen_call_ext(im, &format!("uf_im{}", ii)));
            }
            Ins::Send => {
                vflush(&mut e, &mut vstack, &mut vcache);
                e.push_str(&format!(
                "{{int64_t mid=pop(cx).i;Cell rc=pop(cx);Hdr*h=(Hdr*)rc.i;int64_t tk=(h->tag==HT_OBJ)?1000+(int64_t)h->len:(int64_t)h->tag;const void*lab=0;for(unsigned long q=0;q<sizeof(uf_mt)/sizeof(uf_mt[0]);q++)if(uf_mt[q].tk==tk&&uf_mt[q].mh==mid){{lab=uf_mt[q].lab;break;}}if(!lab)die(\"SEND: no such method\");pushc(cx,rc);cx->cs[cx->csp++]=&&K_{}{};goto *lab;K_{}{}:;}}\n",
                prefix, i, prefix, i
                ))
            }
            Ins::Weave(tasks) => {
                vflush(&mut e, &mut vstack, &mut vcache);
                let n = tasks.len();
                e.push_str(&format!("{{WeaveTask uf_wt[{}];\n", n));
                for (k, t) in tasks.iter().enumerate() {
                    let mut ins = String::new();
                    for &j in &t.inputs {
                        ins.push_str(&format!("{},", j));
                    }
                    e.push_str(&format!(
                        "uf_wt[{}]=(WeaveTask){{{}, {}, {{{}}}, {{0,0,0,0}}, 0}};\n",
                        k,
                        resolve(&t.pc),
                        t.inputs.len(),
                        ins
                    ));
                }
                e.push_str(&format!("uf_weave(cx,uf_wt,{},uflux_run);\n", n));
                for (k, t) in tasks.iter().enumerate() {
                    e.push_str(&format!("var_{}=uf_wt[{}].result;\n", t.name, k));
                }
                e.push_str(&format!("pushc(cx,uf_wt[{}].result);\n", n - 1));
                e.push_str("}\n");
            }
        }
        o.push_str(&e);
    }
    // end of range: make the ds/var state exact for whoever follows
    let mut e = String::new();
    vflush(&mut e, &mut vstack, &mut vcache);
    o.push_str(&e);
}

fn gen(p: &Parsed, structs: &StructMap) -> String {
    let mut o = String::new();
    o.push_str(PRELUDE);
    // reflection: struct layouts sorted by sid, consumed by op_fields
    let mut by_sid: Vec<(&String, &(Vec<(String, i64)>, i64, i64))> = structs.iter().collect();
    by_sid.sort_by_key(|(_, v)| v.2);
    if !by_sid.is_empty() {
        o.push_str(&format!(
            "static const int64_t uf_sids_v[]={{{}}};\nstatic const int64_t uf_nf_v[]={{{}}};\n",
            by_sid.iter().map(|(_, v)| v.2.to_string()).collect::<Vec<_>>().join(","),
            by_sid.iter().map(|(_, v)| v.0.len().to_string()).collect::<Vec<_>>().join(",")
        ));
        let mut fnames = Vec::new();
        for (i, (_, v)) in by_sid.iter().enumerate() {
            o.push_str(&format!(
                "static const char* uf_f_{}[]={{{}}};\n",
                i,
                v.0.iter().map(|(n, _)| format!("\"{}\"", n)).collect::<Vec<_>>().join(",")
            ));
            fnames.push(format!("uf_f_{}", i));
        }
        o.push_str(&format!(
            "static const char** uf_fields_v[]={{{}}};\nstatic void uf_init_reflection(void){{ uf_st_n={}; uf_st_sids=uf_sids_v; uf_st_nf=uf_nf_v; uf_st_fields=uf_fields_v; }}\n",
            fnames.join(","),
            by_sid.len()
        ));
    } else {
        o.push_str("static void uf_init_reflection(void){}\n");
    }
    // string literals
    for (i, s) in p.strings.iter().enumerate() {
        o.push_str(&format!("static const char s{}[] = \"{}\";\n", i, c_escape(s)));
    }
    // extern globals (asm alias avoids clashing with libc declarations)
    for (i, name) in p.externs.iter().enumerate() {
        o.push_str(&format!("extern char uf_x{}[] __asm__(\"{}\");\n", i, name));
    }
    let ext_idx: HashMap<&str, usize> = p.externs.iter().enumerate().map(|(i, n)| (n.as_str(), i)).collect();
    // declarations for imported C functions (unprototyped: args are cast at the
    // call; asm alias avoids clashing with libc prototypes, e.g. printf)
    for (i, im) in p.imports.iter().enumerate() {
        o.push_str(&format!(
            "extern {} uf_im{}() __asm__(\"{}\");\n",
            c_type(&im.ret),
            i,
            im.name
        ));
    }
    // variables
    for v in &p.vars {
        o.push_str(&format!("static Cell var_{};\n", v));
    }
    // label -> instruction index
    let resolve = |name: &str| -> usize {
        *p.labels.get(name).unwrap_or_else(|| panic!("undefined label {}", name))
    };
    o.push_str("\nstatic void uflux_run(Ctx*cx, long pc){\n");
    let n = p.ins.len();
    // Only labels that can be entered dynamically (initial pc, FOR bodies via
    // PushAddr, weave task entries, SEND methods, exports) go into labtab.
    // Every other label is reached exclusively by direct gotos, so leaving it
    // out of any address-taken context lets cc -O2 optimize across the static
    // control flow (loops, conditionals) instead of treating each label as an
    // irreducible indirect-branch target.
    let mut dyn_idx: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    dyn_idx.insert(0);
    for ins in &p.ins {
        match ins {
            Ins::PushAddr(l) => {
                dyn_idx.insert(resolve(l));
            }
            Ins::Weave(tasks) => {
                for t in tasks {
                    dyn_idx.insert(resolve(&t.pc));
                }
            }
            _ => {}
        }
    }
    for (_, _, l) in &p.methods {
        dyn_idx.insert(resolve(l));
    }
    for (_, l) in &p.exports {
        dyn_idx.insert(resolve(l));
    }
    o.push_str("  static const void* labtab[] = {");
    for &i in &dyn_idx {
        o.push_str(&format!("[{}]=&&L_{},", i, i));
    }
    if dyn_idx.is_empty() {
        o.push_str("[0]=&&L_0,");
    }
    o.push_str("};\n  goto *labtab[pc];\n");
    // method dispatch table (SEND): (type key, name hash) -> code address
    o.push_str("  static const struct { int64_t tk; int64_t mh; const void* lab; } uf_mt[] = {");
    if p.methods.is_empty() {
        o.push_str("{0,0,&&L_0}");
    } else {
        for (tk, mh, l) in &p.methods {
            o.push_str(&format!("{{{},{}LL,&&L_{}}},", tk, mh, resolve(l)));
        }
    }
    o.push_str("};\n");
    // Dispatch emission: basic-block stack virtualization (see emit_range
    // above) — jump targets start basic blocks.
    let targets: std::collections::HashSet<usize> = p.labels.values().copied().collect();
    // FOR inlining: a For immediately preceded by PushAddr of a compile-time
    // known label, whose body is structurally inlinable (no early RET, no
    // internal jump leaving the body range), is emitted as a direct C loop
    // over a renamed inline copy of the body (emit_range, recursively for
    // nested FORs). The PushAddr feeding it is elided. Any other FOR
    // (computed address, weird layout, deep recursion) keeps the subroutine
    // path.
    let mut inline_fors: HashMap<usize, (usize, usize)> = HashMap::new();
    let mut suppress: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for j in 1..p.ins.len() {
        if let (Ins::PushAddr(l), Ins::For) = (&p.ins[j - 1], &p.ins[j]) {
            if targets.contains(&(j - 1)) {
                continue; // control can jump onto the PushAddr; keep it
            }
            let bs = resolve(l);
            if let Some(be) = for_body_range(&p.ins, bs) {
                if inlinable_for(p, bs, be) {
                    inline_fors.insert(j, (bs, be));
                    suppress.insert(j - 1);
                }
            }
        }
    }
    emit_range(&mut o, p, &targets, &inline_fors, &suppress, &ext_idx, 0, n, "", 0);
    o.push_str(&format!("L_{}: return;\n}}\n", n));

    // exported wrappers (fixed 4-arg C ABI trampoline, run on the main ctx)
    for (cname, label) in &p.exports {
        let lidx = resolve(label);
        o.push_str(&format!(
            "uint64_t {}(uint64_t a0,uint64_t a1,uint64_t a2,uint64_t a3){{Ctx*cx=main_cx;long base=cx->sp;pushp(cx,(void*)a0);pushp(cx,(void*)a1);pushp(cx,(void*)a2);pushp(cx,(void*)a3);cx->cs[cx->csp++]=0;uflux_run(cx,{});uint64_t r=(cx->sp>base)?(uint64_t)pop(cx).i:0;cx->sp=base;return r;}}\n",
            cname, lidx
        ));
    }
    o.push_str("int main(int argc,char**argv){uf_argc=argc;uf_argv=(void*)argv;uf_init_reflection();uflux_run(main_cx,0);return 0;}\n");
    o
}

fn gen_call_ext(im: &Import, sym: &str) -> String {
    let vararg = im.params.iter().any(|t| t == "...");
    let fixed: Vec<&String> = im.params.iter().filter(|t| *t != "...").collect();
    let mut o = String::from("{");
    if vararg {
        // printf-style: format string is fixed param 0 (ptr); count % directives
        o.push_str("int c=uf_vargc(cx);Cell ex[8];if(c>8)die(\"vararg: too many args\");for(int k=c-1;k>=0;k--)ex[k]=pop(cx);");
        for (k, _) in fixed.iter().enumerate().rev() {
            o.push_str(&format!("Cell a{}=pop(cx);", k));
        }
        if im.ret == "void" {
            o.push_str("switch(c){");
        } else {
            o.push_str(&format!("{} r;switch(c){{", c_retty(&im.ret)));
        }
        let fty: Vec<&str> = fixed.iter().map(|t| c_type(t)).collect();
        for c in 0..=8 {
            let casts: Vec<String> = fixed
                .iter()
                .enumerate()
                .map(|(k, t)| arg_cast(c_type(t), &format!("a{}", k)))
                .collect();
            let mut callargs = casts;
            for k in 0..c {
                callargs.push(format!("ex[{}].i", k));
            }
            let dots = if c > 0 { ",..." } else { "" };
            o.push_str(&format!(
                "case {}: {}(({}(*)({}{})){})({});break;",
                c,
                if im.ret == "void" { "" } else { "r=" },
                c_retty(&im.ret),
                fty.join(","),
                dots,
                sym,
                callargs.join(",")
            ));
        }
        o.push_str("default: die(\"vararg: too many args\");}");
        if im.ret != "void" {
            o.push_str(&ret_push(&im.ret, "r"));
        }
        o.push_str("}\n");
        return o;
    }
    for (k, _) in fixed.iter().enumerate().rev() {
        o.push_str(&format!("Cell a{}=pop(cx);", k));
    }
    let fty: Vec<&str> = fixed.iter().map(|t| c_type(t)).collect();
    let casts: Vec<String> = fixed
        .iter()
        .enumerate()
        .map(|(k, t)| arg_cast(c_type(t), &format!("a{}", k)))
        .collect();
    if im.ret == "void" {
        o.push_str(&format!(
            "((void(*)({})){})({});",
            fty.join(","),
            sym,
            casts.join(",")
        ));
    } else {
        o.push_str(&format!(
            "{} r=(({}(*)({})){})({});",
            c_retty(&im.ret),
            c_retty(&im.ret),
            fty.join(","),
            sym,
            casts.join(",")
        ));
        o.push_str(&ret_push(&im.ret, "r"));
    }
    o.push_str("}\n");
    o
}

fn arg_cast(ct: &str, var: &str) -> String {
    match ct {
        "int64_t" => format!("(int64_t)({0}.tag==T_FLOAT?(int64_t)uf_f({0}):{0}.i)", var),
        "double" => format!("uf_f({})", var),
        "void*" => format!("((void*){}.i)", var),
        "char" => format!("(char){}.i", var),
        _ => var.to_string(),
    }
}

fn ret_push(ret: &str, var: &str) -> String {
    match ret {
        "int" => format!("pushi(cx,(int64_t){});", var),
        "float" => format!("pushf(cx,{});", var),
        "ptr" | "handle" => format!("pushp(cx,{});", var),
        "byte" => format!("pushi(cx,(int64_t){});", var),
        _ => String::new(),
    }
}

// ---------------- driver ----------------
// ---------------- text encoding (v9) ----------------
// Lowercase ASCII mnemonics, whitespace-delimited. Same Tok AST as dense.
// The dense lookback rule is a dense-only artifact: in text mode "-" alone is
// SUB and "-5" is a number, because tokens are space-delimited.

// text mnemonic for each opcode index (normative, SPEC.md)
fn text_mnemonic(idx: usize) -> &'static str {
    match OP_NAMES[idx] {
        "DRP" => "drop",
        name => match name {
            "LIT" => "lit", "DUP" => "dup", "OVR" => "ovr", "SWP" => "swap", "PICK" => "pick",
            "ADD" => "add", "SUB" => "sub", "MUL" => "mul", "AND" => "and", "SHR" => "shr",
            "INC" => "inc", "DEC" => "dec", "JMP" => "jmp", "JZ" => "jz", "JE" => "je",
            "FOR" => "for", "CALL" => "call", "RET" => "ret", "OBJ" => "obj", "GET" => "get",
            "SET" => "set", "SEND" => "send", "ARR" => "arr", "IDX" => "idx", "SETI" => "seti",
            "CLONE" => "clone", "CAST" => "cast", "MACRO" => "macro", "TENSOR" => "tensor",
            "VEC" => "vec", "PIN" => "pin", "UNPIN" => "unpin", "SETV" => "setv", "GETV" => "getv",
            "STR" => "str", "CAT" => "cat", "FMT" => "fmt", "BUF" => "buf", "BUFPTR" => "bufptr",
            "BUFCOPY" => "bufcopy", "ADDR" => "addr", "LOADX" => "loadx", "STOREX" => "storex",
            "SIZEOF" => "sizeof", "OFFSET" => "offset", "STRUCT" => "struct", "MALLOC" => "malloc",
            "FREE" => "free", "SYS" => "sys", "GC" => "gc", "IMPORT" => "import",
            "EXPORT" => "export", "EXTERN" => "extern", "PRINT" => "print", "SCAN" => "scan",
            "DICT" => "dict", "DGET" => "dget", "DPUT" => "dput", "DDEL" => "ddel",
            "DCOUNT" => "dcount", "DKEYS" => "dkeys", "LIST" => "list", "APPEND" => "append",
            "POP" => "pop", "CHAN" => "chan", "ENQ" => "enq", "DEQ" => "deq", "CLOSE" => "close",
            "ATOM" => "atom", "AGET" => "aget", "ASET" => "aset", "AADD" => "aadd", "CAS" => "cas",
            "TYPEOF" => "typeof", "LEN" => "len", "FIELDS" => "fields", "METHOD" => "method",
            "USE" => "use", "MOD" => "mod", "PUB" => "pub", "WEAVE" => "weave", "TASK" => "task",
            "ENDT" => "endt", "WRUN" => "wrun",
            "SH" => "sh", "SHX" => "shx", "SHL" => "shl", "SHP" => "shp",
            "EXEC" => "exec", "RX" => "rx", "RXSUB" => "rxsub",
            "RXSPLIT" => "rxsplit", "GLOB" => "glob", "SPLIT" => "split",
            "JOIN" => "join", "SLICE" => "slice", "FIND" => "find",
            "REPL" => "repl", "TRIM" => "trim", "UP" => "up", "DOWN" => "down",
            "STARTS" => "starts", "ENDS" => "ends",
            _ => unreachable!(),
        },
    }
}

fn mnemonic_index(tok: &str) -> Option<usize> {
    (0..OP_NAMES.len()).find(|&i| text_mnemonic(i) == tok)
}

fn is_reserved(tok: &str) -> bool {
    mnemonic_index(tok).is_some()
}

// split text source into whitespace-delimited tokens; strings keep their
// quotes, '{' '}' are own tokens, ';' comments to end of line
fn text_tokens(src: &str) -> Vec<String> {
    let chars: Vec<char> = src.chars().collect();
    let mut v = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == ';' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '"' {
            let mut s = String::from("\"");
            i += 1;
            while i < chars.len() {
                let c = chars[i];
                s.push(c);
                i += 1;
                if c == '\\' && i < chars.len() {
                    s.push(chars[i]);
                    i += 1;
                    continue;
                }
                if c == '"' {
                    break;
                }
            }
            v.push(s);
            continue;
        }
        if c == '{' || c == '}' {
            v.push(c.to_string());
            i += 1;
            continue;
        }
        let mut s = String::new();
        while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '{' && chars[i] != '}' && chars[i] != ';' {
            s.push(chars[i]);
            i += 1;
        }
        v.push(s);
    }
    v
}

// decode a quoted token (with quotes) into its string value
fn unquote(tok: &str) -> Option<String> {
    let chars: Vec<char> = tok.chars().collect();
    if chars.len() < 2 || chars[0] != '"' || chars[chars.len() - 1] != '"' {
        return None;
    }
    let mut s = String::new();
    let mut i = 1;
    while i < chars.len() - 1 {
        if chars[i] == '\\' && i + 1 < chars.len() - 1 {
            i += 1;
            match chars[i] {
                'n' => s.push('\n'),
                't' => s.push('\t'),
                'r' => s.push('\r'),
                '0' => s.push('\0'),
                '\\' => s.push('\\'),
                '"' => s.push('"'),
                c => s.push(c),
            }
            i += 1;
        } else {
            s.push(chars[i]);
            i += 1;
        }
    }
    Some(s)
}

// bare text number: optional '-', decimal/hex int or float; '_' separators ok
fn parse_text_num(tok: &str) -> Option<Tok> {
    if tok.is_empty() || tok == "-" {
        return None;
    }
    let s = tok.replace('_', "");
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b.to_string()),
        None => (false, s.clone()),
    };
    if body.is_empty() {
        return None;
    }
    if !body.chars().next().unwrap().is_ascii_digit() {
        return None;
    }
    if body.starts_with("0x") || body.starts_with("0X") {
        let v = i64::from_str_radix(&body[2..], 16).ok()?;
        return Some(Tok::PushI(if neg { -v } else { v }));
    }
    if body.contains('.') || body.contains('e') || body.contains('E') {
        let v: f64 = body.parse().ok()?;
        return Some(Tok::PushF(if neg { -v } else { v }));
    }
    let v: i64 = body.parse().ok()?;
    Some(Tok::PushI(if neg { -v } else { v }))
}

struct TextLexer {
    toks: Vec<String>,
    pos: usize,
}

impl TextLexer {
    fn peek(&self) -> Option<&str> {
        self.toks.get(self.pos).map(|s| s.as_str())
    }
    fn next(&mut self) -> Option<String> {
        let t = self.peek().map(|s| s.to_string());
        if t.is_some() {
            self.pos += 1;
        }
        t
    }
    fn err(&self, msg: &str) -> ! {
        panic!("text lex error at token {}: {}", self.pos, msg)
    }
    fn name_token(&mut self, what: &str) -> String {
        let t = self.next().unwrap_or_else(|| self.err(&format!("{} needs a name", what)));
        if is_reserved(&t) {
            self.err(&format!("{}: {} is a reserved word", what, t));
        }
        t
    }
    // stop: 0 = eof, 1 = '}', 2 = "endt"
    fn run(&mut self, out: &mut Vec<Tok>, stop: u8) {
        loop {
            let tok = match self.peek() {
                None => {
                    if stop != 0 {
                        self.err("unterminated block");
                    }
                    return;
                }
                Some(t) => t.to_string(),
            };
            if tok == "}" && stop == 1 {
                self.pos += 1;
                return;
            }
            if tok == "endt" && stop == 2 {
                self.pos += 1;
                return;
            }
            if let Some(s) = unquote(&tok) {
                self.pos += 1;
                out.push(Tok::PushS(s));
                continue;
            }
            if let Some(label) = tok.strip_prefix('\'') {
                self.pos += 1;
                if label.is_empty() {
                    let l = self.name_token("addr");
                    out.push(Tok::Jump("ADDR", l));
                } else {
                    if is_reserved(label) {
                        self.err("addr: reserved word as label");
                    }
                    out.push(Tok::Jump("ADDR", label.to_string()));
                }
                continue;
            }
            if let Some(name) = tok.strip_suffix(':') {
                if !name.is_empty() && name != "'" {
                    self.pos += 1;
                    if is_reserved(name) {
                        self.err(&format!("label {} is a reserved word", name));
                    }
                    out.push(Tok::LabelDef(name.to_string()));
                    continue;
                }
            }
            if let Some(name) = tok.strip_suffix('!') {
                if !name.is_empty() && !tok.starts_with('!') {
                    self.pos += 1;
                    if is_reserved(name) {
                        self.err(&format!("variable {} is a reserved word", name));
                    }
                    out.push(Tok::SetV(name.to_string()));
                    continue;
                }
            }
            if let Some(name) = tok.strip_suffix('@') {
                if !name.is_empty() && !tok.starts_with('@') {
                    self.pos += 1;
                    if is_reserved(name) {
                        self.err(&format!("variable {} is a reserved word", name));
                    }
                    out.push(Tok::GetV(name.to_string()));
                    continue;
                }
            }
            self.pos += 1;
            match tok.as_str() {
                "{" => self.err("stray '{'"),
                "}" => self.err("stray '}'"),
                "+" => out.push(Tok::Op("ADD")),
                "-" => out.push(Tok::Op("SUB")),
                "*" => out.push(Tok::Op("MUL")),
                "&" => out.push(Tok::Op("AND")),
                "=" => {
                    let l = self.name_token("je");
                    out.push(Tok::Jump("JE", l));
                }
                "pub" => out.push(Tok::Pub),
                "use" | "export" | "extern" | "mod" => {
                    let n = self.next().unwrap_or_else(|| self.err(&format!("{} needs \"name\"", tok)));
                    let n = unquote(&n).unwrap_or_else(|| self.err(&format!("{} needs \"name\"", tok)));
                    out.push(match tok.as_str() {
                        "use" => Tok::Use(n),
                        "export" => Tok::Export(n),
                        "mod" => Tok::Mod(n),
                        _ => Tok::Extern(n),
                    });
                }
                "method" => {
                    let t = self.next().unwrap_or_else(|| self.err("method needs TypeName:"));
                    match t.strip_suffix(':') {
                        Some(n) if !n.is_empty() => out.push(Tok::Method(n.to_string())),
                        _ => self.err("method needs TypeName:"),
                    }
                }
                "import" => {
                    // one token: c"name"(params)->ret
                    let n = self.next().unwrap_or_else(|| self.err("import needs a binding"));
                    out.push(self.parse_import(&n));
                }
                "macro" => {
                    let n = self.name_token("macro");
                    if self.next().as_deref() != Some("{") {
                        self.err("macro: expected '{'");
                    }
                    let mut body = Vec::new();
                    self.run(&mut body, 1);
                    out.push(Tok::MacroDef(n, body));
                }
                "struct" => {
                    let n = self.name_token("struct");
                    if self.next().as_deref() != Some("{") {
                        self.err("struct: expected '{'");
                    }
                    let mut fields = Vec::new();
                    loop {
                        let f = self.next().unwrap_or_else(|| self.err("struct: unterminated body"));
                        if f == "}" {
                            break;
                        }
                        let f = f.trim_end_matches(',');
                        if f.is_empty() {
                            continue;
                        }
                        let parts: Vec<&str> = f.splitn(2, ':').collect();
                        if parts.len() != 2 {
                            self.err("struct field must be name:type");
                        }
                        fields.push((parts[0].to_string(), parts[1].to_string()));
                    }
                    out.push(Tok::StructDef(n, fields));
                }
                "weave" => {
                    loop {
                        match self.peek() {
                            Some("wrun") => {
                                self.pos += 1;
                                out.push(Tok::Wrun);
                                break;
                            }
                            Some(_) => {
                                let mut runs = Vec::new();
                                while let Some(t) = self.peek() {
                                    if t == "task" || t == "wrun" {
                                        break;
                                    }
                                    let t = self.next().unwrap();
                                    if is_reserved(&t) {
                                        self.err("weave: reserved word as task name");
                                    }
                                    runs.push(t);
                                }
                                if self.next().as_deref() != Some("task") {
                                    self.err("weave: expected 'task' after task name");
                                }
                                let name = runs.pop().unwrap_or_else(|| self.err("weave: task needs a name"));
                                let mut body = Vec::new();
                                self.run(&mut body, 2);
                                out.push(Tok::Task { name, inputs: runs, body });
                            }
                            None => self.err("weave: unterminated (missing wrun)"),
                        }
                    }
                }
                "task" | "endt" | "wrun" => self.err("task/endt/wrun only valid inside weave..wrun"),
                "lit" => {
                    let n = self.next().unwrap_or_else(|| self.err("lit needs an operand"));
                    if let Some(id) = type_id(&n) {
                        out.push(Tok::PushI(id));
                    } else if let Some(t) = parse_text_num(&n) {
                        out.push(t);
                    } else {
                        self.err(&format!("lit: bad operand {}", n));
                    }
                }
                "jmp" | "jz" | "je" | "call" => {
                    let l = self.name_token(&tok);
                    let op = match tok.as_str() {
                        "jmp" => "JMP",
                        "jz" => "JZ",
                        "je" => "JE",
                        _ => "CALL",
                    };
                    out.push(Tok::Jump(op, l));
                }
                "addr" => {
                    let l = self.name_token("addr");
                    out.push(Tok::Jump("ADDR", l));
                }
                "sys" => {
                    let n = match self.peek() {
                        Some(t) => t.parse::<usize>().ok(),
                        None => None,
                    };
                    if let Some(n) = n {
                        self.pos += 1;
                        out.push(Tok::Sys(n));
                    } else {
                        out.push(Tok::Sys(0));
                    }
                }
                "send" => {
                    match self.peek() {
                        Some(t) if unquote(t).is_some() => {
                            let m = unquote(self.next().unwrap().as_str()).unwrap();
                            out.push(Tok::PushI(fnv64(&m)));
                            out.push(Tok::Op("SEND"));
                        }
                        Some(t) if !is_reserved(t) && !t.ends_with(':') && !t.ends_with('!') && !t.ends_with('@') && !t.starts_with('\'') && unquote(t).is_none() && parse_text_num(t).is_none() => {
                            let m = self.next().unwrap();
                            out.push(Tok::PushI(fnv64(&m)));
                            out.push(Tok::Op("SEND"));
                        }
                        _ => out.push(Tok::Op("SEND")),
                    }
                }
                "sizeof" => {
                    match self.peek() {
                        Some(t) if type_id(t).is_some() => {
                            let id = type_id(self.next().unwrap().as_str()).unwrap();
                            out.push(Tok::PushI(id));
                            out.push(Tok::Op("SIZEOF"));
                        }
                        Some(_) => {
                            let sym = self.name_token("sizeof");
                            out.push(Tok::Ident(format!("@sizeof:{}", sym)));
                        }
                        None => out.push(Tok::Op("SIZEOF")),
                    }
                }
                "offset" => {
                    let sym = self.next().unwrap_or_else(|| self.err("offset needs Struct.field"));
                    let parts: Vec<&str> = sym.splitn(2, '.').collect();
                    if parts.len() == 2 {
                        out.push(Tok::Ident(format!("@offset:{}", sym)));
                    } else {
                        self.err("offset needs Struct.field");
                    }
                }
                "obj" | "cast" | "arr" | "tensor" => {
                    let name = match tok.as_str() {
                        "obj" => "OBJ",
                        "cast" => "CAST",
                        "arr" => "ARR",
                        _ => "TENSOR",
                    };
                    let needs_swp = name == "ARR" || name == "TENSOR";
                    match self.peek() {
                        Some(t) if type_id(t).is_some() => {
                            let id = type_id(self.next().unwrap().as_str()).unwrap();
                            out.push(Tok::PushI(id));
                            if needs_swp {
                                out.push(Tok::Op("SWP"));
                            }
                            out.push(Tok::Op(name));
                        }
                        Some(t) if !is_reserved(t) && !t.ends_with(':') && !t.ends_with('!') && !t.ends_with('@') && parse_text_num(t).is_none() && unquote(t).is_none() && !t.starts_with('\'') => {
                            let kw = self.next().unwrap();
                            if name == "OBJ" {
                                out.push(Tok::Ident(format!("@objsize:{}", kw)));
                            } else if name == "CAST" {
                                out.push(Tok::Ident(format!("@cast:{}", kw)));
                            } else {
                                self.err(&format!("{}: unknown type {}", name, kw));
                            }
                            out.push(Tok::Op(name));
                        }
                        _ => out.push(Tok::Op(name)),
                    }
                }
                "scan" | "print" => out.push(Tok::Op(if tok == "scan" { "SCAN" } else { "PRINT" })),
                "str" => {
                    let n = self.next().unwrap_or_else(|| self.err("str needs a string"));
                    match unquote(&n) {
                        Some(s) => out.push(Tok::PushS(s)),
                        None => self.err("str needs a string"),
                    }
                }
                "setv" | "getv" => self.err("use name! / name@ for variables"),
                _ => {
                    if let Some(idx) = mnemonic_index(&tok) {
                        // plain opcode with no operand handling
                        out.push(Tok::Op(OP_NAMES[idx]));
                    } else if let Some(t) = parse_text_num(&tok) {
                        out.push(t);
                    } else {
                        out.push(Tok::Ident(tok));
                    }
                }
            }
        }
    }

    fn parse_import(&mut self, tok: &str) -> Tok {
        // c"name"(params)->ret
        let rest = tok.strip_prefix('c').unwrap_or_else(|| self.err("import: binding must start with c"));
        let q0 = rest.find('"').unwrap_or_else(|| self.err("import: expected c\"name\""));
        let q1 = rest[q0 + 1..].find('"').map(|i| i + q0 + 1).unwrap_or_else(|| self.err("import: unterminated name"));
        let fname = rest[q0 + 1..q1].to_string();
        let after = &rest[q1 + 1..];
        let p0 = after.find('(').unwrap_or_else(|| self.err("import: expected '('"));
        let p1 = after.rfind(')').unwrap_or_else(|| self.err("import: expected ')'"));
        let params: Vec<String> = after[p0 + 1..p1]
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let ret = after[p1 + 1..]
            .trim_start_matches('-')
            .trim_start_matches('>')
            .trim()
            .to_string();
        if ret.is_empty() {
            self.err("import: expected ->ret");
        }
        Tok::Import(Import { name: fname, params, ret })
    }
}

// lex one source, auto-detecting the encoding: any char >= U+13000 -> dense
fn lex_source(src: &str) -> Vec<Tok> {
    if src.chars().any(|c| c as u32 >= 0x13000) {
        Lexer::new(src).lex()
    } else {
        let toks = text_tokens(src);
        let mut lx = TextLexer { toks, pos: 0 };
        let mut out = Vec::new();
        lx.run(&mut out, 0);
        out
    }
}

// ---------------- round-trip emitters (v9) ----------------
fn escape_str(s: &str) -> String {
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

// dense -> text: mnemonics; glyph names "v57" are already valid text idents
fn emit_text(toks: &[Tok]) -> String {
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
            Tok::Task { name, inputs, body } => {
                if !in_weave {
                    o.push_str("weave\n  ");
                    in_weave = true;
                }
                for i in inputs {
                    o.push_str(&format!("{} ", i));
                }
                o.push_str(&format!("{} task {} endt\n  ", name, emit_text(body)));
            }
            Tok::TaskEnd(_) => {}
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
fn emit_dense(toks: &[Tok]) -> String {
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
                    let cp = CUSTOM_OPS.iter().find(|&&(_, i)| i == idx).map(|&(g, _)| g).unwrap();
                    o.push(char::from_u32(cp).unwrap());
                    o.push(' ');
                }
                Tok::PushI(v) => {
                    if *v >= 0 {
                        o.push_str(&lrun(*v as u64));
                        o.push(' ');
                    } else {
                        o.push(char::from_u32(CUSTOM_OPS[0].0).unwrap()); // LIT
                        o.push_str(&format!("{} ", v));
                    }
                }
                Tok::PushF(v) => {
                    o.push(char::from_u32(CUSTOM_OPS[0].0).unwrap()); // LIT
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
                        let cp = CUSTOM_OPS.iter().find(|&&(_, i)| i == idx).map(|&(g, _)| g).unwrap();
                        o.push(char::from_u32(cp).unwrap());
                        o.push_str(&nm(l, names, next));
                        o.push(' ');
                    }
                }
                Tok::SetV(n) => o.push_str(&format!("{}! ", nm(n, names, next))),
                Tok::GetV(n) => o.push_str(&format!("{}@ ", nm(n, names, next))),
                Tok::Import(im) => o.push_str(&format!("{}c\"{}\"({})->{} ", char::from_u32(CUSTOM_OPS[51].0).unwrap(), im.name, im.params.join(","), im.ret)),
                Tok::Export(n) => o.push_str(&format!("{}\"{}\" ", char::from_u32(CUSTOM_OPS[52].0).unwrap(), escape_str(n))),
                Tok::Extern(n) => o.push_str(&format!("{}\"{}\" ", char::from_u32(CUSTOM_OPS[53].0).unwrap(), escape_str(n))),
                Tok::Use(n) => o.push_str(&format!("{}\"{}\" ", char::from_u32(CUSTOM_OPS[78].0).unwrap(), escape_str(n))),
                Tok::Mod(n) => o.push_str(&format!("{}\"{}\" ", char::from_u32(CUSTOM_OPS[79].0).unwrap(), escape_str(n))),
                Tok::Pub => o.push_str(&format!("{} ", char::from_u32(CUSTOM_OPS[80].0).unwrap())),
                Tok::Method(n) => o.push_str(&format!("{}{}: ", char::from_u32(CUSTOM_OPS[77].0).unwrap(), n)),
                Tok::MacroDef(n, body) => {
                    o.push_str(&format!("{}{} {{ ", char::from_u32(CUSTOM_OPS[28].0).unwrap(), n));
                    rec(body, names, next, weave, o);
                    o.push_str("} ");
                }
                Tok::StructDef(n, fields) => {
                    let fs: Vec<String> = fields.iter().map(|(f, t)| format!("{}:{}", f, t)).collect();
                    o.push_str(&format!("{}{} {{ {} }} ", char::from_u32(CUSTOM_OPS[46].0).unwrap(), n, fs.join(", ")));
                }
                Tok::Sys(n) => o.push_str(&format!("{}{} ", char::from_u32(CUSTOM_OPS[49].0).unwrap(), n)),
                Tok::Ident(n) => o.push_str(&format!("{} ", n)),
                Tok::LabelDef(n) => o.push_str(&format!("{}\n", nm(n, names, next))),
                Tok::Task { name, inputs, body } => {
                    if !*weave {
                        o.push_str(&format!("{}\n", char::from_u32(CUSTOM_OPS[81].0).unwrap()));
                        *weave = true;
                    }
                    for i in inputs {
                        o.push_str(&format!("{} ", nm(i, names, next)));
                    }
                    o.push_str(&format!("{}{} ", nm(name, names, next), char::from_u32(CUSTOM_OPS[82].0).unwrap()));
                    rec(body, names, next, weave, o);
                    o.push_str(&format!("{}\n", char::from_u32(CUSTOM_OPS[83].0).unwrap()));
                }
                Tok::TaskEnd(_) => {}
                Tok::Wrun => {
                    o.push_str(&format!("{}\n", char::from_u32(CUSTOM_OPS[84].0).unwrap()));
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
            "-h" | "--help" => {
                eprintln!("usage: uf input.uf... ['inline source'|-]        compile+run (cached in TMPDIR)");
                eprintln!("       uf -c input.uf... [-o output] [--emit-c|--emit-text|--emit-dense]");
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
        match &output {
            Some(o) => fs::write(o, &s).unwrap_or_else(|e| panic!("cannot write {}: {}", o, e)),
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
