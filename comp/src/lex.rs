use crate::ast::*;

// ---------------- glyph tables ----------------
pub const OP_BASE: u32 = 0x13000; // v7 alias range U+13000..U+13037 — REMOVED in v10
pub const VAR_BASE: u32 = 0x13362; // v-space: variable/label name atoms, runs fold
pub const LIT_BASE: u32 = 0x133A4; // l-space: base-64 digit atoms for self-evaluating numbers
pub const DELIM_BASE: u32 = 0x13100;
pub const TYPE_BASE: u32 = 0x13110;

// v10 opcode glyph table (SPEC_v10_proposal.md; assignments 1:1 and final).
// Disjoint from v-space, l-space, delimiters (U+13100..13108) and type
// glyphs (U+13110..13117). Index = opcode index in OP_NAMES.
// Retired v9 indices (22,24,25,30,31,32,39,57..61,76,77,86,87,151,152) have
// no glyph and no mnemonic: using them is a compile error.
pub const CUSTOM_OPS: [(u32, usize); 174] = [
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
    (0x13251, 23), // ARR    𓉑 house row
    (0x1310E, 26), // CLONE  𓄎 copy
    (0x1312A, 27), // CAST   𓄪 mold
    (0x13133, 28), // MACRO  𓄳
    (0x13253, 29), // TENSOR 𓉓 house grid
    (0x130A4, 33), // SETV   𓂤 hand storing
    (0x1307B, 34), // GETV   𓁻 eye fetching
    (0x1308B, 35), // STR    𓂋 mouth (D21)
    (0x1308C, 36), // CAT    𓂌 mouth joined
    (0x1308D, 37), // FMT    𓂍 mouth shaping
    (0x13256, 38), // BUF    𓉖
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
    (0x132B5, 56), // DICT   𓊵 basket — keyed container
    (0x132B6, 62), // LIST   𓊶 basket row — growable vector
    (0x130A7, 63), // PUSH   𓂧 arm adding
    (0x130A8, 64), // POP    𓂨 hand taking
    (0x132B7, 65), // CHAN   𓊷 vessel — channel
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
    (0x132F9, 78), // USE    𓋹 bringing in a library
    (0x132F8, 79), // MOD    𓋸 naming the unit
    (0x132F7, 80), // PUB    𓋷 making public
    (0x1340D, 81), // WEAVE  𓐍 interlacing threads
    (0x1340E, 82), // TASK   𓐎 one thread of work
    (0x1340F, 83), // ENDT   𓐏 thread end
    (0x13410, 84), // WRUN   𓐐 threads run
    // ---- shell ----
    (0x13189, 85), // SH     𓆉 turtle — carries a shell
    (0x13217, 88), // SHP    𓈗 water — streaming shell
    (0x131A3, 89), // EXEC   𓆣 scarab — direct exec
    // ---- strings ----
    (0x1331C, 90), // MATCH   𓌜 knife — regex cut
    (0x1331D, 91), // REPLACE 𓌝 knife — regex replace
    (0x1331E, 92), // RSPLIT  𓌞 knife — regex split
    (0x1331F, 93), // GLOB   𓌟 blade — filename match
    (0x13320, 94), // SPLIT  𓌠 blade — cut apart
    (0x13321, 95), // JOIN   𓌡 blade joined — tie together
    (0x13322, 96), // SLICE  𓌢 knife slice
    (0x13323, 97), // FIND   𓌣 tool — seek substring
    (0x13324, 98), // REPL   𓌤 tool — replace
    (0x13325, 99), // TRIM   𓌥 knife — shave ends
    (0x13326, 100), // UP    𓌦 raised tool — uppercase
    (0x13327, 101), // DOWN  𓌧 lowered tool — lowercase
    (0x13328, 102), // STARTS 𓌨 head tool — prefix test
    (0x13329, 103), // ENDS  𓌩 tail tool — suffix test
    // ---- v10: arithmetic & logic ----
    (0x1332A, 104), // DIV
    (0x1332B, 105), // REM
    (0x1332C, 106), // EQ
    (0x1332D, 107), // LT
    (0x1332E, 108), // GT
    (0x1332F, 109), // NOT
    (0x13330, 110), // OR
    (0x13331, 111), // XOR
    (0x13332, 112), // SHL
    (0x13333, 113), // BNOT
    // ---- v10: structured control flow ----
    (0x13334, 114), // IF
    (0x13335, 115), // IFELSE
    (0x13336, 116), // WHILE
    (0x13337, 117), // BREAK
    (0x13338, 118), // CONT
    // ---- v10: container protocol & sequences ----
    (0x13339, 119), // GETQ
    (0x1333A, 120), // HAS
    (0x1333B, 121), // ORELSE
    (0x1333C, 122), // KEYS
    (0x1333D, 123), // RANGE
    (0x1333E, 124), // SORT
    (0x1333F, 125), // FILTER
    (0x13400, 126), // SOME
    (0x13401, 127), // EVERY
    // ---- v10: vector ops ----
    (0x13402, 128), // VADD
    (0x13403, 129), // VSUB
    (0x13404, 130), // VMUL
    (0x13405, 131), // VDIV
    (0x13406, 132), // VEADD
    (0x13407, 133), // VESUB
    (0x13408, 134), // VEMUL
    (0x13409, 135), // VEDIV
    (0x1340A, 136), // VEMAX
    (0x1340B, 137), // VEQ
    (0x1340C, 138), // VLT
    (0x13411, 139), // VGT
    (0x13412, 140), // VGE
    (0x13413, 141), // VLE
    (0x13414, 142), // VAND
    (0x13415, 143), // VOR
    (0x13416, 144), // VNOT
    (0x13417, 145), // VCOUNT
    (0x13418, 146), // VGATHER
    (0x13419, 147), // VSUM
    (0x1341A, 148), // VMEAN
    (0x1341B, 149), // VMIN
    (0x1341C, 150), // VMAX
    (0x1341E, 152), // DEL (protocol op; spec table gap, free slot/glyph)
    (0x1341F, 153), // VMAP
    (0x13420, 154), // VFOLD
    // ---- v10: time ----
    (0x13421, 155), // NOW
    (0x13422, 156), // TIME
    (0x13423, 157), // TIMEF
    // ---- v10: bloom ----
    (0x13424, 158), // BLOOM
    (0x13425, 159), // BADD
    (0x13426, 160), // BTEST
    // ---- v10: script I/O ----
    (0x13427, 161), // SLURP
    (0x13428, 162), // SPIT
    (0x13429, 163), // ARGV
    // ---- v10: additional data ops ----
    (0x1342A, 164), // GROUP
    (0x1342B, 165), // AGG
    (0x1342C, 166), // UNIQUE
    (0x1342D, 167), // FLAT
    (0x1342E, 168), // CHUNK
    (0x1342F, 169), // VARGSORT
    (0x13343, 170), // VSEARCHSORTED
    (0x13344, 171), // VWHERE
    // ---- v10: large-data shortcuts ----
    (0x13346, 172), // MMAP
    (0x13347, 173), // FEACH
    (0x13348, 174), // FFOLD
    (0x13349, 175), // FMATCH
    (0x1334A, 176), // BFS
    (0x1334B, 177), // DFS
    (0x1334C, 178), // WFIND
    // ---- v10: JSON ----
    (0x1334D, 179), // JSON
    (0x1334E, 180), // UNJSON
    // ---- v10: iterators ----
    (0x1334F, 181), // ITER
    (0x13350, 182), // NEXT
    (0x13351, 183), // COLLECT
    (0x13352, 184), // IMAP
    (0x13353, 185), // IFILTER
    (0x13354, 186), // FEMIT
    // ---- v10: error containment ----
    (0x13355, 187), // TRY
    (0x13356, 188), // RETRY
    // ---- v10: detached threads ----
    (0x13357, 189), // SPAWN
    (0x13358, 190), // VEMIN
];

