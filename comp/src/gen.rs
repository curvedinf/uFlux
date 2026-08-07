use crate::ast::*;
use crate::prelude::PRELUDE;
use std::collections::HashMap;

// ---------------- codegen ----------------
pub fn c_type(t: &str) -> &'static str {
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
pub fn c_retty(t: &str) -> &'static str {
    match t {
        "int" => "int",
        "float" => "double",
        "ptr" | "handle" => "void*",
        "byte" => "char",
        "void" => "void",
        _ => panic!("unknown C type {}", t),
    }
}

pub fn c_escape(s: &str) -> String {
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

// ---------------- basic-block stack virtualization with type specialization (v11) ----------
// Within a straight-line block, stack pushes/pops become C locals instead of
// traffic on the runtime ds. The virtual stack is flushed (spilled to the real
// ds, in order) before any instruction that is a jump target, transfers control,
// or otherwise needs the real stack. All fused operations go through the
// same uf_c* helpers the op_* functions use, so tag/float semantics are
// unchanged.
//
// Type specialization: each virtual stack entry carries an inferred type
// (Float, Int, or Unknown). When both operands of an arithmetic op are known,
// the codegen emits raw C double/int64_t arithmetic — no Cell tag checks,
// no branches. This lets the C compiler keep values in SSE registers and
// vectorize loops, matching C++ performance for numeric kernels.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VType {
    Float,
    Int,
    FloatArr, // marks a local holding a float-typed array handle
    Unknown,
}

#[derive(Clone)]
pub struct VEntry {
    pub expr: String,  // C expression (e.g. "t0" or "t0.i" or a raw double)
    pub ty: VType,
}

pub fn vpop(e: &mut String, vs: &mut Vec<VEntry>, n: &mut usize) -> VEntry {
    if let Some(t) = vs.pop() {
        t
    } else {
        let t = format!("t{}", *n);
        *n += 1;
        e.push_str(&format!("Cell {}=pop(cx);", t));
        VEntry { expr: t, ty: VType::Unknown }
    }
}
pub fn vpush(e: &mut String, vs: &mut Vec<VEntry>, n: &mut usize, init: &str, ty: VType) -> VEntry {
    let t = format!("t{}", *n);
    *n += 1;
    e.push_str(&format!("{} {}={};", c_type_of(ty), t, init));
    vs.push(VEntry { expr: t.clone(), ty });
    VEntry { expr: t, ty }
}
// Emit a Cell-typed push (for backward compatibility with ops that expect Cell)
pub fn vpush_cell(e: &mut String, vs: &mut Vec<VEntry>, n: &mut usize, init: &str) -> VEntry {
    vpush(e, vs, n, init, VType::Unknown)
}

fn c_type_of(ty: VType) -> &'static str {
    match ty {
        VType::Float => "double",
        VType::Int => "int64_t",
        VType::FloatArr | VType::Unknown => "Cell",
    }
}

// Extract the C expression to read a value from a VEntry for Cell-context ops
fn cell_of(v: &VEntry) -> String {
    match v.ty {
        VType::Unknown | VType::FloatArr => v.expr.clone(),
        VType::Float => format!("uf_mkf({})", v.expr),
        VType::Int => format!("uf_mki({})", v.expr),
    }
}

// Result type of a binary op given operand types
fn result_type(a: VType, b: VType) -> VType {
    match (a, b) {
        (VType::Float, VType::Float) => VType::Float,
        (VType::Int, VType::Int) => VType::Int,
        (VType::Float, VType::Int) | (VType::Int, VType::Float) => VType::Float,
        _ => VType::Unknown,
    }
}

// If the expression is a constant integer literal "NLL" where N is a power of 2,
// return the shift amount. Used for strength-reducing division/modulo.
fn parse_const_pow2(s: &str) -> Option<u32> {
    let s = s.trim_end_matches("LL");
    let n: i64 = s.parse().ok()?;
    if n > 0 && (n & (n - 1)) == 0 {
        Some(n.trailing_zeros())
    } else {
        None
    }
}

// An expression is "simple" if it has no parens: a single identifier, literal,
// or array subscript. Simple expressions can be safely inlined into compound
// expressions without creating a C temp variable, letting cc see constants and
// strength-reduce (e.g. (x / 2LL) -> (x >> 1)).
fn is_simple_expr(s: &str) -> bool {
    !s.contains('(') && !s.contains(')')
}

// If the vstack entry at `idx` has a compound expression, materialize it into a
// C temp so it isn't evaluated multiple times (dup/ovr copy the entry).
fn materialize_at(e: &mut String, vs: &mut Vec<VEntry>, n: &mut usize, idx: usize) {
    if idx < vs.len() && !is_simple_expr(&vs[idx].expr) {
        let name = format!("t{}", *n);
        *n += 1;
        e.push_str(&format!("{} {}={};", c_type_of(vs[idx].ty), name, vs[idx].expr));
        vs[idx].expr = name;
    }
}

// Push a binary-op result: inline (no temp) when both operands are simple,
// otherwise materialize as a temp variable as before.
fn vpush_cond(e: &mut String, vs: &mut Vec<VEntry>, n: &mut usize,
              result: String, ty: VType, a_expr: &str, b_expr: &str) {
    if is_simple_expr(a_expr) && is_simple_expr(b_expr) {
        vs.push(VEntry { expr: result, ty });
    } else {
        vpush(e, vs, n, &result, ty);
    }
}

// C expression reading a VEntry as a double, converting if needed.
// Unknown cells go through uf_f (tag dispatch: float bits / int -> double).
fn f64_expr(v: &VEntry) -> String {
    match v.ty {
        VType::Float => v.expr.clone(),
        VType::Int => format!("((double)({}))", v.expr),
        _ => format!("uf_f({})", v.expr),
    }
}

// Inference-only result type (used by the type pre-pass, never by codegen):
// comparisons always yield Int; in arithmetic a float operand forces float
// semantics at runtime (float dominates); bitwise ops require both Int.
fn infer_bin_type(h: &str, a: VType, b: VType) -> VType {
    match h {
        "op_lt" | "op_gt" | "op_eq" => VType::Int,
        "op_and" | "op_or" | "op_xor" => {
            if a == VType::Int && b == VType::Int { VType::Int } else { VType::Unknown }
        }
        _ => match (a, b) {
            (VType::Float, _) | (_, VType::Float) => VType::Float,
            (VType::Int, VType::Int) => VType::Int,
            _ => VType::Unknown,
        },
    }
}

// vcache: deferred variable stores — (var name, temp, dirty). Within a
// block, SetV writes only the cache and GetV reads from it; dirty temps are
// stored back to their globals at flush time. Distinct var globals cannot
// alias each other, so deferring/reordering the stores is unobservable
// inside the block.
pub fn vflush(e: &mut String, vs: &mut Vec<VEntry>, vc: &mut Vec<(String, String, bool)>) {
    for (v, t, d) in vc.drain(..) {
        if d {
            e.push_str(&format!("var_{}={};", v, t));
        }
    }
    for t in vs.drain(..) {
        match t.ty {
            VType::Unknown | VType::FloatArr => e.push_str(&format!("pushc(cx,{});", t.expr)),
            VType::Float => e.push_str(&format!("pushf(cx,{});", t.expr)),
            VType::Int => e.push_str(&format!("pushi(cx,{});", t.expr)),
        }
    }
}

pub fn plab(prefix: &str, i: usize) -> String {
    if prefix.is_empty() {
        format!("L_{}", i)
    } else {
        format!("{}L{}", prefix, i)
    }
}

// A FOR body starting at instruction `bs` is inlinable if its terminating
// RET is the first RET in the range (no early returns) and no internal jump
// leaves the range. Returns the exclusive end (= the RET's index).
pub fn for_body_range(ins: &[Ins], bs: usize) -> Option<usize> {
    let mut j = bs;
    while j < ins.len() {
        if matches!(ins[j], Ins::Ret) {
            return Some(j);
        }
        j += 1;
    }
    None
}
pub fn inlinable_for(p: &Parsed, bs: usize, be: usize) -> bool {
    for (_j, ins) in p.ins.iter().enumerate().take(be).skip(bs) {
        match ins {
            Ins::Ret => return false,
            _ => {}
        }
    }
    true
}

