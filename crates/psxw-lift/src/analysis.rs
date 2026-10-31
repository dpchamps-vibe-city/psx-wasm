//! Dataflow over a function's [`Graph`]: constant propagation (hardware
//! access sites, code addresses, `gp` values, BIOS dispatch checks),
//! liveness (registers a function reads that the o32 ABI doesn't pass), and
//! load-delay hazards.

use crate::decode::{Insn, Op};
use crate::graph::{Callee, Dest, Graph, Node, Transfer};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Registers the callee may clobber, as the o32 ABI defines.
pub const CALLER_SAVED: u64 = (1 << 1) | (0xFF << 2) | (0xFF << 8) | (1 << 24) | (1 << 25) | HI | LO;
pub const HI: u64 = 1 << 32;
pub const LO: u64 = 1 << 33;
/// Registers a function may legitimately read on entry: arguments, `gp`,
/// `sp`, `ra`, and callee-saved registers (read only to be saved).
const ENTRY_OK: u64 = (0xF << 4) | (1 << 28) | (1 << 29) | (1 << 31) | (0xFF << 16) | (1 << 30);
/// The callee-saved registers, s0-s7 and fp.
pub const S_REGS: u64 = (0xFF << 16) | (1 << 30);

pub type Consts = [Option<u32>; 32];

#[derive(Debug, Clone, Default)]
pub struct Facts {
    /// Register constants on entry to each node.
    pub consts: BTreeMap<Node, Consts>,
    /// Loads and stores at constant I/O addresses: (pc, address).
    pub io: Vec<(u32, u32)>,
    /// Loads, stores and calls at constant BIOS kernel RAM addresses.
    pub kernel: Vec<(u32, u32)>,
    /// Code-range constants built by `addiu`/`ori` (function pointers).
    pub code_consts: BTreeSet<u32>,
    /// Constants written to `gp`.
    pub gp_writes: BTreeSet<u32>,
    /// Registers read before written on some path from entry, outside
    /// what the ABI passes. Bit 32/33 are hi/lo.
    pub entry_reads: u64,
    /// Callee-saved registers whose entry value is used for more than
    /// saving it.
    pub entry_s_reads: u64,
    /// ra on entry is read on some path (to return through, save, or pass
    /// to a tail callee) before anything writes it.
    pub entry_ra: bool,
    /// Calls after which a caller-saved register (other than v0/v1) or a
    /// callee-saved one is read before being written: (pc of the call,
    /// registers, direct callee).
    pub post_call_reads: Vec<(u32, u64, Option<u32>)>,
    /// Direct callees whose result (v0) is read after a call to them.
    pub result_uses: BTreeSet<u32>,
    /// Every way out leaves v0 as a call set it: the function returns
    /// what its last callee returned, whatever its C declares.
    pub returns_callee_v0: bool,
    /// Loads whose next instruction reads the loaded register.
    pub hazards: BTreeMap<Node, u8>,
    /// Registers any instruction of the function writes, hi/lo included.
    pub defs: u64,
    /// Callee-saved registers that may not hold their entry value where the
    /// function returns: hand-written code that returns a value in one.
    pub s_changed: u64,
    /// Direct calls and fall-throughs to code addresses: (pc, target,
    /// registers set on some path from the entry to it, hi/lo and earlier
    /// calls' v0/v1 included).
    pub call_sites: Vec<(u32, u32, u64)>,
    /// Loads whose result nothing reads (or that load into `zero`). They
    /// still happen on hardware, and from an I/O register a read pops a
    /// FIFO, so they are emitted as volatile reads clang keeps.
    pub dead_loads: BTreeSet<Node>,
    /// Loads and stores at a constant address that isn't RAM or the
    /// scratchpad: device registers and the other bus regions. Each is
    /// emitted volatile, between compiler barriers, so clang keeps it,
    /// doesn't merge it, and keeps it in order with every other access.
    /// In a function that reaches registers through pointers in data, also
    /// every access through a base that isn't a constant or the stack
    /// pointer.
    pub device: BTreeSet<Node>,
}