pub fn opcode_index(c: char) -> Option<usize> {
    let cp = c as u32;
    CUSTOM_OPS.iter().find(|&&(g, _)| g == cp).map(|&(_, idx)| idx)
}

// U+13110..U+13117 = int float ptr byte void handle str bool.
// Ids match type_id(): int 0, float 1, ptr 2, byte 3; handle/str are ptr
// aliases, bool a byte alias, void is 4 (SIZEOF void = 8, not useful).
pub fn glyph_type_id(c: char) -> Option<i64> {
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

// 191 opcode slots (0..=190). Retired indices use "~NN" placeholders: they
// have no glyph, no text mnemonic, and no runtime helper — any use is a
// compile error (unassigned glyph / unknown identifier).
pub const OP_NAMES: [&str; 191] = [
    "LIT", "DUP", "OVR", "DRP", "SWP", "PICK", "ADD", "SUB", "MUL", "AND", "SHR", "INC", "DEC",
    "JMP", "JZ", "JE", "FOR", "CALL", "RET", "OBJ", "GET", "SET", "~22", "ARR", "~24", "~25",
    "CLONE", "CAST", "MACRO", "TENSOR", "~30", "~31", "~32", "SETV", "GETV", "STR", "CAT", "FMT",
    "BUF", "~39", "BUFCOPY", "ADDR", "LOADX", "STOREX", "SIZEOF", "OFFSET", "STRUCT", "MALLOC",
    "FREE", "SYS", "GC", "IMPORT", "EXPORT", "EXTERN", "PRINT", "SCAN",
    // v9 kept
    "DICT", "~57", "~58", "~59", "~60", "~61", "LIST", "PUSH", "POP", "CHAN",
    "ENQ", "DEQ", "CLOSE", "ATOM", "AGET", "ASET", "AADD", "CAS", "TYPEOF", "LEN", "~76",
    "~77", "USE", "MOD", "PUB", "WEAVE", "TASK", "ENDT", "WRUN",
    // shell + strings
    "SH", "~86", "~87", "SHP", "EXEC",
    "MATCH", "REPLACE", "RSPLIT", "GLOB", "SPLIT", "JOIN", "SLICE", "FIND", "REPL",
    "TRIM", "UP", "DOWN", "STARTS", "ENDS",
    // v10: arithmetic & logic
    "DIV", "REM", "EQ", "LT", "GT", "NOT", "OR", "XOR", "SHL", "BNOT",
    // v10: structured control flow
    "IF", "IFELSE", "WHILE", "BREAK", "CONT",
    // v10: container protocol & sequences
    "GETQ", "HAS", "ORELSE", "KEYS", "RANGE", "SORT", "FILTER", "SOME", "EVERY",
    // v10: vector ops
    "VADD", "VSUB", "VMUL", "VDIV", "VEADD", "VESUB", "VEMUL", "VEDIV", "VEMAX",
    "VEQ", "VLT", "VGT", "VGE", "VLE", "VAND", "VOR", "VNOT", "VCOUNT", "VGATHER",
    "VSUM", "VMEAN", "VMIN", "VMAX", "~151", "DEL", "VMAP", "VFOLD",
    // v10: time, bloom, script I/O
    "NOW", "TIME", "TIMEF", "BLOOM", "BADD", "BTEST", "SLURP", "SPIT", "ARGV",
    // v10: additional data ops
    "GROUP", "AGG", "UNIQUE", "FLAT", "CHUNK", "VARGSORT", "VSEARCHSORTED", "VWHERE",
    // v10: large-data shortcuts
    "MMAP", "FEACH", "FFOLD", "FMATCH", "BFS", "DFS", "WFIND",
    // v10: JSON, iterators, sinks, containment, threads
    "JSON", "UNJSON", "ITER", "NEXT", "COLLECT", "IMAP", "IFILTER", "FEMIT",
    "TRY", "RETRY", "SPAWN", "VEMIN",
];

pub fn op_index(name: &str) -> Option<usize> {
    OP_NAMES.iter().position(|n| *n == name)
}

pub fn type_id(kw: &str) -> Option<i64> {
    match kw {
        "int" => Some(0),
        "float" => Some(1),
        "ptr" | "handle" => Some(2),
        "byte" => Some(3),
        _ => None,
    }
}

// ---------------- lexer ----------------
pub struct Lexer {
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
                | "INC" | "DEC" | "GET" | "GETQ" | "HAS" | "CLONE" | "CAST" | "ARR" | "TENSOR" | "OBJ"
                | "CAT" | "FMT" | "BUF" | "MALLOC" | "LOADX" | "SIZEOF" | "OFFSET" | "PRINT"
                | "SCAN" | "CALL" | "SYS" | "DICT" | "KEYS" | "LIST"
                | "PUSH" | "POP" | "CHAN" | "DEQ" | "ATOM" | "AGET" | "AADD" | "CAS"
                | "TYPEOF" | "LEN"
                | "SH" | "SHP" | "EXEC"
                | "MATCH" | "REPLACE" | "RSPLIT" | "GLOB" | "SPLIT" | "JOIN" | "SLICE"
                | "FIND" | "REPL" | "TRIM" | "UP" | "DOWN" | "STARTS" | "ENDS"
                | "DIV" | "REM" | "EQ" | "LT" | "GT" | "NOT" | "OR" | "XOR" | "SHL" | "BNOT"
                | "ORELSE" | "RANGE" | "SORT" | "FILTER" | "SOME" | "EVERY"
                | "VADD" | "VSUB" | "VMUL" | "VDIV" | "VEADD" | "VESUB" | "VEMUL" | "VEDIV"
                | "VEMAX" | "VEMIN" | "VEQ" | "VLT" | "VGT" | "VGE" | "VLE" | "VAND" | "VOR"
                | "VNOT" | "VCOUNT" | "VGATHER" | "VSUM" | "VMEAN" | "VMIN" | "VMAX"
                | "VMAP" | "VFOLD" | "VARGSORT" | "VSEARCHSORTED" | "VWHERE"
                | "NOW" | "TIME" | "TIMEF" | "BLOOM" | "BTEST" | "SLURP" | "ARGV"
                | "GROUP" | "AGG" | "UNIQUE" | "FLAT" | "CHUNK"
                | "MMAP" | "FFOLD" | "FMATCH" | "BFS" | "DFS" | "WFIND"
                | "JSON" | "UNJSON" | "ITER" | "NEXT" | "COLLECT" | "IMAP" | "IFILTER"
                | "FEMIT" | "TRY" | "RETRY" | "SPAWN"
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
                // compile-time task scope: (input v-runs)* [count l-run] <name v-run> TASK body ENDT
                // repeated until the WRUN glyph
                loop {
                    self.skip_ws();
                    match self.peek() {
                        Some(g) if opcode_index(g) == Some(84) => {
                            self.pos += 1;
                            out.push(Tok::Wrun);
                            break;
                        }
                        Some(g) if ((g as u32) >= VAR_BASE && (g as u32) < VAR_BASE + 64)
                            || ((g as u32) >= LIT_BASE && (g as u32) < LIT_BASE + 64)
                            || opcode_index(g) == Some(82) =>
                        {
                            let mut runs = Vec::new();
                            let mut count: Option<i64> = None;
                            loop {
                                match self.peek() {
                                    Some(g2) if (g2 as u32) >= VAR_BASE && (g2 as u32) < VAR_BASE + 64 => {
                                        runs.push(self.fold_slots());
                                        self.skip_ws();
                                    }
                                    Some(g2) if (g2 as u32) >= LIT_BASE && (g2 as u32) < LIT_BASE + 64 => {
                                        if count.is_some() {
                                            self.err("WEAVE: more than one worker count");
                                        }
                                        count = Some(self.lrun_u64() as i64);
                                        self.skip_ws();
                                    }
                                    _ => break,
                                }
                            }
                            match self.peek() {
                                Some(g) if opcode_index(g) == Some(82) => self.pos += 1,
                                _ => self.err("WEAVE: expected TASK glyph after task inputs"),
                            }
                            self.skip_ws();
                            let name = match self.peek() {
                                Some(g) if (g as u32) >= VAR_BASE && (g as u32) < VAR_BASE + 64 => self.fold_slots(),
                                _ => self.err("WEAVE: task needs a name"),
                            };
                            let mut body = Vec::new();
                            self.lex_into(&mut body, 2);
                            out.push(Tok::Task { name, inputs: runs, count, body });
                        }
                        _ => self.err("WEAVE: expected task name or WRUN"),
                    }
                }
            }
            "TASK" | "ENDT" | "WRUN" => self.err("TASK/ENDT/WRUN are only valid inside WEAVE..WRUN"),
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

// ---------------- text encoding (v9) ----------------
// Lowercase ASCII mnemonics, whitespace-delimited. Same Tok AST as dense.
// The dense lookback rule is a dense-only artifact: in text mode "-" alone is
// SUB and "-5" is a number, because tokens are space-delimited.

// text mnemonic for each opcode index (normative, SPEC_v10_proposal.md).
// Retired slots return "~NN" which never matches a source token.
pub fn text_mnemonic(idx: usize) -> &'static str {
    match OP_NAMES[idx] {
        "LIT" => "lit", "DUP" => "dup", "OVR" => "ovr", "DRP" => "drop", "SWP" => "swp",
        "PICK" => "pick", "ADD" => "add", "SUB" => "sub", "MUL" => "mul", "AND" => "and",
        "SHR" => "shr", "INC" => "inc", "DEC" => "dec", "JMP" => "jmp", "JZ" => "jz",
        "JE" => "je", "FOR" => "for", "CALL" => "call", "RET" => "ret", "OBJ" => "obj",
        "GET" => "get", "SET" => "set", "ARR" => "arr", "CLONE" => "clone", "CAST" => "cast",
        "MACRO" => "macro", "TENSOR" => "tensor", "SETV" => "setv", "GETV" => "getv",
        "STR" => "str", "CAT" => "cat", "FMT" => "fmt", "BUF" => "buf", "BUFCOPY" => "bufcopy",
        "ADDR" => "addr", "LOADX" => "loadx", "STOREX" => "storex", "SIZEOF" => "sizeof",
        "OFFSET" => "offset", "STRUCT" => "struct", "MALLOC" => "malloc", "FREE" => "free",
        "SYS" => "sys", "GC" => "gc", "IMPORT" => "import", "EXPORT" => "export",
        "EXTERN" => "extern", "PRINT" => "print", "SCAN" => "scan",
        "DICT" => "dict", "LIST" => "list", "PUSH" => "push", "POP" => "pop", "CHAN" => "chan",
        "ENQ" => "enq", "DEQ" => "deq", "CLOSE" => "close", "ATOM" => "atom", "AGET" => "aget",
        "ASET" => "aset", "AADD" => "aadd", "CAS" => "cas", "TYPEOF" => "typeof", "LEN" => "len",
        "USE" => "use", "MOD" => "mod", "PUB" => "pub", "WEAVE" => "weave", "TASK" => "task",
        "ENDT" => "endt", "WRUN" => "wrun",
        "SH" => "sh", "SHP" => "shp", "EXEC" => "exec",
        "MATCH" => "match", "REPLACE" => "replace", "RSPLIT" => "rsplit", "GLOB" => "glob",
        "SPLIT" => "split", "JOIN" => "join", "SLICE" => "slice", "FIND" => "find",
        "REPL" => "repl", "TRIM" => "trim", "UP" => "up", "DOWN" => "down",
        "STARTS" => "starts", "ENDS" => "ends",
        // v10: arithmetic & logic
        "DIV" => "div", "REM" => "rem", "EQ" => "eq", "LT" => "lt", "GT" => "gt",
        "NOT" => "not", "OR" => "or", "XOR" => "xor", "SHL" => "shl", "BNOT" => "bnot",
        // v10: structured control flow
        "IF" => "if", "IFELSE" => "ifelse", "WHILE" => "while", "BREAK" => "break", "CONT" => "cont",
        // v10: container protocol & sequences
        "GETQ" => "getq", "HAS" => "has", "ORELSE" => "orelse", "KEYS" => "keys",
        "RANGE" => "range", "SORT" => "sort", "FILTER" => "filter", "SOME" => "some", "EVERY" => "every",
        // v10: vector ops
        "VADD" => "vadd", "VSUB" => "vsub", "VMUL" => "vmul", "VDIV" => "vdiv",
        "VEADD" => "veadd", "VESUB" => "vesub", "VEMUL" => "vemul", "VEDIV" => "vediv",
        "VEMAX" => "vemax", "VEMIN" => "vemin",
        "VEQ" => "veq", "VLT" => "vlt", "VGT" => "vgt", "VGE" => "vge", "VLE" => "vle",
        "VAND" => "vand", "VOR" => "vor", "VNOT" => "vnot", "VCOUNT" => "vcount",
        "VGATHER" => "vgather", "VSUM" => "vsum", "VMEAN" => "vmean", "VMIN" => "vmin",
        "VMAX" => "vmax", "DEL" => "del", "VMAP" => "vmap", "VFOLD" => "vfold",
        // v10: time, bloom, script I/O
        "NOW" => "now", "TIME" => "time", "TIMEF" => "timef",
        "BLOOM" => "bloom", "BADD" => "badd", "BTEST" => "btest",
        "SLURP" => "slurp", "SPIT" => "spit", "ARGV" => "argv",
        // v10: additional data ops
        "GROUP" => "group", "AGG" => "agg", "UNIQUE" => "unique", "FLAT" => "flat",
        "CHUNK" => "chunk", "VARGSORT" => "vargsort", "VSEARCHSORTED" => "vsearchsorted",
        "VWHERE" => "vwhere",
        // v10: large-data shortcuts
        "MMAP" => "mmap", "FEACH" => "feach", "FFOLD" => "ffold", "FMATCH" => "fmatch",
        "BFS" => "bfs", "DFS" => "dfs", "WFIND" => "wfind",
        // v10: JSON, iterators, sinks, containment, threads
        "JSON" => "json", "UNJSON" => "unjson", "ITER" => "iter", "NEXT" => "next",
        "COLLECT" => "collect", "IMAP" => "imap", "IFILTER" => "ifilter", "FEMIT" => "femit",
        "TRY" => "try", "RETRY" => "retry", "SPAWN" => "spawn",
        other => other, // "~NN" retired placeholders: never a valid source token
    }
}

