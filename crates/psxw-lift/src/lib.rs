//! Lift tier: ahead-of-time translation of MIPS R3000 functions, read from
//! a container's original image, into generated C in the uniform o32 form
//! `uint64_t f(a0, a1, a2, a3)` (docs/design.md §4). Nothing decodes MIPS
//! at run time.
//!
//! - [`program`]: containers, function starts and extents;
//! - [`graph`]: delay-slot-aware control flow, jump tables, call targets;
//! - [`analysis`]: constants (I/O sites, code pointers), liveness, load
//!   delays;
//! - [`emit`]: the C text, against [`RT_HEADER`].
//!
//! Translation runs to a fixed point: call targets and code pointers that
//! name no known function become new starts, splitting an extent only when
//! the address isn't reachable inside it.

pub mod analysis;
pub mod decode;
pub mod emit;
pub mod graph;
pub mod program;
pub mod run;

use analysis::{Facts, reg_list};
use graph::{Callee, Dest, Explorer, Graph, LiftError, Node, Transfer};
use program::{Function, Located, Program, Source};
use psxw_inputs::originals::Originals;
use rayon::prelude::*;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// A BIOS trampoline at `addr` (PsyQ's libapi/libc stubs): `li t2, T;
/// jr t2; li t1, F` with `T` one of the vectors. Returns `(T, F)`.
pub fn bios_trampoline(program: &Program, originals: &Originals, from: &str, addr: u32) -> Option<(u8, u32)> {
    let Some(Located::Code(c)) = program.locate(from, addr) else { return None };
    let w = |i: u32| originals.word(&c, addr + 4 * i).map(decode::decode);
    let (li_t2, jr, li_t1) = (w(0)?, w(1)?, w(2)?);
    let li = |i: &decode::Insn, rt: u8| matches!(i.op, decode::Op::Addiu | decode::Op::Ori) && i.rs == 0 && i.rt == rt;
    let table = li_t2.uimm;
    let ok = li(&li_t2, 10) && jr.op == decode::Op::Jr && jr.rs == 10 && li(&li_t1, 9) && matches!(table, 0xA0 | 0xB0 | 0xC0);
    ok.then_some((table as u8, li_t1.uimm))
}

/// `t1` after the delay slot of the branch at `b` runs: a BIOS call's
/// function number, when it is a constant.
fn t1_after_slot(g: &Graph, facts: &Facts, b: u32) -> Option<u32> {
    let n = Node::Slot(b);
    let d = g.nodes.get(&n)?;
    let mut after = facts.consts.get(&n).copied().unwrap_or([None; 32]);
    if let Some(w) = d.insn.writes() {
        after[w as usize] = None;
        if matches!(d.insn.op, decode::Op::Addiu | decode::Op::Ori) && d.insn.rs == 0 {
            after[w as usize] = Some(if d.insn.op == decode::Op::Ori { d.insn.uimm } else { d.insn.simm as u32 });
        }
    }
    after[9]
}

/// Calls that need non-local control flow (`emit::Exceptions`): `setjmp`
/// sites (a direct call to an A0h:13h trampoline, or the vector itself
/// with t1 = 13h) and `ReturnFromException` (B0h:17h through the vector).
fn exception_sites(program: &Program, originals: &Originals, func: &Function, g: &Graph, facts: &Facts) -> emit::Exceptions {
    let mut x = emit::Exceptions { continuation: func.sources.contains(&Source::Continuation), ..Default::default() };
    for (n, d) in &g.nodes {
        let Node::Slot(b) = *n else { continue };
        let bios = |table: u8| matches!(d.transfer,
            Transfer::Call { callee: Callee::Bios { table: t }, .. } | Transfer::Jump(Dest::Tail(Callee::Bios { table: t })) if t == table);
        match &d.transfer {
            Transfer::Call { callee: Callee::Direct { addr, .. }, .. }
                if bios_trampoline(program, originals, &func.container, *addr) == Some((0xA0, 0x13)) =>
            {
                x.setjmp.insert(b);
            }
            Transfer::Call { callee: Callee::Bios { table: 0xA0 }, .. } if t1_after_slot(g, facts, b) == Some(0x13) => {
                x.setjmp.insert(b);
            }
            _ if bios(0xB0) && t1_after_slot(g, facts, b) == Some(0x17) => {
                x.rfe.insert(b);
            }
            _ => {}
        }
    }
    x
}

