//! Control flow of one function, at the granularity the R3000 executes it.
//!
//! A node is an instruction *in a context*: [`Node::Normal`] executes the
//! word at `pc` and continues at `pc + 4`; [`Node::Slot`] executes the delay
//! slot of the branch at `b` and then performs that branch's transfer. A
//! delay-slot word that is also a branch target is therefore two nodes,
//! which is exactly how the hardware treats it.

use crate::decode::{Insn, Op, decode};
use crate::program::{Function, Located, Program, Source, call_symbol, dynamic_symbol};
use psxw_inputs::originals::Originals;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum Node {
    Normal(u32),
    /// Delay slot of the branch at this address (the word at `+4`).
    Slot(u32),
}

impl Node {
    pub fn pc(self) -> u32 {
        match self {
            Node::Normal(pc) => pc,
            Node::Slot(b) => b + 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Callee {
    /// A function at a known address, through its link symbol.
    Direct { addr: u32, symbol: String },
    /// Through the dispatch table; the address is captured from `reg`
    /// before the delay slot runs.
    Indirect { reg: u8 },
    /// BIOS vector call; the function number is in `t1` after the slot.
    Bios { table: u8 },
    /// A call into BIOS kernel RAM at a fixed address: code that only an
    /// adapter can replace.
    Kernel { addr: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dest {
    Local(u32),
    Tail(Callee),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transfer {
    Seq(Node),
    /// Execution runs past the extent into the next function.
    FallOff(Callee),
    Jump(Dest),
    Cond { taken: Dest, fall: Dest },
    Call { callee: Callee, next: Dest },
    /// `bltzal`/`bgezal`: call only when the condition holds.
    CondCall { callee: Callee, next: Dest },
    Return,
    Table { reg: u8, base: u32, targets: Vec<u32> },
    /// `jr ra` inside an inlined callee: back to one of its call sites.
    InlineReturn { targets: Vec<u32> },
}

#[derive(Debug, Clone)]
pub struct NodeData {
    pub insn: Insn,
    pub transfer: Transfer,
}

#[derive(Debug, Clone, Default)]
pub struct Graph {
    pub entry: u32,
    pub nodes: BTreeMap<Node, NodeData>,
    /// Starts this function needs that the program doesn't have yet.
    pub wanted: BTreeSet<(String, u32, Source)>,
    /// `jr`/`jalr` targets taken as constants from the straight-line
    /// window: branch pc → (register, value). Checked against dataflow.
    pub const_targets: BTreeMap<u32, (u8, u32)>,
    /// Code ranges translated into this function: its own extent first,
    /// then any inlined callees', with the container holding their words.
    pub regions: Vec<(u32, u32, String)>,
    /// Return addresses of inlined calls.
    pub inline_returns: BTreeSet<u32>,
    /// Jump tables inside the function's own text: base → entries. Data,
    /// not code: their base is no function pointer.
    pub embedded_tables: BTreeMap<u32, u32>,
}

impl Graph {
    pub fn succs(&self, n: Node) -> Vec<Node> {
        let local = |d: &Dest| match d {
            Dest::Local(a) => Some(Node::Normal(*a)),
            Dest::Tail(_) => None,
        };
        match &self.nodes[&n].transfer {
            Transfer::Seq(next) => vec![*next],
            Transfer::FallOff(_) | Transfer::Return => vec![],
            Transfer::Jump(d) => local(d).into_iter().collect(),
            Transfer::Cond { taken, fall } => local(taken).into_iter().chain(local(fall)).collect(),
            Transfer::Call { next, .. } | Transfer::CondCall { next, .. } => local(next).into_iter().collect(),
            Transfer::Table { targets, .. } | Transfer::InlineReturn { targets } => {
                let set: BTreeSet<u32> = targets.iter().copied().collect();
                set.into_iter().map(Node::Normal).collect()
            }
        }
    }

    pub fn preds(&self) -> BTreeMap<Node, Vec<Node>> {
        let mut preds: BTreeMap<Node, Vec<Node>> = BTreeMap::new();
        for &n in self.nodes.keys() {
            for s in self.succs(n) {
                preds.entry(s).or_default().push(n);
            }
        }
        preds
    }
}

#[derive(Debug, Clone)]
pub struct LiftError {
    pub pc: u32,
    pub message: String,
}

impl std::fmt::Display for LiftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "0x{:08X}: {}", self.pc, self.message)
    }
}

fn err<T>(pc: u32, message: impl Into<String>) -> Result<T, LiftError> {
    Err(LiftError { pc, message: message.into() })
}

pub struct Explorer<'a> {
    pub program: &'a Program,
    pub originals: &'a Originals,
    pub func: &'a Function,
    /// `jr`/`jalr` sites whose window constant dataflow didn't confirm.
    pub not_const: &'a BTreeSet<u32>,
    /// Functions expanded at their call sites, sharing every register with
    /// the caller (they pass values outside the o32 ABI): start → end.
    pub inline: &'a BTreeMap<(String, u32), u32>,
}

impl Explorer<'_> {
    fn word(&self, pc: u32) -> Result<Insn, LiftError> {
        let container = self
            .program
            .locate(&self.func.container, pc)
            .and_then(|l| match l {
                Located::Code(c) => Some(c),
                _ => None,
            })
            .unwrap_or_else(|| self.func.container.clone());
        match self.originals.word(&container, pc) {
            Some(w) => Ok(decode(w)),
            None => err(pc, "no original bytes at this address"),
        }
    }

    /// Inside this function's extent, past its entry: where hand-written
    /// code keeps a jump table among its instructions.
    fn in_own_text(&self, a: u32) -> bool {
        a > self.func.addr && a < self.func.end
    }

    fn in_extent(&self, g: &Graph, a: u32) -> bool {
        g.regions.iter().any(|(s, e, _)| a >= *s && a < *e)
    }

    /// Targets of a computed jump: every instruction of the branch-free
    /// head of each [`Source::ComputedEntry`] block in this container, with
    /// the block's extent brought into this translation.
    fn computed_targets(&self, g: &mut Graph) -> Vec<u32> {
        let c = &self.func.container;
        let Some(code) = self.program.code(c) else { return Vec::new() };
        let mut targets = Vec::new();
        for f in self.program.functions(c) {
            if f.addr == self.func.addr || !code.starts.get(&f.addr).is_some_and(|s| s.sources.contains(&Source::ComputedEntry)) {
                continue;
            }
            if !self.in_extent(g, f.addr) {
                g.regions.push((f.addr, f.end, c.clone()));
            }
            let mut a = f.addr;
            while a < f.end {
                let Some(w) = self.originals.word(c, a) else { break };
                targets.push(a);
                if decode(w).op.has_delay_slot() {
                    break;
                }
                a += 4;
            }
        }
        targets
    }

    /// Bring an inline function's extent into this translation.
    fn try_inline(&self, g: &mut Graph, addr: u32) -> bool {
        let Some(Located::Code(c)) = self.program.locate(&self.func.container, addr) else { return false };
        match self.inline.get(&(c.clone(), addr)) {
            Some(&end) => {
                g.regions.push((addr, end, c));
                true
            }
            None => false,
        }
    }

    /// The callee for a code address, recording starts the program lacks.
    fn callee(&self, g: &mut Graph, pc: u32, addr: u32, source: Source) -> Result<Callee, LiftError> {
        let main = &self.program.main;
        match self.program.locate(&self.func.container, addr) {
            Some(Located::Code(c)) => {
                let known = self.program.code(&c).is_some_and(|code| code.starts.contains_key(&addr));
                if !known {
                    g.wanted.insert((c.clone(), addr, source));
                }
                Ok(Callee::Direct { addr, symbol: call_symbol(main, &c, addr) })
            }
            Some(Located::Dynamic) => Ok(Callee::Direct { addr, symbol: dynamic_symbol(addr) }),
            Some(Located::Bios(t)) => Ok(Callee::Bios { table: t }),
            Some(Located::Kernel) => Ok(Callee::Kernel { addr }),
            None => err(pc, format!("control transfer to 0x{addr:08X}, which is not code")),
        }
    }

    fn dest(&self, g: &mut Graph, pc: u32, addr: u32) -> Result<Dest, LiftError> {
        if self.in_extent(g, addr) || self.try_inline(g, addr) {
            Ok(Dest::Local(addr))
        } else {
            Ok(Dest::Tail(self.callee(g, pc, addr, Source::CallTarget)?))
        }
    }

    pub fn explore(&self) -> Result<Graph, LiftError> {
        let mut g = Graph {
            entry: self.func.addr,
            regions: vec![(self.func.addr, self.func.end, self.func.container.clone())],
            ..Default::default()
        };
        let mut work = vec![Node::Normal(self.func.addr)];
        while let Some(n) = work.pop() {
            if g.nodes.contains_key(&n) {
                continue;
            }
            let Node::Normal(pc) = n else { unreachable!("slots are created with their branch") };
            if !self.in_extent(&g, pc) {
                return err(pc, "reached outside the function's extent");
            }
            let insn = self.word(pc)?;
            if insn.op == Op::Invalid {
                return err(pc, format!("invalid instruction 0x{:08X}", insn.word));
            }
            if !insn.op.has_delay_slot() {
                let next = pc + 4;
                let transfer = if self.in_extent(&g, next) || self.try_inline(&mut g, next) {
                    work.push(Node::Normal(next));
                    Transfer::Seq(Node::Normal(next))
                } else {
                    Transfer::FallOff(self.callee(&mut g, pc, next, Source::CallTarget)?)
                };
                g.nodes.insert(n, NodeData { insn, transfer });
                continue;
            }
            let slot_pc = pc + 4;
            if !self.in_extent(&g, slot_pc) {
                return err(pc, "delay slot lies past the function's extent");
            }
            let slot = self.word(slot_pc)?;
            if slot.op == Op::Invalid {
                return err(slot_pc, format!("invalid instruction 0x{:08X}", slot.word));
            }
            if slot.op.has_delay_slot() {
                return err(slot_pc, "branch in a delay slot");
            }
            let transfer = self.transfer(&mut g, pc, &insn)?;
            g.nodes.insert(n, NodeData { insn, transfer: Transfer::Seq(Node::Slot(pc)) });
            g.nodes.insert(Node::Slot(pc), NodeData { insn: slot, transfer: transfer.clone() });
            let mut push_dest = |d: &Dest| {
                if let Dest::Local(a) = d {
                    work.push(Node::Normal(*a));
                }
            };
            match &transfer {
                Transfer::Jump(d) => push_dest(d),
                Transfer::Cond { taken, fall } => {
                    push_dest(taken);
                    push_dest(fall);
                }
                Transfer::Call { next, .. } | Transfer::CondCall { next, .. } => push_dest(next),
                Transfer::Table { targets, .. } => work.extend(targets.iter().map(|&t| Node::Normal(t))),
                _ => {}
            }
            work.extend(g.inline_returns.iter().map(|&a| Node::Normal(a)));
        }
        // `jr ra` outside the function's own extent returns from an inlined
        // callee.
        let (own_start, own_end, _) = g.regions[0].clone();
        let returns: Vec<u32> = g.inline_returns.iter().copied().collect();
        for (n, d) in g.nodes.iter_mut() {
            if let (Node::Slot(b), Transfer::Return) = (n, &d.transfer) {
                if !(own_start..own_end).contains(b) {
                    d.transfer = Transfer::InlineReturn { targets: returns.clone() };
                }
            }
        }
        Ok(g)
    }

    fn transfer(&self, g: &mut Graph, pc: u32, insn: &Insn) -> Result<Transfer, LiftError> {
        let after = pc + 8;
        Ok(match insn.op {
            Op::Beq if insn.rs == insn.rt => Transfer::Jump(self.dest(g, pc, insn.branch_target(pc))?),
            Op::Bgez if insn.rs == 0 => Transfer::Jump(self.dest(g, pc, insn.branch_target(pc))?),
            Op::Beq | Op::Bne | Op::Blez | Op::Bgtz | Op::Bltz | Op::Bgez => Transfer::Cond {
                taken: self.dest(g, pc, insn.branch_target(pc))?,
                fall: self.dest(g, pc, after)?,
            },
            Op::Bgezal if insn.rs == 0 => Transfer::Call {
                callee: self.callee(g, pc, insn.branch_target(pc), Source::CallTarget)?,
                next: self.dest(g, pc, after)?,
            },
            Op::Bltzal | Op::Bgezal => Transfer::CondCall {
                callee: self.callee(g, pc, insn.branch_target(pc), Source::CallTarget)?,
                next: self.dest(g, pc, after)?,
            },
            Op::J => Transfer::Jump(self.dest(g, pc, insn.jump_target(pc))?),
            Op::Jal => {
                let target = insn.jump_target(pc);
                if self.in_extent(g, target) && self.inline.keys().any(|(_, a)| *a == target) || self.try_inline(g, target)
                {
                    g.inline_returns.insert(after);
                    Transfer::Jump(Dest::Local(target))
                } else {
                    Transfer::Call {
                        callee: self.callee(g, pc, target, Source::CallTarget)?,
                        next: self.dest(g, pc, after)?,
                    }
                }
            }
            Op::Jr if insn.rs == 31 => Transfer::Return,
            Op::Jr => match self.window(pc, insn.rs) {
                Window::Const(c) if self.program.locate(&self.func.container, c).is_some() => {
                    g.const_targets.insert(pc, (insn.rs, c));
                    match self.program.locate(&self.func.container, c) {
                        Some(Located::Bios(t)) => Transfer::Jump(Dest::Tail(Callee::Bios { table: t })),
                        _ => Transfer::Jump(self.dest(g, pc, c)?),
                    }
                }
                Window::Table { base, len } => {
                    let mut targets = Vec::with_capacity(len as usize);
                    for i in 0..len {
                        let at = base + 4 * i;
                        let t = self
                            .originals
                            .word(&self.func.container, at)
                            .or_else(|| self.originals.word(&self.program.main, at));
                        match t {
                            Some(t) if self.in_extent(g, t) && t & 3 == 0 => targets.push(t),
                            Some(t) => {
                                return err(
                                    pc,
                                    format!("jump table 0x{base:08X}[{i}] = 0x{t:08X} lies outside the function"),
                                );
                            }
                            None => return err(pc, format!("jump table 0x{base:08X} is not in any original image")),
                        }
                    }
                    Transfer::Table { reg: insn.rs, base, targets }
                }
                Window::Unbounded(base) => {
                    return err(pc, format!("jump table at 0x{base:08X} has no recognizable bound"));
                }
                // Hand-written code's table inside the function's own text:
                // its entries are the words that point into the function.
                Window::Embedded(base) => {
                    let mut targets = Vec::new();
                    while targets.len() < 256 {
                        let at = base + 4 * targets.len() as u32;
                        match self.originals.word(&self.func.container, at) {
                            Some(t) if self.in_extent(g, t) && t & 3 == 0 && !(base..at + 4).contains(&t) => targets.push(t),
                            _ => break,
                        }
                    }
                    if targets.is_empty() {
                        Transfer::Jump(Dest::Tail(Callee::Indirect { reg: insn.rs }))
                    } else {
                        g.embedded_tables.insert(base, targets.len() as u32);
                        Transfer::Table { reg: insn.rs, base, targets }
                    }
                }
                // Base plus a scaled index, the base not constant: a jump
                // into a block entered at computed offsets. Each such block's
                // branch-free head is expanded here, every instruction of it
                // a target, so the registers the jump carries stay shared.
                Window::Computed => {
                    let targets = self.computed_targets(g);
                    match targets.first() {
                        Some(&base) => Transfer::Table { reg: insn.rs, base, targets },
                        None => Transfer::Jump(Dest::Tail(Callee::Indirect { reg: insn.rs })),
                    }
                }
                Window::Unknown | Window::Const(_) => Transfer::Jump(Dest::Tail(Callee::Indirect { reg: insn.rs })),
            },
            Op::Jalr => {
                let callee = match self.window(pc, insn.rs) {
                    Window::Const(c) if self.program.locate(&self.func.container, c).is_some() => {
                        g.const_targets.insert(pc, (insn.rs, c));
                        self.callee(g, pc, c, Source::CallTarget)?
                    }
                    _ => Callee::Indirect { reg: insn.rs },
                };
                Transfer::Call { callee, next: self.dest(g, pc, after)? }
            }
            _ => unreachable!("not a branch: {:?}", insn.op),
        })
    }

    /// Evaluate `reg` just before `pc` by straight-line symbolic execution
    /// of the preceding instructions, to recognize constant targets and the
    /// GCC/ASPSX jump-table idiom:
    ///
    /// ```text
    /// sltiu t, idx, N ; beqz t, default ; sll s, idx, 2
    /// lui at, %hi(T)  ; addu at, at, s  ; lw r, %lo(T)(at) ; jr r
    /// ```
    fn window(&self, pc: u32, reg: u8) -> Window {
        const SPAN: u32 = 160;
        if self.not_const.contains(&pc) {
            return Window::Unknown;
        }
        let start = pc.saturating_sub(4 * SPAN).max(self.func.addr);
        let mut sym = SymState::new();
        let mut a = start;
        while a < pc {
            let Ok(insn) = self.word(a) else { return Window::Unknown };
            sym.step(&insn);
            a += 4;
        }
        match sym.regs[reg as usize].clone() {
            Sym::Const(c) => Window::Const(c),
            Sym::Sum(terms, _) if terms.iter().any(|t| matches!(t, Sym::Shl(..))) => Window::Computed,
            Sym::Load(addr) => {
                let (terms, base) = addr.flatten();
                match terms.as_slice() {
                    [Sym::Shl(idx, 2)] => match sym.bound_for(idx) {
                        Some(len) => Window::Table { base, len },
                        None if self.in_own_text(base) => Window::Embedded(base),
                        None => Window::Unbounded(base),
                    },
                    [_] if self.in_own_text(base) => Window::Embedded(base),
                    _ => Window::Unknown,
                }
            }
            _ => Window::Unknown,
        }
    }
}

enum Window {
    Const(u32),
    Table { base: u32, len: u32 },
    Unbounded(u32),
    /// A table of code addresses in the function's own text.
    Embedded(u32),
    /// A base plus a scaled index, the base not constant.
    Computed,
    Unknown,
}

/// Symbolic register values for [`Explorer::window`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Sym {
    Const(u32),
    /// The register's value at the window start, or an unknown result.
    Opaque(u32),
    Shl(Box<Sym>, u8),
    /// Sum of terms plus a constant.
    Sum(Vec<Sym>, u32),
    Load(Box<Sym>),
    /// Result of `sltiu` number `id`.
    Less(u32),
}