pub fn mnemonic_index(tok: &str) -> Option<usize> {
    (0..OP_NAMES.len()).find(|&i| text_mnemonic(i) == tok)
}

pub fn is_reserved(tok: &str) -> bool {
    mnemonic_index(tok).is_some()
}

// split text source into whitespace-delimited tokens; strings keep their
// quotes, '{' '}' are own tokens, ';' comments to end of line
pub fn text_tokens(src: &str) -> Vec<String> {
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
pub fn unquote(tok: &str) -> Option<String> {
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
pub fn parse_text_num(tok: &str) -> Option<Tok> {
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

pub struct TextLexer {
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
                                let mut count: Option<i64> = None;
                                while let Some(t) = self.peek() {
                                    if t == "task" || t == "wrun" {
                                        break;
                                    }
                                    let t = self.next().unwrap();
                                    if is_reserved(&t) {
                                        self.err("weave: reserved word as task name");
                                    }
                                    // a numeric literal in input position is the
                                    // fanout worker count (1..64, checked later)
                                    if let Some(Tok::PushI(v)) = parse_text_num(&t) {
                                        if count.is_some() {
                                            self.err("weave: more than one worker count");
                                        }
                                        count = Some(v);
                                        continue;
                                    }
                                    runs.push(t);
                                }
                                if self.next().as_deref() != Some("task") {
                                    self.err("weave: expected 'task' after task inputs");
                                }
                                let name = self.name_token("task");
                                let mut body = Vec::new();
                                self.run(&mut body, 2);
                                out.push(Tok::Task { name, inputs: runs, count, body });
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
pub fn lex_source(src: &str) -> Vec<Tok> {
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
