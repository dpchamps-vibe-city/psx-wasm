//! Dataflow over a function's [`Graph`]: constant propagation (hardware
//! access sites, code addresses, `gp` values, BIOS dispatch checks),
//! liveness (registers a function reads that the o32 ABI doesn't pass), and
//! load-delay hazards.

use crate::decode::{Insn, Op};
use crate::emit::Exceptions;
use crate::graph::{Callee, Graph, Node, Transfer};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Registers the callee may clobber, as the o32 ABI defines.
pub const CALLER_SAVED: u64 = (1 << 1) | (0xFF << 2) | (0xFF << 8) | (1 << 24) | (1 << 25) | HI | LO;
pub const HI: u64 = 1 << 32;
pub const LO: u64 = 1 << 33;
/// Registers a function may legitimately read on entry: arguments, `gp`,
/// `sp`, `ra`, and callee-saved registers (read only to be saved).
const ENTRY_OK: u64 = (0xF << 4) | (1 << 28) | (1 << 29) | (1 << 31) | (0xFF << 16) | (1 << 30);
const S_REGS: u64 = (0xFF << 16) | (1 << 30);

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
    /// Calls after which a caller-saved register (other than v0/v1) is read
    /// before being written: (pc of the call, registers, direct callee).
    pub post_call_reads: Vec<(u32, u64, Option<u32>)>,
    /// Direct callees whose result (v0) is read after a call to them.
    pub result_uses: BTreeSet<u32>,
    /// Every way out leaves v0 as a call set it: the function returns
    /// what its last callee returned, whatever its C declares.
    pub returns_callee_v0: bool,
    /// Loads whose next instruction reads the loaded register.
    pub hazards: BTreeMap<Node, u8>,
    /// Registers any instruction of the function writes.
    pub defs: u64,
    /// Direct calls and fall-throughs to code addresses: (pc, target,
    /// registers written in the straight-line block leading to it).
    pub call_sites: Vec<(u32, u32, u64)>,
    /// Loads whose result nothing reads (or that load into `zero`). They
    /// still happen on hardware, and from an I/O register a read pops a
    /// FIFO, so they are emitted as volatile reads clang keeps.
    pub dead_loads: BTreeSet<Node>,
}

