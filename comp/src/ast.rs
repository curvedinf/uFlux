use std::collections::HashMap;

// ---------------- tokens ----------------
#[derive(Clone, Debug)]
pub struct Import {
    pub name: String,
    pub params: Vec<String>, // may contain "..."
    pub ret: String,
}

#[derive(Clone, Debug)]
pub enum Tok {
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
    Task { name: String, inputs: Vec<String>, count: Option<i64>, body: Vec<Tok> }, // inside WEAVE..WRUN
    TaskEnd(String),             // internal: end of task body, holds skip label
    Wrun,                        // WRUN — schedule the collected tasks
    MacroDef(String, Vec<Tok>),
    StructDef(String, Vec<(String, String)>), // field name, type name
    Sys(usize),                // arity immediate (default 0)
    Ident(String),             // macro invocation site
    LabelDef(String),
}

// ---------------- parser ----------------
#[derive(Clone, Debug)]
pub enum Ins {
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
    // v10 structured control flow (operands on the stack: code addresses)
    If,                          // cond body_addr ->
    IfElse,                      // cond then_addr else_addr ->
    While,                       // cond_addr body_addr ->
    Break,                       // -> (nearest enclosing loop)
    Cont,                        // -> (nearest enclosing loop)
}

#[derive(Clone, Debug)]
pub struct WeaveMeta {
    pub name: String,      // task name (also the variable it publishes to)
    pub pc: String,        // entry label
    pub inputs: Vec<usize>, // task indices in the same weave
    pub count: i64,        // fanout worker count (1 = ordinary task)
}

pub struct Parsed {
    pub ins: Vec<Ins>,
    pub labels: HashMap<String, usize>,
    pub imports: Vec<Import>,
    pub exports: Vec<(String, String)>, // C name, label
    pub externs: Vec<String>,
    pub strings: Vec<String>,
    pub vars: Vec<String>,
    pub uses: Vec<String>,
    pub modname: Option<String>,
    pub pubs: Vec<String>, // label names marked PUB (exported cross-TU)
    pub methods: Vec<(i64, i64, String)>, // (type key, method name hash, label)
}

// struct layouts: field name -> offset, total size, struct id. Shared across
// TUs and manifests (structs are global; labels/vars/macros are file-local).
pub type StructMap = HashMap<String, (Vec<(String, i64)>, i64, i64)>;
