//! Generated C for one function, in the o32 form
//! `uint64_t f(uint32_t a0, uint32_t a1, uint32_t a2, uint32_t a3)`.
//!
//! Registers are locals; control flow is labels and `goto`. A branch's
//! condition (or a jump's target) is read before its delay slot runs. A
//! load whose next instruction reads the loaded register is emitted with
//! that instruction inlined after it, reading the old value: the R3000 load
//! delay. Loop heads carry a compiler memory barrier, so every iteration
//! performs its loads as the original does.

use crate::analysis::Facts;
use crate::decode::{Insn, Op, disasm};
use crate::graph::{Callee, Dest, Graph, LiftError, Node, Transfer};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

/// A register read redirected to another expression (the pre-load value).
type Ov = Option<(u8, String)>;

pub struct Emitter<'a> {
    g: &'a Graph,
    facts: &'a Facts,
    preds: BTreeMap<Node, Vec<Node>>,
    skip: BTreeSet<Node>,
    temps: BTreeSet<String>,
    pub externs: BTreeSet<String>,
    body: String,
}

impl<'a> Emitter<'a> {
    pub fn new(g: &'a Graph, facts: &'a Facts) -> Result<Self, LiftError> {
        let preds = g.preds();
        for (&n, _) in &facts.hazards {
            let succs = g.succs(n);
            let ok = matches!(n, Node::Normal(_)) && succs.len() == 1 && matches!(succs[0], Node::Normal(_));
            if !ok {
                return Err(LiftError {
                    pc: n.pc(),
                    message: "load-delay hazard after a load in a delay slot".into(),
                });
            }
        }
        Ok(Self {
            g,
            facts,
            preds,
            skip: BTreeSet::new(),
            temps: BTreeSet::new(),
            externs: BTreeSet::new(),
            body: String::new(),
        })
    }

    /// The function's C definition.
    pub fn function(mut self, cname: &str, comment: &str) -> (String, BTreeSet<String>) {
        let order: Vec<Node> = self.g.nodes.keys().copied().filter(|n| matches!(n, Node::Normal(_))).collect();
        let entry = Node::Normal(self.g.entry);
        if order.first() != Some(&entry) {
            let _ = writeln!(self.body, "  goto L_{:08X};", self.g.entry);
        }
        let mut pending: Option<Node> = None;
        for &n in &order {
            if self.skip.contains(&n) {
                continue;
            }
            if let Some(p) = pending.take() {
                if p != n {
                    let _ = writeln!(self.body, "  goto L_{:08X};", p.pc());
                }
            }
            pending = self.normal(n, None, true);
        }
        if let Some(p) = pending {
            let _ = writeln!(self.body, "  goto L_{:08X};", p.pc());
        }

        let mut out = String::new();
        let _ = writeln!(out, "/* {comment} */");
        let _ = writeln!(out, "uint64_t {cname}(uint32_t a0, uint32_t a1, uint32_t a2, uint32_t a3) {{");
        let mut decls = Vec::new();
        for r in 1..32u32 {
            let init = match r {
                4..=7 => format!("a{}", r - 4),
                28 => "psxw_gp_get()".to_string(),
                29 => "psxw_sp_get()".to_string(),
                _ => "0".to_string(),
            };
            decls.push(format!("r{r} = {init}"));
        }
        let _ = writeln!(out, "  uint32_t {};", decls.join(", "));
        let mut temps = vec!["hi = 0".to_string(), "lo = 0".into(), "c_ = 0".into(), "t_ = 0".into()];
        temps.extend(self.temps.iter().map(|t| format!("{t} = 0")));
        let _ = writeln!(out, "  uint32_t {};", temps.join(", "));
        let _ = writeln!(out, "  (void)a0; (void)a1; (void)a2; (void)a3; (void)c_; (void)t_;");
        out.push_str(&self.body);
        out.push_str("}\n");
        (out, self.externs)
    }

    fn line(&mut self, s: &str) {
        self.body.push_str("  ");
        self.body.push_str(s);
        self.body.push('\n');
    }

