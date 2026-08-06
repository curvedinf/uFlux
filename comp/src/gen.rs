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

// ---------------- basic-block stack virtualization (used by gen) ----------
// Within a straight-line block (between jump targets and control-flow
// instructions), stack pushes/pops become C locals instead of traffic on the
// runtime ds. The virtual stack is flushed (spilled to the real ds, in
// order) before any instruction that is a jump target, transfers control, or
// otherwise needs the real stack — so the observable ds state at every block
// boundary is exactly the naive one. All fused operations go through the
// same uf_c* helpers the op_* functions use, so tag/float semantics are
// unchanged.
pub fn vpop(e: &mut String, vs: &mut Vec<String>, n: &mut usize) -> String {
    if let Some(t) = vs.pop() {
        t
    } else {
        let t = format!("t{}", *n);
        *n += 1;
        e.push_str(&format!("Cell {}=pop(cx);", t));
        t
    }
}
pub fn vpush(e: &mut String, vs: &mut Vec<String>, n: &mut usize, init: String) -> String {
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
pub fn vflush(e: &mut String, vs: &mut Vec<String>, vc: &mut Vec<(String, String, bool)>) {
    for (v, t, d) in vc.drain(..) {
        if d {
            e.push_str(&format!("var_{}={};", v, t));
        }
    }
    for t in vs.drain(..) {
        e.push_str(&format!("pushc(cx,{});", t));
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
                vpush(&mut e, &mut vstack, &mut vtmp, format!("uf_mkp((void*)&uf_sl{})", idx));
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
                            let is_inlined_ffold = *h == "op_ffold" && depth < 8 && inline_ffolds.contains_key(&i);
                            if is_inlined_ffold {
                                if let Some((bs, be)) = inline_ffolds.get(&i).copied() {
                                    vflush(&mut e, &mut vstack, &mut vcache);
                                    e.push_str("{Cell _ff_acc=pop(cx),_ff_p=pop(cx);FILE*_fp=fopen(uf_sptr(_ff_p),\"r\");if(!_fp)die(\"FFOLD: cannot open file\");char*_line=0;size_t _ncap=0;ssize_t m;long fr=cx->lsp++;if(cx->lsp>=64)die(\"loops nested too deep\");cx->loops[fr].cspl=cx->csp;cx->loops[fr].cont=&&K_FF_C_");
                                    e.push_str(&format!("{}{};cx->loops[fr].end=&&K_FF_E_{}{};while((m=getline(&_line,&_ncap,_fp))>=0){{while(m>0&&(_line[m-1]=='\\n'||_line[m-1]=='\\r'))_line[--m]=0;Cell _ls=uf_str_new(_line,(size_t)m);pushc(cx,_ff_acc);pushc(cx,_ls);\n", prefix, i, prefix, i));
                                    let inner = format!("{}FF{}_", prefix, i);
                                    emit_range(&mut e, p, targets, inline_fors, inline_ffolds, suppress, ext_idx, bs, be, &inner, depth + 1);
                                    e.push_str(&format!("K_FF_C_{}{}:;_ff_acc=pop(cx);}}K_FF_E_{}{}:;cx->lsp=fr;free(_line);fclose(_fp);pushc(cx,_ff_acc);}}\n", prefix, i, prefix, i));
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
                        emit_range(&mut e, p, targets, inline_fors, inline_ffolds, suppress, ext_idx, bs, be, &inner, depth + 1);
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
                // dynamic loop frame so BREAK/CONT (possibly nested in CALLed
                // quotations) can unwind to this loop's end/continue points
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
    // label -> instruction index
    let resolve = |name: &str| -> usize {
        *p.labels.get(name).unwrap_or_else(|| panic!("undefined label {}", name))
    };
    o.push_str("\nstatic void uflux_run(Ctx*cx, long pc){\n  if(pc<0){ goto *(void*)uf_entry_addr; }\n");
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
        }
    }
    emit_range(&mut o, p, &targets, &inline_fors, &inline_ffolds, &suppress, &ext_idx, 0, n, "", 0);
    o.push_str(&format!("L_{}: return;\n}}\n", n));

    // exported wrappers (fixed 4-arg C ABI trampoline, run on the main ctx)
    for (cname, label) in &p.exports {
        let lidx = resolve(label);
        o.push_str(&format!(
            "uint64_t {}(uint64_t a0,uint64_t a1,uint64_t a2,uint64_t a3){{Ctx*cx=main_cx;long base=cx->sp;pushp(cx,(void*)a0);pushp(cx,(void*)a1);pushp(cx,(void*)a2);pushp(cx,(void*)a3);cx->cs[cx->csp++]=0;uflux_run(cx,{});uint64_t r=(cx->sp>base)?(uint64_t)pop(cx).i:0;cx->sp=base;return r;}}\n",
            cname, lidx
        ));
    }
    let lits_arg = if p.strings.is_empty() { "0,0".to_string() } else { format!("uf_lits,{}", p.strings.len()) };
    let roots_arg = if p.vars.is_empty() { "0,0".to_string() } else { format!("uf_vroots,{}", p.vars.len()) };
    o.push_str(&format!("int main(int argc,char**argv){{uf_argc=argc;uf_argv=(void*)argv;uf_init_reflection();uf_init_lits({});uf_gc_setroots({});uf_gc_init();uflux_run(main_cx,0);return 0;}}\n", lits_arg, roots_arg));
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
