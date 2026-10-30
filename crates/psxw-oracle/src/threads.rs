//! BIOS threads (M13 task 13, D57): OpenThread, ChangeThread and
//! CloseThread on the target's BIOS, against psx-bios.
//!
//! **A vector** is a list of steps OP_BIOS_THREADS runs on the target's own
//! kernel, seen through its table of tables (`oracle/runner/threads.c`):
//! fill free TCBs with a pattern (so what OpenThread writes shows),
//! open threads, switch to them and back (each switch is a round of the
//! runner's thread body), close them, and read the tables. Results name
//! TCBs by index and sp/gp by their difference from what the caller had:
//! each BIOS lays out its own kernel memory.
//!
//! **The check** runs the same steps on psx-bios's [`Machine`] through its
//! B0h dispatcher. psx-bios only keeps the books (the runtime switches the
//! guest's stacks), so this module plays the runner's part around them:
//! after ChangeThread it enters the target thread as the runtime would,
//! runs the thread body's round (what it notes, then its ChangeThread
//! back), and enters the switching thread again.

use crate::commands::DeviceUnderTest;
use crate::store::Store;
use crate::vector::{BiosThreads, Vector, VectorId, thread_step_words};
use crate::wire::{self, TS_CHANGE, TS_CLOSE, TS_FILL, TS_OPEN, TS_TABLES};
use anyhow::{Context, Result, bail, ensure};
use psx_bios::{Flow, Host, Machine, Regs, Sdk};
use psx_devices::mem::VecMem;
use psxw_abi::kernel as k;
use std::collections::BTreeMap;

pub const SET: &str = "synthetic-threads";
/// The thread entry's address (the start of the scratch region).
pub const PC: u32 = wire::SCRATCH_BASE;
/// SYSTEM.CNF `TCB=` on the runner disc (`oracle/runner/disc/system.cnf`).
pub const TCBS: u32 = 4;
/// Thread `s`'s stack top.
fn stack(s: u32) -> u32 {
    wire::SCRATCH_BASE + 0x8000 + s * 0x1000
}
/// Thread `s`'s gp: anything (the runner never uses gp), but distinct.
fn gp(s: u32) -> u32 {
    0x8011_0000 + s * 0x100
}

const TCB_WORDS: u32 = k::TCB_SIZE / 4;

fn step(op: u32, a: u32, b: u32, c: u32) -> [u32; 4] {
    [op, a, b, c]
}

fn fill_free(pattern: u32, sr: u32) -> Vec<[u32; 4]> {
    (1..TCBS).map(|i| step(TS_FILL, i, pattern, sr)).collect()
}

fn open(slot: u32) -> [u32; 4] {
    step(TS_OPEN, slot, stack(slot), gp(slot))
}

fn change(slot: u32) -> [u32; 4] {
    step(TS_CHANGE, slot, 0, 0)
}

fn close(slot: u32) -> [u32; 4] {
    step(TS_CLOSE, slot, 0, 0)
}

const TABLES: [u32; 4] = [TS_TABLES, 0, 0, 0];

/// The step lists. Fill patterns keep the low byte zero and bits 16 and 22
/// clear, so a pattern word taken for a status register is harmless; the
/// SR word itself is set apart (40000000h: COP2 on, interrupts off; and
/// 40000404h, which a game may write so its threads run with interrupts on).
pub fn scripts() -> Vec<(&'static str, Vec<[u32; 4]>)> {
    let mut out = Vec::new();

    let mut s = vec![TABLES];
    s.extend(fill_free(0x4000_0000, 0x4000_0000));
    s.extend([open(0), TABLES, change(0), change(0), change(0), TABLES, close(0), TABLES, close(0)]);
    out.push(("one-thread", s));

    let mut s = fill_free(0x0000_0000, 0x4000_0404);
    s.extend([open(0), change(0), change(0), close(0)]);
    out.push(("sr-interrupts-on", s));

    let mut s = fill_free(0x4000_0000, 0x4000_0000);
    s.extend([
        open(0),
        open(1),
        TABLES,
        change(1),
        change(0),
        change(1),
        close(0),
        TABLES,
        // TCB 1 is free again: the next thread takes it, afresh. Refilled
        // first: the target's BIOS saved the closed thread's registers
        // there at its last switch, which psx-bios doesn't (they're wasm
        // locals, D57; it keeps only sp and gp).
        step(TS_FILL, 1, 0x4000_0000, 0x4000_0000),
        open(2),
        change(2),
        change(1),
        close(1),
        close(2),
        TABLES,
    ]);
    out.push(("two-threads", s));

    let mut s = fill_free(0x4000_0000, 0x4000_0000);
    s.extend([open(0), open(1), open(2), open(3), TABLES, close(1), open(4), TABLES, close(0), close(2), close(4), TABLES]);
    out.push(("table-full", s));
    out
}