    /// Emit a `Normal` node (with its delay slot, if a branch). Returns the
    /// node control falls through to, if any.
    fn normal(&mut self, n: Node, ov: Ov, label: bool) -> Option<Node> {
        let pc = n.pc();
        let insn = self.g.nodes[&n].insn;
        if label {
            // A loop head: the original re-reads memory every iteration,
            // and interrupt handlers and callbacks (run at safepoints the
            // linker adds later) can change it, so clang must not keep
            // loads in registers across iterations.
            let head = self.preds.get(&n).is_some_and(|ps| ps.iter().any(|p| p.pc() >= pc));
            let barrier = if head { " __asm__ __volatile__(\"\" ::: \"memory\");" } else { "" };
            let _ = writeln!(self.body, "L_{pc:08X}:;{barrier}");
        }
        if insn.op.has_delay_slot() {
            return self.group(pc, &ov);
        }
        let mut ov_next: Ov = None;
        if let Some(&r) = self.facts.hazards.get(&n) {
            let tmp = format!("o_{pc:08X}");
            self.temps.insert(tmp.clone());
            let read = reg(r, &ov);
            self.line(&format!("{tmp} = {read};"));
            ov_next = Some((r, tmp));
        }
        let s = stmt(&insn, pc, &ov);
        self.line(&format!("{s} /* {pc:08X}: {} */", disasm(&insn, pc)));
        match self.g.nodes[&n].transfer.clone() {
            Transfer::Seq(next) => {
                if ov_next.is_some() {
                    if self.preds.get(&next).is_some_and(|p| p.len() == 1) {
                        self.skip.insert(next);
                    }
                    return self.normal(next, ov_next, false);
                }
                Some(next)
            }
            Transfer::FallOff(c) => {
                let call = self.call(&c);
                self.line(&format!("return {call}; /* falls into the next function */"));
                None
            }
            other => unreachable!("non-branch with transfer {other:?}"),
        }
    }

    fn group(&mut self, b: u32, ov: &Ov) -> Option<Node> {
        let head = self.g.nodes[&Node::Normal(b)].insn;
        let slot = self.g.nodes[&Node::Slot(b)].clone();
        let rs = reg(head.rs, ov);
        let rt = reg(head.rt, ov);
        let cond = match head.op {
            Op::Beq => Some(format!("{rs} == {rt}")),
            Op::Bne => Some(format!("{rs} != {rt}")),
            Op::Blez => Some(format!("(int32_t){rs} <= 0")),
            Op::Bgtz => Some(format!("(int32_t){rs} > 0")),
            Op::Bltz | Op::Bltzal => Some(format!("(int32_t){rs} < 0")),
            Op::Bgez | Op::Bgezal => Some(format!("(int32_t){rs} >= 0")),
            _ => None,
        };
        let dis = disasm(&head, b);
        let mut c_expr = cond.clone();
        if let Some(c) = &cond {
            let slot_writes = slot.insn.writes().map_or(0, |r| 1u32 << r);
            if head.reads() & slot_writes != 0 || ov.is_some() {
                self.line(&format!("c_ = {c}; /* {b:08X}: {dis} */"));
                c_expr = Some("c_".into());
            }
        }
        if matches!(head.op, Op::Jr | Op::Jalr) {
            self.line(&format!("t_ = {rs}; /* {b:08X}: {dis} */"));
        }
        match head.op {
            Op::Jal | Op::Bltzal | Op::Bgezal => self.line(&format!("r31 = 0x{:08X}u;", b + 8)),
            Op::Jalr if head.rd != 0 => self.line(&format!("r{} = 0x{:08X}u;", head.rd, b + 8)),
            _ => {}
        }
        let s = stmt(&slot.insn, b + 4, &None);
        self.line(&format!("{s} /* {:08X}: {} (delay slot) */", b + 4, disasm(&slot.insn, b + 4)));
        let c = c_expr.unwrap_or_default();
        match slot.transfer {
            Transfer::Jump(d) => {
                let d = self.dest(&d);
                self.line(&format!("{d} /* {b:08X}: {dis} */"));
                None
            }
            Transfer::Cond { taken, fall } => {
                let t = self.dest(&taken);
                self.line(&format!("if ({c}) {t} /* {b:08X}: {dis} */"));
                self.fall(&fall)
            }
            Transfer::Call { callee, next } => {
                let call = self.call(&callee);
                self.line(&format!(
                    "{{ uint64_t v_ = {call}; r2 = (uint32_t)v_; r3 = (uint32_t)(v_ >> 32); }} /* {b:08X}: {dis} */"
                ));
                self.fall(&next)
            }
            Transfer::CondCall { callee, next } => {
                let call = self.call(&callee);
                self.line(&format!(
                    "if ({c}) {{ uint64_t v_ = {call}; r2 = (uint32_t)v_; r3 = (uint32_t)(v_ >> 32); }} /* {b:08X}: {dis} */"
                ));
                self.fall(&next)
            }
            Transfer::Return => {
                self.line("return ((uint64_t)r3 << 32) | r2;");
                None
            }
            Transfer::Table { base: _, targets, .. } | Transfer::InlineReturn { targets } => {
                self.line("switch (t_) {");
                let set: BTreeSet<u32> = targets.into_iter().collect();
                for t in set {
                    self.line(&format!("case 0x{t:08X}u: goto L_{t:08X};"));
                }
                self.line(&format!("default: psxw_bad_jump(0x{b:08X}u, t_);"));
                self.line("}");
                None
            }
            Transfer::Seq(_) | Transfer::FallOff(_) => unreachable!("branch without a transfer"),
        }
    }

