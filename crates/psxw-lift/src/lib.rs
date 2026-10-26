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
    let (c, externs) = emit::Emitter::new(&graph, &facts)?.function(&cname, &comment);
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
                translate(program, originals, f, inline).map_err(|e| (f.clone(), e))
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
        let mut added = false;
        for (c, a, source) in wanted {
            if program.code(&c).is_some_and(|code| code.starts.contains_key(&a)) {
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
            let pc = n.pc();
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
            let bios = match &d.transfer {
                Transfer::Jump(Dest::Tail(Callee::Bios { table })) => Some(table),
                Transfer::Call { callee: Callee::Bios { table }, .. } => Some(table),
                Transfer::Call { next: Dest::Tail(Callee::Bios { table }), .. } => Some(table),
                _ => None,
            };
            if let Some(table) = bios {
                let st = t.facts.consts.get(n).copied().unwrap_or([None; 32]);
                // t1 after the slot instruction runs.
                let mut after = st;
                if let Some(w) = d.insn.writes() {
                    after[w as usize] = None;
                    if d.insn.op == Op::Addiu && d.insn.rs == 0 {
                        after[w as usize] = Some(d.insn.simm as u32);
                    }
                }
                let func = after[9].map_or("?".to_string(), |v| format!("{v:02X}"));
                r.bios_calls.push(format!("0x{pc:08X}: {table:02X}h:{func}h"));
            }
        }
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