/// Store the vectors as the `synthetic-threads` set.
pub fn install(store: &Store) -> Result<Vec<VectorId>> {
    let mut ids = Vec::new();
    for (_, steps) in scripts() {
        ids.push(store.put_vector(&Vector::BiosThreads(BiosThreads { pc: PC, tcbs: TCBS, steps }))?);
    }
    store.replace_set(SET, ids.iter().copied())?;
    Ok(ids)
}

// ---- psx-bios, driven like the runner drives the target's BIOS ------------

struct NullHost;

impl Host for NullHost {
    fn log(&mut self, _level: u32, _msg: &str) {}
    fn wait_vblank(&mut self, _frame: u64) {}
    fn disc_read(&mut self, _lba: u32, _out: &mut [u8; 2352]) -> bool {
        false
    }
    fn has_disc(&mut self) -> bool {
        false
    }
    fn pad_state(&mut self, _port: u32) -> Vec<u8> {
        Vec::new()
    }
    fn storage_read(&mut self, _name: &str) -> Option<Vec<u8>> {
        None
    }
    fn storage_write(&mut self, _name: &str, _data: &[u8]) -> bool {
        false
    }
    fn report(&mut self, _json: &str) {}
}

struct SimRegs {
    sp: u32,
    gp: u32,
}

impl Regs for SimRegs {
    fn sp(&self) -> u32 {
        self.sp
    }
    fn set_sp(&mut self, v: u32) {
        self.sp = v;
    }
    fn gp(&self) -> u32 {
        self.gp
    }
    fn set_gp(&mut self, v: u32) {
        self.gp = v;
    }
}

struct NoSdk;

impl Sdk for NoSdk {
    fn on_event(_m: &mut Machine<Self>, _kind: u32, _data: u32) {}
    fn ready(_m: &Machine<Self>, _cond: u32) -> bool {
        true
    }
    fn on_vblank(_m: &mut Machine<Self>) {}
}

/// The runner's main thread: its sp and gp at a call (any values; results
/// are differences).
const MAIN_SP: u32 = 0x801F_FE00;
const MAIN_GP: u32 = 0x8001_C230;
/// How far below its entry sp the thread body's ChangeThread is.
const BODY_FRAME: u32 = 0x40;

fn b0(m: &mut Machine<NoSdk>, func: u32, a: [u32; 4]) -> Result<u32> {
    match psx_bios::bios::call(m, 0xB0, func, a).map_err(|e| anyhow::anyhow!(e))? {
        Flow::Return(v) => Ok(v as u32),
        Flow::Block { .. } | Flow::Call { .. } => bail!("B0h:{func:02X}h blocked"),
    }
}

#[derive(Default, Clone, Copy)]
struct Call {
    sp: u32,
    gp: u32,
}

/// The runner's `th` record and each thread's locals.
#[derive(Default)]
struct Body {
    runs: u32,
    entry_sp: u32,
    entry_gp: u32,
    pcb_index: u32,
    resume_v0: u32,
    resume_sp_delta: u32,
    call: Call,
    /// Per TCB: what its last ChangeThread returned, and its sp at that
    /// call.
    pending: BTreeMap<u32, (u32, Call)>,
}

struct Sim {
    m: Machine<NoSdk>,
    /// The thread entry every thread must start at.
    pc: u32,
    body: Body,
}