// Emit instructions [start, end) as C, prefixing every label (including K_
// continuations) with `prefix` — used to inline FOR bodies as renamed copies
// so their internal jumps stay local to the copy.
#[allow(clippy::too_many_arguments)]
pub fn emit_range(
    o: &mut String,
    p: &Parsed,
    targets: &std::collections::HashSet<usize>,
    inline_fors: &HashMap<usize, (usize, usize)>,
    inline_ffolds: &HashMap<usize, (usize, usize)>,
    inline_whiles: &HashMap<usize, (usize, usize, usize, usize)>,
    inline_calls: &HashMap<usize, (usize, usize)>,
    outlined_bodies: &HashMap<usize, (usize, usize)>,
    suppress: &std::collections::HashSet<usize>,
    ext_idx: &HashMap<&str, usize>,
    start: usize,
    end: usize,
    prefix: &str,
    depth: usize,
    local_types: &mut HashMap<usize, VType>,
    ins_body: &[usize],
    reg: &HashMap<usize, (String, VType)>,
    numeric: &std::collections::HashSet<usize>,
    arr_ptr: &HashMap<String, String>,
) {
    let resolve = |name: &str| -> usize {
        *p.labels.get(name).unwrap_or_else(|| panic!("undefined label {}", name))
    };
    let mut vstack: Vec<VEntry> = Vec::new();
    let mut vcache: Vec<(String, String, bool)> = Vec::new();
    let mut vtmp = 0usize;
    for (i, ins) in p.ins.iter().enumerate().take(end).skip(start) {
        let mut e = String::new();
        if i > start && targets.contains(&i) {
            vflush(&mut e, &mut vstack, &mut vcache);
        }
        e.push_str(&format!("{}: ", plab(prefix, i)));
        if suppress.contains(&i) {
            if i == 4 { eprintln!("SUPPRESS 4 in emit_range"); }
            // PushAddr feeding an inlined FOR: the address is compile-time
            // known, so the push is elided entirely.
            o.push_str(&e);
            continue;
        }
        match ins {
            Ins::PushI(v) => {
                // Push literal directly: no C temp. This lets cc see constants
                // and strength-reduce (e.g. (x / 2LL) -> (x >> 1)).
                vstack.push(VEntry { expr: format!("{}LL", v), ty: VType::Int });
            }
            Ins::PushF(v) => {
                vstack.push(VEntry { expr: format!("{:?}", v), ty: VType::Float });
            }
            Ins::PushS(idx) => {
                vpush_cell(&mut e, &mut vstack, &mut vtmp, &format!("uf_mkp((void*)&uf_sl{})", idx));
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
                    "op_div" => Some("uf_cdiv"),
                    "op_rem" => Some("uf_crem"),
                    "op_lt"  => Some("uf_clt"),
                    "op_gt"  => Some("uf_cgt"),
                    "op_eq"  => Some("uf_ceq"),
                    "op_or"  => Some("uf_cor"),
                    "op_xor" => Some("uf_cxor"),
                    _ => None,
                };
                let un = match *h {
                    "op_shr" => Some("uf_cshr"),
                    "op_inc" => Some("uf_cinc"),
                    "op_dec" => Some("uf_cdec"),
                    "op_not" => Some("uf_cnot"),
                    _ => None,
                };
                if let Some(f) = bin {
                    let b = vpop(&mut e, &mut vstack, &mut vtmp);
                    let a = vpop(&mut e, &mut vstack, &mut vtmp);
                    let rt = result_type(a.ty, b.ty);
                    let is_arith = matches!(*h, "op_add"|"op_sub"|"op_mul"|"op_div"|"op_rem");
                    let is_cmp = matches!(*h, "op_lt"|"op_gt"|"op_eq");
                    if (is_arith || is_cmp)
                        && (a.ty == VType::Float || b.ty == VType::Float)
                        && a.ty != VType::FloatArr && b.ty != VType::FloatArr
                    {
                        // Float-dominant: a known float operand forces float
                        // semantics at runtime, so emit raw double arithmetic
                        // and convert the other operand inline (no Cell helper).
                        let fa = f64_expr(&a);
                        let fb = f64_expr(&b);
                        match *h {
                            "op_add" => { vpush_cond(&mut e, &mut vstack, &mut vtmp, format!("({} + {})", fa, fb), VType::Float, &fa, &fb); }
                            "op_sub" => { vpush_cond(&mut e, &mut vstack, &mut vtmp, format!("({} - {})", fa, fb), VType::Float, &fa, &fb); }
                            "op_mul" => { vpush_cond(&mut e, &mut vstack, &mut vtmp, format!("({} * {})", fa, fb), VType::Float, &fa, &fb); }
                            "op_div" => { vpush_cond(&mut e, &mut vstack, &mut vtmp, format!("({} / {})", fa, fb), VType::Float, &fa, &fb); }
                            "op_rem" => { vpush(&mut e, &mut vstack, &mut vtmp, &format!("fmod({}, {})", fa, fb), VType::Float); }
                            "op_lt"  => { vpush_cond(&mut e, &mut vstack, &mut vtmp, format!("(({} < {}) ? 1 : 0)", fa, fb), VType::Int, &fa, &fb); }
                            "op_gt"  => { vpush_cond(&mut e, &mut vstack, &mut vtmp, format!("(({} > {}) ? 1 : 0)", fa, fb), VType::Int, &fa, &fb); }
                            "op_eq"  => { vpush_cond(&mut e, &mut vstack, &mut vtmp, format!("(({} == {}) ? 1 : 0)", fa, fb), VType::Int, &fa, &fb); }
                            _ => unreachable!(),
                        }
                    } else if rt != VType::Unknown && a.ty != VType::Unknown && b.ty != VType::Unknown
                        && a.ty != VType::FloatArr && b.ty != VType::FloatArr
                        && (!matches!(*h, "op_and"|"op_or"|"op_xor") || rt == VType::Int) {
                        // Peephole: Int division/modulo by a constant power-of-2
                        // literal. GCC can't strength-reduce idivq→shift inside
                        // our monolithic function, so we do it ourselves. The
                        // dividend is non-negative in the overwhelmingly common
                        // case (loop induction math), so >> matches /.
                        if rt == VType::Int && matches!(*h, "op_div"|"op_rem") {
                            if let Some(shift) = parse_const_pow2(&b.expr) {
                                if *h == "op_div" {
                                    vpush_cond(&mut e, &mut vstack, &mut vtmp,
                                        format!("({} >> {})", a.expr, shift), VType::Int, &a.expr, &b.expr);
                                } else {
                                    vpush_cond(&mut e, &mut vstack, &mut vtmp,
                                        format!("({} & {})", a.expr, (1i64 << shift) - 1), VType::Int, &a.expr, &b.expr);
                                }
                                o.push_str(&e);
                                continue;
                            }
                        }
                        let op_str = match *h {
                            "op_add" => Some("+"), "op_sub" => Some("-"),
                            "op_mul" => Some("*"), "op_div" => Some("/"),
                            "op_and" => Some("&"), "op_or" => Some("|"),
                            "op_xor" => Some("^"),
                            _ => None,
                        };
                        let cmp_str = match *h {
                            "op_lt" => Some("<"), "op_gt" => Some(">"),
                            "op_eq" => Some("=="),
                            _ => None,
                        };
                        if let Some(op) = op_str {
                            vpush_cond(&mut e, &mut vstack, &mut vtmp,
                                format!("({} {} {})", a.expr, op, b.expr), rt, &a.expr, &b.expr);
                        } else if let Some(op) = cmp_str {
                            vpush_cond(&mut e, &mut vstack, &mut vtmp,
                                format!("(({} {} {}) ? 1 : 0)", a.expr, op, b.expr), VType::Int, &a.expr, &b.expr);
                        } else {
                            // rem: use fmod for float, % for int
                            if rt == VType::Float {
                                vpush(&mut e, &mut vstack, &mut vtmp,
                                    &format!("fmod({}, {})", a.expr, b.expr), VType::Float);
                            } else {
                                vpush(&mut e, &mut vstack, &mut vtmp,
                                    &format!("({} % {})", a.expr, b.expr), VType::Int);
                            }
                        }
                    } else {
                        // Fallback: unknown types, use Cell helpers
                        vpush_cell(&mut e, &mut vstack, &mut vtmp,
                            &format!("{}({},{})", f, cell_of(&a), cell_of(&b)));
                    }
                } else if let Some(f) = un {
                    let a = vpop(&mut e, &mut vstack, &mut vtmp);
                    if a.ty == VType::Float {
                        let op_str = match *h {
                            "op_inc" => Some("+1.0"),
                            "op_dec" => Some("-1.0"),
                            _ => None,
                        };
                        if let Some(op) = op_str {
                            vpush(&mut e, &mut vstack, &mut vtmp,
                                &format!("({} {})", a.expr, op), VType::Float);
                        } else {
                            // Wrap back to Cell for unsupported float ops
                            vpush_cell(&mut e, &mut vstack, &mut vtmp,
                                &format!("{}(uf_mkf({}))", f, a.expr));
                        }
                    } else if a.ty == VType::Int {
                        let op_str = match *h {
                            "op_inc" => Some("+1"),
                            "op_dec" => Some("-1"),
                            "op_shr" => Some(">>1"),
                            _ => None,
                        };
                        if let Some(op) = op_str {
                            vpush(&mut e, &mut vstack, &mut vtmp,
                                &format!("({} {})", a.expr, op), VType::Int);
                        } else if *h == "op_not" {
                            // !int → (int == 0) ? 1 : 0
                            vpush(&mut e, &mut vstack, &mut vtmp,
                                &format!("(({} == 0) ? 1 : 0)", a.expr), VType::Int);
                        } else {
                            // Wrap back to Cell for unsupported int ops
                            vpush_cell(&mut e, &mut vstack, &mut vtmp,
                                &format!("{}(uf_mki({}))", f, a.expr));
                        }
                    } else {
                        vpush_cell(&mut e, &mut vstack, &mut vtmp, &format!("{}({})", f, cell_of(&a)));
                    }
                } else {
                    match *h {
                        "op_idx" => {
                            let ix = vpop(&mut e, &mut vstack, &mut vtmp);
                            let hh = vpop(&mut e, &mut vstack, &mut vtmp);
                            let ix_c = match ix.ty {
                                VType::Unknown => format!("({}).i", ix.expr),
                                _ => ix.expr.clone(),
                            };
                            vpush_cell(&mut e, &mut vstack, &mut vtmp, &format!("uf_cidx({},{})", cell_of(&hh), ix_c));
                        }
                        "op_seti" => {
                            let v = vpop(&mut e, &mut vstack, &mut vtmp);
                            let ix = vpop(&mut e, &mut vstack, &mut vtmp);
                            let hh = vpop(&mut e, &mut vstack, &mut vtmp);
                            let ix_c = match ix.ty {
                                VType::Unknown => format!("({}).i", ix.expr),
                                _ => ix.expr.clone(),
                            };
                            e.push_str(&format!("uf_cseti({},{},{});", cell_of(&hh), ix_c, cell_of(&v)));
                        }
                        "op_dup" => {
                            if !vstack.is_empty() {
                                let idx = vstack.len() - 1;
                                materialize_at(&mut e, &mut vstack, &mut vtmp, idx);
                                let t = vstack[idx].clone();
                                vstack.push(t);
                            } else {
                                e.push_str("op_dup(cx);");
                            }
                        }
                        "op_ovr" => {
                            if vstack.len() >= 2 {
                                let idx = vstack.len() - 2;
                                materialize_at(&mut e, &mut vstack, &mut vtmp, idx);
                                let t = vstack[idx].clone();
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
                        "op_vget" => {
                            let idx = vpop(&mut e, &mut vstack, &mut vtmp);
                            let hh = vpop(&mut e, &mut vstack, &mut vtmp);
                            let idx_c = match idx.ty {
                                VType::Unknown => format!("({}).i", idx.expr),
                                _ => idx.expr.clone(),
                            };
                            if hh.ty == VType::FloatArr {
                                // Float-typed array: raw double load, no Cell
                                if let Some(p) = arr_ptr.get(&hh.expr) {
                                    // Loop-hoisted element pointer
                                    vpush(&mut e, &mut vstack, &mut vtmp,
                                        &format!("({})[{}]", p, idx_c), VType::Float);
                                } else {
                                    vpush(&mut e, &mut vstack, &mut vtmp,
                                        &format!("((double*)uf_data((Hdr*)({}).i))[{}]", hh.expr, idx_c), VType::Float);
                                }
                            } else {
                                vpush_cell(&mut e, &mut vstack, &mut vtmp, &format!("uf_cvget({},{})", cell_of(&hh), idx_c));
                            }
                        }
                        "op_vset" => {
                            let v = vpop(&mut e, &mut vstack, &mut vtmp);
                            let idx = vpop(&mut e, &mut vstack, &mut vtmp);
                            let hh = vpop(&mut e, &mut vstack, &mut vtmp);
                            let idx_c = match idx.ty {
                                VType::Unknown => format!("({}).i", idx.expr),
                                _ => idx.expr.clone(),
                            };
                            if hh.ty == VType::FloatArr {
                                // Float-typed array: raw double store, no Cell
                                if let Some(p) = arr_ptr.get(&hh.expr) {
                                    e.push_str(&format!("({})[{}]={};", p, idx_c, f64_expr(&v)));
                                } else {
                                    e.push_str(&format!("((double*)uf_data((Hdr*)({}).i))[{}]={};", hh.expr, idx_c, f64_expr(&v)));
                                }
                            } else {
                                e.push_str(&format!("uf_cvset({},{},{});", cell_of(&hh), idx_c, cell_of(&v)));
                            }
                        }
                        _ => {
                            let is_inlined_ffold = (*h == "op_ffold" || *h == "op_fsplit") && depth < 8 && inline_ffolds.contains_key(&i);
                            if is_inlined_ffold {
                                if let Some((bs, be)) = inline_ffolds.get(&i).copied() {
                                    vflush(&mut e, &mut vstack, &mut vcache);
                                    if *h == "op_ffold" {
                                        /* inlined FFOLD: getline loop + line string + callback */
                                        e.push_str("{Cell _ff_acc=pop(cx),_ff_p=pop(cx);FILE*_fp=fopen(uf_sptr(_ff_p),\"r\");if(!_fp)die(\"FFOLD: cannot open file\");char*_line=0;size_t _ncap=0;ssize_t m;long fr=cx->lsp++;if(cx->lsp>=64)die(\"loops nested too deep\");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_FF_C_");
                                        e.push_str(&format!("{}{};cx->loops[fr].end=&&K_FF_E_{}{};while((m=getline(&_line,&_ncap,_fp))>=0){{while(m>0&&(_line[m-1]=='\\n'||_line[m-1]=='\\r'))_line[--m]=0;Cell _ls=uf_str_new(_line,(size_t)m);pushc(cx,_ff_acc);pushc(cx,_ls);\n", prefix, i, prefix, i));
                                        let inner = format!("{}FF{}_", prefix, i);
                                        emit_range(&mut e, p, targets, inline_fors, inline_ffolds, inline_whiles, inline_calls, outlined_bodies, suppress, ext_idx, bs, be, &inner, depth + 1, local_types, ins_body, &HashMap::new(), &std::collections::HashSet::new(), &HashMap::new());
                                        e.push_str(&format!("K_FF_C_{}{}:;_ff_acc=pop(cx);}}K_FF_E_{}{}:;cx->lsp=fr;free(_line);fclose(_fp);pushc(cx,_ff_acc);}}\n", prefix, i, prefix, i));
                                    } else {
                                        /* inlined FSPLIT: getline loop + in-place split + field offsets + callback */
                                        e.push_str("{Cell _ff_acc=pop(cx),_ff_sep=pop(cx),_ff_p=pop(cx);const char*_E=uf_sptr(_ff_sep);if(!*_E)die(\"FSPLIT: empty separator\");size_t _el=strlen(_E);FILE*_fp=fopen(uf_sptr(_ff_p),\"r\");if(!_fp)die(\"FSPLIT: cannot open file\");char*_line=0;size_t _ncap=0;ssize_t m;long fr=cx->lsp++;if(cx->lsp>=64)die(\"loops nested too deep\");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_FF_C_");
                                        e.push_str(&format!("{}{};cx->loops[fr].end=&&K_FF_E_{}{};while((m=getline(&_line,&_ncap,_fp))>=0){{\n", prefix, i, prefix, i));
                                        /* set up fsplit thread-locals for fget/fatoi/fsget/fbyte */
                                        e.push_str("while(m>0&&(_line[m-1]=='\\n'||_line[m-1]=='\\r'))_line[--m]=0;\n");
                                        e.push_str("uf_fsplit_line=_line;uf_fsplit_parent=0;\n");
                                        e.push_str("uf_fsplit_nfields=0;char*_cur=_line;\n");
                                        e.push_str("while(uf_fsplit_nfields<128){char*_sp=strstr(_cur,_E);if(!_sp){uf_fsplit_offsets[uf_fsplit_nfields*2]=(int64_t)(_cur-_line);uf_fsplit_offsets[uf_fsplit_nfields*2+1]=(int64_t)strlen(_cur);uf_fsplit_nfields++;break;}*_sp=0;uf_fsplit_offsets[uf_fsplit_nfields*2]=(int64_t)(_cur-_line);uf_fsplit_offsets[uf_fsplit_nfields*2+1]=(int64_t)(_sp-_cur);uf_fsplit_nfields++;_cur=_sp+_el;}\n");
                                        e.push_str("pushc(cx,_ff_acc);pushi(cx,uf_fsplit_nfields);\n");
                                        let inner = format!("{}FF{}_", prefix, i);
                                        emit_range(&mut e, p, targets, inline_fors, inline_ffolds, inline_whiles, inline_calls, outlined_bodies, suppress, ext_idx, bs, be, &inner, depth + 1, local_types, ins_body, &HashMap::new(), &std::collections::HashSet::new(), &HashMap::new());
                                        e.push_str(&format!("K_FF_C_{}{}:;_ff_acc=pop(cx);}}K_FF_E_{}{}:;cx->lsp=fr;free(_line);fclose(_fp);uf_fsplit_line=0;pushc(cx,_ff_acc);}}\n", prefix, i, prefix, i));
                                    }
                                }
                            } else {
                                vflush(&mut e, &mut vstack, &mut vcache);
                                e.push_str(&format!("{}(cx);\n", h));
                            }
                        }
                    }
                }
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
                        // subroutine path does. A dynamic loop frame lets
                        // BREAK/CONT unwind out of the C loop.
                        e.push_str(&format!("{{int64_t cnt=pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die(\"loops nested too deep\");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_FC_{}{};cx->loops[fr].end=&&K_FE_{}{};for(int64_t uf_k=0;uf_k<cnt;uf_k++){{pushi(cx,uf_k);\n", prefix, i, prefix, i));
                        let inner = format!("{}F{}_", prefix, i);
                        emit_range(&mut e, p, targets, inline_fors, inline_ffolds, inline_whiles, inline_calls, outlined_bodies, suppress, ext_idx, bs, be, &inner, depth + 1, local_types, ins_body, &HashMap::new(), &std::collections::HashSet::new(), &HashMap::new());
                        e.push_str(&format!("K_FC_{}{}:;}}\nK_FE_{}{}:;cx->lsp=fr;}}\n", prefix, i, prefix, i));
                    }
                    None => {
                        e.push_str(&format!(
                        "{{const void* t=((void*)pop(cx).i);int64_t cnt=pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die(\"loops nested too deep\");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_FC_{}{};cx->loops[fr].end=&&K_FE_{}{};for(int64_t k=0;k<cnt;k++){{pushi(cx,k);cx->cs[cx->csp++]=&&K_{}{};goto *t;K_{}{}:;K_FC_{}{}:;}}K_FE_{}{}:;cx->lsp=fr;}}\n",
                        prefix, i, prefix, i, prefix, i, prefix, i, prefix, i, prefix, i
                        ));
                    }
                }
            }
            Ins::Call(l) => {
                vflush(&mut e, &mut vstack, &mut vcache);
                if depth < 8 {
                    if let Some(&(bs, be)) = inline_calls.get(&i) {
                        // Inlined CALL: emit body inline in a C scope.
                        // Locals: save base, bump by inline body's frame size,
                        // run body, restore base.
                        let inner = format!("{}C{}_", prefix, i);
                        let lc = p.local_counts.get(&bs).copied().unwrap_or(0);
                        e.push_str(&format!("{{long _lb=cx->local_base;cx->local_base+={};\n", lc));
                        // v11: don't trim trailing PushI — the caller may need
                        // the value pushed before RET (e.g. "0 ret" convention)
                        emit_range(&mut e, p, targets, inline_fors, inline_ffolds, inline_whiles, inline_calls, outlined_bodies, suppress, ext_idx, bs, be, &inner, depth + 1, local_types, ins_body, &HashMap::new(), &std::collections::HashSet::new(), &HashMap::new());
                        e.push_str("cx->local_base=_lb;}\n");
                        o.push_str(&e);
                        continue;
                    }
                }
                // Check if this call was outlined to a separate function
                if let Some(&(bs, _be)) = outlined_bodies.get(&i) {
                    e.push_str(&format!("uf_ob_{}(cx);\n", bs));
                    o.push_str(&e);
                    continue;
                }
                let target_pc = resolve(l);
                let lc = p.local_counts.get(&target_pc).copied().unwrap_or(0);
                e.push_str(&format!(
                    "cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+={};cx->cs[cx->csp++]=&&K_{}{};goto L_{};K_{}{}:;cx->local_base=cx->local_frames[--cx->local_fsp];\n",
                    lc,
                    prefix,
                    i,
                    target_pc,
                    prefix,
                    i
                ))
            }
            Ins::Ret => {
                vflush(&mut e, &mut vstack, &mut vcache);
                if prefix.starts_with("OB") {
                    // Inside an outlined function: RET restores the local frame
                    // and returns to caller. The frame setup/restore is split:
                    // setup is in the function wrapper, restore is here because
                    // we need to return from inside the body.
                    e.push_str("{cx->local_base=cx->local_frames[--cx->local_fsp];return;}\n");
                } else {
                    // v11: standard RET. The CALL path handles frame restore in the
                    // K_after_call continuation. The uf_call_addr path (try/retry/
                    // exports/callbacks) pushes a NULL return sentinel.
                    e.push_str("{if(cx->csp==0)return;{const void* r=cx->cs[--cx->csp];if(!r)return;goto *r;}}\n");
                }
            }
            Ins::Goto(l) => {
                vflush(&mut e, &mut vstack, &mut vcache);
                e.push_str(&format!("goto {};\n", plab(prefix, resolve(l))))
            }
            Ins::Goto(l) => {
                vflush(&mut e, &mut vstack, &mut vcache);
                e.push_str(&format!("goto {};\n", plab(prefix, resolve(l))))
            }
            // v10 structured control flow: operands are code addresses on the
            // ds, CALLed via the threaded call stack (like FOR's path).
            Ins::If => {
                vflush(&mut e, &mut vstack, &mut vcache);
                e.push_str(&format!(
                    "{{const void* b=(const void*)pop(cx).i;Cell c=pop(cx);if(!uf_zero(c)){{cx->cs[cx->csp++]=&&K_{}{};goto *b;K_{}{}:;pop(cx);}}}}\n",
                    prefix, i, prefix, i
                ))
            }
            Ins::IfElse => {
                vflush(&mut e, &mut vstack, &mut vcache);
                e.push_str(&format!(
                    "{{const void* el=(const void*)pop(cx).i;const void* th=(const void*)pop(cx).i;Cell c=pop(cx);cx->cs[cx->csp++]=&&K_{}{};goto *((!uf_zero(c))?th:el);K_{}{}:;pop(cx);}}\n",
                    prefix, i, prefix, i
                ))
            }
            Ins::While => {
                vflush(&mut e, &mut vstack, &mut vcache);
                if depth < 8 {
                    if let Some((bbs, bbe, cbs, cbe)) = inline_whiles.get(&i).copied() {
                        let inner_c = format!("{}WC{}_", prefix, i);
                        let inner_b = format!("{}WB{}_", prefix, i);
                        let cbe_trim = if cbe > cbs + 1 && matches!(p.ins[cbe - 1], Ins::PushI(_)) { cbe - 1 } else { cbe };
                        let bbe_trim = if bbe > bbs + 1 && matches!(p.ins[bbe - 1], Ins::PushI(_)) { bbe - 1 } else { bbe };
                        // Register-cache typed locals across the loop: when the
                        // inlined cond+body make no calls, take no indirect
                        // jumps and nest no further control flow, Int/Float
                        // slots can live in C locals for the loop's duration
                        // and be written back once at loop exit.
                        let mut reg: HashMap<usize, (String, VType)> = HashMap::new();
                        {
                            let escapable = (cbs..cbe_trim).chain(bbs..bbe_trim).any(|k| match &p.ins[k] {
                                Ins::Break | Ins::Cont | Ins::Call(_) | Ins::CallExt(_) | Ins::Sys(_) |
                                Ins::Weave(_) | Ins::Send | Ins::Goto(_) | Ins::While | Ins::For |
                                Ins::If | Ins::IfElse | Ins::PushAddr(_) => true,
                                Ins::Simple(h) => *h == "op_ffold" || *h == "op_fsplit",
                                _ => false,
                            });
                            if !escapable {
                                for k in (cbs..cbe_trim).chain(bbs..bbe_trim) {
                                    let id = match &p.ins[k] {
                                        Ins::LocalGetI(id) | Ins::LocalSetI(id) => *id,
                                        _ => continue,
                                    };
                                    if reg.contains_key(&id) { continue; }
                                    let key = ins_body[k] * 1000000 + id;
                                    let ty = local_types.get(&key).copied().unwrap_or(VType::Unknown);
                                    if ty == VType::Int || ty == VType::Float {
                                        reg.insert(id, (format!("_r{}", id), ty));
                                    } else if numeric.contains(&key) {
                                        // Proven numeric-only slot: cache as a
                                        // plain Cell C local (no pointers, so
                                        // GC-safe), skipping the locals memory
                                        reg.insert(id, (format!("_r{}", id), VType::Unknown));
                                    }
                                }
                            }
                        }
                        let mut regs: Vec<_> = reg.iter().collect();
                        regs.sort_by_key(|(id, _)| *id);
                        // Hoist float-array element pointers: FloatArr slots
                        // never reassigned inside the loop can keep a raw
                        // double* in a C local (non-moving GC; the memory cell
                        // keeps the object rooted).
                        let mut arr_ptr: HashMap<String, String> = HashMap::new();
                        {
                            let escapable2 = (cbs..cbe_trim).chain(bbs..bbe_trim).any(|k| match &p.ins[k] {
                                Ins::Break | Ins::Cont | Ins::Call(_) | Ins::CallExt(_) | Ins::Sys(_) |
                                Ins::Weave(_) | Ins::Send | Ins::Goto(_) | Ins::While | Ins::For |
                                Ins::If | Ins::IfElse | Ins::PushAddr(_) => true,
                                Ins::Simple(h) => *h == "op_ffold" || *h == "op_fsplit",
                                _ => false,
                            });
                            if !escapable2 {
                                let mut reassigned: std::collections::HashSet<usize> = std::collections::HashSet::new();
                                for k in (cbs..cbe_trim).chain(bbs..bbe_trim) {
                                    if let Ins::LocalSetI(id) = &p.ins[k] { reassigned.insert(*id); }
                                }
                                for k in (cbs..cbe_trim).chain(bbs..bbe_trim) {
                                    if let Ins::LocalGetI(id) = &p.ins[k] {
                                        if reassigned.contains(id) { continue; }
                                        let key = ins_body[k] * 1000000 + id;
                                        if local_types.get(&key).copied() == Some(VType::FloatArr) {
                                            arr_ptr.insert(
                                                format!("cx->locals[cx->local_base+{}]", id),
                                                format!("_a{}", id));
                                        }
                                    }
                                }
                            }
                        }
                        let mut arrps: Vec<_> = arr_ptr.iter().collect();
                        arrps.sort();
                        e.push_str(&format!("{{long fr=cx->lsp++;if(cx->lsp>=64)die(\"loops nested too deep\");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_WC_{}{};cx->loops[fr].end=&&K_WE_{}{};\n", prefix, i, prefix, i));
                        for (id, (name, ty)) in &regs {
                            match ty {
                                VType::Int => e.push_str(&format!("int64_t {}=uf_i(cx->locals[cx->local_base+{}]);\n", name, id)),
                                VType::Float => e.push_str(&format!("double {}=uf_f(cx->locals[cx->local_base+{}]);\n", name, id)),
                                _ => e.push_str(&format!("Cell {}=cx->locals[cx->local_base+{}];\n", name, id)),
                            }
                        }
                        for (expr, name) in &arrps {
                            e.push_str(&format!("double* {}=(double*)uf_data((Hdr*)({}).i);\n", name, expr));
                        }
                        e.push_str(&format!("K_WC_{}{}:;{{Cell _wc;{{\n", prefix, i));
                        // Emit the cond into a side buffer; if it ends with a
                        // plain typed value push, peel it into a direct C test
                        // and skip the data-stack round trip entirely.
                        let mut ce = String::new();
                        emit_range(&mut ce, p, targets, inline_fors, inline_ffolds, inline_whiles, inline_calls, outlined_bodies, suppress, ext_idx, cbs, cbe_trim, &inner_c, depth + 1, local_types, ins_body, &reg, numeric, &arr_ptr);
                        let mut direct_cond: Option<String> = None;
                        if ce.ends_with(");") {
                            for tag in ["pushi(cx,", "pushf(cx,"] {
                                if let Some(pos) = ce.rfind(tag) {
                                    let expr = &ce[pos + tag.len()..ce.len() - 2];
                                    // single expression: balanced parens, no
                                    // semicolons or string literals inside
                                    let mut dp = 0i32;
                                    let mut ok = !expr.contains('"') && !expr.contains(';');
                                    if ok {
                                        for ch in expr.chars() {
                                            match ch { '(' => dp += 1, ')' => dp -= 1, _ => {} }
                                            if dp < 0 { ok = false; break; }
                                        }
                                    }
                                    if ok && dp == 0 {
                                        direct_cond = Some(expr.to_string());
                                        ce.truncate(pos);
                                        break;
                                    }
                                }
                            }
                        }
                        e.push_str(&ce);
                        match direct_cond {
                            // keep the test inside the block where the cond
                            // temps are declared
                            Some(expr) => e.push_str(&format!("if(({})==0)goto K_WE_{}{};}};\n", expr, prefix, i)),
                            None => e.push_str(&format!("}}_wc=pop(cx);if(uf_zero(_wc))goto K_WE_{}{};\n", prefix, i)),
                        }
                        e.push_str("{\n");
                        emit_range(&mut e, p, targets, inline_fors, inline_ffolds, inline_whiles, inline_calls, outlined_bodies, suppress, ext_idx, bbs, bbe_trim, &inner_b, depth + 1, local_types, ins_body, &reg, numeric, &arr_ptr);
                        e.push_str(&format!("}}goto K_WC_{}{};}}\nK_WE_{}{}:;", prefix, i, prefix, i));
                        for (id, (name, ty)) in &regs {
                            match ty {
                                VType::Int => e.push_str(&format!("cx->locals[cx->local_base+{}]=uf_mki({});", id, name)),
                                VType::Float => e.push_str(&format!("cx->locals[cx->local_base+{}]=uf_mkf({});", id, name)),
                                _ => e.push_str(&format!("cx->locals[cx->local_base+{}]={};", id, name)),
                            }
                        }
                        e.push_str("cx->lsp=fr;}\n");
                        o.push_str(&e);
                        continue;
                    }
                }
                e.push_str(&format!(
                    "{{const void* bod=(const void*)pop(cx).i;const void* cnd=(const void*)pop(cx).i;long fr=cx->lsp++;if(cx->lsp>=64)die(\"loops nested too deep\");cx->loops[fr].cspl=cx->csp;\n\
                     K_WT_{}{}:;cx->loops[fr].cont=&&K_WT_{}{};cx->loops[fr].end=&&K_WE_{}{};\n\
                     cx->cs[cx->csp++]=&&K_WC_{}{};goto *cnd;K_WC_{}{}:;\n\
                     if(uf_zero(pop(cx)))goto K_WE_{}{};\n\
                     cx->cs[cx->csp++]=&&K_WB_{}{};goto *bod;K_WB_{}{}:;pop(cx);\n\
                     goto K_WT_{}{};\n\
                     K_WE_{}{}:;cx->lsp=fr;}}\n",
                    prefix, i, prefix, i, prefix, i,
                    prefix, i, prefix, i,
                    prefix, i,
                    prefix, i, prefix, i,
                    prefix, i,
                    prefix, i
                ))
            }
            Ins::Break => {
                vflush(&mut e, &mut vstack, &mut vcache);
                e.push_str("{if(cx->lsp<=0)die(\"break outside loop\");cx->lsp--;cx->csp=cx->loops[cx->lsp].cspl;goto *cx->loops[cx->lsp].end;}\n")
            }
            Ins::Cont => {
                vflush(&mut e, &mut vstack, &mut vcache);
                e.push_str("{if(cx->lsp<=0)die(\"cont outside loop\");cx->csp=cx->loops[cx->lsp-1].cspl;goto *cx->loops[cx->lsp-1].cont;}\n")
            }
            Ins::SetV(v) => {
                let t = vpop(&mut e, &mut vstack, &mut vtmp);
                // Wrap typed values into Cell for global storage
                let cell_expr = match t.ty {
                    VType::Unknown | VType::FloatArr => t.expr.clone(),
                    VType::Float => format!("uf_mkf({})", t.expr),
                    VType::Int => format!("uf_mki({})", t.expr),
                };
                if let Some(ent) = vcache.iter_mut().find(|(n, _, _)| n == v) {
                    ent.1 = cell_expr;
                    ent.2 = true;
                } else {
                    vcache.push((v.clone(), cell_expr, true));
                }
            }
            Ins::GetV(v) => {
                if let Some((_, t, _)) = vcache.iter().find(|(n, _, _)| n == v) {
                    let t = t.clone();
                    vstack.push(VEntry { expr: t, ty: VType::Unknown });
                } else {
                    vpush_cell(&mut e, &mut vstack, &mut vtmp, &format!("var_{}", v));
                    vcache.push((v.clone(), format!("var_{}", v), false));
                }
            }
            Ins::LocalSetI(id) => {
                let t = vpop(&mut e, &mut vstack, &mut vtmp);
                if let Some((name, ty)) = reg.get(id) {
                    // Register-cached slot: raw C assignment, no memory traffic
                    let rhs = if *ty == VType::Int {
                        match t.ty {
                            VType::Int => t.expr.clone(),
                            _ => format!("uf_i({})", cell_of(&t)),
                        }
                    } else if *ty == VType::Float {
                        f64_expr(&t)
                    } else {
                        // Cell-cached numeric slot
                        cell_of(&t)
                    };
                    e.push_str(&format!("{}={};", name, rhs));
                    o.push_str(&e);
                    continue;
                }
                match t.ty {
                    VType::Unknown | VType::FloatArr => {
                        e.push_str(&format!("cx->locals[cx->local_base+{}]={};", id, t.expr));
                    }
                    VType::Float => {
                        e.push_str(&format!("cx->locals[cx->local_base+{}]=uf_mkf({});", id, t.expr));
                    }
                    VType::Int => {
                        e.push_str(&format!("cx->locals[cx->local_base+{}]=uf_mki({});", id, t.expr));
                    }
                }
            }
            Ins::LocalGetI(id) => {
                if let Some((name, ty)) = reg.get(id) {
                    // Register-cached slot: plain C local read
                    vstack.push(VEntry { expr: name.clone(), ty: *ty });
                    o.push_str(&e);
                    continue;
                }
                let key = ins_body[i] * 1000000 + id;
                let ty = local_types.get(&key).copied().unwrap_or(VType::Unknown);
                match ty {
                    VType::Unknown => {
                        vpush_cell(&mut e, &mut vstack, &mut vtmp,
                            &format!("cx->locals[cx->local_base+{}]", id));
                    }
                    VType::Float => {
                        vpush(&mut e, &mut vstack, &mut vtmp,
                            &format!("uf_f(cx->locals[cx->local_base+{}])", id), VType::Float);
                    }
                    VType::Int => {
                        vpush(&mut e, &mut vstack, &mut vtmp,
                            &format!("uf_i(cx->locals[cx->local_base+{}])", id), VType::Int);
                    }
                    VType::FloatArr => {
                        let slot_expr = format!("cx->locals[cx->local_base+{}]", id);
                        if arr_ptr.contains_key(&slot_expr) {
                            // Loop-hoisted pointer exists: push raw expression
                            // (not a temp) so vget's arr_ptr lookup matches
                            vstack.push(VEntry { expr: slot_expr, ty: VType::FloatArr });
                        } else {
                            vpush(&mut e, &mut vstack, &mut vtmp, &slot_expr, VType::FloatArr);
                        }
                    }
                }
            }
            // Unresolved locals (should have been resolved by resolve_locals)
            Ins::LocalSet(n) => panic!("unresolved local {} at pc {}", n, i),
            Ins::LocalGet(n) => panic!("unresolved local {} at pc {}", n, i),
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
                    let ins: Vec<String> = t.inputs.iter().map(|j| j.to_string()).collect();
                    e.push_str(&format!("int uf_wi{}[]={{{}}};\n", k, ins.join(",")));
                    e.push_str(&format!(
                        "uf_wt[{}]=(WeaveTask){{{}, {}, uf_wi{}, {}, {{0,0}}, 0, 0,0,0,0,0}};\n",
                        k,
                        resolve(&t.pc),
                        t.inputs.len(),
                        k,
                        t.count
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

// Pre-pass: determine local variable types by simulating the virtual stack.
// For each call body, tracks what type each LocalSetI stores. If all stores
// to a slot agree, the slot is typed. Conflicts → Unknown.
fn compute_local_types(p: &Parsed) -> (HashMap<usize, VType>, Vec<usize>, std::collections::HashSet<usize>) {
    // Determine call-entry labels (same logic as resolve_locals)
    let mut call_entries: std::collections::HashSet<usize> = std::collections::HashSet::new();
    call_entries.insert(0);
    if let Some(el) = &p.entry_label {
        if let Some(&pc) = p.labels.get(el) { call_entries.insert(pc); }
    }
    for ins in &p.ins {
        if let Ins::Call(l) = ins { if let Some(&pc) = p.labels.get(l) { call_entries.insert(pc); } }
    }
    for ins in &p.ins {
        if let Ins::Weave(tasks) = ins { for t in tasks { if let Some(&pc) = p.labels.get(&t.pc) { call_entries.insert(pc); } } }
    }
    for (_, l) in &p.exports { if let Some(&pc) = p.labels.get(l) { call_entries.insert(pc); } }

    let mut all_label_pcs: Vec<usize> = p.labels.values().copied().collect();
    all_label_pcs.push(0);
    all_label_pcs.sort();
    all_label_pcs.dedup();

    // Initial body assignment by position
    let mut label_body: HashMap<usize, usize> = HashMap::new();
    {
        let mut cur_body = 0usize;
        for &pc in &all_label_pcs {
            if call_entries.contains(&pc) { cur_body = pc; }
            label_body.insert(pc, cur_body);
        }
    }
    // Propagate through PushAddr
    let mut changed = true;
    while changed {
        changed = false;
        let ranges: Vec<(usize, usize)> = all_label_pcs.iter().copied().zip({
            let mut nexts = all_label_pcs.iter().copied().skip(1).collect::<Vec<_>>();
            nexts.push(p.ins.len());
            nexts.into_iter()
        }).collect();
        for &(lpc, end) in &ranges {
            let body = *label_body.get(&lpc).unwrap_or(&0);
            for i in lpc..end {
                if i >= p.ins.len() { break; }
                if let Ins::PushAddr(target) = &p.ins[i] {
                    if let Some(&target_pc) = p.labels.get(target) {
                        let target_body = *label_body.get(&target_pc).unwrap_or(&0);
                        if target_body != body { label_body.insert(target_pc, body); changed = true; }
                    }
                }
            }
        }
    }

    // Map instruction index to body
    let mut ins_body: Vec<usize> = vec![0usize; p.ins.len()];
    {
        let mut cur_body = 0usize;
        for i in 0..p.ins.len() {
            if all_label_pcs.contains(&i) {
                if let Some(&b) = label_body.get(&i) { cur_body = b; }
            }
            ins_body[i] = cur_body;
        }
    }

    // Continuation labels (PushAddr targets): while/if/for/quotation bodies
    // whose RET returns to a dispatcher that consumes exactly one value.
    // Their RET candidates must drop the top value — it never reaches the
    // function's caller.
    let mut cont_pcs: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for ins in &p.ins {
        if let Ins::PushAddr(t) = ins {
            if let Some(&pc) = p.labels.get(t) { cont_pcs.insert(pc); }
        }
    }

    // For each body, simulate the stack to infer types of LocalSetI operands.
    // Process ALL instructions in the body as one continuous stream — types
    // flow from one continuation label to the next within the same body.

    // Group instruction indices by body, preserving source order
    let mut body_to_ins: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..p.ins.len() {
        body_to_ins.entry(ins_body[i]).or_default().push(i);
    }
    let bodies: Vec<(usize, Vec<usize>)> = body_to_ins.into_iter().collect();
    // Sort by body start PC for deterministic fixed-point convergence
    let mut bodies = bodies;
    bodies.sort_by_key(|(b, _)| *b);

    // Function prologue: consecutive LocalSetI at a body start pop call
    // parameters (params[0] = top of caller stack, params[1] = next, ...).
    let mut prologue: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut prologue_pcs: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for (body, _) in &bodies {
        let mut params = Vec::new();
        let mut j = *body;
        while j < p.ins.len() {
            if let Ins::LocalSetI(id) = p.ins[j] {
                params.push(id);
                prologue_pcs.insert(j);
                j += 1;
            } else {
                break;
            }
        }
        prologue.insert(*body, params);
    }

    // Fixed-point iteration: repeat until no type changes.
    // Key is (body_start_pc, slot_id) — slots in different call frames
    // are independent and must not be merged.
    // The OUTER loop adds interprocedural propagation: call-site stack
    // snapshots seed callee parameter slots, and callee RET stacks type
    // the values a CALL leaves on the caller's stack.
    let mut result: HashMap<(usize, usize), VType> = HashMap::new();
    let mut ret_types: HashMap<usize, Vec<VType>> = HashMap::new();
    let mut numeric_slots: std::collections::HashSet<usize> = std::collections::HashSet::new();
    // conflicted: slots with irreconcilable provable store types (sticky).
    // seeded: parameter slots typed by call-site evidence.
    let mut conflicted: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    let mut seeded: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    let mut changed_outer = true;
    let mut outer_iter = 0;
    while changed_outer && outer_iter < 10 {
        outer_iter += 1;
        changed_outer = false;
        // Parameter slots without call-site evidence stay opaque: in-body
        // stores alone can never prove a parameter's type.
        let mut param_unseeded: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
        for (body, params) in &prologue {
            for &slot in params {
                if !seeded.contains(&(*body, slot)) {
                    param_unseeded.insert((*body, slot));
                }
            }
        }
        // Caller stack snapshots at each CALL (target body pc, stack before
        // the call) and per-body stacks at each RET — recorded during the
        // last inner pass and consumed below.
        let mut call_snaps: Vec<(usize, Vec<VType>)> = Vec::new();
        let mut ret_cands: HashMap<usize, Vec<Vec<VType>>> = HashMap::new();
        // Slots proven to hold only numeric (int/float) cells — no pointers,
        // so they are safe to keep in C locals across loop iterations.
        let mut round_numeric: std::collections::HashSet<usize> = std::collections::HashSet::new();
        let mut changed = true;
        let mut max_iter = 0;
        while changed && max_iter < 20 {
            max_iter += 1;
            changed = false;
            call_snaps.clear();
            ret_cands.clear();
            round_numeric.clear();
            for (body, indices) in &bodies {
            // Simulate with opacity tracking. A stack entry is (type, opaque):
            // "opaque" means the value came from a source the inference cannot
            // see through (globals, extern calls, unknown-ety arrays, stack
            // underflow, unseeded parameters). Storing an opaque value poisons
            // the slot for this round; a slot commits to a type only when ALL
            // its stores carry that exact provable type.
            let mut records: HashMap<usize, Vec<VType>> = HashMap::new();
            let mut poisoned: std::collections::HashSet<usize> = std::collections::HashSet::new();
            let mut type_stack: Vec<(VType, bool)> = Vec::new();
            let mut in_cont = false;

            for &i in indices {
                if call_entries.contains(&i) {
                    in_cont = false;
                } else if cont_pcs.contains(&i) {
                    in_cont = true;
                }
            match &p.ins[i] {
                Ins::PushI(_) => type_stack.push((VType::Int, false)),
                Ins::PushF(_) => type_stack.push((VType::Float, false)),
                Ins::PushS(_) | Ins::PushAddr(_) => type_stack.push((VType::Unknown, true)),
                Ins::Simple(h) => {
                    let bin = matches!(*h, "op_add"|"op_sub"|"op_mul"|"op_and"|"op_div"|"op_rem"|"op_lt"|"op_gt"|"op_eq"|"op_or"|"op_xor");
                    let un = matches!(*h, "op_shr"|"op_inc"|"op_dec"|"op_not");
                    if bin {
                        let b = type_stack.pop().unwrap_or((VType::Unknown, true));
                        let a = type_stack.pop().unwrap_or((VType::Unknown, true));
                        let rt = infer_bin_type(h, a.0, b.0);
                        type_stack.push((rt, rt == VType::Unknown && (a.1 || b.1)));
                    } else if un {
                        let a = type_stack.pop().unwrap_or((VType::Unknown, true));
                        // inc/dec preserve type; others return Int
                        let r = if matches!(*h, "op_inc"|"op_dec") { a.0 } else { VType::Int };
                        type_stack.push((r, r == VType::Unknown && a.1));
                    } else if matches!(*h, "op_dup") {
                        if let Some(&t) = type_stack.last() { type_stack.push(t); }
                    } else if matches!(*h, "op_ovr") {
                        if type_stack.len() >= 2 { type_stack.push(type_stack[type_stack.len()-2]); }
                    } else if matches!(*h, "op_drp") { type_stack.pop(); }
                    else if matches!(*h, "op_swp") {
                        let n = type_stack.len();
                        if n >= 2 { type_stack.swap(n-1, n-2); }
                    } else if matches!(*h, "op_vget"|"op_idx") {
                        type_stack.pop(); // index
                        // FloatArr handle (float-array local or propagated
                        // parameter) means vget yields a raw Float.
                        let hty = type_stack.pop().unwrap_or((VType::Unknown, true));
                        type_stack.push(if hty.0 == VType::FloatArr { (VType::Float, false) } else { (VType::Unknown, true) });
                    } else if matches!(*h, "op_vset"|"op_seti") {
                        type_stack.pop(); type_stack.pop(); type_stack.pop();
                    } else if matches!(*h, "op_atoi"|"op_len") {
                        // Result provably Int regardless of input
                        type_stack.pop();
                        type_stack.push((VType::Int, false));
                    } else if matches!(*h, "op_atof") {
                        type_stack.pop();
                        type_stack.push((VType::Float, false));
                    } else if matches!(*h, "op_get") {
                        type_stack.pop(); type_stack.pop();
                        type_stack.push((VType::Unknown, true));
                    } else if matches!(*h, "op_argv") {
                        type_stack.push((VType::Unknown, true));
                    } else {
                        // Unknown op: flush stack conservatively
                        type_stack.clear();
                    }
                }
                Ins::LocalSetI(id) => {
                    // Float-array handle pattern:
                    //   <len> PushI(1) SWP ARR LocalSetI(id)
                    // or <len> PushI(1) ARR LocalSetI(id)
                    let mut is_farr = false;
                    if i >= 3 {
                        if let (Ins::PushI(ty), Ins::Simple(h1), Ins::Simple(h2)) =
                            (&p.ins[i-3], &p.ins[i-2], &p.ins[i-1]) {
                            if *h1 == "op_swp" && *h2 == "op_arr" && *ty == 1 { is_farr = true; }
                        }
                    }
                    if !is_farr && i >= 2 {
                        if let (Ins::PushI(ty), Ins::Simple(h)) = (&p.ins[i-2], &p.ins[i-1]) {
                            if *h == "op_arr" && *ty == 1 { is_farr = true; }
                        }
                    }
                    if is_farr {
                        type_stack.pop();
                        records.entry(*id).or_default().push(VType::FloatArr);
                        continue;
                    }
                    match type_stack.pop() {
                        None => {
                            // Stack underflow: the value comes from outside the
                            // simulated stream. Prologue pops are the call
                            // parameters — seeded separately, so skip them.
                            if !prologue_pcs.contains(&i) {
                                poisoned.insert(*id);
                            }
                        }
                        Some((t, opq)) => {
                            if t != VType::Unknown {
                                records.entry(*id).or_default().push(t);
                            } else if opq {
                                poisoned.insert(*id);
                            }
                        }
                    }
                }
                Ins::LocalGetI(id) => {
                    if param_unseeded.contains(&(*body, *id)) {
                        // Unproven parameter: opaque
                        type_stack.push((VType::Unknown, true));
                    } else {
                        let ty = result.get(&(*body, *id)).copied().unwrap_or(VType::Unknown);
                        type_stack.push((ty, false));
                    }
                }
                Ins::SetV(_) => { type_stack.pop(); }
                Ins::GetV(_) => { type_stack.push((VType::Unknown, true)); }
                Ins::Ret => {
                    // Record the return-stack candidate for this body; the
                    // outer loop uses it to type what CALL leaves behind.
                    // Continuation RETs feed a dispatcher that consumes one
                    // value — drop the top.
                    let mut cand: Vec<VType> = type_stack.iter().map(|e| e.0).collect();
                    if in_cont && !cand.is_empty() { cand.pop(); }
                    ret_cands.entry(*body).or_default().push(cand);
                    type_stack.clear();
                }
                Ins::If | Ins::IfElse | Ins::While | Ins::For | Ins::Break | Ins::Cont | Ins::Goto(_) => {
                    type_stack.clear();
                }
                Ins::Call(l) => {
                    if let Some(&tpc) = p.labels.get(l) {
                        call_snaps.push((tpc, type_stack.iter().map(|e| e.0).collect()));
                        if let Some(rts) = ret_types.get(&tpc) {
                            // Model the call: callee pops its prologue params
                            // and pushes its (previously inferred) returns.
                            let np = prologue.get(&tpc).map(|v| v.len()).unwrap_or(0);
                            for _ in 0..np { type_stack.pop(); }
                            for &t in rts { type_stack.push((t, t == VType::Unknown)); }
                        } else {
                            // Callee returns not yet known — be conservative
                            type_stack.clear();
                        }
                    } else {
                        type_stack.clear();
                    }
                }
                Ins::CallExt(_) => {
                    type_stack.clear();
                }
                Ins::Sys(_) | Ins::Weave(_) | Ins::Send => { type_stack.clear(); }
                Ins::Extern(_) => { type_stack.push((VType::Unknown, true)); }
                Ins::LocalSet(_) | Ins::LocalGet(_) => { type_stack.push((VType::Unknown, true)); }
            }
        }

            // Commit slot types from this round's evidence. A slot commits
            // only when every store carries the same provable type; opaque
            // stores block commitment; conflicting provable types poison the
            // slot permanently.
            let mut slots: std::collections::HashSet<usize> = records.keys().copied().collect();
            slots.extend(poisoned.iter().copied());
            for slot in slots {
                let key = (*body, slot);
                if conflicted.contains(&key) { continue; }
                if param_unseeded.contains(&key) { continue; } // params: seeds only
                let known: Vec<VType> = records.get(&slot).cloned().unwrap_or_default();
                if known.is_empty() { continue; } // fully opaque or untouched
                if !poisoned.contains(&slot)
                    && known.iter().all(|&t| t == VType::Int || t == VType::Float)
                {
                    round_numeric.insert(*body * 1000000 + slot);
                }
                // When all stores are numeric (Int and/or Float), promote to
                // Float if any Float is present — Int→Float is lossless, so a
                // slot that receives both is provably Float. Only non-numeric
                // mixing is a genuine conflict.
                let all_numeric = known.iter().all(|&t| t == VType::Int || t == VType::Float);
                if !all_numeric && known.iter().any(|&t| t != known[0]) {
                    // Genuine non-numeric type conflict — permanently Unknown
                    conflicted.insert(key);
                    if result.get(&key).copied() != Some(VType::Unknown) {
                        result.insert(key, VType::Unknown);
                        changed = true;
                    }
                    continue;
                }
                let resolved = if all_numeric && known.iter().any(|&t| t == VType::Float) {
                    VType::Float
                } else {
                    known[0]
                };
                if poisoned.contains(&slot) { continue; } // opaque store present
                // Merge with existing type: numeric types merge to Float
                let cur = result.get(&key).copied();
                let merged = match (cur, resolved) {
                    (None, _) | (Some(VType::Unknown), _) => Some(resolved),
                    (Some(a), b) if a == b => Some(a),
                    (Some(VType::Int), VType::Float) | (Some(VType::Float), VType::Int) => Some(VType::Float),
                    _ => None,
                };
                match merged {
                    None => {
                        conflicted.insert(key);
                        result.insert(key, VType::Unknown);
                        changed = true;
                    }
                    Some(m) if cur == Some(m) => {}
                    Some(m) => {
                        result.insert(key, m);
                        changed = true;
                    }
                }
            }
        }
        }

        // Finalize return-stack types. Dispatcher (continuation) returns are
        // always shorter than real function returns, so merge only the
        // max-length candidates; per position, a known type beats Unknown,
        // two different known types conflict back to Unknown.
        let mut new_ret_types: HashMap<usize, Vec<VType>> = HashMap::new();
        for (body, cands) in &ret_cands {
            if cands.is_empty() { continue; }
            let nvals = cands.iter().map(|c| c.len()).max().unwrap();
            let top: Vec<&Vec<VType>> = cands.iter().filter(|c| c.len() == nvals).collect();
            let mut rt = vec![VType::Unknown; nvals];
            for c in top {
                for k in 0..nvals {
                    if c[k] != VType::Unknown {
                        if rt[k] == VType::Unknown { rt[k] = c[k]; }
                        else if rt[k] != c[k] { rt[k] = VType::Unknown; }
                    }
                }
            }
            new_ret_types.insert(*body, rt);
        }
        if new_ret_types != ret_types {
            ret_types = new_ret_types;
            changed_outer = true;
        }

        // Seed callee parameter slots from call-site stack snapshots.
        // Conflicting call sites poison the slot (left Unknown).
        let mut seeds: HashMap<(usize, usize), VType> = HashMap::new();
        let mut poison: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
        for (tpc, snap) in &call_snaps {
            if let Some(params) = prologue.get(tpc) {
                for (k, &slot) in params.iter().enumerate() {
                    let ty = if snap.len() > k { snap[snap.len() - 1 - k] } else { VType::Unknown };
                    if ty == VType::Unknown { continue; }
                    let key = (*tpc, slot);
                    if poison.contains(&key) { continue; }
                    match seeds.get(&key) {
                        None => { seeds.insert(key, ty); }
                        Some(&prev) if prev == ty => {}
                        _ => { seeds.remove(&key); poison.insert(key); }
                    }
                }
            }
        }
        for (key, ty) in seeds {
            if conflicted.contains(&key) { continue; }
            let cur = result.get(&key).copied().unwrap_or(VType::Unknown);
            if cur == VType::Unknown {
                result.insert(key, ty);
                seeded.insert(key);
                changed_outer = true;
            }
        }
        numeric_slots = round_numeric;
    }

    if std::env::var("UF_DEBUG_TYPES").is_ok() {
        let mut dbg: Vec<_> = result.iter().collect();
        dbg.sort_by_key(|(k, _)| *k);
        for ((b, s), t) in dbg {
            eprintln!("TYPE body={} slot={} {:?}", b, s, t);
        }
        eprintln!("prologue: {:?}", prologue);
        eprintln!("conflicted: {:?}", conflicted);
        eprintln!("ret_types: {:?}", ret_types);
        eprintln!("seeded: {:?}", seeded);
    }

    (
        result.into_iter().map(|((b, s), t)| (b * 1000000 + s, t)).collect(),
        ins_body,
        numeric_slots,
    )
}

pub fn gen(p: &Parsed, structs: &StructMap) -> String {
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
        let mut foffs = Vec::new();
        for (i, (_, v)) in by_sid.iter().enumerate() {
            o.push_str(&format!(
                "static const char* uf_f_{}[]={{{}}};\n",
                i,
                v.0.iter().map(|(n, _)| format!("\"{}\"", n)).collect::<Vec<_>>().join(",")
            ));
            o.push_str(&format!(
                "static const int64_t uf_o_{}[]={{{}}};\n",
                i,
                v.0.iter().map(|(_, off)| off.to_string()).collect::<Vec<_>>().join(",")
            ));
            fnames.push(format!("uf_f_{}", i));
            foffs.push(format!("uf_o_{}", i));
        }
        o.push_str(&format!(
            "static const char** uf_fields_v[]={{{}}};\nstatic const int64_t* uf_offs_v[]={{{}}};\nstatic void uf_init_reflection(void){{ uf_st_n={}; uf_st_sids=uf_sids_v; uf_st_nf=uf_nf_v; uf_st_fields=uf_fields_v; uf_st_offs=uf_offs_v; }}\n",
            fnames.join(","),
            foffs.join(","),
            by_sid.len()
        ));
    } else {
        o.push_str("static void uf_init_reflection(void){}\n");
    }
    // string literals: GC-registered tag-9 str objects (pinned), so every
    // string in the language is a first-class str handle
    for (i, s) in p.strings.iter().enumerate() {
        let blen = s.len();
        o.push_str(&format!(
            "static struct {{ void* gc_next; void* gc_parent; uint64_t gc_flags; uint64_t tag; uint64_t len; uint64_t esz; uint64_t ety; uint64_t mlen; const char* mdata; char d[{}]; }} uf_sl{} = {{0,0,0,9,{},1,0,0,0,\"{}\"}};\n",
            blen + 1,
            i,
            blen,
            c_escape(s)
        ));
    }
    if !p.strings.is_empty() {
        o.push_str(&format!(
            "static void* uf_lits[] = {{{}}};\n",
            (0..p.strings.len()).map(|i| format!("(void*)&uf_sl{}", i)).collect::<Vec<_>>().join(",")
        ));
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
    // GC roots: all variables are precise roots
    if !p.vars.is_empty() {
        o.push_str(&format!(
            "static Cell* uf_vroots[] = {{{}}};\n",
            p.vars.iter().map(|v| format!("&var_{}", v)).collect::<Vec<_>>().join(",")
        ));
    }
    // v11: per-label local frame sizes. We emit a flat array indexed by
    // instruction PC (sparse, but simple and fast). uf_local_counts[pc] gives
    // the frame size for the call-entry label starting at pc.
    let n_ins = p.ins.len();
    o.push_str(&format!("static long uf_lc_v[{}];\n", n_ins + 1));
    if !p.local_counts.is_empty() {
        o.push_str("static void uf_init_locals(void){");
        for (&pc, &count) in &p.local_counts {
            o.push_str(&format!("uf_lc_v[{}]={};", pc, count));
        }
        o.push_str("}\n");
    } else {
        o.push_str("static void uf_init_locals(void){}\n");
    }
    o.push_str("static long uf_lc(long pc){ return (pc>=0&&(unsigned long)pc<(unsigned long)(sizeof(uf_lc_v)/sizeof(uf_lc_v[0])))?uf_lc_v[pc]:0; }\n");
    // label -> instruction index
    let resolve = |name: &str| -> usize {
        *p.labels.get(name).unwrap_or_else(|| panic!("undefined label {}", name))
    };
    o.push_str("\nstatic void uflux_run(Ctx*cx, long pc){\n  if(pc<0){ goto *(void*)uf_entry_addr; }\n  /* v11: set up the entry label's local frame */\n  cx->local_frames[cx->local_fsp++]=cx->local_base; cx->local_base+=uf_lc(pc);\n");
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
    // Add init-TU entry points to dyn_idx so their labels are in labtab
    for &ipc in &p.init_pcs {
        dyn_idx.insert(ipc);
    }
    o.push_str("  static const void* labtab[] = {");
    for &i in &dyn_idx {
        o.push_str(&format!("[{}]=&&L_{},", i, i));
    }
    if dyn_idx.is_empty() {
        o.push_str("[0]=&&L_0,");
    }
    o.push_str("};\n");
    // init-TU spawns: each init.uf entry point gets a detached pthread,
    // spawned exactly once when main_cx enters at pc 0. We run this before
    // the labtab dispatch so it executes on the initial call with pc=0.
    if !p.init_pcs.is_empty() {
        o.push_str("  if(pc==0 && cx==main_cx) {");
        for &ipc in &p.init_pcs {
            o.push_str(&format!(
                "{{ pthread_t th; if(pthread_create(&th,0,uf_init_worker,(void*)&&L_{})) die(\"init thread\"); pthread_detach(th); }}",
                ipc
            ));
        }
        o.push_str("  }\n");
    }
    // ENTRY: if the program has an entry label, pc 0 jumps to it (replaces jmp main)
    if let Some(el) = &p.entry_label {
        let eidx = resolve(el);
        o.push_str(&format!("  if(pc==0) goto L_{};\n", eidx));
    }
    o.push_str("  goto *labtab[pc];\n");
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
    let mut inline_ffolds: HashMap<usize, (usize, usize)> = HashMap::new();
    let mut inline_whiles: HashMap<usize, (usize, usize, usize, usize)> = HashMap::new();
    let mut inline_calls: HashMap<usize, (usize, usize)> = HashMap::new();
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
        // FFOLD inlining: detect PushAddr(label), Simple("op_ffold")
        if let (Ins::PushAddr(l), Ins::Simple(h)) = (&p.ins[j - 1], &p.ins[j]) {
            if *h == "op_ffold" && !targets.contains(&(j - 1)) {
                let bs = resolve(l);
                if let Some(be) = for_body_range(&p.ins, bs) {
                    if inlinable_for(p, bs, be) {
                        inline_ffolds.insert(j, (bs, be));
                        suppress.insert(j - 1);
                    }
                }
            }
            // FSPLIT inlining: same pattern as FFOLD
            if *h == "op_fsplit" && !targets.contains(&(j - 1)) {
                let bs = resolve(l);
                if let Some(be) = for_body_range(&p.ins, bs) {
                    if inlinable_for(p, bs, be) {
                        inline_ffolds.insert(j, (bs, be)); // reuse the same map
                        suppress.insert(j - 1);
                    }
                }
            }
        }
        // CALL inlining: detect Ins::Call(label) where body is inlinable.
        // v11: skip inlining if the target body (or any PushAddr continuation
        // within it) uses local variables — the inlined copy's local_base
        // offset would mismatch the original's PushAddr continuation labels.
        if let Ins::Call(l) = &p.ins[j] {
            if !targets.contains(&j) {
                let bs = resolve(l);
                if let Some(be) = for_body_range(&p.ins, bs) {
                    if inlinable_for(p, bs, be) {
                        let has_locals = (bs..be).any(|k| matches!(p.ins[k], Ins::LocalSet(_) | Ins::LocalGet(_) | Ins::LocalSetI(_) | Ins::LocalGetI(_)));
                        if !has_locals {
                            inline_calls.insert(j, (bs, be));
                        }
                    }
                }
            }
        }
        // IR order: PushAddr(cond_label) at j-2, PushAddr(body_label) at j-1
        if j >= 2 {
            if let (Ins::PushAddr(cl), Ins::PushAddr(bl), Ins::While) = (&p.ins[j - 2], &p.ins[j - 1], &p.ins[j]) {
                if !targets.contains(&(j - 2)) && !targets.contains(&(j - 1)) {
                    let bbs = resolve(bl);
                    let cbs = resolve(cl);
                    if let (Some(bbe), Some(cbe)) = (for_body_range(&p.ins, bbs), for_body_range(&p.ins, cbs)) {
                        if inlinable_for(p, bbs, bbe) && inlinable_for(p, cbs, cbe) {
                            inline_whiles.insert(j, (bbs, bbe, cbs, cbe));
                            suppress.insert(j - 2);
                            suppress.insert(j - 1);
                        }
                    }
                }
            }
        }
    }
    // Compute types and body mapping BEFORE outlined-body detection so we
    // can find the full extent of each call body (including continuation labels).
    let (mut local_types, ins_body, numeric_slots) = compute_local_types(p);
    
    // Outlined call bodies: detect call bodies that use locals and contain
    // while loops but don't call other uf bodies (only externs). These are
    // emitted as separate C functions so GCC's register allocator handles
    // the hot inner loops without being overwhelmed by the monolithic
    // dispatch function's register pressure.
    let mut outlined_bodies: HashMap<usize, (usize, usize)> = HashMap::new(); // call_pc -> (body_start, body_end)
    let mut outlined_emitted: std::collections::HashSet<usize> = std::collections::HashSet::new(); // body_starts already emitted
    for j in 1..p.ins.len() {
        if let Ins::Call(l) = &p.ins[j] {
            let bs = resolve(l);
            if outlined_emitted.contains(&bs) { continue; }
            // Find the full body extent using ins_body: all instructions k
            // where ins_body[k] == bs form the body (including continuation
            // labels like while-cond/while-body that belong to this body).
            let be = {
                let mut end = bs + 1;
                while end < p.ins.len() && ins_body[end] == bs { end += 1; }
                end
            };
            // Must use locals
            let has_locals = (bs..be).any(|k| matches!(p.ins[k], Ins::LocalSetI(_) | Ins::LocalGetI(_)));
            if !has_locals { continue; }
            // Must contain at least one while loop (the hot pattern)
            let has_while = (bs..be).any(|k| matches!(p.ins[k], Ins::While));
            if !has_while { continue; }
            // Must not call other uf bodies (only externs/simple ops)
            let calls_others = (bs..be).any(|k| matches!(&p.ins[k], Ins::Call(_)));
            if calls_others { continue; }
            // Must not have unsuppressed PushAddr (inlined-while PushAddrs are
            // suppressed and fine — they never use the call-stack mechanism)
            let has_pushaddr = (bs..be).any(|k| matches!(&p.ins[k], Ins::PushAddr(_)) && !suppress.contains(&k));
            if has_pushaddr { continue; }
            outlined_bodies.insert(j, (bs, be));
            outlined_emitted.insert(bs);
        }
    }

    // Emit outlined body functions BEFORE uflux_run
    let mut outlined_fns = String::new();
    for (&_call_pc, &(bs, be)) in &outlined_bodies {
        let fname = format!("uf_ob_{}", bs);
        let lc = p.local_counts.get(&bs).copied().unwrap_or(0);
        outlined_fns.push_str(&format!(
            "static void {}(Ctx*cx){{cx->local_frames[cx->local_fsp++]=cx->local_base;cx->local_base+={};\n",
            fname, lc
        ));
        let ob_prefix = format!("OB{}_", bs);
        // Emit the body code — use empty reg/arr_ptr maps; the inlined while
        // loops within the body will do their own register caching.
        emit_range(&mut outlined_fns, p, &targets, &inline_fors, &inline_ffolds, &inline_whiles, &inline_calls, &outlined_bodies, &suppress, &ext_idx, bs, be, &ob_prefix, 0, &mut local_types, &ins_body, &HashMap::new(), &numeric_slots, &HashMap::new());
        // If the body has no explicit RET at the end (shouldn't happen, but
        // be safe), restore the frame.
        outlined_fns.push_str("cx->local_base=cx->local_frames[--cx->local_fsp];}\n");
    }
    // Insert outlined functions after uf_lc but before uflux_run's body
    if !outlined_fns.is_empty() {
        // Find the uflux_run function definition (not the forward declaration).
        // The forward declaration is "static void uflux_run(Ctx*cx, long pc);"
        // while the definition is "static void uflux_run(Ctx*cx, long pc){".
        let search = "static void uflux_run(Ctx*cx, long pc){";
        let insert_pos = o.find(search).unwrap_or_else(|| {
            // Fallback: find the last occurrence of uflux_run
            o.rfind("static void uflux_run").unwrap_or(o.len())
        });
        o.insert_str(insert_pos, &outlined_fns);
    }
    emit_range(&mut o, p, &targets, &inline_fors, &inline_ffolds, &inline_whiles, &inline_calls, &outlined_bodies, &suppress, &ext_idx, 0, n, "", 0, &mut local_types, &ins_body, &HashMap::new(), &numeric_slots, &HashMap::new());
    o.push_str(&format!("L_{}: return;\n}}\n", n));

    // exported wrappers (fixed 4-arg C ABI trampoline, run on the main ctx)
    for (cname, label) in &p.exports {
        let lidx = resolve(label);
        o.push_str(&format!(
            "uint64_t {}(uint64_t a0,uint64_t a1,uint64_t a2,uint64_t a3){{Ctx*cx=main_cx;long base=cx->sp;long _lb=cx->local_base;pushp(cx,(void*)a0);pushp(cx,(void*)a1);pushp(cx,(void*)a2);pushp(cx,(void*)a3);uflux_run(cx,{});uint64_t r=(cx->sp>base)?(uint64_t)pop(cx).i:0;cx->sp=base;cx->local_base=_lb;return r;}}\n",
            cname, lidx
        ));
    }
    let lits_arg = if p.strings.is_empty() { "0,0".to_string() } else { format!("uf_lits,{}", p.strings.len()) };
    let roots_arg = if p.vars.is_empty() { "0,0".to_string() } else { format!("uf_vroots,{}", p.vars.len()) };
    o.push_str(&format!("int main(int argc,char**argv){{uf_argc=argc;uf_argv=(void*)argv;uf_init_reflection();uf_init_locals();uf_init_lits({});uf_gc_setroots({});uf_gc_init();uflux_run(main_cx,0);return 0;}}\n", lits_arg, roots_arg));
    o
}

pub fn gen_call_ext(im: &Import, sym: &str) -> String {
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

pub fn arg_cast(ct: &str, var: &str) -> String {
    match ct {
        "int64_t" => format!("(int64_t)({0}.tag==T_FLOAT?(int64_t)uf_f({0}):{0}.i)", var),
        "double" => format!("uf_f({})", var),
        "void*" => format!("(void*)uf_sptr({})", var),
        "char" => format!("(char){}.i", var),
        _ => var.to_string(),
    }
}

pub fn ret_push(ret: &str, var: &str) -> String {
    match ret {
        "int" => format!("pushi(cx,(int64_t){});", var),
        "float" => format!("pushf(cx,{});", var),
        "ptr" | "handle" => format!("pushp(cx,{});", var),
        "byte" => format!("pushi(cx,(int64_t){});", var),
        _ => String::new(),
    }
}

// ---------------- driver ----------------