impl Sym {
    fn flatten(&self) -> (Vec<Sym>, u32) {
        match self {
            Sym::Const(c) => (vec![], *c),
            Sym::Sum(t, k) => (t.clone(), *k),
            other => (vec![other.clone()], 0),
        }
    }

    fn add(&self, other: &Sym) -> Sym {
        let (mut t, k) = self.flatten();
        let (t2, k2) = other.flatten();
        t.extend(t2);
        t.sort();
        let k = k.wrapping_add(k2);
        match (t.len(), k) {
            (0, _) => Sym::Const(k),
            (1, 0) => t.pop().unwrap_or(Sym::Const(0)),
            _ => Sym::Sum(t, k),
        }
    }
}

struct SymState {
    regs: Vec<Sym>,
    /// Stack slots written in the window (`sw r, k(sp)`), by address.
    stack: BTreeMap<Sym, Sym>,
    next_id: u32,
    /// `sltiu` results: id → (operand, bound).
    less: BTreeMap<u32, (Sym, u32)>,
    /// `sltiu` ids tested against zero by a branch.
    tested: BTreeSet<u32>,
}

impl SymState {
    fn new() -> Self {
        let mut regs: Vec<Sym> = (0..32).map(Sym::Opaque).collect();
        regs[0] = Sym::Const(0);
        Self { regs, stack: BTreeMap::new(), next_id: 32, less: BTreeMap::new(), tested: BTreeSet::new() }
    }