impl Sim {
    fn word(&self, a: u32) -> u32 {
        self.m.mem.r32(a & 0x1FFF_FFFF)
    }
    fn count(&self) -> u32 {
        self.word(0x114) / k::TCB_SIZE
    }
    fn tcb(&self, i: u32) -> u32 {
        (self.word(0x110) & 0x1FFF_FFFF) + i * k::TCB_SIZE
    }
    fn tcb_word(&self, i: u32, w: u32) -> u32 {
        self.word(self.tcb(i) + 4 * w)
    }
    fn current(&self) -> u32 {
        let cur = self.word(self.word(0x108)) & 0x1FFF_FFFF;
        let base = self.word(0x110) & 0x1FFF_FFFF;
        match cur.checked_sub(base) {
            Some(d) if d % k::TCB_SIZE == 0 && d / k::TCB_SIZE < self.count() => d / k::TCB_SIZE,
            _ => u32::MAX,
        }
    }
    fn handle_index(&self, h: u32) -> Option<u32> {
        (h & 0xFFFF_0000 == 0xFF00_0000 && (h & 0xFFFF) < self.count()).then_some(h & 0xFFFF)
    }
    fn reg(r: u32) -> u32 {
        (k::TCB_REGS / 4) + r
    }

    /// ChangeThread, then what the runtime does: enter the target.
    /// Returns the target's TCB and whether it starts afresh.
    fn switch(&mut self, h: u32) -> Result<(u32, bool)> {
        let v = b0(&mut self.m, 0x10, [h, 0, 0, 0])?;
        let cur = self.m.kernel.threads.current;
        self.body.pending.insert(cur, (v, Call { sp: self.m.regs.sp(), gp: self.m.regs.gp() }));
        let to = self.m.kernel.threads.switch_to.take().context("ChangeThread didn't switch")?;
        let fresh = self.m.enter_thread(to);
        if let Some(pc) = fresh {
            ensure!(pc == self.pc, "a thread started at {pc:#010x}, not its entry");
        }
        Ok((to, fresh.is_some()))
    }

    fn change(&mut self, h: u32, w: &mut [u32]) -> Result<()> {
        let main = self.current();
        let Some(i) = self.handle_index(h) else { return Ok(()) };
        if self.tcb_word(i, 0) != k::TCB_USED || main == u32::MAX || i == main {
            return Ok(());
        }
        let back = 0xFF00_0000 | main;
        let main_call = Call { sp: self.m.regs.sp(), gp: self.m.regs.gp() };
        let (to, fresh) = self.switch(h)?;
        ensure!(to == i, "ChangeThread({h:#010x}) switched to TCB {to}");
        // The thread body's round.
        let (v0, delta) = if fresh {
            self.body.entry_sp = self.m.regs.sp();
            self.body.entry_gp = self.m.regs.gp();
            (0, 0)
        } else {
            let (v, at) = self.body.pending.get(&i).copied().unwrap_or_default();
            (v, self.m.regs.sp().wrapping_sub(at.sp))
        };
        self.body.runs += 1;
        self.body.resume_v0 = v0;
        self.body.resume_sp_delta = delta;
        self.body.pcb_index = self.current();
        if fresh {
            let sp = self.m.regs.sp() - BODY_FRAME;
            self.m.regs.set_sp(sp);
        }
        self.body.call = Call { sp: self.m.regs.sp(), gp: self.m.regs.gp() };
        let (back_to, _) = self.switch(back)?;
        ensure!(back_to == main, "the thread's ChangeThread back went to TCB {back_to}");
        let (v0_main, _) = self.body.pending.get(&main).copied().unwrap_or_default();
        let r = Self::reg;
        w.copy_from_slice(&[
            1,
            v0_main,
            self.body.runs,
            self.body.entry_sp,
            self.body.entry_gp,
            self.body.pcb_index,
            self.body.resume_v0,
            self.body.resume_sp_delta,
            self.tcb_word(i, r(29)).wrapping_sub(self.body.call.sp),
            self.tcb_word(i, r(28)).wrapping_sub(self.body.call.gp),
            self.tcb_word(main, r(29)).wrapping_sub(main_call.sp),
            self.tcb_word(main, r(28)).wrapping_sub(main_call.gp),
            self.current(),
        ]);
        Ok(())
    }
}