    fn fall(&mut self, d: &Dest) -> Option<Node> {
        match d {
            Dest::Local(a) => Some(Node::Normal(*a)),
            Dest::Tail(c) => {
                let call = self.call(c);
                self.line(&format!("return {call};"));
                None
            }
        }
    }

    fn dest(&mut self, d: &Dest) -> String {
        match d {
            Dest::Local(a) => format!("goto L_{a:08X};"),
            Dest::Tail(c) => format!("return {};", self.call(c)),
        }
    }

    fn call(&mut self, c: &Callee) -> String {
        match c {
            Callee::Direct { symbol, .. } => {
                self.externs.insert(symbol.clone());
                format!("{symbol}(r4, r5, r6, r7)")
            }
            Callee::Indirect { .. } => "psxw_call_indirect(t_, r4, r5, r6, r7)".into(),
            Callee::Bios { table } => format!("psxw_bios(0x{table:02X}u, r9, r4, r5, r6, r7)"),
            Callee::Kernel { addr } => format!("psxw_kernel_call(0x{addr:08X}u, r4, r5, r6, r7)"),
        }
    }
}

fn reg(r: u8, ov: &Ov) -> String {
    match ov {
        _ if r == 0 => "0u".into(),
        Some((o, name)) if *o == r => name.clone(),
        _ => format!("r{r}"),
    }
}

fn write(r: u8, expr: String, pure: bool) -> String {
    match r {
        0 if pure => ";".into(),
        0 => format!("(void)({expr});"),
        28 => format!("r28 = {expr}; psxw_gp_set(r28);"),
        29 => format!("r29 = {expr}; psxw_sp_set(r29);"),
        _ => format!("r{r} = {expr};"),
    }
}

