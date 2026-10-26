//! Dataflow over a function's [`Graph`]: constant propagation (hardware
//! access sites, code addresses, `gp` values, BIOS dispatch checks),
//! liveness (registers a function reads that the o32 ABI doesn't pass), and
//! load-delay hazards.

use crate::decode::{Insn, Op};
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

/// The BIOS kernel's RAM. Address 0 is excluded: a load through a constant
/// null is a game bug, not kernel access.
fn is_kernel(addr: u32) -> bool {
    (1..0x1_0000).contains(&(addr & 0x1FFF_FFFF))
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
        if is_call(&d.transfer) {
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
                if v & 3 == 0 && in_code(v) {
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
    let (tu, td) = match &d.transfer {
        Transfer::Call { callee, .. } | Transfer::CondCall { callee, .. } => {
            (uses_of(callee), (1 << 2) | (1 << 3) | (CALLER_SAVED & !(1 << 31)))
        }
        Transfer::Jump(crate::graph::Dest::Tail(c)) | Transfer::FallOff(c) => (uses_of(c), 0),
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
        for s in g.succs(n) {
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