/// psx-bios through the vector's steps; the result in OP_BIOS_THREADS's
/// layout.
pub fn run(t: &BiosThreads) -> Result<Vec<u8>> {
    let mut m = Machine::new(
        Box::new(VecMem::default()),
        Box::new(NullHost),
        Box::new(SimRegs { sp: MAIN_SP, gp: MAIN_GP }),
        psxw_abi::BootOptions::default(),
        NoSdk,
    );
    m.lay_out_threads(t.tcbs);
    let mut sim = Sim { m, pc: t.pc, body: Body::default() };
    let mut slots = [u32::MAX; wire::THREAD_SLOTS as usize];
    let mut out = Vec::new();
    for s in &t.steps {
        let [op, a, b, c] = *s;
        let n = thread_step_words(op).with_context(|| format!("thread step {op}"))? as usize;
        let mut w = vec![0u32; n];
        match op {
            TS_TABLES => {
                w[0] = sim.word(0x10C);
                w[1] = sim.word(0x114);
                w[2] = sim.current();
                for i in 0..sim.count().min(wire::THREAD_MAX_TCBS) {
                    w[3 + i as usize] = sim.tcb_word(i, 0);
                }
            }
            TS_FILL => {
                if a < sim.count() && sim.tcb_word(a, 0) != k::TCB_USED {
                    let base = sim.tcb(a);
                    for i in 1..TCB_WORDS {
                        sim.m.mem.w32(base + 4 * i, b | i << 8);
                    }
                    sim.m.mem.w32(base + k::TCB_SR, c);
                }
            }
            TS_OPEN => {
                let h = b0(&mut sim.m, 0x0E, [t.pc, b, c, 0])?;
                slots[a as usize] = h;
                w[0] = h;
                if let Some(i) = sim.handle_index(h) {
                    for j in 0..TCB_WORDS {
                        w[1 + j as usize] = sim.tcb_word(i, j);
                    }
                }
            }
            TS_CHANGE => sim.change(slots[a as usize], &mut w)?,
            TS_CLOSE => {
                let h = slots[a as usize];
                if let Some(i) = sim.handle_index(h) {
                    w[0] = b0(&mut sim.m, 0x0F, [h, 0, 0, 0])?;
                    w[1] = sim.tcb_word(i, 0);
                }
            }
            _ => bail!("thread step {op}"),
        }
        out.extend(w.iter().flat_map(|v| v.to_le_bytes()));
    }
    Ok(out)
}

/// psx-bios's threads as the oracle's `threads` device.
pub struct Device;

impl DeviceUnderTest for Device {
    fn device(&self) -> &str {
        "threads"
    }

    fn name(&self) -> String {
        "psx-bios threads".into()
    }

    fn run(&mut self, v: &Vector, _: &Store) -> Result<Vec<u8>> {
        match v {
            Vector::BiosThreads(t) => run(t),
            other => bail!("the threads device cannot run a {} vector", other.kind()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(b: &[u8]) -> Vec<u32> {
        b.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
    }

    #[test]
    fn scripts_run_on_psx_bios() {
        for (name, steps) in scripts() {
            let v = BiosThreads { pc: PC, tcbs: TCBS, steps };
            Vector::BiosThreads(v.clone()).check().unwrap();
            let got = run(&v).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            assert_eq!(got.len(), Vector::BiosThreads(v.clone()).result_len(), "{name}");
        }
        // One thread: the tables, the TCB OpenThread wrote, three rounds.
        let (_, steps) = &scripts()[0];
        let w = words(&run(&BiosThreads { pc: PC, tcbs: TCBS, steps: steps.clone() }).unwrap());
        assert_eq!(&w[..7], &[4, 4 * 0xC0, 0, 0x4000, 0x1000, 0x1000, 0x1000]);
        let open = &w[11..60];
        assert_eq!(open[0], 0xFF00_0001);
        let tcb = &open[1..];
        assert_eq!((tcb[0], tcb[1]), (0x4000, 0x1000));
        assert_eq!(tcb[(0x88 / 4) as usize], PC);
        assert_eq!((tcb[2 + 28], tcb[2 + 29], tcb[2 + 30]), (gp(0), stack(0), stack(0)));
        assert_eq!(tcb[(0x94 / 4) as usize], 0x4000_0000);
        assert_eq!(tcb[2 + 4], 0x4000_0000 | 6 << 8, "a0 keeps the fill");
        let rounds: Vec<&[u32]> = (0..3).map(|r| &w[71 + 13 * r..71 + 13 * (r + 1)]).collect();
        assert_eq!(rounds[0], &[1, 1, 1, stack(0), gp(0), 1, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(rounds[2], &[1, 1, 3, stack(0), gp(0), 1, 1, 0, 0, 0, 0, 0, 0]);
        let close = &w[71 + 39 + 11..71 + 39 + 13];
        assert_eq!(close, &[1, 0x1000]);
    }
}