/// Included by every generated file.
pub const RT_HEADER: &str = include_str!("psxw_rt.h");
pub const RT_HEADER_NAME: &str = "psxw_rt.h";

/// One translated function.
#[derive(Debug, Clone)]
pub struct Translation {
    pub func: Function,
    pub cname: String,
    pub c: String,
    pub graph: Graph,
    pub facts: Facts,
    pub externs: BTreeSet<String>,
}

pub fn translate(
    program: &Program,
    originals: &Originals,
    func: &Function,
    inline: &BTreeMap<(String, u32), u32>,
    chain: &BTreeSet<String>,
) -> Result<Translation, LiftError> {
    let in_code = |a: u32| {
        program.code(&func.container).is_some_and(|c| c.in_text(a))
            || program.code(&program.main).is_some_and(|c| c.in_text(a))
    };
    // A window constant must agree with dataflow over the finished graph;
    // otherwise that site is re-explored as an indirect transfer.
    let mut not_const = BTreeSet::new();
    let (graph, facts) = loop {
        let graph = Explorer { program, originals, func, not_const: &not_const, inline }.explore()?;
        let facts = analysis::analyze(&graph, &in_code);
        let wrong: Vec<u32> = graph
            .const_targets
            .iter()
            .filter(|&(b, &(r, c))| facts.consts.get(&Node::Normal(*b)).map(|st| st[r as usize]) != Some(Some(c)))
            .map(|(b, _)| *b)
            .collect();
        if wrong.is_empty() {
            break (graph, facts);
        }
        not_const.extend(wrong);
    };
    let cname = program::translated_symbol(&program.main, &func.container, func.addr);
    let comment = format!(
        "{} 0x{:08X} {} (extent 0x{:08X}..0x{:08X})",
        func.container, func.addr, func.name, func.addr, func.end
    );
    if func.sources.contains(&Source::Continuation) && func.sources.iter().any(|s| *s != Source::Continuation) {
        return Err(LiftError {
            pc: func.addr,
            message: "a setjmp return address that is also a function entry".into(),
        });
    }
    let exceptions = exception_sites(program, originals, func, &graph, &facts);
    let mut graph = graph;
    for &b in &exceptions.setjmp {
        graph.wanted.insert((func.container.clone(), b + 8, Source::Continuation));
    }
    let reads_chain = chain.contains(&program::call_symbol(&program.main, &func.container, func.addr));
    let (c, externs) =
        emit::Emitter::new(&graph, &facts)?.exceptions(exceptions).chain(chain, reads_chain).function(&cname, &comment);
    Ok(Translation { func: func.clone(), cname, c, graph, facts, externs })
}

/// Result of translating every function of the requested containers.
pub struct Outcome {
    pub ok: Vec<Translation>,
    pub failed: Vec<(Function, LiftError)>,
    /// Rounds until no new starts or inline functions appeared.
    pub rounds: u32,
    /// Functions expanded at call sites: start → end.
    pub inline: BTreeMap<(String, u32), u32>,
    /// Why each was inlined.
    pub inline_reasons: BTreeMap<(String, u32), String>,
}

/// Function tables in initialized data: words pointing at the first
/// instruction of code nothing reaches yet, past only nops from reachable
/// code or the start of a text range. (Jump-table entries point at
/// reachable labels; chance data rarely lands exactly on a gap's start.)
fn data_pointers(
    program: &Program,
    originals: &Originals,
    reach: &BTreeMap<(String, u32), BTreeSet<u32>>,
) -> BTreeSet<(String, u32)> {
    let mut reachable: BTreeMap<&str, BTreeSet<u32>> = BTreeMap::new();
    for ((c, _), pcs) in reach {
        reachable.entry(c.as_str()).or_default().extend(pcs);
    }
    let empty = BTreeSet::new();
    let mut out = BTreeSet::new();
    for (id, code) in &program.containers {
        for r in &code.data {
            let mut a = (r.start + 3) & !3;
            while a + 4 <= r.end {
                let w = originals.word(id, a);
                a += 4;
                let Some(w) = w else { continue };
                let Some(Located::Code(tc)) = program.locate(id, w) else { continue };
                let Some(target) = program.code(&tc) else { continue };
                let seen = reachable.get(tc.as_str()).unwrap_or(&empty);
                if target.starts.contains_key(&w) || seen.contains(&w) || originals.word(&tc, w).is_none_or(|i| i == 0) {
                    continue;
                }
                let mut p = w;
                let gap_start = loop {
                    if target.text.iter().any(|t| t.start == p) || seen.contains(&(p - 4)) {
                        break true;
                    }
                    if originals.word(&tc, p - 4) != Some(0) {
                        break false;
                    }
                    p -= 4;
                };
                if gap_start {
                    out.insert((tc, w));
                }
            }
        }
    }
    out
}