/// `through_pointers`: the function reaches device registers through
/// pointers in data ([`crate::program::Program::through_pointers`]).
pub fn analyze(g: &Graph, in_code: &dyn Fn(u32) -> bool, through_pointers: bool) -> Facts {
    let mut f = Facts::default();
    constants(g, in_code, through_pointers, &mut f);
    liveness(g, &mut f);
    hazards(g, &mut f);
    sites(g, &mut f);
    f.returns_callee_v0 = returns_callee_v0(g);
    f
}

/// Forward over the graph: does v0 hold a callee's result on every exit
/// (a return, or a tail call, which returns the callee's own)?
fn returns_callee_v0(g: &Graph) -> bool {
    use crate::graph::Dest;
    // Per node: v0 on entry to it came from a call on every path so far.
    let mut from_call: BTreeMap<Node, bool> = BTreeMap::new();
    let mut work = VecDeque::from([(Node::Normal(g.entry), false)]);
    while let Some((n, v)) = work.pop_front() {
        let v = from_call.get(&n).map_or(v, |&c| c && v);
        if from_call.get(&n) == Some(&v) {
            continue;
        }
        from_call.insert(n, v);
        let d = &g.nodes[&n];
        let after = v && d.insn.writes() != Some(2);
        match &d.transfer {
            Transfer::Call { next: Dest::Local(a), .. } => work.push_back((Node::Normal(*a), true)),
            _ => work.extend(g.succs(n).into_iter().map(|s| (s, after))),
        }
    }
    let mut exits = 0;
    for (n, &v) in &from_call {
        let d = &g.nodes[n];
        let ok = match &d.transfer {
            Transfer::Return | Transfer::InlineReturn { .. } => v && d.insn.writes() != Some(2),
            Transfer::Jump(Dest::Tail(_)) | Transfer::FallOff(_) | Transfer::Call { next: Dest::Tail(_), .. } => true,
            _ => continue,
        };
        if !ok {
            return false;
        }
        exits += 1;
    }
    exits > 0
}

/// Registers an instruction writes: its GPR, and hi/lo.
fn insn_defs(i: &Insn) -> u64 {
    let hilo = match i.op {
        Op::Mthi => HI,
        Op::Mtlo => LO,
        Op::Mult | Op::Multu | Op::Div | Op::Divu => HI | LO,
        _ => 0,
    };
    i.writes().map_or(0, |r| 1u64 << r) | hilo
}

/// Forward over the graph, from the entry: `state` on entry to each node,
/// where `step` gives a node's state on exit from its state on entry and
/// `join` merges two states (it must only lose information, so the walk
/// ends).
fn forward<T: Clone + PartialEq>(
    g: &Graph,
    succs: &dyn Fn(Node) -> Vec<Node>,
    step: impl Fn(Node, &T) -> T,
    join: impl Fn(&T, &T) -> T,
    entry: T,
) -> BTreeMap<Node, T> {
    let mut state = BTreeMap::from([(Node::Normal(g.entry), entry)]);
    let mut work = VecDeque::from([Node::Normal(g.entry)]);
    while let Some(n) = work.pop_front() {
        let out = step(n, &state[&n]);
        for s in succs(n) {
            let new = state.get(&s).map_or_else(|| out.clone(), |old| join(old, &out));
            if state.get(&s) != Some(&new) {
                state.insert(s, new);
                work.push_back(s);
            }
        }
    }
    state
}

/// A value as the return-address check sees it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Val {
    /// ra on entry: the caller's return address.
    Ra,
    Const(u32),
    /// sp on entry plus an offset.
    Sp(u32),
    /// Not ra on entry, nor anything derived from sp: another register's
    /// value on entry (plus a constant), or a word loaded from memory the
    /// function didn't store ra to.
    Other,
    Top,
}

/// Registers, and memory words by address: in the stack (`true`, by offset
/// from sp on entry) or at a constant address (a global, the scratchpad).
#[derive(Clone, PartialEq, Debug)]
struct Frame {
    regs: [Val; 32],
    words: BTreeMap<(bool, u32), Val>,
}

fn join_val(a: Val, b: Val) -> Val {
    use Val::*;
    match (a, b) {
        _ if a == b => a,
        (Const(_) | Other, Const(_) | Other) => Other,
        _ => Top,
    }
}

