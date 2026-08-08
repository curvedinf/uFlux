use crate::ast::*;
use crate::lex::*;
use std::collections::{HashMap, VecDeque};

// FNV-1a 64 over bytes (matches uf_fnv in the C prelude)
pub fn fnv64(s: &str) -> i64 {
    let mut h: u64 = 1469598103934665603;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    h as i64
}

// method dispatch type key: struct names -> 1000+sid, container names -> HT_*,
// scalar type keywords -> 0..3
pub fn method_typekey(name: &str, structs: &StructMap) -> Option<i64> {
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

pub fn parse(toks: Vec<Tok>, structs: &mut StructMap) -> Parsed {
    let mut macros: HashMap<String, Vec<Tok>> = HashMap::new();
    let imports: Vec<Import> = Vec::new();
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
        init_pcs: Vec::new(),
        entry_label: None,
        local_counts: HashMap::new(),
        local_names: HashMap::new(),
    };
    let mut q: VecDeque<Tok> = toks.into();
    let mut pending_export: Option<String> = None;
    let mut pending_pub = false;
    let mut pending_method: Option<String> = None;
    let mut weave_ctr = 0usize;
    let mut cur_weave: Vec<(String, Vec<String>, i64)> = Vec::new();
    let mut expand_depth = 0usize;
    while let Some(t) = q.pop_front() {
        match t {
            Tok::Op("RET") => p.ins.push(Ins::Ret),
            Tok::Op("FOR") => p.ins.push(Ins::For),
            Tok::Op("IF") => p.ins.push(Ins::If),
            Tok::Op("IFELSE") => p.ins.push(Ins::IfElse),
            Tok::Op("WHILE") => p.ins.push(Ins::While),
            Tok::Op("BREAK") => p.ins.push(Ins::Break),
            Tok::Op("CONT") => p.ins.push(Ins::Cont),
            Tok::Op(name) if name.starts_with('~') => {
                panic!("opcode index {} is retired in v10", name)
            }
            Tok::Op(name) => p.ins.push(simple_ins(name)),
            Tok::PushI(v) => p.ins.push(Ins::PushI(v)),
            Tok::PushF(v) => p.ins.push(Ins::PushF(v)),
            Tok::PushS(s) => {
                let idx = p.strings.len();
                p.strings.push(s);
                p.ins.push(Ins::PushS(idx));
            }
            Tok::Jump(op, label) => match op {
                "ADDR" => p.ins.push(Ins::PushAddr(label)),
                "CALL" => {
                    if let Some(ii) = p.imports.iter().position(|im| im.name == label) {
                        p.ins.push(Ins::CallExt(ii));
                    } else {
                        p.ins.push(Ins::Call(label));
                    }
                }
                "JMP" => panic!("jmp removed — use entry/if/while/for"),
                "JZ" => panic!("jz removed — use if/ifelse/while"),
                "JE" => panic!("je removed — use if/ifelse"),
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
            Tok::LocalSet(n) => {
                p.ins.push(Ins::LocalSet(n));
            }
            Tok::LocalGet(n) => {
                p.ins.push(Ins::LocalGet(n));
            }
            Tok::IncLocal(n) => {
                p.ins.push(Ins::PushI(1));
                p.ins.push(Ins::LocalGet(n.clone()));
                p.ins.push(simple_ins("ADD"));
                p.ins.push(Ins::LocalSet(n));
            }
            Tok::AddLocal(n) => {
                p.ins.push(Ins::LocalGet(n.clone()));
                p.ins.push(simple_ins("ADD"));
                p.ins.push(Ins::LocalSet(n));
            }
            Tok::IncGlobal(n) => {
                p.ins.push(Ins::PushI(1));
                p.ins.push(Ins::GetV(n.clone()));
                p.ins.push(simple_ins("ADD"));
                p.ins.push(Ins::SetV(n));
            }
            Tok::AddGlobal(n) => {
                p.ins.push(Ins::GetV(n.clone()));
                p.ins.push(simple_ins("ADD"));
                p.ins.push(Ins::SetV(n));
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
            Tok::Task { name, inputs, count, body } => {
                // v10 fanout: count literal 1..64; count > 1 requires exactly
                // one input (checked at WRUN where the input is resolved)
                if let Some(c) = count {
                    if !(1..=64).contains(&c) {
                        panic!("WEAVE: task {} worker count {} out of range 1..64", name, c);
                    }
                }
                let skip = format!("__wskip{}", weave_ctr);
                weave_ctr += 1;
                p.ins.push(Ins::Goto(skip.clone()));
                p.labels.insert(name.clone(), p.ins.len());
                // task bodies are self-contained: their labels are task-local,
                // so two tasks may reuse the same v-names
                let prefix = format!("{}/", name);
                let body = prefix_task_labels(body, &prefix);
                q.push_front(Tok::TaskEnd(skip));
                for t in body.into_iter().rev() {
                    q.push_front(t);
                }
                cur_weave.push((name, inputs, count.unwrap_or(1)));
            }
            Tok::TaskEnd(skip) => {
                p.ins.push(Ins::Ret);
                p.labels.insert(skip, p.ins.len());
            }
            Tok::Wrun => {
                // static DAG checks: inputs must name tasks in this weave; acyclic
                let names: Vec<&String> = cur_weave.iter().map(|(n, _, _)| n).collect();
                let mut metas: Vec<WeaveMeta> = Vec::new();
                for (n, ins, count) in &cur_weave {
                    let mut idxs = Vec::new();
                    for inp in ins {
                        match names.iter().position(|x| *x == inp) {
                            Some(i) => idxs.push(i),
                            None => panic!("WEAVE: task {} has unknown input {}", n, inp),
                        }
                    }
                    if *count > 1 && idxs.is_empty() {
                        panic!("WEAVE: fanout task {} ({} workers) must have at least one input", n, count);
                    }
                    metas.push(WeaveMeta { name: n.clone(), pc: n.clone(), inputs: idxs, count: *count });
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
            Tok::Entry => {
                p.labels.insert("entry".to_string(), p.ins.len());
                p.entry_label = Some("entry".to_string());
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
    // v10: break/cont outside a loop are compile errors. A "function" here is
    // the instruction region starting at a label used as a code address
    // (CALL/ADDR/weave task entry); a loop is a FOR/WHILE whose body address
    // is a compile-time-known PushAddr. break/cont must textually appear in a
    // loop body (nested quotation bodies have their own labels, so nested
    // loops resolve to the innermost body as required).
    {
        let mut entries: std::collections::HashSet<usize> = std::collections::HashSet::new();
        let mut loop_bodies: std::collections::HashSet<usize> = std::collections::HashSet::new();
        let resolve = |l: &String| -> Option<usize> { p.labels.get(l).copied() };
        for (i, ins) in p.ins.iter().enumerate() {
            match ins {
                Ins::Call(l) => {
                    if let Some(t) = resolve(l) {
                        entries.insert(t);
                    }
                }
                Ins::PushAddr(l) => {
                    // A PushAddr that directly feeds a control-flow op
                    // (if/ifelse/while/for) is a continuation of the current
                    // body, not a new function entry: it runs in the caller's
                    // frame, so break/cont inside it bind to the caller's
                    // loop. Everything else (stored/computed addresses)
                    // starts a new entry.
                    let feeds_cf = match p.ins.get(i + 1) {
                        Some(Ins::If) | Some(Ins::IfElse) | Some(Ins::For) | Some(Ins::While) => true,
                        Some(Ins::PushAddr(_)) => {
                            matches!(p.ins.get(i + 2), Some(Ins::While) | Some(Ins::IfElse))
                        }
                        _ => false,
                    };
                    if !feeds_cf {
                        if let Some(t) = resolve(l) {
                            entries.insert(t);
                        }
                    }
                }
                Ins::Weave(ms) => {
                    for m in ms {
                        if let Some(t) = resolve(&m.pc) {
                            entries.insert(t);
                        }
                    }
                }
                Ins::For => {
                    if i > 0 {
                        if let Ins::PushAddr(l) = &p.ins[i - 1] {
                            if let Some(t) = resolve(l) {
                                loop_bodies.insert(t);
                            }
                        }
                    }
                }
                Ins::While => {
                    if i > 0 {
                        if let Ins::PushAddr(l) = &p.ins[i - 1] {
                            if let Some(t) = resolve(l) {
                                loop_bodies.insert(t);
                            }
                        }
                    }
                    if i > 1 {
                        if let Ins::PushAddr(l) = &p.ins[i - 2] {
                            if let Some(t) = resolve(l) {
                                loop_bodies.insert(t);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        let mut cur_fn: Option<usize> = None;
        for (i, ins) in p.ins.iter().enumerate() {
            if entries.contains(&i) || loop_bodies.contains(&i) {
                cur_fn = Some(i);
            }
            if matches!(ins, Ins::Break | Ins::Cont) {
                let ok = match cur_fn {
                    Some(f) => loop_bodies.contains(&f),
                    None => false,
                };
                if !ok {
                    panic!("{} outside a loop (compile error)",
                        if matches!(ins, Ins::Break) { "break" } else { "cont" });
                }
            }
        }
    }
    // Note: resolve_locals is deferred to merge_tus, where all TU offsets
    // are finalized. Calling it here would produce per-TU slot IDs and frame
    // sizes that are invalidated when instructions are offset during merge.
    p
}

// v11: resolve implicit local variables into per-call-body slot IDs.
//
// A "call body" is a group of labels that share a single local-variable frame.
// Each CALL target (or the entry label / pc 0) starts a new call body. Labels
// reached via PushAddr (if/while/for bodies) are continuations that share the
// frame of the call body that references them.
//
// To determine ownership, we use a worklist algorithm: starting from each
// call-entry label, we propagate the body ID to all PushAddr targets within
// the same body's instruction range. This correctly handles continuation
// labels that appear after other call entries in the instruction stream.
pub fn resolve_locals(p: &mut Parsed) {
    if p.ins.is_empty() {
        return;
    }
    // Determine which labels are "call entries" (get their own frame).
    let mut call_entries: std::collections::HashSet<usize> = std::collections::HashSet::new();
    call_entries.insert(0);
    if let Some(el) = &p.entry_label {
        if let Some(&pc) = p.labels.get(el) {
            call_entries.insert(pc);
        }
    }
    for ins in &p.ins {
        if let Ins::Call(l) = ins {
            if let Some(&pc) = p.labels.get(l) {
                call_entries.insert(pc);
            }
        }
    }
    for ins in &p.ins {
        if let Ins::Weave(tasks) = ins {
            for t in tasks {
                if let Some(&pc) = p.labels.get(&t.pc) {
                    call_entries.insert(pc);
                }
            }
        }
    }
    for (_, l) in &p.exports {
        if let Some(&pc) = p.labels.get(l) {
            call_entries.insert(pc);
        }
    }

    // Build a sorted list of label PCs to delimit label regions.
    // Always include pc 0 (the implicit entry point) even if there's no label.
    let mut all_label_pcs: Vec<usize> = p.labels.values().copied().collect();
    all_label_pcs.push(0);
    all_label_pcs.sort();
    all_label_pcs.dedup();

    // For each label PC, find the nearest preceding call-entry PC.
    // That call-entry is the "body owner" for that label's region.
    // But continuation labels (PushAddr targets) may belong to a body whose
    // entry is further back, past other call entries. We fix this with a
    // propagation pass: each PushAddr instruction inside body X that references
    // label L means L belongs to body X (regardless of position).
    //
    // Step 1: Initial assignment by position (nearest preceding call entry).
    let mut label_body: HashMap<usize, usize> = HashMap::new(); // label_pc -> body_entry_pc
    {
        let mut cur_body = 0usize; // pc 0 is always a call entry
        for &pc in &all_label_pcs {
            if call_entries.contains(&pc) {
                cur_body = pc;
            }
            label_body.insert(pc, cur_body);
        }
    }

    // Step 2: Propagate body ownership through PushAddr references.
    // For each label's instruction range, find PushAddr instructions and
    // reassign their target labels to the same body.
    let mut changed = true;
    while changed {
        changed = false;
        // Build (label_pc, next_label_pc) ranges
        let mut ranges: Vec<(usize, usize)> = all_label_pcs.iter().copied().zip({
            let mut nexts = all_label_pcs.iter().copied().skip(1).collect::<Vec<_>>();
            nexts.push(p.ins.len());
            nexts.into_iter()
        }).collect();
        ranges.sort();
        for &(lpc, end) in &ranges {
            let body = *label_body.get(&lpc).unwrap_or(&0);
            for i in lpc..end {
                if i >= p.ins.len() { break; }
                if let Ins::PushAddr(target) = &p.ins[i] {
                    if let Some(&target_pc) = p.labels.get(target) {
                        let target_body = *label_body.get(&target_pc).unwrap_or(&0);
                        if target_body != body {
                            label_body.insert(target_pc, body);
                            changed = true;
                        }
                    }
                }
            }
        }
    }

    // Step 3: For each instruction, determine its body by finding the nearest
    // preceding label and looking up that label's body.
    let mut ins_body: Vec<usize> = vec![0usize; p.ins.len()];
    {
        let mut cur_body = 0usize;
        for i in 0..p.ins.len() {
            if all_label_pcs.contains(&i) {
                if let Some(&b) = label_body.get(&i) {
                    cur_body = b;
                }
            }
            ins_body[i] = cur_body;
        }
    }

    // Step 4: Collect local names per body, assigning slot IDs.
    let mut body_slots: HashMap<usize, HashMap<String, usize>> = HashMap::new();
    for (i, ins) in p.ins.iter().enumerate() {
        let body = ins_body[i];
        let slots = body_slots.entry(body).or_default();
        match ins {
            Ins::LocalSet(n) | Ins::LocalGet(n) => {
                if !slots.contains_key(n) {
                    let id = slots.len();
                    slots.insert(n.clone(), id);
                }
            }
            _ => {}
        }
    }

    // Step 5: Record frame sizes and resolve instructions.
    for (&body, slots) in &body_slots {
        p.local_counts.insert(body, slots.len());
        let mut names: Vec<(String, usize)> = slots.iter().map(|(n, &id)| (n.clone(), id)).collect();
        names.sort_by_key(|(_, id)| *id);
        p.local_names.insert(body, names.into_iter().map(|(n, _)| n).collect());
    }
    for (i, ins) in p.ins.iter_mut().enumerate() {
        let body = ins_body[i];
        match ins {
            Ins::LocalSet(n) => {
                if let Some(&id) = body_slots.get(&body).and_then(|s| s.get(n)) {
                    *ins = Ins::LocalSetI(id);
                }
            }
            Ins::LocalGet(n) => {
                if let Some(&id) = body_slots.get(&body).and_then(|s| s.get(n)) {
                    *ins = Ins::LocalGetI(id);
                }
            }
            _ => {}
        }
    }
}
pub fn prefix_task_labels(body: Vec<Tok>, prefix: &str) -> Vec<Tok> {
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
pub fn merge_tus(tus: Vec<Parsed>, mods: Vec<String>, init_flags: &[bool]) -> Parsed {
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
        init_pcs: Vec::new(),
        entry_label: None,
        local_counts: HashMap::new(),
        local_names: HashMap::new(),
    };
    for (tu_idx, (tu, defmod)) in tus.into_iter().zip(mods).enumerate() {
        let modname = tu.modname.clone().unwrap_or(defmod);
        let off = m.ins.len();
        if init_flags.get(tu_idx).copied().unwrap_or(false) {
            m.init_pcs.push(off);
        }
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
                Ins::Goto(l) => Ins::Goto(local(l)),
                Ins::For => Ins::For,
                Ins::Call(l) => Ins::Call(local(l)),
                Ins::CallExt(ii) => Ins::CallExt(imp_map[*ii]),
                Ins::Ret => Ins::Ret,
                Ins::SetV(v) => Ins::SetV(format!("{}__{}", modname, v)),
                Ins::GetV(v) => Ins::GetV(format!("{}__{}", modname, v)),
                Ins::LocalSet(n) => Ins::LocalSet(n.clone()),
                Ins::LocalGet(n) => Ins::LocalGet(n.clone()),
                Ins::LocalSetI(id) => Ins::LocalSetI(*id),
                Ins::LocalGetI(id) => Ins::LocalGetI(*id),
                Ins::Extern(n) => Ins::Extern(n.clone()),
                Ins::Send => Ins::Send,
                Ins::Weave(metas) => Ins::Weave(
                    metas
                        .iter()
                        .map(|t| WeaveMeta {
                            name: format!("{}__{}", modname, t.name),
                            pc: local(&t.pc),
                            inputs: t.inputs.clone(),
                            count: t.count,
                        })
                        .collect(),
                ),
                Ins::Sys(a) => Ins::Sys(*a),
                Ins::If => Ins::If,
                Ins::IfElse => Ins::IfElse,
                Ins::While => Ins::While,
                Ins::Break => Ins::Break,
                Ins::Cont => Ins::Cont,
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
        // carry entry_label from the first TU only
        if tu_idx == 0 {
            if let Some(el) = &tu.entry_label {
                m.entry_label = Some(format!("{}:{}", modname, el));
            }
        }
    }
    resolve_locals(&mut m);
    m
}

pub fn simple_ins(name: &'static str) -> Ins {
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
        "CLONE" => "op_clone",
        "CAST" => "op_cast",
        "TENSOR" => "op_tensor",
        "CAT" => "op_cat",
        "FMT" => "op_fmt",
        "BUF" => "op_buf",
        "BUFCOPY" => "op_bufcopy",
        "LOADX" => "op_loadx",
        "STOREX" => "op_storex",
        "SIZEOF" => "op_sizeof",
        "MALLOC" => "op_malloc",
        "FREE" => "op_free",
        "GC" => "op_gc",
        "PRINT" => "op_print",
        "SCAN" => "op_scan",
        "DICT" => "op_dict",
        "LIST" => "op_list",
        "PUSH" => "op_push",
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
        "SH" => "op_sh",
        "SHP" => "op_shp",
        "EXEC" => "op_exec",
        "MATCH" => "op_match",
        "REPLACE" => "op_replace",
        "RSPLIT" => "op_rsplit",
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
        // v10: arithmetic & logic
        "DIV" => "op_div",
        "REM" => "op_rem",
        "EQ" => "op_eq",
        "LT" => "op_lt",
        "GT" => "op_gt",
        "NOT" => "op_not",
        "OR" => "op_or",
        "XOR" => "op_xor",
        "SHL" => "op_shl",
        "BNOT" => "op_bnot",
        // v10: container protocol & sequences
        "DEL" => "op_del",
        "GETQ" => "op_getq",
        "HAS" => "op_has",
        "ORELSE" => "op_orelse",
        "KEYS" => "op_keys",
        "RANGE" => "op_range",
        "SORT" => "op_sort",
        "FILTER" => "op_filter",
        "SOME" => "op_some",
        "EVERY" => "op_every",
        // v10: vector ops
        "VADD" => "op_vadd",
        "VSUB" => "op_vsub",
        "VMUL" => "op_vmul",
        "VDIV" => "op_vdiv",
        "VEADD" => "op_veadd",
        "VESUB" => "op_vesub",
        "VEMUL" => "op_vemul",
        "VEDIV" => "op_vediv",
        "VEMAX" => "op_vemax",
        "VEMIN" => "op_vemin",
        "VEQ" => "op_veq",
        "VLT" => "op_vlt",
        "VGT" => "op_vgt",
        "VGE" => "op_vge",
        "VLE" => "op_vle",
        "VAND" => "op_vand",
        "VOR" => "op_vor",
        "VNOT" => "op_vnot",
        "VCOUNT" => "op_vcount",
        "VGATHER" => "op_vgather",
        "VSUM" => "op_vsum",
        "VMEAN" => "op_vmean",
        "VMIN" => "op_vmin",
        "VMAX" => "op_vmax",
        "VMAP" => "op_vmap",
        "VFOLD" => "op_vfold",
        // v10: time, bloom, script I/O
        "NOW" => "op_now",
        "TIME" => "op_time",
        "TIMEF" => "op_timef",
        "BLOOM" => "op_bloom",
        "BADD" => "op_badd",
        "BTEST" => "op_btest",
        "SLURP" => "op_slurp",
        "SPIT" => "op_spit",
        "ARGV" => "op_argv",
        // v10: additional data ops
        "GROUP" => "op_group",
        "AGG" => "op_agg",
        "UNIQUE" => "op_unique",
        "FLAT" => "op_flat",
        "CHUNK" => "op_chunk",
        "VARGSORT" => "op_vargsort",
        "VSEARCHSORTED" => "op_vsearchsorted",
        "VWHERE" => "op_vwhere",
        // v10: large-data shortcuts
        "MMAP" => "op_mmap",
        "FEACH" => "op_feach",
        "FFOLD" => "op_ffold",
        "FSPLIT" => "op_fsplit",
        "FGET" => "op_fget",
        "FATOI" => "op_fatoi",
        "FATOF" => "op_fatof",
        "FSGET" => "op_fsget",
        "FBYTE" => "op_fbyte",
        "VGET" => "op_vget",
        "VSET" => "op_vset",
        "ADDTO" => "op_addto",
        "FADDTO" => "op_faddto",
        "FINC" => "op_finc",
        "INCR" => "op_count",
        "FMATCH" => "op_fmatch",
        "BFS" => "op_bfs",
        "DFS" => "op_dfs",
        "WFIND" => "op_wfind",
        // v10: JSON, iterators, sinks, containment, threads
        "JSON" => "op_json",
        "UNJSON" => "op_unjson",
        "ITER" => "op_iter",
        "NEXT" => "op_next",
        "COLLECT" => "op_collect",
        "IMAP" => "op_imap",
        "IFILTER" => "op_ifilter",
        "FEMIT" => "op_femit",
        "TRY" => "op_try",
        "RETRY" => "op_retry",
        "SPAWN" => "op_spawn",
        "ATOI" => "op_atoi",
        "ATOF" => "op_atof",
        "ITOA" => "op_itoa",
        "FTOA" => "op_ftoa",
        "HASARGS" => "op_hasargs",
        "ARGI" => "op_argi",
        "SORTKEYS" => "op_sortkeys",
        "TOPN" => "op_topn",
        "RANGEFOLD" => "op_rangefold",
        other => panic!("no helper for {}", other),
    };
    Ins::Simple(helper)
}