/// The C statement for a non-control instruction.
pub fn stmt(i: &Insn, pc: u32, ov: &Ov) -> String {
    let s = reg(i.rs, ov);
    let t = reg(i.rt, ov);
    let imm = format!("0x{:X}u", i.simm as u32);
    let uimm = format!("0x{:X}u", i.uimm);
    let addr = if i.simm == 0 { s.clone() } else { format!("{s} + {imm}") };
    let w = |r: u8, e: String| write(r, e, true);
    match i.op {
        Op::Sll if i.word == 0 => ";".into(),
        Op::Sll => w(i.rd, format!("{t} << {}", i.sa)),
        Op::Srl => w(i.rd, format!("{t} >> {}", i.sa)),
        Op::Sra => w(i.rd, format!("(uint32_t)((int32_t){t} >> {})", i.sa)),
        Op::Sllv => w(i.rd, format!("{t} << ({s} & 31)")),
        Op::Srlv => w(i.rd, format!("{t} >> ({s} & 31)")),
        Op::Srav => w(i.rd, format!("(uint32_t)((int32_t){t} >> ({s} & 31))")),
        Op::Mfhi => w(i.rd, "hi".into()),
        Op::Mflo => w(i.rd, "lo".into()),
        Op::Mthi => format!("hi = {s};"),
        Op::Mtlo => format!("lo = {s};"),
        Op::Mult => format!("{{ uint64_t p_ = psxw_mult({s}, {t}); lo = (uint32_t)p_; hi = (uint32_t)(p_ >> 32); }}"),
        Op::Multu => format!("{{ uint64_t p_ = (uint64_t){s} * {t}; lo = (uint32_t)p_; hi = (uint32_t)(p_ >> 32); }}"),
        Op::Div => format!("{{ uint64_t q_ = psxw_div({s}, {t}); lo = (uint32_t)q_; hi = (uint32_t)(q_ >> 32); }}"),
        Op::Divu => format!("{{ uint64_t q_ = psxw_divu({s}, {t}); lo = (uint32_t)q_; hi = (uint32_t)(q_ >> 32); }}"),
        Op::Add => format!("{{ uint32_t s_ = psxw_add_trap({s}, {t}, 0x{pc:08X}u); {} }}", w(i.rd, "s_".into())),
        Op::Addu => w(i.rd, format!("{s} + {t}")),
        Op::Sub => format!("{{ uint32_t s_ = psxw_sub_trap({s}, {t}, 0x{pc:08X}u); {} }}", w(i.rd, "s_".into())),
        Op::Subu => w(i.rd, format!("{s} - {t}")),
        Op::And => w(i.rd, format!("{s} & {t}")),
        Op::Or => w(i.rd, format!("{s} | {t}")),
        Op::Xor => w(i.rd, format!("{s} ^ {t}")),
        Op::Nor => w(i.rd, format!("~({s} | {t})")),
        Op::Slt => w(i.rd, format!("(uint32_t)((int32_t){s} < (int32_t){t})")),
        Op::Sltu => w(i.rd, format!("(uint32_t)({s} < {t})")),
        Op::Addi => format!("{{ uint32_t s_ = psxw_add_trap({s}, {imm}, 0x{pc:08X}u); {} }}", w(i.rt, "s_".into())),
        Op::Addiu => w(i.rt, format!("{s} + {imm}")),
        Op::Slti => w(i.rt, format!("(uint32_t)((int32_t){s} < {})", i.simm)),
        Op::Sltiu => w(i.rt, format!("(uint32_t)({s} < {imm})")),
        Op::Andi => w(i.rt, format!("{s} & {uimm}")),
        Op::Ori => w(i.rt, format!("{s} | {uimm}")),
        Op::Xori => w(i.rt, format!("{s} ^ {uimm}")),
        Op::Lui => w(i.rt, format!("0x{:X}u", i.uimm << 16)),
        Op::Mfc0 => write(i.rt, format!("psxw_cop0_read({})", i.rd), false),
        Op::Mtc0 => format!("psxw_cop0_write({}, {t});", i.rd),
        Op::Rfe => "psxw_rfe();".into(),
        Op::Mfc2 => write(i.rt, format!("psx_gte_mfc2({})", i.rd), false),
        Op::Cfc2 => write(i.rt, format!("psx_gte_cfc2({})", i.rd), false),
        Op::Mtc2 => format!("psx_gte_mtc2({}, {t});", i.rd),
        Op::Ctc2 => format!("psx_gte_ctc2({}, {t});", i.rd),
        Op::Cop2 => format!("psx_gte_exec(0x{:07X}u);", i.word & 0x01FF_FFFF),
        Op::Lb => w(i.rt, format!("(uint32_t)(int32_t)(int8_t)PSXW_LB({addr})")),
        Op::Lbu => w(i.rt, format!("(uint32_t)PSXW_LB({addr})")),
        Op::Lh => w(i.rt, format!("(uint32_t)(int32_t)(int16_t)PSXW_LH({addr})")),
        Op::Lhu => w(i.rt, format!("(uint32_t)PSXW_LH({addr})")),
        Op::Lw => w(i.rt, format!("PSXW_LW({addr})")),
        Op::Lwl => w(i.rt, format!("psxw_lwl({addr}, {t})")),
        Op::Lwr => w(i.rt, format!("psxw_lwr({addr}, {t})")),
        Op::Sb => format!("PSXW_SB({addr}, {t});"),
        Op::Sh => format!("PSXW_SH({addr}, {t});"),
        Op::Sw => format!("PSXW_SW({addr}, {t});"),
        Op::Swl => format!("psxw_swl({addr}, {t});"),
        Op::Swr => format!("psxw_swr({addr}, {t});"),
        Op::Lwc2 => format!("psx_gte_mtc2({}, PSXW_LW({addr}));", i.rt),
        Op::Swc2 => format!("PSXW_SW({addr}, psx_gte_mfc2({}));", i.rt),
        Op::Syscall => write(2, format!("psxw_syscall({}, {}, {}, {})", reg(4, ov), reg(5, ov), reg(6, ov), reg(7, ov)), false),
        Op::Break => format!("__psxw_break(0x{:X});", (i.word >> 6) & 0xF_FFFF),
        Op::Invalid
        | Op::Jr
        | Op::Jalr
        | Op::J
        | Op::Jal
        | Op::Beq
        | Op::Bne
        | Op::Blez
        | Op::Bgtz
        | Op::Bltz
        | Op::Bgez
        | Op::Bltzal
        | Op::Bgezal => unreachable!("{:?} is not a plain statement", i.op),
    }
}
