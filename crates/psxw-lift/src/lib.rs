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

use analysis::{Facts, S_REGS, reg_list};
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
/// with t1 = 13h) and returns from an exception: `ReturnFromException`
/// (B0h:17h through the vector), or an exception handler's own `jr k0;
/// rfe` (a jump with `rfe` in its delay slot is the R3000's return to the
/// interrupted code).
fn exception_sites(program: &Program, originals: &Originals, func: &Function, g: &Graph, facts: &Facts) -> emit::Exceptions {
    let mut x = emit::Exceptions { continuation: func.sources.contains(&Source::Continuation), ..Default::default() };
    for (n, d) in &g.nodes {
        let Node::Slot(b) = *n else { continue };
        let bios = |table: u8| matches!(d.transfer,
            Transfer::Call { callee: Callee::Bios { table: t }, .. } | Transfer::Jump(Dest::Tail(Callee::Bios { table: t })) if t == table);
        if d.insn.op == decode::Op::Rfe && g.nodes[&Node::Normal(b)].insn.op == decode::Op::Jr {
            x.rfe.insert(b);
        }
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
    /// Returns and tail jumps where ra isn't the caller's return address
    /// (TR3, [`analysis::ForeignReturns::wrong`]). The translation would
    /// still return to the caller, so [`translate_all`] fails it once
    /// discovery is done.
    pub foreign_returns: Vec<(u32, Option<u32>, String)>,
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
    let returns = analysis::foreign_returns(&graph, &exceptions.rfe);
    let mut graph = graph;
    for &b in &exceptions.setjmp {
        graph.wanted.insert((func.container.clone(), b + 8, Source::Continuation));
    }
    let reads_chain = chain.contains(&program::call_symbol(&program.main, &func.container, func.addr));
    let (c, externs) = emit::Emitter::new(&graph, &facts)?
        .exceptions(exceptions)
        .chain(chain, reads_chain)
        .guarded(returns.guarded)
        .function(&cname, &comment);
    Ok(Translation { func: func.clone(), cname, c, graph, facts, externs, foreign_returns: returns.wrong })
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

/// Most instructions a data pointer may enter before a text range.
const EARLY_ENTRY: u32 = 8;

/// Code the inputs' split filed as data: a function table entry (a data
/// word beside one holding a known function start) pointing at a
/// container's data just before one of its text ranges, where every word
/// up to the range is an instruction a function can begin with: not a nop,
/// no control transfer, no memory access off $zero (what a RAM pointer
/// decodes as), reading only what a caller passes (a0-a3, gp, sp) or what
/// an earlier one wrote. bof4's `ovl_4504` begins a function two
/// instructions before its text, and `ovl_4444` calls it through such a
/// table. Words of data that pass for such code make an unused function.
fn early_entries(program: &Program, originals: &Originals) -> BTreeSet<(String, u32)> {
    const PASSED: u32 = 0xF0 | 1 << 28 | 1 << 29;
    let starts: BTreeSet<u32> = program.containers.values().flat_map(|c| c.starts.keys().copied()).collect();
    let mut text_starts: BTreeMap<u32, Vec<&str>> = BTreeMap::new();
    for (id, code) in &program.containers {
        for t in &code.text {
            text_starts.entry(t.start).or_default().push(id);
        }
    }
    let entry_like = |c: &str, from: u32, to: u32| {
        let mut set = PASSED;
        (from..to).step_by(4).all(|pc| {
            let Some(w) = originals.word(c, pc).filter(|&w| w != 0) else { return false };
            let i = decode::decode(w);
            use decode::Op::*;
            // A RAM pointer decodes as a load or store off $zero.
            let absolute = i.rs == 0 && matches!(i.op, Lb | Lh | Lwl | Lw | Lbu | Lhu | Lwr | Sb | Sh | Swl | Sw | Swr | Lwc2 | Swc2);
            let plain = !matches!(i.op, Invalid | Syscall | Break | Rfe) && !i.op.has_delay_slot() && !absolute;
            let ok = plain && i.reads() & !set == 0;
            if let Some(r) = i.writes() {
                set |= 1 << r;
            }
            ok
        })
    };
    let mut out = BTreeSet::new();
    for (id, code) in &program.containers {
        for r in &code.data {
            let mut a = (r.start + 3) & !3;
            while a + 4 <= r.end {
                let w = originals.word(id, a);
                let beside = [a.wrapping_sub(4), a + 4].iter().any(|&n| originals.word(id, n).is_some_and(|v| starts.contains(&v)));
                a += 4;
                let Some(w) = w.filter(|w| w & 3 == 0 && beside) else { continue };
                for (&start, ids) in text_starts.range(w + 1..=w.saturating_add(4 * EARLY_ENTRY)) {
                    for &tc in ids {
                        let target = &program.containers[tc];
                        if target.data.iter().any(|d| d.contains(&w)) && entry_like(tc, w, start) {
                            out.insert((tc.to_string(), w));
                        }
                    }
                }
            }
        }
    }
    out
}

/// Registers passed outside the o32 ABI that no expansion carries (D33),
/// by index into `ok`. A translated caller that passes registers expands
/// its callee; the lift fails where a call can't carry them:
/// - a call that isn't expanded (`jalr` or `bgezal` to a known function,
///   or a caller with C, whose C can't set registers) setting registers its
///   callee reads on entry: the callee fails;
/// - a function its callers pass registers to whose address is taken: it
///   fails, since a call through a pointer passes only o32's;
/// - a translated caller that reads, after a call that isn't expanded,
///   registers the callee leaves outside o32 (a known callee that writes
///   them, or one through a pointer, the BIOS or the kernel): the caller
///   fails.
///
/// What a function reads on entry that no caller sets is an uninitialised
/// value (a C variable GCC left in a register), and reads 0 as C's do
/// (D27). Decompiled C is ABI-conformant: a C function's own entry reads
/// are uninitialised locals, and after a call its C reads no registers.
fn unpassed(program: &Program, ok: &[Translation], takes: &BTreeMap<(String, u32), u64>) -> BTreeMap<usize, LiftError> {
    let index: BTreeMap<(&str, u32), usize> = ok.iter().enumerate().map(|(i, t)| ((t.func.container.as_str(), t.func.addr), i)).collect();
    let callee = |from: &str, a: u32| match program.locate(from, a) {
        Some(Located::Code(c)) => index.get(&(c.as_str(), a)).copied(),
        _ => None,
    };
    let defined = |t: &Translation| t.func.sources.contains(&Source::Defined);
    // What a translation reads on entry that its own start doesn't supply:
    // a continuation starts from its buffer, and a call from a translation
    // passes v0 as GCC's static chain.
    let unsupplied = |t: &Translation, chain: bool| {
        let mut supplied = if chain { 1u64 << 2 } else { 0 };
        if t.func.sources.contains(&Source::Continuation) {
            supplied |= 1 << 2 | emit::JMP_BUF.iter().fold(0, |a, (r, _)| a | 1u64 << r);
        }
        (t.facts.entry_reads | t.facts.entry_s_reads) & !supplied
    };
    let name = |t: &Translation| format!("{} 0x{:08X}", t.func.container, t.func.addr);
    let mut out: BTreeMap<usize, LiftError> = BTreeMap::new();
    let mut fail = |i: usize, pc: u32, message: String| {
        out.entry(i).or_insert(LiftError { pc, message });
    };
    let mut taken: BTreeSet<usize> = ok
        .iter()
        .enumerate()
        .filter(|(_, t)| t.func.sources.iter().any(|s| matches!(s, Source::CodePointer | Source::DataPointer)))
        .map(|(i, _)| i)
        .collect();
    for (j, t) in ok.iter().enumerate() {
        taken.extend(t.facts.code_consts.iter().filter_map(|&a| callee(&t.func.container, a)));
        for &(pc, target, set) in &t.facts.call_sites {
            let Some(i) = callee(&t.func.container, target) else { continue };
            if defined(&ok[i]) {
                continue;
            }
            let passed = unsupplied(&ok[i], !defined(t)) & set;
            if passed != 0 {
                let how = if defined(t) { "its C can't set them" } else { "the call isn't expanded" };
                let regs = reg_list(passed);
                fail(i, ok[i].func.addr, format!("reads {regs} on entry, which {} sets before calling it at 0x{pc:08X}; {how}", name(t)));
            }
        }
        if defined(t) {
            continue;
        }
        for &(pc, regs, target) in &t.facts.post_call_reads {
            let left = match target.and_then(|a| callee(&t.func.container, a)) {
                Some(i) => regs & (ok[i].facts.defs & !S_REGS | ok[i].facts.s_changed),
                // Through a pointer, the BIOS or the kernel: what's left is
                // the callee's.
                None => regs & !S_REGS,
            };
            if left != 0 {
                let regs = reg_list(left);
                fail(j, pc, format!("reads {regs} after the call at 0x{pc:08X}, which leaves them as the callee did; the call isn't expanded"));
            }
        }
    }
    for i in taken {
        let t = &ok[i];
        let key = (t.func.container.clone(), t.func.addr);
        let passed = takes.get(&key).copied().unwrap_or(0) & unsupplied(t, false);
        if passed != 0 && !t.func.sources.contains(&Source::ComputedEntry) {
            let regs = reg_list(passed);
            fail(i, t.func.addr, format!("reads {regs} on entry, which its callers pass, and its address is taken: a call through a pointer passes only a0-a3"));
        }
    }
    out
}

/// Translations that return, or tail-jump, with ra other than the caller's
/// return address (TR3), by index into `ok`: a wasm return goes back to
/// the caller whatever ra holds. A tail jump is fine when its callee
/// writes ra before reading it (the rest of the same function, which
/// reloads ra from the frame). A block entered at computed offsets runs
/// only expanded into the function that jumps into it (design §4), where
/// its jumps are checked.
fn foreign_returns(program: &Program, ok: &[Translation]) -> BTreeMap<usize, LiftError> {
    let index: BTreeMap<(&str, u32), usize> = ok.iter().enumerate().map(|(i, t)| ((t.func.container.as_str(), t.func.addr), i)).collect();
    let mut out = BTreeMap::new();
    for (i, t) in ok.iter().enumerate() {
        if t.func.sources.contains(&Source::ComputedEntry) {
            continue;
        }
        let reads_ra = |a: u32| match program.locate(&t.func.container, a) {
            Some(Located::Code(c)) => index.get(&(c.as_str(), a)).is_none_or(|&j| ok[j].facts.entry_ra),
            _ => true,
        };
        if let Some((pc, callee, holds)) = t.foreign_returns.iter().find(|(_, c, _)| c.is_none_or(reads_ra)) {
            let jr_ra = t.graph.nodes.get(&Node::Normal(*pc)).is_some_and(|d| d.insn.op == decode::Op::Jr && d.insn.rs == 31);
            let what = match callee {
                None if jr_ra => "jr ra".to_string(),
                None => "a tail jump".to_string(),
                Some(a) => format!("a tail jump to 0x{a:08X}, which returns through ra,"),
            };
            let message = format!("{what} where {holds}, not the caller's return address: the translation returns to the caller");
            out.insert(i, LiftError { pc: *pc, message });
        }
    }
    out
}

/// Translate to a fixed point over function starts and inline functions.
///
/// A function with no C is inlined at its call sites (and fall-throughs)
/// on evidence of register passing (D33): a caller sets, on some path
/// before the call, registers the callee reads on entry that the o32 ABI
/// doesn't pass, or reads after the call registers the ABI doesn't return
/// that the callee leaves set (hand-written asm that passes values in
/// temporaries, callee-saved registers or hi/lo). Decompiled C is
/// ABI-conformant by construction, so its entry reads are uninitialized
/// locals, not arguments. Where a call can't carry them the lift fails
/// ([`unpassed`]).
pub fn translate_all(program: &mut Program, originals: &Originals, containers: &[String]) -> Outcome {
    // In address order: one an earlier one's range already holds is only
    // a start.
    for (c, a) in early_entries(program, originals) {
        if program.code(&c).is_some_and(|code| code.in_text(a)) || program.extend_text(&c, a, 4 * EARLY_ENTRY) {
            program.add(&c, a, None, Source::DataPointer, true);
        }
    }
    let mut rounds = 0;
    let mut inline: BTreeMap<(String, u32), u32> = BTreeMap::new();
    let mut inline_reasons: BTreeMap<(String, u32), String> = BTreeMap::new();
    // Registers each function's callers were seen to pass it.
    let mut takes: BTreeMap<(String, u32), u64> = BTreeMap::new();
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
        // A block only a code pointer reaches that reads registers the ABI
        // doesn't pass is entered by a computed jump, part way.
        for t in results.iter().flatten() {
            let pointer_only = t.func.sources.iter().all(|&s| matches!(s, Source::CodePointer | Source::ComputedEntry));
            if pointer_only && t.facts.entry_reads != 0 {
                added |= program.add_source(&t.func.container, t.func.addr, Source::ComputedEntry);
            }
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
                    let passed = (cf.entry_reads | cf.entry_s_reads) & pre_defs;
                    if passed != 0 {
                        *takes.entry((c.clone(), target)).or_default() |= passed;
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
                    let returned = regs & (cf.defs & !S_REGS | cf.s_changed);
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
            // nested functions), so it is translated, and so is the parent,
            // which expands it. Every translation that reads v0 on entry
            // takes it from a translated caller that doesn't expand it
            // (one that passes v0 on, or a call that isn't expanded) this
            // way, as the hardware would.
            let chain: BTreeSet<String> = out
                .ok
                .iter()
                .filter(|t| t.facts.entry_reads & (1 << 2) != 0)
                .map(|t| program::call_symbol(&program.main, &t.func.container, t.func.addr))
                .collect();
            let mut dropped: BTreeMap<usize, LiftError> = BTreeMap::new();
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
                for (i, r) in redone {
                    match r {
                        Ok(t) => out.ok[i] = t,
                        Err(e) => {
                            dropped.insert(i, e);
                        }
                    }
                }
            }
            for (i, e) in unpassed(program, &out.ok, &takes) {
                dropped.entry(i).or_insert(e);
            }
            for (i, e) in foreign_returns(program, &out.ok) {
                dropped.entry(i).or_insert(e);
            }
            let ok = std::mem::take(&mut out.ok);
            for (i, t) in ok.into_iter().enumerate() {
                match dropped.remove(&i) {
                    Some(e) => out.failed.push((t.func, e)),
                    None => out.ok.push(t),
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
        let Node::Slot(b) = *n else { continue };
        let table = match &d.transfer {
            Transfer::Jump(Dest::Tail(Callee::Bios { table })) => *table,
            Transfer::Call { callee: Callee::Bios { table }, .. } => *table,
            Transfer::Call { next: Dest::Tail(Callee::Bios { table }), .. } => *table,
            _ => continue,
        };
        out.push(BiosCall { pc: n.pc(), table, func: t1_after_slot(&t.graph, &t.facts, b) });
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
        r.post_call_reads = t
            .facts
            .post_call_reads
            .iter()
            .filter(|(_, regs, _)| regs & !S_REGS != 0)
            .map(|(pc, regs, _)| format!("0x{pc:08X}: {}", reg_list(regs & !S_REGS)))
            .collect();
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

    const BASE: u32 = 0x8001_0000;

    /// Translate `words` placed at [`BASE`], with function starts at the
    /// given (address, size) pairs.
    fn lift_words(words: &[u32], starts: &[(u32, u32)]) -> Outcome {
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
                    starts: starts.iter().map(|&(a, n)| (a, start(n))).collect(),
                },
            )]
            .into(),
        };
        let originals = Originals::from_images(vec![("test".into(), BASE, code)]);
        translate_all(&mut program, &originals, &["test".into()])
    }

    /// Accesses at a constant device address are volatile and fenced, so
    /// clang keeps both of two equal stores and keeps RAM accesses on their
    /// side; RAM stays plain memory.
    #[test]
    fn device_accesses_are_volatile() {
        let words = [
            0x3C08_1F80, // lui t0, 0x1F80
            0x2409_0001, // addiu t1, zero, 1
            0x3C0A_8001, // lui t2, 0x8001
            0xAD09_1810, // sw t1, 0x1810(t0)
            0xAD09_1810, // sw t1, 0x1810(t0)
            0xAD49_0100, // sw t1, 0x100(t2)
            0x8D0B_1814, // lw t3, 0x1814(t0)
            0,           // nop
            0xAD4B_0104, // sw t3, 0x104(t2)
            0x03E0_0008, // jr ra
            0,           // nop
        ];
        let out = lift_words(&words, &[(BASE, 4 * words.len() as u32)]);
        assert!(out.failed.is_empty(), "{:?}", out.failed);
        let c = &out.ok[0].c;
        assert_eq!(c.matches("PSXW_FENCE(); PSXW_SWV(r8 + 0x1810u, r9); PSXW_FENCE();").count(), 2, "{c}");
        assert!(c.contains("PSXW_FENCE(); r11 = (uint32_t)PSXW_LWV(r8 + 0x1814u); PSXW_FENCE();"), "{c}");
        assert!(c.contains("PSXW_SW(r10 + 0x100u, r9);") && c.contains("PSXW_SW(r10 + 0x104u, r11);"), "{c}");
    }

    /// A callee inlined for the registers it reads ends in a tail jump to
    /// another function: that function returns to the inlined call's return
    /// site, not out of the caller.
    #[test]
    fn tail_jump_out_of_inlined_callee() {
        let (other, inlined) = (BASE + 0x24, BASE + 0x2C);
        let words = [
            0x27BD_FFE8,                               // addiu sp, sp, -24
            0xAFBF_0010,                               // sw ra, 16(sp)
            0x2408_0005,                               // addiu t0, zero, 5
            0x0C00_0000 | inlined >> 2 & 0x03FF_FFFF,  // jal inlined
            0,                                         // nop
            0x2442_0001,                               // addiu v0, v0, 1
            0x8FBF_0010,                               // lw ra, 16(sp)
            0x03E0_0008,                               // jr ra
            0x27BD_0018,                               // addiu sp, sp, 24
            0x03E0_0008,                               // other: jr ra
            0x2482_0007,                               //   addiu v0, a0, 7
            0x0100_2021,                               // inlined: addu a0, t0, zero
            0x0800_0000 | other >> 2 & 0x03FF_FFFF,    //   j other
            0,                                         //   nop
        ];
        let out = lift_words(&words, &[(BASE, 0x24), (other, 8), (inlined, 0xC)]);
        assert!(out.failed.is_empty(), "{:?}", out.failed);
        assert!(out.inline.contains_key(&("test".to_string(), inlined)), "{:?}", out.inline);
        let t = out.ok.iter().find(|t| t.func.addr == BASE).expect("translated");
        let ret = BASE + 0x14;
        assert!(!t.c.contains(&format!("return psxw_f_{other:08X}(")), "{}", t.c);
        assert!(t.c.contains(&format!("t_ = r31; switch (t_) {{ case 0x{ret:08X}u: goto L_{ret:08X};")), "{}", t.c);
        assert_eq!(t.facts.entry_reads, 0, "v0 at the return site is the tail callee's");
    }

    /// Register passing is seen across a branch and a join, through hi/lo,
    /// and in a callee-saved register a callee returns: both callees are
    /// expanded into the caller.
    #[test]
    fn register_passing_evidence() {
        let (takes, gives) = (BASE + 0x3C, BASE + 0x48);
        let words = [
            0x27BD_FFE8,                             // addiu sp, sp, -24
            0xAFBF_0010,                             // sw ra, 16(sp)
            0x2408_0005,                             // addiu t0, zero, 5
            0x1080_0002,                             // beqz a0, 1f
            0,                                       // nop
            0x24A5_0001,                             // addiu a1, a1, 1
            0x0085_0018,                             // 1: mult a0, a1
            0x0C00_0000 | takes >> 2 & 0x03FF_FFFF,  // jal takes
            0,                                       // nop
            0x0C00_0000 | gives >> 2 & 0x03FF_FFFF,  // jal gives
            0,                                       // nop
            0x0050_1021,                             // addu v0, v0, s0
            0x8FBF_0010,                             // lw ra, 16(sp)
            0x03E0_0008,                             // jr ra
            0x27BD_0018,                             // addiu sp, sp, 24
            0x0000_1010,                             // takes: mfhi v0
            0x03E0_0008,                             //   jr ra
            0x0048_1021,                             //   addu v0, v0, t0
            0x03E0_0008,                             // gives: jr ra
            0x2410_0001,                             //   addiu s0, zero, 1
        ];
        let out = lift_words(&words, &[(BASE, 0x3C), (takes, 0xC), (gives, 8)]);
        assert!(out.failed.is_empty(), "{:?}", out.failed);
        let why = |a: u32| out.inline_reasons.get(&("test".to_string(), a)).cloned().unwrap_or_default();
        assert!(why(takes).starts_with("reads t0,hi on entry"), "{}", why(takes));
        assert!(why(gives).starts_with("writes s0"), "{}", why(gives));
    }

    /// A call the translation can't expand (`jalr` to a known function)
    /// passes t0, which the callee reads: the lift fails instead of the
    /// callee reading 0.
    #[test]
    fn unexpanded_register_passing_fails() {
        let callee = BASE + 0x28;
        let words = [
            0x27BD_FFE8,                    // addiu sp, sp, -24
            0xAFBF_0010,                    // sw ra, 16(sp)
            0x3C19_0000 | callee >> 16,     // lui t9, %hi(callee)
            0x2739_0000 | callee & 0xFFFF,  // addiu t9, t9, %lo(callee)
            0x2408_0005,                    // addiu t0, zero, 5
            0x0320_F809,                    // jalr t9
            0,                              // nop
            0x8FBF_0010,                    // lw ra, 16(sp)
            0x03E0_0008,                    // jr ra
            0x27BD_0018,                    // addiu sp, sp, 24
            0x03E0_0008,                    // callee: jr ra
            0x0100_1021,                    //   addu v0, t0, zero
        ];
        let out = lift_words(&words, &[(BASE, 0x28), (callee, 8)]);
        let failed: Vec<String> = out.failed.iter().filter(|(f, _)| f.addr == callee).map(|(_, e)| e.message.clone()).collect();
        assert!(matches!(failed.as_slice(), [m] if m.starts_with("reads t0 on entry")), "{:?}", out.failed);
    }

    /// `la ra, L; j f`: f returns to L, not to the caller, so the lift
    /// fails instead of the tail call returning to the caller.
    #[test]
    fn tail_jump_with_another_return_address_fails() {
        let (f, l) = (BASE + 0x10, BASE + 0x40);
        let words = [
            0x3C1F_0000 | l >> 16,            // lui ra, %hi(L)
            0x27FF_0000 | l & 0xFFFF,         // addiu ra, ra, %lo(L)
            0x0800_0000 | f >> 2 & 0x03FF_FFFF, // j f
            0,                                // nop
            0x03E0_0008,                      // f: jr ra
            0,                                //   nop
        ];
        let out = lift_words(&words, &[(BASE, 0x10), (f, 8)]);
        let failed: Vec<&str> = out.failed.iter().filter(|(t, _)| t.addr == BASE).map(|(_, e)| e.message.as_str()).collect();
        assert!(matches!(failed.as_slice(), [m] if m.contains(&format!("ra holds 0x{l:08X}"))), "{:?}", out.failed);
    }

    /// A path that leaves with ra loaded from elsewhere than where it was
    /// saved: the `jr ra` returns only when ra is the caller's, and traps
    /// otherwise.
    #[test]
    fn return_with_another_return_address_on_one_path_is_guarded() {
        let words = [
            0x27BD_FFE8, // addiu sp, sp, -24
            0xAFBF_0010, // sw ra, 16(sp)
            0x1080_0004, // beqz a0, 1f
            0,           // nop
            0x8FBF_0010, // lw ra, 16(sp)
            0x1000_0003, // b 2f
            0,           // nop
            0x8CBF_0000, // 1: lw ra, 0(a1)
            0,           // nop
            0x03E0_0008, // 2: jr ra
            0x27BD_0018, // addiu sp, sp, 24
        ];
        let out = lift_words(&words, &[(BASE, 4 * words.len() as u32)]);
        assert!(out.failed.is_empty(), "{:?}", out.failed);
        let c = &out.ok[0].c;
        assert!(c.contains(&format!("case 0x00000000u: return ((uint64_t)r3 << 32) | r2; default: psxw_bad_jump(0x{:08X}u, t_);", BASE + 0x24)), "{c}");
    }

    /// An exception handler's own epilogue restores ra from the interrupted
    /// context and returns with `jr k0; rfe`: a return from the exception,
    /// not a tail call to the interrupted code.
    #[test]
    fn handler_returns_from_exception() {
        let words = [
            0x3C1A_8001, // lui k0, 0x8001
            0x8F5F_007C, // lw ra, 0x7C(k0)
            0x8F5A_0080, // lw k0, 0x80(k0)
            0,           // nop
            0x0340_0008, // jr k0
            0x4200_0010, // rfe
        ];
        let out = lift_words(&words, &[(BASE, 4 * words.len() as u32)]);
        assert!(out.failed.is_empty(), "{:?}", out.failed);
        let c = &out.ok[0].c;
        assert!(c.contains("psxw_return_from_exception(); return 0;") && !c.contains("psxw_call_indirect"), "{c}");
    }

    /// `bltzal ra` tests ra as it was before the link overwrites it.
    #[test]
    fn link_branch_tests_ra_before_linking() {
        let words = [
            0x03E0_8021, // addu s0, ra, zero
            0x07F0_0003, // bltzal ra, callee
            0,           // nop
            0x0200_0008, // jr s0
            0,           // nop
            0x03E0_0008, // callee: jr ra
            0,           // nop
        ];
        let out = lift_words(&words, &[(BASE, 0x14), (BASE + 0x14, 8)]);
        assert!(out.failed.is_empty(), "{:?}", out.failed);
        let c = &out.ok.iter().find(|t| t.func.addr == BASE).expect("translated").c;
        let test = c.find("c_ = (int32_t)r31 < 0;").expect(c);
        let link = c.find(&format!("r31 = 0x{:08X}u;", BASE + 0xC)).expect(c);
        assert!(test < link && c.contains("if (c_) {"), "{c}");
    }

    /// A BIOS trampoline sets its function number with `addiu` or `ori`.
    #[test]
    fn bios_call_numbers() {
        for li_t1 in [0x2409_0010 /* addiu t1, zero, 0x10 */, 0x3409_0010 /* ori t1, zero, 0x10 */] {
            // addiu t2, zero, 0xB0 ; jr t2 ; li t1, 0x10
            let out = lift_words(&[0x240A_00B0, 0x0140_0008, li_t1], &[(BASE, 12)]);
            assert!(out.failed.is_empty(), "{:?}", out.failed);
            let calls: Vec<(u8, Option<u32>)> = bios_calls(&out.ok[0]).iter().map(|c| (c.table, c.func)).collect();
            assert_eq!(calls, [(0xB0, Some(0x10))], "{li_t1:08X}");
        }
    }

    /// A GCC nested function reads its parent's frame through v0, which the
    /// parent sets before the call. A call that isn't expanded (here
    /// `bgezal`; a `jal` would expand the child) passes it.
    #[test]
    fn static_chain() {
        const BASE: u32 = 0x8001_0000;
        let child = BASE + 0x28;
        let words: [u32; 13] = [
            0x27BD_FFE8, // addiu sp, sp, -24
            0xAFBF_0010, // sw ra, 16(sp)
            0x03A0_1021, // addu v0, sp, zero (the chain)
            0x0411_0006, // bgezal zero, child
            0,           // nop
            0,           // nop
            0,           // nop
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

    /// A function table entry two instructions before a text range (code
    /// the split filed as data, bof4's `ovl_4504`) becomes a start and moves
    /// the range; one at a string there doesn't.
    #[test]
    fn early_entry_before_text() {
        const BASE: u32 = 0x8001_0000;
        let words: [u32; 15] = [
            BASE + 0x10, // table: the string,
            BASE + 0x34, // a known function,
            BASE + 0x18, // the entry
            0,
            0x0064_3825, // "%8d\0" (decodes as or a3, v1, a0)
            0x0064_3825,
            0x3C02_8011, // entry: lui v0, 0x8011
            0x9442_4900, // lhu v0, 0x4900(v0)
            0x1040_0002, // text: beqz v0, 1f
            0,
            0x2402_0001, // addiu v0, zero, 1
            0x03E0_0008, // 1: jr ra
            0,
            0x03E0_0008, // other: jr ra
            0,
        ];
        let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let start = |name: &str, size: u32| Start { name: Some(name.into()), size: Some(size), sources: [Source::Symbol].into(), boundary: true };
        let mut program = Program {
            main: "test".into(),
            containers: [(
                "test".to_string(),
                Code {
                    id: "test".into(),
                    text: vec![BASE + 0x20..BASE + 0x3C],
                    data: vec![BASE..BASE + 0x20],
                    starts: [(BASE + 0x20, start("split", 0x14)), (BASE + 0x34, start("other", 8))].into(),
                },
            )]
            .into(),
        };
        let originals = Originals::from_images(vec![("test".into(), BASE, code)]);
        let out = translate_all(&mut program, &originals, &["test".into()]);
        assert!(out.failed.is_empty(), "{:?}", out.failed);
        assert!(out.ok.iter().any(|t| t.func.addr == BASE + 0x18));
        assert!(!out.ok.iter().any(|t| t.func.addr == BASE + 0x10));
        assert_eq!(program.code("test").unwrap().text[0].start, BASE + 0x18);
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

    /// Reads whose result nothing uses still happen: a driver drains an I/O
    /// FIFO with them. A load into zero and a loop of discarded loads are
    /// volatile reads; a used load stays an ordinary one.
    #[test]
    fn discarded_loads_are_kept() {
        const BASE: u32 = 0x8001_0000;
        let words: [u32; 10] = [
            0x8486_0000, // lh a2, 0(a0) (only a result: sign-extended still)
            0x9080_0000, // lbu zero, 0(a0)
            0x2405_0008, // addiu a1, zero, 8
            0x9082_0000, // 1: lbu v0, 0(a0) (discarded)
            0x24A5_FFFF, // addiu a1, a1, -1
            0x14A0_FFFD, // bnez a1, 1b
            0,           // nop
            0x9083_0000, // lbu v1, 0(a0)
            0x03E0_0008, // jr ra
            0x0060_1021, // addu v0, v1, zero
        ];
        let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let start = Start { name: None, size: Some(code.len() as u32), sources: [Source::Symbol].into(), boundary: true };
        let mut program = Program {
            main: "test".into(),
            containers: [(
                "test".to_string(),
                Code { id: "test".into(), text: vec![BASE..BASE + code.len() as u32], data: vec![], starts: [(BASE, start)].into() },
            )]
            .into(),
        };
        let originals = Originals::from_images(vec![("test".into(), BASE, code)]);
        let out = translate_all(&mut program, &originals, &["test".into()]);
        let c = &out.ok.iter().find(|t| t.func.addr == BASE).expect("translated").c;
        assert_eq!(c.matches("PSXW_LBV(").count(), 2, "{c}");
        assert!(c.contains("(uint32_t)(int32_t)(int16_t)PSXW_LHV("), "{c}");
        assert!(c.contains("r3 = (uint32_t)PSXW_LB("), "{c}");
    }

    /// Hand-written code that jumps through a table of addresses inside its
    /// own text, indexed by a byte offset it loads: the entries are local
    /// labels, and the table's address is no function.
    #[test]
    fn embedded_jump_table() {
        const BASE: u32 = 0x8001_0000;
        let table = BASE + 0x18;
        let (t0, t1) = (BASE + 0x20, BASE + 0x28);
        let words: [u32; 12] = [
            0x8C8C_0000,                      // lw t4, 0(a0) (a byte offset)
            0x3C0D_0000 | (table >> 16),      // lui t5, %hi(table)
            0x35AD_0000 | (table & 0xFFFF),   // ori t5, t5, %lo(table)
            0x018D_6821,                      // addu t5, t4, t5
            0x8DAC_0000,                      // lw t4, 0(t5)
            0x0180_0008,                      // jr t4
            t0,                               // table: t0
            t1,                               //        t1
            0x03E0_0008,                      // t0: jr ra
            0x2402_0001,                      //     addiu v0, zero, 1
            0x03E0_0008,                      // t1: jr ra
            0x2402_0002,                      //     addiu v0, zero, 2
        ];
        let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let start = Start { name: None, size: Some(code.len() as u32), sources: [Source::Symbol].into(), boundary: true };
        let mut program = Program {
            main: "test".into(),
            containers: [(
                "test".to_string(),
                Code { id: "test".into(), text: vec![BASE..BASE + code.len() as u32], data: vec![], starts: [(BASE, start)].into() },
            )]
            .into(),
        };
        let originals = Originals::from_images(vec![("test".into(), BASE, code)]);
        let out = translate_all(&mut program, &originals, &["test".into()]);
        assert!(out.failed.is_empty(), "{:?}", out.failed);
        assert_eq!(out.ok.len(), 1, "the table's address became a function");
        let c = &out.ok[0].c;
        assert!(c.contains(&format!("case 0x{t0:08X}u: goto L_{t0:08X};")), "{c}");
        assert!(c.contains(&format!("case 0x{t1:08X}u: goto L_{t1:08X};")), "{c}");
        assert!(!c.contains("psxw_call_indirect"), "{c}");
    }
}