    fn fresh(&mut self) -> Sym {
        self.next_id += 1;
        Sym::Opaque(self.next_id)
    }

    fn set(&mut self, r: u8, v: Sym) {
        if r != 0 {
            self.regs[r as usize] = v;
        }
    }

    fn step(&mut self, i: &Insn) {
        let s = self.regs[i.rs as usize].clone();
        let t = self.regs[i.rt as usize].clone();
        match i.op {
            Op::Lui => self.set(i.rt, Sym::Const(i.uimm << 16)),
            Op::Addiu | Op::Addi => self.set(i.rt, s.add(&Sym::Const(i.simm as u32))),
            Op::Addu | Op::Add => self.set(i.rd, s.add(&t)),
            Op::Ori => {
                let v = match s {
                    Sym::Const(c) => Sym::Const(c | i.uimm),
                    _ => self.fresh(),
                };
                self.set(i.rt, v)
            }
            Op::Or if i.rt == 0 => self.set(i.rd, s),
            Op::Or if i.rs == 0 => self.set(i.rd, t),
            // Not folded even for a constant: a table index set to a constant
            // on some path must keep its identity with the bounds check.
            Op::Sll => {
                let v = if i.sa == 0 { t } else { Sym::Shl(Box::new(t), i.sa) };
                self.set(i.rd, v)
            }
            Op::Lw => {
                let addr = s.add(&Sym::Const(i.simm as u32));
                let v = self.stack.get(&addr).cloned().unwrap_or_else(|| Sym::Load(Box::new(addr)));
                self.set(i.rt, v)
            }
            Op::Sw if i.rs == 29 => {
                self.stack.insert(s.add(&Sym::Const(i.simm as u32)), t);
            }
            Op::Sltiu => {
                self.next_id += 1;
                let id = self.next_id;
                self.less.insert(id, (s, i.simm as u32));
                self.set(i.rt, Sym::Less(id))
            }
            Op::Beq | Op::Bne => {
                for (a, b) in [(&s, &t), (&t, &s)] {
                    if let (Sym::Less(id), Sym::Const(0)) = (a, b) {
                        self.tested.insert(*id);
                    }
                }
            }
            Op::Jal | Op::Jalr => {
                for r in [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 24, 25, 31] {
                    let v = self.fresh();
                    self.set(r, v);
                }
            }
            _ => {
                if let Some(r) = i.writes() {
                    let v = self.fresh();
                    self.set(r, v);
                }
            }
        }
    }