/// Translate to a fixed point over function starts and inline functions.
///
/// A function with no C is inlined at its call sites (and fall-throughs)
/// when it reads registers on entry that the o32 ABI doesn't pass, or when
/// a caller reads registers after calling it that the ABI doesn't return:
/// hand-written asm that passes values in temporaries. Decompiled C is
/// ABI-conformant by construction, so its entry reads are uninitialized
/// locals, not arguments.
pub fn translate_all(program: &mut Program, originals: &Originals, containers: &[String]) -> Outcome {
    let mut rounds = 0;
    let mut inline: BTreeMap<(String, u32), u32> = BTreeMap::new();
    let mut inline_reasons: BTreeMap<(String, u32), String> = BTreeMap::new();
    loop {
        rounds += 1;
        let funcs: Vec<Function> = containers.iter().flat_map(|c| program.functions(c)).collect();
        // Decompiled C is ABI-conformant, so the translations paired with it
        // (and replaced by it) never inline their callees.
        let no_inline = BTreeMap::new();
        let results: Vec<Result<Translation, (Function, LiftError)>> = funcs
            .par_iter()
            .map(|f| {
                let inline = if f.sources.contains(&Source::Defined) { &no_inline } else { &inline };
                translate(program, originals, f, inline, &BTreeSet::new()).map_err(|e| (f.clone(), e))
            })
            .collect();

        // Reachable instruction addresses per function, to tell interior
        // entries from new functions.
        let mut reach: BTreeMap<(String, u32), BTreeSet<u32>> = BTreeMap::new();
        let mut wanted: BTreeSet<(String, u32, Source)> = BTreeSet::new();
        for t in results.iter().flatten() {
            let pcs = t.graph.nodes.keys().map(|n| n.pc()).collect();
            reach.insert((t.func.container.clone(), t.func.addr), pcs);
            wanted.extend(t.graph.wanted.iter().cloned());
            for &a in &t.facts.code_consts {
                if let Some(Located::Code(c)) = program.locate(&t.func.container, a) {
                    wanted.insert((c, a, Source::CodePointer));
                }
            }
        }
        for (c, a) in data_pointers(program, originals, &reach) {
            wanted.insert((c, a, Source::DataPointer));
        }
        let mut added = false;
        for (c, a, source) in wanted {
            if program.code(&c).is_some_and(|code| code.starts.contains_key(&a)) {
                // A continuation changes how an existing start is entered.
                if source == Source::Continuation {
                    added |= program.add_source(&c, a, source);
                }
                continue;
            }
            let enclosing = program.functions(&c).into_iter().filter(|f| f.addr < a && a < f.end).max_by_key(|f| f.addr);
            let interior = enclosing.is_some_and(|f| reach.get(&(c.clone(), f.addr)).is_some_and(|r| r.contains(&a)));
            added |= program.add(&c, a, None, source, !interior);
        }
        if !added {
            let ends: BTreeMap<(String, u32), (u32, bool)> = funcs
                .iter()
                .map(|f| ((f.container.clone(), f.addr), (f.end, f.sources.contains(&Source::Defined))))
                .collect();
            let mut want = |c: &str, a: u32, why: String| {
                if let Some(&(end, false)) = ends.get(&(c.to_string(), a)) {
                    if inline.insert((c.to_string(), a), end).is_none() {
                        inline_reasons.insert((c.to_string(), a), why);
                        added = true;
                    }
                }
            };
            // Inline only on evidence of register passing: a caller writes
            // what the callee reads on entry, or reads after the call what
            // the callee writes.
            let facts: BTreeMap<(String, u32), &Facts> =
                results.iter().flatten().map(|t| ((t.func.container.clone(), t.func.addr), &t.facts)).collect();
            let callee_of = |from: &str, a: u32| match program.locate(from, a) {
                Some(Located::Code(c)) => Some(c),
                _ => None,
            };
            // Only callers that are translated in the game (not C) can inline.
            for t in results.iter().flatten().filter(|t| !t.func.sources.contains(&Source::Defined)) {
                for &(pc, target, pre_defs) in &t.facts.call_sites {
                    let Some(c) = callee_of(&t.func.container, target) else { continue };
                    let Some(cf) = facts.get(&(c.clone(), target)) else { continue };
                    let passed = cf.entry_reads & pre_defs;
                    if passed != 0 {
                        let why = format!(
                            "reads {} on entry, which {} 0x{:08X} sets before 0x{pc:08X}",
                            reg_list(passed),
                            t.func.container,
                            t.func.addr
                        );
                        want(&c, target, why);
                    }
                }
                for &(pc, regs, callee) in &t.facts.post_call_reads {
                    let Some(a) = callee else { continue };
                    let Some(c) = callee_of(&t.func.container, a) else { continue };
                    let Some(cf) = facts.get(&(c.clone(), a)) else { continue };
                    let returned = regs & cf.defs;
                    if returned != 0 {
                        let why = format!(
                            "writes {}, which {} 0x{:08X} reads after calling it at 0x{pc:08X}",
                            reg_list(returned),
                            t.func.container,
                            t.func.addr
                        );
                        want(&c, a, why);
                    }
                }
            }
        }
        if !added {
            let mut out = Outcome { ok: Vec::new(), failed: Vec::new(), rounds, inline, inline_reasons };
            for r in results {
                match r {
                    Ok(t) => out.ok.push(t),
                    Err(e) => out.failed.push(e),
                }
            }
            // GCC's static chain: a nested function reads its parent's
            // frame through v0, which the parent sets before the call and
            // the o32 call doesn't pass. Its C is unusable (clang has no
            // nested functions), so it is translated, and so is the parent;
            // neither is inlined, since decompiled C is otherwise taken to
            // be ABI-conformant. Every translation that reads v0 on entry
            // takes it from the caller this way, as the hardware would.
            let chain: BTreeSet<String> = out
                .ok
                .iter()
                .filter(|t| t.facts.entry_reads & (1 << 2) != 0)
                .map(|t| program::call_symbol(&program.main, &t.func.container, t.func.addr))
                .collect();
            if !chain.is_empty() {
                let again: Vec<usize> = (0..out.ok.len())
                    .filter(|&i| {
                        let t = &out.ok[i];
                        chain.contains(&program::call_symbol(&program.main, &t.func.container, t.func.addr))
                            || t.externs.iter().any(|e| chain.contains(e))
                    })
                    .collect();
                let no_inline = BTreeMap::new();
                let redone: Vec<(usize, Result<Translation, LiftError>)> = again
                    .par_iter()
                    .map(|&i| {
                        let f = &out.ok[i].func;
                        let inline = if f.sources.contains(&Source::Defined) { &no_inline } else { &out.inline };
                        (i, translate(program, originals, f, inline, &chain))
                    })
                    .collect();
                let mut dropped = BTreeSet::new();
                for (i, r) in redone {
                    match r {
                        Ok(t) => out.ok[i] = t,
                        Err(e) => {
                            out.failed.push((out.ok[i].func.clone(), e));
                            dropped.insert(i);
                        }
                    }
                }
                let ok = std::mem::take(&mut out.ok);
                out.ok = ok.into_iter().enumerate().filter(|(i, _)| !dropped.contains(i)).map(|(_, t)| t).collect();
            }
            return out;
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionReport {
    pub container: String,
    pub addr: String,
    pub end: String,
    pub name: String,
    pub sources: Vec<Source>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interior_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdk_member: Option<String>,
    pub insns: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub jump_tables: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub indirect_tail_calls: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub indirect_calls: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub bios_calls: Vec<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub syscalls: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub cop0: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub cop2: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub io: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub kernel: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub entry_reads: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub entry_s_reads: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub post_call_reads: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub falls_into: Option<String>,
    /// Callees expanded into this function.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inlined: Vec<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub load_delay_hazards: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// A call through the BIOS tables at `0xA0`/`0xB0`/`0xC0`: the function
/// number is `t1` after the delay slot (None when it isn't a constant).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BiosCall {
    pub pc: u32,
    pub table: u8,
    pub func: Option<u32>,
}

pub fn bios_calls(t: &Translation) -> Vec<BiosCall> {
    let mut out = Vec::new();
    for (n, d) in &t.graph.nodes {
        let Node::Slot(_) = *n else { continue };
        let table = match &d.transfer {
            Transfer::Jump(Dest::Tail(Callee::Bios { table })) => *table,
            Transfer::Call { callee: Callee::Bios { table }, .. } => *table,
            Transfer::Call { next: Dest::Tail(Callee::Bios { table }), .. } => *table,
            _ => continue,
        };
        let st = t.facts.consts.get(n).copied().unwrap_or([None; 32]);
        // t1 after the slot instruction runs.
        let mut after = st;
        if let Some(w) = d.insn.writes() {
            after[w as usize] = None;
            if d.insn.op == decode::Op::Addiu && d.insn.rs == 0 {
                after[w as usize] = Some(d.insn.simm as u32);
            }
        }
        out.push(BiosCall { pc: n.pc(), table, func: after[9] });
    }
    out
}

impl FunctionReport {
    pub fn new(func: &Function, sdk_member: Option<String>, t: Option<&Translation>, error: Option<&LiftError>) -> Self {
        let mut r = FunctionReport {
            container: func.container.clone(),
            addr: format!("0x{:08X}", func.addr),
            end: format!("0x{:08X}", func.end),
            name: func.name.clone(),
            sources: func.sources.iter().copied().collect(),
            interior_of: func.interior_of.map(|a| format!("0x{a:08X}")),
            sdk_member,
            insns: 0,
            jump_tables: Vec::new(),
            indirect_tail_calls: Vec::new(),
            indirect_calls: Vec::new(),
            bios_calls: Vec::new(),
            syscalls: 0,
            cop0: 0,
            cop2: 0,
            io: Vec::new(),
            kernel: Vec::new(),
            entry_reads: String::new(),
            entry_s_reads: String::new(),
            post_call_reads: Vec::new(),
            falls_into: None,
            inlined: Vec::new(),
            load_delay_hazards: 0,
            error: error.map(|e| e.to_string()),
        };
        let Some(t) = t else { return r };
        r.insns = t.graph.nodes.keys().filter(|n| matches!(n, Node::Normal(_))).count();
        for (n, d) in &t.graph.nodes {
            let op = d.insn.op;
            use decode::Op;
            match op {
                Op::Syscall => r.syscalls += 1,
                Op::Mfc0 | Op::Mtc0 | Op::Rfe => r.cop0 += 1,
                Op::Mfc2 | Op::Mtc2 | Op::Cfc2 | Op::Ctc2 | Op::Cop2 | Op::Lwc2 | Op::Swc2 => r.cop2 += 1,
                _ => {}
            }
            let Node::Slot(b) = *n else {
                if let Transfer::FallOff(Callee::Direct { addr, .. }) = &d.transfer {
                    r.falls_into = Some(format!("0x{addr:08X}"));
                }
                continue;
            };
            match &d.transfer {
                Transfer::Table { base, targets, .. } => {
                    r.jump_tables.push(format!("0x{b:08X}: 0x{base:08X}[{}]", targets.len()))
                }
                Transfer::Jump(Dest::Tail(Callee::Indirect { .. })) => r.indirect_tail_calls.push(format!("0x{b:08X}")),
                Transfer::Call { callee: Callee::Indirect { .. }, .. }
                | Transfer::CondCall { callee: Callee::Indirect { .. }, .. } => {
                    r.indirect_calls.push(format!("0x{b:08X}"))
                }
                _ => {}
            }
        }
        r.bios_calls = bios_calls(t)
            .iter()
            .map(|c| format!("0x{:08X}: {:02X}h:{}h", c.pc, c.table, c.func.map_or("?".into(), |f| format!("{f:02X}"))))
            .collect();
        r.io = t.facts.io.iter().map(|(pc, a)| format!("0x{pc:08X} → 0x{a:08X}")).collect();
        r.kernel = t.facts.kernel.iter().map(|(pc, a)| format!("0x{pc:08X} → 0x{a:08X}")).collect();
        r.entry_reads = reg_list(t.facts.entry_reads);
        r.entry_s_reads = reg_list(t.facts.entry_s_reads);
        r.post_call_reads =
            t.facts.post_call_reads.iter().map(|(pc, regs, _)| format!("0x{pc:08X}: {}", reg_list(*regs))).collect();
        r.inlined = t
            .graph
            .regions
            .iter()
            .skip(1)
            .map(|(a, _, c)| format!("{c} 0x{a:08X}"))
            .collect();
        r.load_delay_hazards = t.facts.hazards.len();
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use program::{Code, Start};

    /// A GCC nested function reads its parent's frame through v0, which the
    /// parent sets before the call (here ahead of a branch, so it isn't
    /// register passing the inliner sees): the call passes it.
    #[test]
    fn static_chain() {
        const BASE: u32 = 0x8001_0000;
        let child = BASE + 0x28;
        let words: [u32; 13] = [
            0x27BD_FFE8,                              // addiu sp, sp, -24
            0xAFBF_0010,                              // sw ra, 16(sp)
            0x03A0_1021,                              // addu v0, sp, zero (the chain)
            0x1000_0001,                              // b 1f
            0,                                        // nop
            0x0C00_0000 | (child >> 2 & 0x03FF_FFFF), // 1: jal child
            0,                                        // nop
            0x8FBF_0010,                              // lw ra, 16(sp)
            0x03E0_0008,                              // jr ra
            0x27BD_0018,                              // addiu sp, sp, 24
            0x8C42_0000,                              // child: lw v0, 0(v0)
            0x03E0_0008,                              // jr ra
            0,                                        // nop
        ];
        let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let start = |name: &str, size: u32| Start { name: Some(name.into()), size: Some(size), sources: [Source::Symbol].into(), boundary: true };
        let mut program = Program {
            main: "test".into(),
            containers: [(
                "test".to_string(),
                Code {
                    id: "test".into(),
                    text: vec![BASE..BASE + code.len() as u32],
                    data: vec![],
                    starts: [(BASE, start("parent", 0x28)), (child, start("child", 0xC))].into(),
                },
            )]
            .into(),
        };
        let originals = Originals::from_images(vec![("test".into(), BASE, code)]);
        let out = translate_all(&mut program, &originals, &["test".into()]);
        assert!(out.failed.is_empty(), "{:?}", out.failed);
        let c = |a: u32| out.ok.iter().find(|t| t.func.addr == a).map(|t| t.c.clone()).expect("translated");
        assert!(c(child).contains("r2 = psxw_chain_get()"), "{}", c(child));
        assert!(c(BASE).contains(&format!("(psxw_chain_set(r2), psxw_f_{child:08X}(r4, r5, r6, r7))")), "{}", c(BASE));
    }

    /// A wrapper whose last act is a call returns that callee's v0, as a
    /// dropped `return g();` does; writing v0 after the call doesn't.
    #[test]
    fn returns_callee_v0() {
        const BASE: u32 = 0x8001_0000;
        let callee = BASE + 0x40;
        let jal = 0x0C00_0000 | (callee >> 2 & 0x03FF_FFFF);
        let wrapper = |after: u32| -> Vec<u32> {
            vec![
                0x27BD_FFE8, // addiu sp, sp, -24
                0xAFBF_0010, // sw ra, 16(sp)
                jal,         // jal callee
                0,           // nop
                after,       // what follows the call
                0x8FBF_0010, // lw ra, 16(sp)
                0x03E0_0008, // jr ra
                0x27BD_0018, // addiu sp, sp, 24
            ]
        };
        for (after, expect) in [(0, true), (0x2402_0001 /* addiu v0, zero, 1 */, false)] {
            let mut words = wrapper(after);
            words.resize(0x10, 0);
            words.extend([0x03E0_0008, 0x2402_0007]); // callee: jr ra; addiu v0, zero, 7
            let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            let start = |size: u32| Start { name: None, size: Some(size), sources: [Source::Symbol].into(), boundary: true };
            let mut program = Program {
                main: "test".into(),
                containers: [(
                    "test".to_string(),
                    Code {
                        id: "test".into(),
                        text: vec![BASE..BASE + code.len() as u32],
                        data: vec![],
                        starts: [(BASE, start(0x20)), (callee, start(8))].into(),
                    },
                )]
                .into(),
            };
            let originals = Originals::from_images(vec![("test".into(), BASE, code)]);
            let out = translate_all(&mut program, &originals, &["test".into()]);
            let t = out.ok.iter().find(|t| t.func.addr == BASE).expect("translated");
            assert_eq!(t.facts.returns_callee_v0, expect, "after the call: {after:08X}");
        }
    }
}