/// One node's effect on [`Frame`]: constants and sp offsets through
/// `addiu`/`addu`/`subu`/`ori`/moves, words stored and loaded back, a
/// call's link, and a call clobbering the temporaries (an inlined one, the
/// registers its code writes; it keeps sp). Other functions are taken not
/// to write the words this one stores. A load of ra from a stack word the
/// function never stored, at or above sp on entry, is in its caller's
/// frame: the function shares that frame (it is the rest of a function
/// that falls or calls into it), and the word is that function's saved ra.
fn frame_step(g: &Graph, n: Node, st: &Frame, inlined_defs: u64) -> Frame {
    use Val::*;
    let d = &g.nodes[&n];
    let i = &d.insn;
    let mut out = st.clone();
    let v = |r: u8| st.regs[r as usize];
    let add = |a: Val, b: Val| match (a, b) {
        (Const(x), Const(y)) => Const(x.wrapping_add(y)),
        (Sp(x), Const(y)) | (Const(y), Sp(x)) => Sp(x.wrapping_add(y)),
        (x, Const(0)) | (Const(0), x) => x,
        (Other, Const(_)) | (Const(_), Other) => Other,
        _ => Top,
    };
    let at = match v(i.rs) {
        Sp(o) => Some((true, o.wrapping_add(i.simm as u32))),
        Const(c) => Some((false, c.wrapping_add(i.simm as u32))),
        _ => None,
    };
    let value = match i.op {
        Op::Lui => Const(i.uimm << 16),
        Op::Addiu | Op::Addi => add(v(i.rs), Const(i.simm as u32)),
        Op::Addu | Op::Add => add(v(i.rs), v(i.rt)),
        Op::Subu | Op::Sub => match (v(i.rs), v(i.rt)) {
            (x, Const(y)) => add(x, Const(y.wrapping_neg())),
            _ => Top,
        },
        Op::Ori => match v(i.rs) {
            Const(x) => Const(x | i.uimm),
            x if i.uimm == 0 => x,
            _ => Top,
        },
        Op::Or => match (v(i.rs), v(i.rt)) {
            (Const(x), Const(y)) => Const(x | y),
            (x, Const(0)) | (Const(0), x) => x,
            _ => Top,
        },
        Op::Jal | Op::Jalr | Op::Bltzal | Op::Bgezal => Const(n.pc().wrapping_add(8)),
        Op::Lw => match (v(i.rs), at.map(|a| (a, st.words.get(&a)))) {
            (_, Some((_, Some(&x)))) => x,
            (_, Some(((true, o), None))) if i.rt == 31 && (o as i32) >= 0 => Ra,
            (Sp(_) | Const(_) | Other, _) => Other,
            _ => Top,
        },
        _ => Top,
    };
    if let (true, Some((stack, a))) = (i.op.is_store(), at) {
        // The words the store's bytes touch hold it (a word store) or no
        // value known; `swl`/`swr` stay in their aligned word.
        let (a, size) = match i.op {
            Op::Sb => (a, 1),
            Op::Sh => (a, 2),
            Op::Swl | Op::Swr => (a & !3, 4),
            _ => (a, 4),
        };
        out.words.retain(|&(s, k), _| s != stack || !((k.wrapping_sub(a) as i32) < size && (a.wrapping_sub(k) as i32) < 4));
        out.words.insert((stack, a & !3), if i.op == Op::Sw { v(i.rt) } else { Top });
    }
    if let Some(r) = i.writes() {
        out.regs[r as usize] = value;
    }
    let clobbered = match &d.transfer {
        Transfer::Call { .. } | Transfer::CondCall { .. } => CALLER_SAVED & !(1 << 31),
        _ if inlined_call(g, n) => inlined_defs & !(1 << 29),
        _ => 0,
    };
    for r in 1..32 {
        if clobbered & (1 << r) != 0 {
            out.regs[r] = Top;
        }
    }
    out.regs[0] = Const(0);
    out
}

