//! Threads (B0h:0Eh-10h) over the kernel's thread control blocks in guest
//! RAM (D57; psx-spx "BIOS Thread Functions"). Games read and edit the
//! TCBs directly (a thread's saved status register, say), so they live
//! where the kernel's do, laid out from the table of tables
//! (psxw-abi [`kernel`](psxw_abi::kernel)).
//!
//! The BIOS only keeps the books here. Switching the guest's call stacks is
//! the runtime's job: ChangeThread records the target in
//! [`Threads::switch_to`], and the runtime suspends the running thread and
//! starts or resumes that one ([`Machine::enter_thread`]).

use crate::machine::{Machine, Sdk};
use psxw_abi::kernel as k;

/// The main thread's handle; thread `i`'s is this ORed with `i`.
pub const HANDLE: u32 = 0xFF00_0000;
/// SYSTEM.CNF's default `TCB=`.
pub const DEFAULT_TCBS: u32 = 4;
/// The TCB word after the status, which OpenThread sets to 1000h (the
/// device oracle's BIOS does; M13 task 13).
pub const TCB_FLAGS2: u32 = 0x04;

#[derive(Debug, Clone, Default)]
pub struct Threads {
    /// TCBs laid out; 0 until the boot (or the first thread call) lays
    /// them out.
    pub count: u32,
    /// The running thread's TCB.
    pub current: u32,
    /// ChangeThread's target, for the runtime to switch to.
    pub switch_to: Option<u32>,
    /// TCBs OpenThread filled since the runtime last started them: their
    /// next switch starts the thread afresh.
    pub fresh: u64,
}

/// A TCB's physical address.
pub fn tcb(i: u32) -> u32 {
    k::TCBS + i * k::TCB_SIZE
}

/// KSEG1, where the console's BIOS points at its tables.
fn kseg1(phys: u32) -> u32 {
    0xA000_0000 | phys
}

fn reg(i: u32, r: u32) -> u32 {
    tcb(i) + k::TCB_REGS + 4 * r
}

impl<S: Sdk> Machine<S> {
    /// Lay out the PCB and `n` TCBs (SYSTEM.CNF `TCB=`): TCB 0 is the
    /// running main thread, the rest are free.
    pub fn lay_out_threads(&mut self, n: u32) {
        let n = n.clamp(1, k::MAX_TCBS);
        let mem = &mut *self.mem;
        mem.w32(k::PCB_ENTRY, kseg1(k::PCB));
        mem.w32(k::PCB_ENTRY + 4, 4);
        mem.w32(k::TCB_ENTRY, kseg1(k::TCBS));
        mem.w32(k::TCB_ENTRY + 4, n * k::TCB_SIZE);
        mem.write(k::TCBS, &vec![0; (n * k::TCB_SIZE) as usize]);
        for i in 0..n {
            mem.w32(tcb(i) + k::TCB_STATUS, if i == 0 { k::TCB_USED } else { k::TCB_FREE });
        }
        mem.w32(k::PCB, kseg1(tcb(0)));
        self.kernel.threads = Threads { count: n, ..Threads::default() };
    }

    fn threads_ready(&mut self) {
        if self.kernel.threads.count == 0 {
            self.lay_out_threads(DEFAULT_TCBS);
        }
    }

    /// The TCB index a handle names, if it is a thread in use.
    fn thread_index(&self, h: u32) -> Option<u32> {
        let i = h & 0xFFFF;
        (h & 0xFFFF_0000 == HANDLE && i < self.kernel.threads.count && self.mem.r32(tcb(i) + k::TCB_STATUS) == k::TCB_USED)
            .then_some(i)
    }

    /// OpenThread(pc, sp, gp): a free TCB gets the thread's start address,
    /// its stack (as sp and fp) and gp, and 1000h in the word after its
    /// status. Nothing else in it changes (its saved SR stays what the game
    /// left there). The handle, or -1 when none is free.
    pub fn open_thread(&mut self, pc: u32, sp: u32, gp: u32) -> u32 {
        self.threads_ready();
        let n = self.kernel.threads.count;
        let Some(i) = (1..n).find(|&i| self.mem.r32(tcb(i) + k::TCB_STATUS) != k::TCB_USED) else { return u32::MAX };
        let mem = &mut *self.mem;
        mem.w32(tcb(i) + k::TCB_STATUS, k::TCB_USED);
        mem.w32(tcb(i) + TCB_FLAGS2, 0x1000);
        mem.w32(tcb(i) + k::TCB_EPC, pc);
        mem.w32(reg(i, 28), gp);
        mem.w32(reg(i, 29), sp);
        mem.w32(reg(i, 30), sp);
        self.kernel.threads.fresh |= 1 << i;
        HANDLE | i
    }

    /// CloseThread(handle): the TCB is free again. A thread may close
    /// itself; it runs on until it switches away, and never resumes.
    pub fn close_thread(&mut self, h: u32) -> u32 {
        self.threads_ready();
        if let Some(i) = self.thread_index(h).filter(|&i| i != 0) {
            self.mem.w32(tcb(i) + k::TCB_STATUS, k::TCB_FREE);
            self.kernel.threads.fresh &= !(1 << i);
        }
        1
    }