    fn bound_for(&self, idx: &Sym) -> Option<u32> {
        self.less
            .iter()
            .rev()
            .find(|(id, (operand, _))| operand == idx && self.tested.contains(id))
            .map(|(_, (_, n))| *n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(words: &[u32]) -> SymState {
        let mut s = SymState::new();
        for &w in words {
            s.step(&decode(w));
        }
        s
    }

    #[test]
    fn recognizes_gcc_jump_table() {
        // sltiu v0, a0, 5 ; beqz v0, +x ; sll v0, a0, 2 ; lui at, 0x8001
        // addu at, at, v0 ; lw v0, 0x1234(at)
        let s = run(&[0x2C82_0005, 0x1040_0010, 0x0004_1080, 0x3C01_8001, 0x0022_0821, 0x8C22_1234]);
        let Sym::Load(addr) = &s.regs[2] else { panic!("{:?}", s.regs[2]) };
        let (terms, base) = addr.flatten();
        assert_eq!(base, 0x8001_1234);
        let [Sym::Shl(idx, 2)] = terms.as_slice() else { panic!("{terms:?}") };
        assert_eq!(s.bound_for(idx), Some(5));
    }

    #[test]
    fn constant_bios_target() {
        // addiu t2, zero, 0xB0
        let s = run(&[0x240A_00B0]);
        assert_eq!(s.regs[10], Sym::Const(0xB0));
    }
}