pub fn analyze(g: &Graph, in_code: &dyn Fn(u32) -> bool) -> Facts {
    let mut f = Facts::default();
    constants(g, in_code, &mut f);
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

fn sites(g: &Graph, f: &mut Facts) {
    let preds = g.preds();
    for (&n, d) in &g.nodes {
        f.defs |= d.insn.writes().map_or(0, |r| 1u64 << r);
        let target = match &d.transfer {
            Transfer::Call { callee: Callee::Direct { addr, .. }, .. }
            | Transfer::Jump(crate::graph::Dest::Tail(Callee::Direct { addr, .. }))
            | Transfer::FallOff(Callee::Direct { addr, .. }) => *addr,
            _ => continue,
        };
        // Walk back through the block: unique predecessors that flow
        // straight into this node.
        let mut defs = d.insn.writes().map_or(0, |r| 1u64 << r);
        let mut cur = n;
        for _ in 0..64 {
            let Some([p]) = preds.get(&cur).map(Vec::as_slice) else { break };
            let pd = &g.nodes[p];
            if !matches!(pd.transfer, Transfer::Seq(_)) {
                break;
            }
            defs |= pd.insn.writes().map_or(0, |r| 1u64 << r);
            cur = *p;
        }
        f.call_sites.push((n.pc(), target, defs));
    }
}

fn is_io(addr: u32) -> bool {
    (0x1F80_1000..0x1F80_3000).contains(&(addr & 0x1FFF_FFFF))
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

fn constants(g: &Graph, in_code: &dyn Fn(u32) -> bool, f: &mut Facts) {
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
            if let Some(base) = st[i.rs as usize] {
                let a = base.wrapping_add(i.simm as u32);
                if is_io(a) {
                    f.io.push((n.pc(), a));
                } else if is_kernel(a) {
                    f.kernel.push((n.pc(), a));
                }
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
    let (tu, td) = match &d.transfer {
        Transfer::Call { callee, .. } | Transfer::CondCall { callee, .. } => (uses_of(callee), call_defs),
        Transfer::Jump(crate::graph::Dest::Tail(c)) | Transfer::FallOff(c) => {
            (uses_of(c), if g.tail_returns(n) { call_defs } else { 0 })
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
    f.entry_s_reads = at_entry & S_REGS;
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
            let bad = after & CALLER_SAVED & !((1 << 2) | (1 << 3) | (1 << 31));
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
        // File/device table pointers and their RAM-backed control blocks.
        for addr in [0x140, 0x150, psxw_abi::kernel::FCBS, psxw_abi::kernel::DCBS] {
            assert!(!super::is_kernel(addr));
            assert!(!super::is_kernel(0xA000_0000 | addr));
        }
        // Unprovided tables and the BIOS patch area still require an adaptor.
        assert!(super::is_kernel(0x0000_0160));
        assert!(super::is_kernel(0xA000_DF80));
        assert!(!super::is_kernel(0));
    }
}

/// The instructions the transport proof follows while it tracks a value:
/// each touches only the translation's own registers (C locals, but for
/// [`PUBLISHED`] ones), `hi`/`lo` and guest memory, as [`Insn::reads`] and
/// [`Insn::writes`] say (`emit::tests::transport_model_matches_the_emitter`
/// checks those against the emitter for every instruction), and branches
/// and jumps as `emit::Emitter::group` performs them. The rest hand
/// registers to the runtime or merge into them (syscalls, coprocessor
/// moves, partial loads and stores, `break`, `rfe`): the proof refuses
/// them while the value may be in any register.
pub const MODELLED: &[Op] = &[
    Op::Sll, Op::Srl, Op::Sra, Op::Sllv, Op::Srlv, Op::Srav, Op::Mfhi, Op::Mflo, Op::Mthi, Op::Mtlo, Op::Mult, Op::Multu, Op::Div,
    Op::Divu, Op::Add, Op::Addu, Op::Sub, Op::Subu, Op::And, Op::Or, Op::Xor, Op::Nor, Op::Slt, Op::Sltu, Op::Addi, Op::Addiu,
    Op::Slti, Op::Sltiu, Op::Andi, Op::Ori, Op::Xori, Op::Lui, Op::Lb, Op::Lbu, Op::Lh, Op::Lhu, Op::Lw, Op::Sb, Op::Sh, Op::Sw,
    Op::Beq, Op::Bne, Op::Blez, Op::Bgtz, Op::Bltz, Op::Bgez, Op::Bltzal, Op::Bgezal, Op::J, Op::Jal, Op::Jr, Op::Jalr,
];

/// Registers whose writes the emitter publishes to the guest's shared
/// globals (`psxw_gp_set`, `psxw_sp_set`): a value there is visible
/// outside the translation.
pub const PUBLISHED: u32 = (1 << 28) | (1 << 29);

/// A register-to-register move: (destination, source).
fn move_of(i: &Insn) -> Option<(u8, u8)> {
    match i.op {
        Op::Addu | Op::Or if i.rt == 0 => Some((i.rd, i.rs)),
        Op::Addu | Op::Or if i.rs == 0 => Some((i.rd, i.rt)),
        Op::Addiu if i.simm == 0 => Some((i.rt, i.rs)),
        Op::Ori if i.uimm == 0 => Some((i.rt, i.rs)),
        _ => None,
    }
}

/// Why a native handle can't pass in an argument register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The function reads the value as a number: arithmetic, a comparison,
    /// an address.
    Observed(String),
    /// The value leaves where the guest sees it: stored, or to the runtime.
    Escapes(String),
    /// A use this proof doesn't follow (into a callee, a return, across a
    /// BIOS call, on some paths only): not shown ineligible, not proven.
    Unproven(String),
}

/// An argument register that can carry a native handle: the `jalr` sites
/// (branch pcs) that call through its entry value, and whether every way
/// out returns it in `v0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Carrier {
    pub jalrs: Vec<u32>,
    pub returned: bool,
}

/// Representation transport (plans/dispatch-binding B): for each argument
/// register `a0..a3` (by index), what its entry value reaches when nothing
/// else observes it ([`Carrier`]); else why not. The domain is closed
/// (docs/design.md §2, "The acceptance boundary"): while the value may be in
/// any register, every instruction is [`MODELLED`] and every transfer is
/// one of the kinds matched below, as the emitter performs it; anything
/// else is [`Refusal::Unproven`].
///
/// The value may be moved between the translation's own registers and
/// tested against zero. A `jalr` the emitter makes a dispatch-table call
/// must call through it on every path that reaches it. Anything else that
/// reads it (arithmetic, a comparison, a store, an address, a call's
/// argument registers or `v0`) observes it as a number; a move into a
/// [`PUBLISHED`] register lets it escape. It may be returned in `v0` only
/// on every way out. Calls change only `v0`/`v1` here: a translation's
/// registers are its own C locals. A load the next instruction reads
/// around (the emitter's load-delay hazard, `facts.hazards`) may not
/// overwrite it.
pub fn transport(g: &Graph, facts: &Facts, x: &Exceptions) -> BTreeMap<u8, Result<Carrier, Refusal>> {
    (0..4)
        .map(|k| {
            let r = if x.continuation {
                Err(Refusal::Unproven("a setjmp continuation's entry registers come from a jmp_buf".into()))
            } else {
                transport_of(g, facts, x, k)
            };
            (k, r)
        })
        .collect()
}

/// Whether the emitted code for transfer `t` can leave the function
/// (`emit::Emitter::group`, `tail`, `back`): a return, a tail call (whose
/// target 0 returns too), an inlined callee's `jr ra` (0 is the caller's
/// return address), and `rfe`'s jump.
fn leaves(t: &Transfer, rfe: bool) -> bool {
    use crate::graph::Dest;
    let tail = |d: &Dest| matches!(d, Dest::Tail(_));
    match t {
        Transfer::Seq(_) | Transfer::Table { .. } => false,
        Transfer::Return | Transfer::InlineReturn { .. } | Transfer::FallOff(_) => true,
        Transfer::Jump(d) => rfe || tail(d),
        Transfer::Cond { taken, fall } => tail(taken) || tail(fall),
        Transfer::Call { next, .. } | Transfer::CondCall { next, .. } => tail(next),
    }
}

fn transport_of(g: &Graph, facts: &Facts, x: &Exceptions, k: u8) -> Result<Carrier, Refusal> {
    use Refusal::*;
    use crate::graph::Dest;
    let bit = |r: u8| 1u32 << r;
    // What a call hands on: a0..a3, and v0 to a static-chain callee.
    const PASSED: u32 = 0xF0 | 1 << 2;
    // Per register: certainly the value (`yes`), possibly it (`maybe`).
    let entry = Node::Normal(g.entry);
    let mut states: BTreeMap<Node, (u32, u32)> = BTreeMap::from([(entry, (bit(4 + k), bit(4 + k)))]);
    let mut work = VecDeque::from([entry]);
    let mut consumers = BTreeSet::new();
    // Per way out of the function: whether `v0` is the value there.
    let mut exits: BTreeMap<Node, bool> = BTreeMap::new();
    while let Some(n) = work.pop_front() {
        let (mut yes, mut maybe) = states[&n];
        let d = &g.nodes[&n];
        let i = &d.insn;
        let pc = n.pc();
        // The branch (or last instruction) the transfer belongs to.
        let (Node::Normal(at) | Node::Slot(at)) = n;
        // Calls the emitter lowers specially: setjmp stores registers to
        // memory, rfe unwinds.
        let special = x.setjmp.contains(&at) || x.rfe.contains(&at);
        let clear = |yes: &mut u32, maybe: &mut u32, r: u8| {
            *yes &= !bit(r);
            *maybe &= !bit(r);
        };
        if maybe != 0 && !MODELLED.contains(&i.op) {
            return Err(Unproven(format!("{:?} at 0x{pc:08X} has effects the proof doesn't model", i.op)));
        }
        // The emitter gives the next instruction the loaded register's old
        // value: that read is of the value, after this "clears" it.
        if i.op.is_load() && facts.hazards.get(&n).is_some_and(|&r| maybe & bit(r) != 0) {
            return Err(Unproven(format!("the load at 0x{pc:08X} overwrites it while the next instruction still reads it")));
        }
        match (move_of(i), i.op) {
            (Some((rd, rs)), _) if maybe & bit(rs) != 0 && PUBLISHED & bit(rd) != 0 => {
                return Err(Escapes(format!("moved into ${} at 0x{pc:08X}, which the guest shares", if rd == 28 { "gp" } else { "sp" })));
            }
            (Some((rd, rs)), _) => {
                let (y, m) = (yes & bit(rs), maybe & bit(rs));
                clear(&mut yes, &mut maybe, rd);
                if y != 0 {
                    yes |= bit(rd);
                }
                if m != 0 {
                    maybe |= bit(rd);
                }
            }
            (None, Op::Jalr) => {
                if yes & bit(i.rs) != 0 {
                    // The emitter makes only a plain dispatch-table call
                    // native (`emit::Emitter::group`).
                    let call = g.nodes.get(&Node::Slot(pc)).map(|s| &s.transfer);
                    if !matches!(call, Some(Transfer::Call { callee: Callee::Indirect { .. }, .. })) || special {
                        return Err(Unproven(format!("the jalr at 0x{pc:08X} isn't a plain call through the dispatch table")));
                    }
                    consumers.insert(pc);
                } else if maybe & bit(i.rs) != 0 {
                    return Err(Unproven(format!("the jalr at 0x{pc:08X} calls through it on some paths only")));
                }
                if let Some(r) = i.writes() {
                    clear(&mut yes, &mut maybe, r);
                }
            }
            // A null test.
            (None, Op::Beq | Op::Bne) if i.rs == 0 || i.rt == 0 => {}
            (None, Op::Jr) if maybe & bit(i.rs) != 0 => {
                return Err(Unproven(format!("the jr at 0x{pc:08X} jumps through it")));
            }
            (None, op) => {
                if op.is_store() && maybe & bit(i.rt) != 0 {
                    return Err(Escapes(format!("stored at 0x{pc:08X}")));
                }
                if i.reads() & maybe != 0 {
                    return Err(Observed(format!("{op:?} at 0x{pc:08X} uses it")));
                }
                if let Some(r) = i.writes() {
                    clear(&mut yes, &mut maybe, r);
                }
            }
        }
        yes &= !1;
        maybe &= !1;
        // The transfer, as the emitter performs it. The kinds the proof
        // follows while the value may be held are matched first; every
        // other kind is refused then. With no register holding it, the
        // value is dead (never stored, passed or in hi/lo): nothing there
        // sees it, and a way out returns something else.
        match &d.transfer {
            Transfer::Seq(_) | Transfer::Table { .. } => {}
            Transfer::Jump(Dest::Local(_)) | Transfer::Cond { taken: Dest::Local(_), fall: Dest::Local(_) } if !special => {}
            Transfer::Call { callee: Callee::Direct { .. } | Callee::Indirect { .. }, next: Dest::Local(_) } if !special => {
                if maybe & PASSED != 0 {
                    return Err(Unproven(format!("it is passed on to the call at 0x{pc:08X}")));
                }
                clear(&mut yes, &mut maybe, 2);
                clear(&mut yes, &mut maybe, 3);
            }
            Transfer::Return => {
                if maybe & bit(3) != 0 || (maybe & bit(2) != 0 && yes & bit(2) == 0) {
                    return Err(Unproven(format!("it is returned at 0x{pc:08X} on some paths only")));
                }
                exits.insert(n, yes & bit(2) != 0);
            }
            // A tail call out of the function's own code returns the
            // callee's result; through a register, target 0 returns v1:v0.
            Transfer::Jump(Dest::Tail(Callee::Direct { .. } | Callee::Indirect { .. }))
            | Transfer::FallOff(Callee::Direct { .. } | Callee::Indirect { .. })
                if !special && !g.inlined(at) =>
            {
                if maybe & (PASSED | bit(3)) != 0 {
                    return Err(Unproven(format!("it is passed on to the tail call at 0x{pc:08X}")));
                }
                exits.insert(n, false);
            }
            // Conditional calls, BIOS and kernel calls, setjmp and rfe, an
            // inlined callee's returns and tails, conditional tails.
            t @ (Transfer::Jump(_)
            | Transfer::Cond { .. }
            | Transfer::Call { .. }
            | Transfer::CondCall { .. }
            | Transfer::FallOff(_)
            | Transfer::InlineReturn { .. }) => {
                if maybe != 0 {
                    return Err(Unproven(format!("it may be held at 0x{pc:08X}, a transfer outside the proof's domain")));
                }
                if leaves(t, x.rfe.contains(&at)) {
                    exits.insert(n, false);
                }
            }
        }
        for s in g.succs(n) {
            let changed = match states.get_mut(&s) {
                None => {
                    states.insert(s, (yes, maybe));
                    true
                }
                Some((y, m)) => {
                    let (ny, nm) = (*y & yes, *m | maybe);
                    let changed = (ny, nm) != (*y, *m);
                    (*y, *m) = (ny, nm);
                    changed
                }
            };
            if changed {
                work.push_back(s);
            }
        }
    }
    let returned = match (exits.values().any(|&r| r), exits.values().all(|&r| r)) {
        (false, _) => false,
        (true, true) => true,
        (true, false) => return Err(Unproven("it is returned on some ways out only".into())),
    };
    if consumers.is_empty() && !returned {
        return Err(Unproven("nothing calls through it or returns it".into()));
    }
    Ok(Carrier { jalrs: consumers.into_iter().collect(), returned })
}