/// Returns and tail jumps where ra may not be the caller's return address
/// (TR3): `jr ra` returns from the translation, and a tail callee's `jr ra`
/// returns from it too, so either goes back to the caller whatever ra
/// holds. `la ra, L; j f`, a saved ra written over, and a longjmp's `lw ra`
/// from its buffer show here. A tail callee that writes ra before it reads
/// it (the rest of a function, which reloads it from the frame) doesn't
/// care what ra holds. A jump through another register holding ra on entry
/// returns. `skip`: tail jumps that never return (`ReturnFromException`).
/// Falling or calling into the next function shares the frame with it,
/// and that is checked on its own; a `jr ra` in inlined code dispatches on
/// ra, failing at run time.
pub fn foreign_returns(g: &Graph, skip: &BTreeSet<u32>) -> ForeignReturns {
    let mut regs = [Val::Other; 32];
    regs[0] = Val::Const(0);
    regs[29] = Val::Sp(0);
    regs[31] = Val::Ra;
    let entry = Frame { regs, words: BTreeMap::new() };
    let join = |a: &Frame, b: &Frame| {
        let mut out = a.clone();
        for r in 0..32 {
            out.regs[r] = join_val(a.regs[r], b.regs[r]);
        }
        for (k, &x) in &b.words {
            out.words.insert(*k, a.words.get(k).map_or(Val::Top, |&y| join_val(x, y)));
        }
        for (k, x) in out.words.iter_mut() {
            if !b.words.contains_key(k) {
                *x = Val::Top;
            }
        }
        out
    };
    let inlined_defs = g.nodes.iter().filter(|(n, _)| g.inlined(n.pc())).fold(0u64, |a, (_, d)| a | insn_defs(&d.insn));
    let states = forward(g, &|n| o32_succs(g, n), |n, s| frame_step(g, n, s, inlined_defs), join, entry);
    let mut out = ForeignReturns::default();
    for (&n, st) in &states {
        let Node::Slot(b) = n else { continue };
        if g.tail_returns(n) {
            continue;
        }
        let callee = |c: &Callee| match c {
            Callee::Direct { addr, .. } => Some(*addr),
            _ => None,
        };
        let tails: Vec<Option<u32>> = match &g.nodes[&n].transfer {
            Transfer::Return => vec![None],
            // `jr` through a register holding ra on entry (read before the
            // slot runs) returns.
            Transfer::Jump(Dest::Tail(Callee::Indirect { reg })) if st.regs[*reg as usize] == Val::Ra => Vec::new(),
            Transfer::Jump(Dest::Tail(c)) if !skip.contains(&b) => vec![callee(c)],
            Transfer::Cond { taken, fall } => [taken, fall]
                .into_iter()
                .filter_map(|d| match d {
                    Dest::Tail(c) => Some(callee(c)),
                    Dest::Local(_) => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        if tails.is_empty() {
            continue;
        }
        let ra = frame_step(g, n, st, inlined_defs).regs[31];
        let back = matches!(ra, Val::Const(c) if g.inline_returns.contains(&c));
        if matches!(g.nodes[&n].transfer, Transfer::Return) && (ra == Val::Top || back) {
            out.guarded.insert(b);
            continue;
        }
        let holds = match ra {
            Val::Ra => continue,
            Val::Const(c) => format!("ra holds 0x{c:08X}"),
            Val::Other => "ra was loaded from memory it wasn't saved to".into(),
            _ => "ra may not hold what it held on entry, or what was saved of it".into(),
        };
        out.wrong.extend(tails.into_iter().map(|t| (b, t, holds.clone())));
    }
    out
}

/// [`foreign_returns`]' findings.
#[derive(Debug, Clone, Default)]
pub struct ForeignReturns {
    /// Returns and tail jumps where ra isn't the caller's return address:
    /// (branch pc, the tail callee when it is a known function, what ra
    /// holds).
    pub wrong: Vec<(u32, Option<u32>, String)>,
    /// `jr ra` where ra is the caller's on some paths and may not be on
    /// others (a path that leaves without restoring it): it dispatches on
    /// ra as an inlined callee's `jr ra` does, returning only when ra is
    /// the caller's, and trapping at run time otherwise.
    pub guarded: BTreeSet<u32>,
}

/// Successors for callee-saved registers read on entry: an inlined call is
/// a call, which the o32 ABI says leaves them as they were (the callee's
/// own facts say whether it does, and decide its expansion), and a `jr ra`
/// outside the function's own extent returns. So a callee inlined at two
/// call sites doesn't lead from one call to the other's return site, where
/// the caller reads the registers it saved and set.
fn o32_succs(g: &Graph, n: Node) -> Vec<Node> {
    match (n, &g.nodes[&n].transfer) {
        (Node::Slot(b), _) if inlined_call(g, n) => vec![Node::Normal(b + 8)],
        (_, Transfer::InlineReturn { .. }) => Vec::new(),
        _ => g.next(n),
    }
}

/// The delay slot of a `jal` expanded in place: a jump into the callee,
/// which returns past the slot.
fn inlined_call(g: &Graph, n: Node) -> bool {
    match (n, &g.nodes[&n].transfer) {
        (Node::Slot(b), Transfer::Jump(Dest::Local(_))) => {
            g.nodes[&Node::Normal(b)].insn.op == Op::Jal && g.inline_returns.contains(&(b + 8))
        }
        _ => false,
    }
}

/// A way out of the function: a return (an inlined callee's `jr ra` too,
/// with ra the caller's), or a tail transfer, whose callee returns to this
/// function's caller.
fn is_exit(g: &Graph, n: Node) -> bool {
    matches!(
        g.nodes[&n].transfer,
        Transfer::Return
            | Transfer::InlineReturn { .. }
            | Transfer::Jump(Dest::Tail(_))
            | Transfer::FallOff(_)
            | Transfer::Call { next: Dest::Tail(_), .. }
    )
}

fn sites(g: &Graph, f: &mut Facts) {
    // Registers set on some path from the entry: a value passed to a callee
    // can be set ahead of a branch or a join, and a call sets v0 and v1
    // (the locals a callee's other registers would be stay as they were).
    let set = forward(
        g,
        &|n| g.succs(n),
        |n, s| {
            let call = matches!(g.nodes[&n].transfer, Transfer::Call { .. } | Transfer::CondCall { .. }) || g.tail_returns(n);
            *s | insn_defs(&g.nodes[&n].insn) | if call { (1 << 2) | (1 << 3) } else { 0 }
        },
        |a, b| *a | *b,
        0u64,
    );
    // Callee-saved registers changed and not restored: a load into one the
    // function saves restores it.
    let saved = g.nodes.values().filter(|d| d.insn.op == Op::Sw).fold(0u64, |a, d| a | 1 << d.insn.rt) & S_REGS;
    let restore = |n: Node, &s: &u64| {
        let i = &g.nodes[&n].insn;
        let w = i.writes().map_or(0, |r| 1u64 << r) & S_REGS;
        if i.op.is_load() && w & saved != 0 { s & !w } else { s | w }
    };
    let changed = forward(g, &|n| g.succs(n), restore, |a, b| *a | *b, 0u64);
    for (&n, d) in &g.nodes {
        f.defs |= insn_defs(&d.insn);
        if is_exit(g, n) {
            f.s_changed |= changed.get(&n).map_or(0, |s| restore(n, s));
        }
        let target = match &d.transfer {
            Transfer::Call { callee: Callee::Direct { addr, .. }, .. }
            | Transfer::Jump(Dest::Tail(Callee::Direct { addr, .. }))
            | Transfer::FallOff(Callee::Direct { addr, .. }) => *addr,
            _ => continue,
        };
        f.call_sites.push((n.pc(), target, set.get(&n).copied().unwrap_or(0) | insn_defs(&d.insn)));
    }
}

fn is_io(addr: u32) -> bool {
    (0x1F80_1000..0x1F80_3000).contains(&(addr & 0x1FFF_FFFF))
}

/// Main RAM (its 8 MiB of mirrors) through KUSEG, KSEG0 or KSEG1, or the
/// scratchpad through KUSEG or KSEG0 (psx-spx "Memory Map"): plain memory,
/// where an access does nothing but read or write its bytes.
fn is_memory(addr: u32) -> bool {
    let p = addr & 0x1FFF_FFFF;
    let kuseg = addr < 0x2000_0000;
    let ram = p < 0x0080_0000 && (kuseg || (0x8000_0000..0xC000_0000).contains(&addr));
    let scratchpad = (0x1F80_0000..0x1F80_0400).contains(&p) && (kuseg || (0x8000_0000..0xA000_0000).contains(&addr));
    ram || scratchpad
}

/// The BIOS kernel's RAM, but for the tables the runtime's BIOS keeps in
/// guest RAM (psxw-abi `kernel::RAM_BACKED`), which games read and write
/// directly. Address 0 is excluded: a load through a constant null is a
/// game bug, not kernel access.
fn is_kernel(addr: u32) -> bool {
    (1..0x1_0000).contains(&(addr & 0x1FFF_FFFF)) && !psxw_abi::kernel::ram_backed(addr)
}

/// Constant effect of one instruction.
fn step(i: &Insn, pc: u32, st: &mut Consts) {
    let s = st[i.rs as usize];
    let t = st[i.rt as usize];
    let both = |f: fn(u32, u32) -> u32| s.zip(t).map(|(a, b)| f(a, b));
    let v = match i.op {
        Op::Lui => Some(i.uimm << 16),
        Op::Addiu | Op::Addi => s.map(|a| a.wrapping_add(i.simm as u32)),
        Op::Ori => s.map(|a| a | i.uimm),
        Op::Andi => s.map(|a| a & i.uimm),
        Op::Xori => s.map(|a| a ^ i.uimm),
        Op::Addu | Op::Add => both(u32::wrapping_add),
        Op::Subu | Op::Sub => both(u32::wrapping_sub),
        Op::Or => both(|a, b| a | b),
        Op::And => both(|a, b| a & b),
        Op::Xor => both(|a, b| a ^ b),
        Op::Sll => t.map(|a| a << i.sa),
        Op::Srl => t.map(|a| a >> i.sa),
        Op::Jal | Op::Bltzal | Op::Bgezal | Op::Jalr => Some(pc + 8),
        _ => None,
    };
    if let Some(r) = i.writes() {
        st[r as usize] = v;
    }
    st[0] = Some(0);
}

fn clobber_call(st: &mut Consts) {
    for r in 1..32 {
        if CALLER_SAVED & (1 << r) != 0 {
            st[r] = None;
        }
    }
}

fn is_call(t: &Transfer) -> bool {
    matches!(t, Transfer::Call { .. } | Transfer::CondCall { .. })
}

fn constants(g: &Graph, in_code: &dyn Fn(u32) -> bool, through_pointers: bool, f: &mut Facts) {
    let mut entry: Consts = [None; 32];
    entry[0] = Some(0);
    let mut work = VecDeque::from([Node::Normal(g.entry)]);
    f.consts.insert(Node::Normal(g.entry), entry);
    while let Some(n) = work.pop_front() {
        let mut st = f.consts[&n];
        let d = &g.nodes[&n];
        step(&d.insn, n.pc(), &mut st);
        // A tail transfer out of inlined code comes back as a call does.
        if is_call(&d.transfer) || g.tail_returns(n) {
            clobber_call(&mut st);
        }
        for s in g.succs(n) {
            let changed = match f.consts.get_mut(&s) {
                None => {
                    f.consts.insert(s, st);
                    true
                }
                Some(old) => {
                    let mut changed = false;
                    for r in 0..32 {
                        if old[r].is_some() && old[r] != st[r] {
                            old[r] = None;
                            changed = true;
                        }
                    }
                    changed
                }
            };
            if changed {
                work.push_back(s);
            }
        }
    }
    for (n, st) in &f.consts {
        let i = &g.nodes[n].insn;
        if let Transfer::Call { callee: Callee::Kernel { addr }, .. } | Transfer::Jump(crate::graph::Dest::Tail(Callee::Kernel { addr })) =
            &g.nodes[n].transfer
        {
            f.kernel.push((n.pc(), *addr));
        }
        if i.op.is_load() || i.op.is_store() {
            match st[i.rs as usize] {
                Some(base) => {
                    let a = base.wrapping_add(i.simm as u32);
                    if !is_memory(a) {
                        f.device.insert(*n);
                    }
                    if is_io(a) {
                        f.io.push((n.pc(), a));
                    } else if is_kernel(a) {
                        f.kernel.push((n.pc(), a));
                    }
                }
                None if through_pointers && i.rs != 29 => {
                    f.device.insert(*n);
                }
                None => {}
            }
        }
        if matches!(i.op, Op::Addiu | Op::Ori) {
            let mut after = *st;
            step(i, n.pc(), &mut after);
            if let Some(v) = after[i.rt as usize] {
                if v & 3 == 0 && in_code(v) && !g.embedded_tables.contains_key(&v) {
                    f.code_consts.insert(v);
                }
            }
        }
        // The %hi half of `lui gp; addiu gp` isn't a gp value.
        if i.writes() == Some(28) && i.op != Op::Lui {
            let mut after = *st;
            step(i, n.pc(), &mut after);
            if let Some(v) = after[28] {
                f.gp_writes.insert(v);
            }
        }
    }
}

/// (use, def) of a node as GPR bits plus hi/lo. With `args`, calls read the
/// argument registers; without, only what instructions read counts.
fn use_def(g: &Graph, n: Node, args: bool) -> (u64, u64, u64) {
    let d = &g.nodes[&n];
    let i = &d.insn;
    let mut uses = i.reads() as u64;
    // Compilers emit lwl/lwr in pairs that together overwrite the whole
    // register; the merge with its old value is not a meaningful use.
    if matches!(i.op, Op::Lwl | Op::Lwr) {
        uses &= !(1u64 << i.rt);
    }
    // Saving a callee-saved register is not a use of its value.
    let mut save_exempt = 0u64;
    if i.op == Op::Sw && i.rs == 29 && S_REGS & (1 << i.rt) != 0 {
        save_exempt = 1 << i.rt;
    }
    let mut defs = i.writes().map_or(0, |r| 1u64 << r);
    match i.op {
        Op::Mfhi => uses |= HI,
        Op::Mflo => uses |= LO,
        Op::Mthi => defs |= HI,
        Op::Mtlo => defs |= LO,
        Op::Mult | Op::Multu | Op::Div | Op::Divu => defs |= HI | LO,
        _ => {}
    }
    // The transfer happens after the slot instruction.
    let uses_of = |c: &Callee| if args { call_uses(c) } else { 0 };
    let call_defs = (1 << 2) | (1 << 3) | (CALLER_SAVED & !(1 << 31));
    // A tail callee returns through ra (out of inlined code, ra is
    // dispatched on as the inlined `jr ra` would).
    let tail_uses = |c: &Callee| uses_of(c) | if args && !g.tail_returns(n) { 1 << 31 } else { 0 };
    let (tu, td) = match &d.transfer {
        Transfer::Call { callee, .. } | Transfer::CondCall { callee, .. } => (uses_of(callee), call_defs),
        Transfer::Jump(Dest::Tail(c)) | Transfer::FallOff(c) => (tail_uses(c), if g.tail_returns(n) { call_defs } else { 0 }),
        Transfer::Cond { taken, fall } => {
            let tails = [taken, fall].into_iter().filter_map(|d| match d {
                Dest::Tail(c) => Some(tail_uses(c)),
                Dest::Local(_) => None,
            });
            (tails.fold(0, |a, u| a | u), 0)
        }
        _ => (0, 0),
    };
    // Order: slot uses, slot defs, then the call's uses and defs.
    let uses = uses | (tu & !defs);
    let defs = defs | td;
    (uses & !save_exempt, defs, save_exempt)
}

fn call_uses(c: &Callee) -> u64 {
    let args = 0xF << 4;
    match c {
        Callee::Bios { .. } => args | (1 << 9) | (1 << 29),
        _ => args | (1 << 29),
    }
}

fn live_in(g: &Graph, nodes: &[Node], succs: &BTreeMap<Node, Vec<Node>>, args: bool) -> BTreeMap<Node, u64> {
    let ud: BTreeMap<Node, (u64, u64, u64)> = nodes.iter().map(|&n| (n, use_def(g, n, args))).collect();
    let mut live_in: BTreeMap<Node, u64> = nodes.iter().map(|&n| (n, 0)).collect();
    let mut changed = true;
    while changed {
        changed = false;
        for &n in nodes.iter().rev() {
            let out = succs[&n].iter().fold(0, |acc, s| acc | live_in[s]);
            let (u, d, _) = ud[&n];
            let inn = u | (out & !d);
            if inn != live_in[&n] {
                live_in.insert(n, inn);
                changed = true;
            }
        }
    }
    live_in
}

fn liveness(g: &Graph, f: &mut Facts) {
    let nodes: Vec<Node> = g.nodes.keys().copied().collect();
    let succs: BTreeMap<Node, Vec<Node>> = nodes.iter().map(|&n| (n, g.succs(n))).collect();
    let at_entry = live_in(g, &nodes, &succs, true)[&Node::Normal(g.entry)];
    f.entry_reads = at_entry & !ENTRY_OK & ((1 << 34) - 2);
    f.entry_ra = at_entry & (1 << 31) != 0;
    let o32: BTreeMap<Node, Vec<Node>> = nodes.iter().map(|&n| (n, o32_succs(g, n))).collect();
    f.entry_s_reads = live_in(g, &nodes, &o32, true)[&Node::Normal(g.entry)] & S_REGS;
    // After a call, only reads by instructions count: argument setup for a
    // later call doesn't depend on what the callee left behind.
    let live_in = live_in(g, &nodes, &succs, false);
    for &n in &nodes {
        let i = &g.nodes[&n].insn;
        if matches!(i.op, Op::Lb | Op::Lbu | Op::Lh | Op::Lhu | Op::Lw) {
            let out = succs[&n].iter().fold(0, |acc, s| acc | live_in[s]);
            if i.rt == 0 || out & (1 << i.rt) == 0 {
                f.dead_loads.insert(n);
            }
        }
    }
    for &n in &nodes {
        if let Transfer::Call { callee, .. } | Transfer::CondCall { callee, .. } = &g.nodes[&n].transfer {
            let after = succs[&n].iter().fold(0, |acc, s| acc | live_in[s]);
            if let (true, Callee::Direct { addr, .. }) = (after & (1 << 2) != 0, callee) {
                f.result_uses.insert(*addr);
            }
            let bad = after & (CALLER_SAVED | S_REGS) & !((1 << 2) | (1 << 3) | (1 << 31));
            if bad != 0 {
                let target = match callee {
                    Callee::Direct { addr, .. } => Some(*addr),
                    _ => None,
                };
                f.post_call_reads.push((n.pc(), bad, target));
            }
        }
    }
}

fn hazards(g: &Graph, f: &mut Facts) {
    for (&n, d) in &g.nodes {
        if !d.insn.op.is_delayed_load() {
            continue;
        }
        let Some(r) = d.insn.writes() else { continue };
        for s in g.next(n) {
            let si = &g.nodes[&s].insn;
            let merges = matches!(d.insn.op, Op::Lwl | Op::Lwr)
                && matches!(si.op, Op::Lwl | Op::Lwr)
                && si.rt == r;
            if !merges && si.reads() & (1 << r) != 0 {
                f.hazards.insert(n, r);
            }
        }
    }
}

/// Human-readable register set.
pub fn reg_list(bits: u64) -> String {
    let mut names: Vec<&str> = (0..32).filter(|r| bits & (1 << r) != 0).map(|r| crate::decode::REG_NAMES[r]).collect();
    if bits & HI != 0 {
        names.push("hi");
    }
    if bits & LO != 0 {
        names.push("lo");
    }
    names.join(",")
}

#[cfg(test)]
mod tests {
    #[test]
    fn kernel_tables_in_ram_are_not_kernel_access() {
        // The TCB array pointer and the TCBs themselves (D57).
        assert!(!super::is_kernel(0x0000_0110));
        assert!(!super::is_kernel(0xA000_E010 + 0xC0 + 0x94));
        // Tables the BIOS doesn't lay out, and its patch area, are.
        assert!(super::is_kernel(0x0000_0158));
        assert!(super::is_kernel(0xA000_DF80));
        assert!(!super::is_kernel(0));
    }

    #[test]
    fn memory_regions() {
        assert!(super::is_memory(0x8001_0000) && super::is_memory(0xA07F_FFFC) && super::is_memory(0x0000_0100));
        assert!(super::is_memory(0x1F80_03FC) && super::is_memory(0x9F80_0000));
        // The scratchpad isn't in KSEG1; I/O, expansion, unmapped space
        // and KSEG2's cache control aren't memory.
        assert!(!super::is_memory(0xBF80_0000));
        assert!(!super::is_memory(0x1F80_1810) && !super::is_memory(0xBF80_1070) && !super::is_memory(0x1F00_0084));
        assert!(!super::is_memory(0x0080_0000) && !super::is_memory(0xFFFE_0130) && !super::is_memory(0x2000_0000));
    }
}