    /// ChangeThread(handle): save the running thread's sp and gp in its
    /// TCB, make the target current in the PCB, and leave the switch to the
    /// runtime. Returns 1, which the suspended thread sees when it resumes.
    pub fn change_thread(&mut self, h: u32) -> Result<u32, String> {
        self.threads_ready();
        let i = self.thread_index(h).ok_or_else(|| format!("ChangeThread({h:#010x}): no such thread"))?;
        let cur = self.kernel.threads.current;
        if i == cur {
            return Ok(1);
        }
        let (sp, gp) = (self.regs.sp(), self.regs.gp());
        self.mem.w32(reg(cur, 29), sp);
        self.mem.w32(reg(cur, 28), gp);
        self.mem.w32(k::PCB, kseg1(tcb(i)));
        self.kernel.threads.switch_to = Some(i);
        Ok(1)
    }

    /// The runtime switches to TCB `i`: it becomes current, and its sp and
    /// gp are the guest's. Returns its start address when it starts afresh
    /// (OpenThread since it last ran), `None` when it resumes.
    pub fn enter_thread(&mut self, i: u32) -> Option<u32> {
        self.kernel.threads.current = i;
        let (sp, gp) = (self.mem.r32(reg(i, 29)), self.mem.r32(reg(i, 28)));
        self.regs.set_sp(sp);
        self.regs.set_gp(gp);
        let fresh = self.kernel.threads.fresh & (1 << i) != 0;
        self.kernel.threads.fresh &= !(1 << i);
        fresh.then(|| self.mem.r32(tcb(i) + k::TCB_EPC))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::machine;

    #[test]
    fn tables_and_thread_books() {
        let mut m = machine();
        m.lay_out_threads(4);
        assert_eq!(m.mem.r32(k::TCB_ENTRY), 0xA000_0000 | k::TCBS);
        assert_eq!(m.mem.r32(k::TCB_ENTRY + 4), 4 * 0xC0);
        assert_eq!(m.mem.r32(m.mem.r32(k::PCB_ENTRY)), 0xA000_0000 | k::TCBS);
        // A game edits a free TCB's saved SR before opening it (enabling
        // COP2 in the thread); OpenThread keeps it.
        let tcbs = m.mem.r32(k::TCB_ENTRY);
        m.mem.w32(tcbs + 0xC0 + k::TCB_SR, 0x4000_0404);
        let h = m.open_thread(0x8001_0000, 0x801F_F600, 0x8017_C524);
        assert_eq!(h, 0xFF00_0001);
        assert_eq!(m.mem.r32(tcb(1) + k::TCB_SR), 0x4000_0404);
        assert_eq!(m.mem.r32(tcb(1) + TCB_FLAGS2), 0x1000);
        assert_eq!(m.mem.r32(reg(1, 29)), 0x801F_F600);
        assert_eq!(m.change_thread(h), Ok(1));
        assert_eq!(m.kernel.threads.switch_to.take(), Some(1));
        assert_eq!(m.mem.r32(reg(0, 29)), 0x801F_F000, "the main thread's sp is saved");
        assert_eq!(m.enter_thread(1), Some(0x8001_0000));
        assert_eq!(m.regs.sp(), 0x801F_F600);
        // Back to main: it resumes; the thread itself resumes next time.
        assert_eq!(m.change_thread(HANDLE), Ok(1));
        let to = m.kernel.threads.switch_to.take();
        assert_eq!(to, Some(0));
        assert_eq!(m.enter_thread(0), None);
        assert_eq!(m.enter_thread(1), None);
        assert_eq!(m.close_thread(h), 1);
        assert!(m.change_thread(h).is_err(), "a closed thread can't be switched to");
        assert_eq!(m.open_thread(0x8002_0000, 0x801F_F800, 0), h, "its TCB is reused, afresh");
        assert_eq!(m.enter_thread(1), Some(0x8002_0000));
    }

    #[test]
    fn bios_rand_is_the_kernel_lcg() {
        let mut m = machine();
        let rand = |m: &mut Machine<crate::testing::NoSdk>| match crate::bios::call(m, 0xA0, 0x2F, [0; 4]) {
            Ok(crate::Flow::Return(v)) => v as u32,
            _ => panic!("rand"),
        };
        let _ = crate::bios::call(&mut m, 0xA0, 0x30, [1, 0, 0, 0]);
        // x = x * 0x41C64E6D + 0x3039; (x >> 16) & 0x7FFF.
        let x1 = 1u32.wrapping_mul(0x41C6_4E6D).wrapping_add(0x3039);
        assert_eq!(rand(&mut m), (x1 >> 16) & 0x7FFF);
        let x2 = x1.wrapping_mul(0x41C6_4E6D).wrapping_add(0x3039);
        assert_eq!(rand(&mut m), (x2 >> 16) & 0x7FFF);
    }
}
