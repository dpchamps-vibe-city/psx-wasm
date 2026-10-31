//! libetc (chip irq, D59): the interrupt layer (`intr.o`, `intr_vb.o`,
//! `intr_dma.o`) and `VSync` (`vsync.o`), as adapters.
//!
//! **State stays where libetc keeps it** (D67): `intr.o`'s context (its
//! flags, the line handlers, its mask), `intr_vb.o`'s `VSyncCallbacks`
//! table and `Vcount`, `intr_dma.o`'s `DMACallback` table, `vsync.o`'s last
//! `Vcount` and `Hcount`, found through the objects' code ([`Lib`],
//! [`VsyncO`]). The hardware they drive stays where the
//! emulated chips' devices see it (`psx_bios::io`): the interrupt
//! controller's I_STAT and I_MASK, the DMA controller's DPCR and DICR, root
//! counter 1 and GPUSTAT. Libraries that stay original read and write
//! those registers themselves (libpad's handlers test I_STAT, libgpu sets
//! DPCR), so the adapters use the devices' own register semantics
//! ([`crate::reg`]), never a copy.
//!
//! **Dispatch.** libetc's dispatcher is entered after the BIOS's
//! `SysEnqIntRP` chains, as on the device (D45), but as [`IrqStep::Sdk`]
//! steps instead of the custom exception exit: the chains still run first
//! (libpad's and libapi's elements), then for every pending line libetc
//! handles, in line order, it acknowledges I_STAT and calls the line's
//! handler, and looks again until none is left. Handlers are guest calls
//! in the exception, queued at the front so the dispatcher sees what each
//! one changed, like the original's loops over the registers. They run on
//! `intr.o`'s own stack, where the original's exit from the exception
//! enters its dispatcher: the interrupted code's may be the scratchpad,
//! which DMA doesn't reach (btn's menu; libcd reads sector headers into
//! its callback's frame).
//!
//! **Time** (D71): every adapter spends its original's blocks on the path
//! it takes, the dispatcher's from where the exception's exit resumes
//! (`startIntr`'s `setjmp`), with its loop headers' quanta
//! ([`crate::irq_header`]); `ResetCallback`'s clearing of the context is a
//! loop that takes safepoints. libapi's stubs count (a syscall's 4, a
//! vector's 3), `setjmp`'s doesn't: the lift lowers its call to stores.
//!
//! **libetc's own handlers are at their own addresses,** which
//! `InterruptCallback` returns and takes back (libsnd's tick handler takes
//! line 0 and calls libetc's first). Line 0 is `intr_vb.o`'s vblank handler
//! (Vcount, then the eight `VSyncCallbacks` channels), which touches no
//! hardware and runs as guest code over the state in RAM. Line 3 is
//! `intr_dma.o`'s DMA handler (DICR's flags, acknowledged one by one, each
//! channel's `DMACallback`): DICR is the chip's hardware, which a native
//! chip doesn't run as guest code, so it is an adapter bound by its place
//! (`libetc/intr_dma:0x4c`).
//!
//! Semantics are PsyQ 4.7's objects (`psxw sdk disasm`; psyq-decomp
//! `libetc/{intr,intr_vb,intr_dma,vsync}`), including what a caller can
//! see of a misuse: `InterruptCallback` does nothing before
//! `ResetCallback`, `StopCallback` leaves interrupts disabled.
//!
//! **What isn't reproduced.** The custom exception exit: the dispatcher is
//! entered as adapter steps, not by the BIOS longjmp-ing into the context's
//! `jmp_buf`, which `ResetCallback` doesn't fill (registers are wasm
//! locals). A callback that leaves with ReturnFromException from inside
//! libetc's own handler isn't supported.

use crate::{Adapter, M, Psyq, Result, bios, reg, ret, set_reg};
use psx_bios::{Flow, GuestCall, IrqStep};

pub const ADAPTERS: &[(&str, Adapter)] = &[
    ("ResetCallback", reset_callback),
    ("StopCallback", stop_callback),
    ("RestartCallback", restart_callback),
    ("CheckCallback", check_callback),
    ("InterruptCallback", interrupt_callback),
    ("DMACallback", dma_callback),
    ("VSyncCallback", vsync_callback),
    ("VSyncCallbacks", vsync_callbacks),
    ("GetIntrMask", get_intr_mask),
    ("SetIntrMask", set_intr_mask),
    ("VSync", vsync),
    // intr_dma.o's handler, static: its DICR accesses are the chip's
    // hardware, which a native chip doesn't run as guest code.
    ("libetc/intr_dma:0x4c", dma_handler),
];

const I_STAT: u32 = psx_devices::irq::I_STAT;
const I_MASK: u32 = psx_devices::irq::I_MASK;
const DPCR: u32 = psx_devices::dma::DPCR;
const DICR: u32 = psx_devices::dma::DICR;
/// Root counter 1's mode register.
const RCNT1_MODE: u32 = 0x1F80_1114;
const GPUSTAT: u32 = 0x1F80_1814;
/// Interrupt lines (I_STAT bits 0-10).
const LINES: usize = 11;
/// `vsync.o`'s waits (PsyQ 4.7), the original's instructions an iteration
/// (D71): the Vcount wait's countdown (+1A0h..+1BCh and +1E8h..+1FCh)
/// and the interlaced field's (+10Ch..+120h).
const VCOUNT_PASS: u32 = 14;
const FIELD_PASS: u32 = 6;

/// libetc's adapter state: where the objects are, and what the dispatcher
/// needs between its steps. Everything libetc itself keeps is in RAM.
#[derive(Debug, Default)]
pub struct Etc {
    pub lib: Option<Lib>,
    pub vsync: Option<VsyncO>,
    /// I_STAT as the dispatcher's pass read it, which the original leaves
    /// in a0 for the handlers.
    pub stat: u16,
    /// The interrupted code's sp while the dispatcher runs on `intr.o`'s
    /// stack.
    pub saved_sp: u32,
}

/// Where `intr.o`, `intr_vb.o` and `intr_dma.o` keep libetc's state (D67),
/// from `intr.o`'s code (psyq-decomp `libetc-intr.h`; plans/spec-fidelity.md,
/// Findings): the driver-table pointer `intr.o`'s functions load first, the
/// table's context, the `jal`s of `startIntrVSync` and `startIntrDMA`, and
/// their operands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lib {
    /// `intr.o`'s text (`ResetCallback`).
    pub intr: u32,
    pub driver: u32,
    pub context: u32,
    /// The dispatcher's count of unhandled exceptions in a row.
    pub spurious: u32,
    /// `intr_vb.o`: the `VSyncCallbacks` table, `Vcount`, its handler and
    /// the table's setter.
    pub vb_table: u32,
    pub vcount: u32,
    pub vb_handler: u32,
    pub vb_setter: u32,
    /// `intr_dma.o`: the `DMACallback` table, its handler and setter.
    pub dma_table: u32,
    pub dma_handler: u32,
    pub dma_setter: u32,
}

/// `vsync.o`'s state, from `VSync`'s code: `Vcount` (intr_vb's), and the
/// `Vcount` and root counter 1 the last waiting call left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VsyncO {
    pub text: u32,
    pub vcount: u32,
    pub last: u32,
    pub hcount: u32,
}

/// `intr.o`'s functions in its text, and its code operands.
mod at {
    pub const RESET: u32 = 0x0;
    pub const INTERRUPT: u32 = 0x30;
    pub const DMA: u32 = 0x60;
    pub const VSYNC_CALLBACK: u32 = 0x90;
    pub const VSYNC_CALLBACKS: u32 = 0xC4;
    pub const STOP: u32 = 0xF4;
    pub const RESTART: u32 = 0x124;
    pub const CHECK: u32 = 0x154;
    pub const JAL_START_VSYNC: u32 = 0x228;
    pub const JAL_START_DMA: u32 = 0x238;
    pub const SPURIOUS_PAIR: u32 = 0x3B4;
}

/// The context's fields (`IntrContext`).
mod ctx {
    pub const ACTIVE: u32 = 0x00;
    pub const DISPATCHING: u32 = 0x02;
    pub const LINES: u32 = 0x04;
    pub const MASK: u32 = 0x30;
    pub const SAVED_MASK: u32 = 0x32;
    pub const SAVED_DPCR: u32 = 0x34;
    /// Its size in words, all of which `ResetCallback` clears.
    pub const WORDS: u32 = 0x41A;
    /// Where the dispatcher's stack starts (`stack[1024 - 20]`).
    pub const STACK_TOP: u32 = 0x1018;
}

/// The driver table's setters.
mod driver {
    pub const DMA: u32 = 0x04;
    pub const VBLANK: u32 = 0x14;
    pub const CONTEXT: u32 = 0x1C;
}

fn pair(m: &M, at: u32) -> Option<u32> {
    crate::libspu::pair(m, at, at + 4)
}

/// libetc from the running `intr.o` adapter, `off` into the text.
fn lib(m: &mut M, off: u32) -> std::result::Result<Lib, String> {
    let text = m.sdk.at.wrapping_sub(off);
    if let Some(l) = m.sdk.etc.lib.filter(|l| l.intr == text) {
        return Ok(l);
    }
    let err = |what: &str| format!("libetc/intr at {text:#010x}: no {what} where PsyQ 4.7's object has it");
    let driver = m.mem.r32(pair(m, text + at::RESET).ok_or_else(|| err("driver table"))?);
    let vb = crate::libspu::jal(m, text + at::JAL_START_VSYNC).ok_or_else(|| err("startIntrVSync"))?;
    let dma = crate::libspu::jal(m, text + at::JAL_START_DMA).ok_or_else(|| err("startIntrDMA"))?;
    let find = |m: &M, what: &str, a: u32| pair(m, a).ok_or_else(|| err(what));
    let l = Lib {
        intr: text,
        driver,
        context: m.mem.r32(driver + driver::CONTEXT),
        spurious: find(m, "the spurious count", text + at::SPURIOUS_PAIR)?,
        vb_table: find(m, "VSyncCallbacks' table", vb + 4)?,
        vcount: find(m, "Vcount", vb + 0x58)?,
        vb_handler: find(m, "the vblank handler", vb + 0x30)?,
        vb_setter: find(m, "VSyncCallbacks' setter", vb + 0x40)?,
        dma_table: find(m, "DMACallback's table", dma + 4)?,
        dma_handler: find(m, "the DMA handler", dma + 0x24)?,
        dma_setter: find(m, "DMACallback's setter", dma + 0x34)?,
    };
    m.sdk.etc.lib = Some(l);
    Ok(l)
}

/// `vsync.o` from the running `VSync` adapter.
fn vsync_o(m: &mut M) -> std::result::Result<VsyncO, String> {
    let text = m.sdk.at;
    if let Some(v) = m.sdk.etc.vsync.filter(|v| v.text == text) {
        return Ok(v);
    }
    let err = |what: &str| format!("libetc/vsync at {text:#010x}: no {what} where PsyQ 4.7's object has it");
    let v = VsyncO {
        text,
        vcount: pair(m, text + 0x60).ok_or_else(|| err("Vcount"))?,
        hcount: pair(m, text + 0x48).ok_or_else(|| err("its Hcount"))?,
        last: pair(m, text + 0x84).ok_or_else(|| err("its last Vcount"))?,
    };
    m.sdk.etc.vsync = Some(v);
    Ok(v)
}

impl Lib {
    pub fn active(&self, m: &M) -> bool {
        m.mem.r16(self.context + ctx::ACTIVE) != 0
    }

    fn dispatching(&self, m: &M) -> bool {
        m.mem.r16(self.context + ctx::DISPATCHING) != 0
    }

    pub fn line(&self, m: &M, i: u32) -> u32 {
        m.mem.r32(self.context + ctx::LINES + 4 * i)
    }

    pub fn mask(&self, m: &M) -> u16 {
        m.mem.r16(self.context + ctx::MASK)
    }

    pub fn vcount(&self, m: &M) -> u32 {
        m.mem.r32(self.vcount)
    }
}

/// `intr.o`'s stack, its context's `stack[1024]`, from `ResetCallback` at
/// `reset` (`r32` reads a guest word): `.data` is what its first
/// `lui`/`lw` pair (`.text` +0) loads, less 0x10E0; the driver table at
/// `.data` + 0x10C0 holds the context's address in its eighth word, and
/// the stack follows the context's 0x68 bytes of fields. The original's
/// dispatcher starts at `stack[1024 - 20]`.
pub fn intr_stack(r32: &mut dyn FnMut(u32) -> u32, reset: u32) -> Option<std::ops::Range<u32>> {
    let (hi, lo) = (r32(reset), r32(reset.wrapping_add(4)));
    if hi >> 26 != 0x0F || (lo >> 21) & 31 != (hi >> 16) & 31 {
        return None;
    }
    let data = (hi << 16).wrapping_add(lo as u16 as i16 as u32).wrapping_sub(0x10E0);
    let context = r32(data.wrapping_add(0x10C0 + 0x1C));
    (context != 0).then(|| context + 0x68..context + 0x68 + 4 * 1024)
}

// --- The dispatcher -------------------------------------------------------

/// [`IrqStep::Sdk`] steps: kind in bits 24-31, the next line in bits
/// 16-23, the pending lines from it in bits 0-15.
mod step {
    /// The dispatcher's loop: pending = libetc's mask & I_STAT & I_MASK.
    pub const DISPATCH: u32 = 1;
    /// The lines still pending, from one.
    pub const LINE: u32 = 2;
    /// The same, back from that line's handler.
    pub const AFTER: u32 = 3;

    pub fn make(kind: u32, line: u32, rest: u32) -> u32 {
        kind << 24 | (line & 0xFF) << 16 | (rest & 0xFFFF)
    }
}

/// The step the exception handler runs after the chains: the dispatcher,
/// while libetc's context is installed.
pub fn dispatch_step(m: &M) -> Option<u32> {
    m.sdk.etc.lib.filter(|l| l.active(m)).map(|_| step::make(step::DISPATCH, 0, 0))
}

pub fn irq_step(m: &mut M, s: u32) {
    let r = match s >> 24 {
        step::DISPATCH => dispatch(m),
        step::LINE => line(m, (s >> 16) & 0xFF, s & 0xFFFF),
        step::AFTER => after_line(m, (s >> 16) & 0xFF, s & 0xFFFF),
        _ => Err(format!("unknown step {s:#x}")),
    };
    if let Err(e) = r {
        m.log(&format!("libetc dispatcher: {e}"));
    }
}

/// Queue `steps` at the front of the exception's, in order.
fn front(m: &mut M, steps: &[IrqStep]) {
    for s in steps.iter().rev() {
        m.irq_calls.push_front(*s);
    }
}

/// One of the dispatcher's messages through the game's `printf`, queued
/// to run next: its format's `lui`/`addiu` and the `jal`, at these offsets
/// in `intr.o`.
fn message(m: &mut M, l: Lib, (hi, lo, jal): (u32, u32, u32), args: [u32; 2]) {
    let t = l.intr;
    match (crate::libspu::jal(m, t + jal), crate::libspu::pair(m, t + hi, t + lo)) {
        (Some(f), Some(fmt)) => front(m, &[IrqStep::Chain(GuestCall { addr: f, args: [fmt, args[0], args[1], 0] }, None)]),
        _ => m.log(&format!("libetc/intr at {t:#010x}: no printf where PsyQ 4.7's object has it")),
    }
}

/// A call of `f` with the a0 the original leaves; the other argument
/// registers hold whatever it last put there, which nothing reads.
fn guest(f: u32, a0: u32) -> GuestCall {
    GuestCall { addr: f, args: [a0, 0, 0, 0] }
}

fn pending(m: &mut M, l: Lib) -> std::result::Result<u32, String> {
    let stat = reg(m, I_STAT, 2)?;
    m.sdk.etc.stat = stat as u16;
    Ok(l.mask(m) as u32 & stat & reg(m, I_MASK, 2)?)
}

/// `intr.o`'s dispatcher (+26Ch), at the top of its loop: entered from
/// `startIntr`'s `setjmp` (+1FCh), where the exception's exit resumes, or
/// looking again after the lines (+35Ch). It spends the original's blocks
/// on its path (D71), in the exception: a quantum passes only at its loop
/// headers (+300h, +30Ch).
fn dispatch(m: &mut M) -> std::result::Result<(), String> {
    let Some(l) = m.sdk.etc.lib else { return Ok(()) };
    let first = !l.dispatching(m);
    if first {
        crate::irq_path(m, &[(0x1FC, 0x208), (0x26C, 0x29C)]);
    }
    if !l.active(m) {
        // "unexpected interrupt(%04x)\n" with I_STAT; the original returns
        // from the exception.
        crate::irq_path(m, &[(0x2A0, 0x2B4)]);
        let stat = reg(m, I_STAT, 2)?;
        message(m, l, (0x2B0, 0x2B8, 0x2B4), [stat, 0]);
        crate::irq_path(m, &[(0x2BC, 0x2C0)]);
        crate::take(m, crate::VECTOR_STUB);
        return Ok(());
    }
    // The exception's first pass: onto libetc's stack, as the original's
    // exit from the exception (`HookEntryInt`) enters it. The interrupted
    // code's may be the scratchpad, where no DMA reaches (libcd reads
    // sector headers into its callback's frame).
    if first {
        crate::irq_path(m, &[(0x2C4, 0x2F4)]);
        m.sdk.etc.saved_sp = m.regs.sp();
        m.regs.set_sp(l.context + ctx::STACK_TOP);
    } else {
        crate::irq_path(m, &[(0x35C, 0x388)]);
    }
    m.mem.w16(l.context + ctx::DISPATCHING, 1);
    let p = pending(m, l)?;
    if p != 0 {
        if first {
            crate::irq_path(m, &[(0x2F8, 0x2FC)]);
        }
        crate::irq_header(m);
        crate::irq_path(m, &[(0x300, 0x308)]);
        front(m, &[IrqStep::Sdk(step::make(step::LINE, 0, p))]);
        return Ok(());
    }
    // Nothing left for libetc. An unmasked interrupt nobody handles makes
    // the exception come back; after 2048 in a row it gives up on them.
    crate::irq_path(m, &[(0x38C, 0x3B0)]);
    if reg(m, I_STAT, 2)? & reg(m, I_MASK, 2)? != 0 {
        crate::irq_path(m, &[(0x3B4, 0x3D4)]);
        let n = m.mem.r32(l.spurious);
        m.mem.w32(l.spurious, n + 1);
        if n > 2048 {
            crate::irq_path(m, &[(0x3D8, 0x3EC)]);
            let (stat, mask) = (reg(m, I_STAT, 2)?, reg(m, I_MASK, 2)?);
            // "intr timeout(%04x:%04x)\n".
            message(m, l, (0x3D8, 0x3DC, 0x3E8), [stat, mask]);
            crate::irq_path(m, &[(0x3F0, 0x404)]);
            m.mem.w32(l.spurious, 0);
            set_reg(m, I_STAT, 2, 0)?;
        }
    } else {
        crate::irq_path(m, &[(0x408, 0x40C)]);
        m.mem.w32(l.spurious, 0);
    }
    // ReturnFromException, through libapi's stub.
    crate::irq_path(m, &[(0x410, 0x418)]);
    crate::take(m, crate::VECTOR_STUB);
    m.regs.set_sp(m.sdk.etc.saved_sp);
    m.mem.w16(l.context + ctx::DISPATCHING, 0);
    Ok(())
}

/// The dispatcher's pass over the pending lines `rest` from line `i`, at
/// its loop's header (+30Ch): acknowledge the next one and call its
/// handler, then go on.
fn line(m: &mut M, mut i: u32, mut rest: u32) -> std::result::Result<(), String> {
    let Some(l) = m.sdk.etc.lib else { return Ok(()) };
    loop {
        crate::irq_header(m);
        crate::irq_path(m, &[(0x30C, 0x314)]);
        if i as usize >= LINES {
            break;
        }
        crate::irq_path(m, &[(0x318, 0x31C)]);
        if rest & 1 != 0 {
            crate::irq_path(m, &[(0x320, 0x33C)]);
            set_reg(m, I_STAT, 2, !(1u32 << i) & 0xFFFF)?;
            let h = l.line(m, i);
            if h != 0 {
                crate::irq_path(m, &[(0x340, 0x344)]);
                let next = IrqStep::Sdk(step::make(step::AFTER, i, rest));
                front(m, &[IrqStep::Chain(guest(h, m.sdk.etc.stat as u32), None), next]);
                return Ok(());
            }
        }
        crate::irq_path(m, &[(0x348, 0x358)]);
        i += 1;
        rest >>= 1;
        if rest == 0 {
            break;
        }
    }
    front(m, &[IrqStep::Sdk(step::make(step::DISPATCH, 0, 0))]);
    Ok(())
}

/// Back from line `i`'s handler (+348h): the next line, or look again.
fn after_line(m: &mut M, i: u32, rest: u32) -> std::result::Result<(), String> {
    crate::irq_path(m, &[(0x348, 0x358)]);
    if rest >> 1 != 0 {
        return line(m, i + 1, rest >> 1);
    }
    front(m, &[IrqStep::Sdk(step::make(step::DISPATCH, 0, 0))]);
    Ok(())
}

// --- intr.o ----------------------------------------------------------------

/// `intr.o`'s line setter (`InterruptCallback`'s body): with the context
/// installed, the handler, libetc's mask and I_MASK (masked meanwhile),
/// and the kernel's acknowledge flags for the lines its own handlers
/// share (vblank: the pad and RCntCNT3; root counters 0-2). The previous
/// handler. `intr.o` +43Ch's blocks.
fn set_line(m: &mut M, l: Lib, irq: usize, h: u32) -> std::result::Result<u32, String> {
    crate::path(m, &[(0x43C, 0x47C)]);
    let at = l.context + ctx::LINES + 4 * irq as u32;
    let prev = m.mem.r32(at);
    if h == prev {
        crate::path(m, &[(0x564, 0x580)]);
        return Ok(prev);
    }
    crate::path(m, &[(0x480, 0x48C)]);
    if !l.active(m) {
        crate::path(m, &[(0x560, 0x580)]);
        return Ok(prev);
    }
    crate::path(m, &[(0x490, 0x4A8)]);
    let mut mask = reg(m, I_MASK, 2)?;
    set_reg(m, I_MASK, 2, 0)?;
    let bit = 1u16 << irq;
    let off = h == 0;
    m.mem.w32(at, h);
    let lines = l.mask(m);
    // +4ACh or +4CCh, the same length.
    crate::path(m, &[(0x4AC, 0x4C8), (0x4EC, 0x4F0)]);
    if off {
        mask &= !(bit as u32);
        m.mem.w16(l.context + ctx::MASK, lines & !bit);
    } else {
        mask |= bit as u32;
        m.mem.w16(l.context + ctx::MASK, lines | bit);
    }
    let off = off as u32;
    // ChangeClearPAD and ChangeClearRCnt, through libapi's stubs.
    if irq == 0 {
        crate::path(m, &[(0x4F4, 0x4FC)]);
        crate::take(m, crate::VECTOR_STUB);
        bios(m, 0xB0, 0x5B, [off, 0, 0, 0])?;
        crate::path(m, &[(0x500, 0x508)]);
        crate::take(m, crate::VECTOR_STUB);
        bios(m, 0xC0, 0x0A, [3, off, 0, 0])?;
        crate::path(m, &[(0x50C, 0x50C)]);
    }
    crate::path(m, &[(0x510, 0x514)]);
    for (n, (a, b)) in [(4, (0x518, 0x524)), (5, (0x530, 0x53C)), (6, (0x548, 0x54C))] {
        if irq == n {
            crate::take(m, crate::VECTOR_STUB);
            bios(m, 0xC0, 0x0A, [n as u32 - 4, off, 0, 0])?;
            crate::path(m, &[(a, b)]);
        }
        if n < 6 {
            crate::path(m, &[[(0x528, 0x52C), (0x540, 0x544)][n - 4]]);
        }
    }
    crate::path(m, &[(0x550, 0x580)]);
    set_reg(m, I_MASK, 2, mask)?;
    Ok(prev)
}

/// `ResetCallback`: once (until `StopCallback`), mask and acknowledge every
/// interrupt, set every DMA channel's priority with none enabled, clear
/// the context, install the dispatcher, then `startIntrVSync` (root
/// counter 1 counts hblanks, Vcount and the channels from zero, the
/// vblank handler on line 0, its setter in the driver table) and
/// `startIntrDMA` (no DMA callbacks, DICR's enables cleared, the DMA
/// handler on line 3, its setter in the table), and leave the critical
/// section. The context's address. Its blocks are `intr.o`'s
/// `ResetCallback` and `startIntr` (+194h), whose `setjmp` the lift lowers
/// to stores (no stub's), `intr_vb.o`'s and `intr_dma.o`'s.
fn reset_callback(m: &mut M, _a: [u32; 4]) -> Result {
    let l = lib(m, at::RESET)?;
    crate::path(m, &[(0x0, 0x1C), (0x194, 0x1B4)]);
    if l.active(m) {
        crate::path(m, &[(0x25C, 0x268), (0x20, 0x2C)]);
        return ret(0);
    }
    crate::path(m, &[(0x1B8, 0x1F0)]);
    set_reg(m, I_MASK, 2, 0)?;
    let v = reg(m, I_MASK, 2)?;
    set_reg(m, I_STAT, 2, v)?;
    set_reg(m, DPCR, 4, 0x3333_3333)?;
    // Clearing the context, a word a pass (+69Ch).
    crate::path(m, &[(0x69C, 0x6A4)]);
    Ok(crate::spin(m, 4, ctx::WORDS as u64, move |m| match start_intr(m, l) {
        Ok(v) => Flow::ret(v),
        Err(e) => {
            m.log(&format!("ResetCallback: {e}"));
            Flow::ret(0)
        }
    }))
}

/// `startIntr` past its context's clearing (+6B8h on).
fn start_intr(m: &mut M, l: Lib) -> std::result::Result<u32, String> {
    crate::path(m, &[(0x6B8, 0x6BC), (0x1F4, 0x200), (0x20C, 0x220)]);
    crate::take(m, crate::VECTOR_STUB);
    m.mem.write(l.context, &vec![0; 4 * ctx::WORDS as usize]);
    m.mem.w16(l.context + ctx::ACTIVE, 1);
    // startIntrVSync, its table cleared a word a pass.
    crate::path(m, &[(0x224, 0x22C), (0x0, 0x2C), (0xF0, 0xF8), (0x10C, 0x110)]);
    crate::take(m, 8 * 4);
    set_reg(m, RCNT1_MODE, 4, 0x100)?;
    m.mem.w32(l.vcount, 0);
    m.mem.write(l.vb_table, &[0; 32]);
    crate::path(m, &[(0x30, 0x3C), (0x30, 0x4C)]);
    set_line(m, l, 0, l.vb_handler)?;
    crate::path(m, &[(0x50, 0x5C), (0x40, 0x54), (0x230, 0x23C)]);
    m.mem.w32(l.driver + driver::VBLANK, l.vb_setter);
    // startIntrDMA, likewise.
    crate::path(m, &[(0x0, 0x14), (0x278, 0x280), (0x294, 0x298)]);
    crate::take(m, 8 * 4);
    m.mem.write(l.dma_table, &[0; 32]);
    set_reg(m, DICR, 4, 0)?;
    crate::path(m, &[(0x18, 0x30), (0x30, 0x4C)]);
    set_line(m, l, 3, l.dma_handler)?;
    crate::path(m, &[(0x50, 0x5C), (0x34, 0x48), (0x240, 0x24C)]);
    m.mem.w32(l.driver + driver::DMA, l.dma_setter);
    // _96_remove (the BIOS's CD-ROM driver out, which leaves a critical
    // section entered), then ExitCriticalSection.
    crate::take(m, crate::VECTOR_STUB);
    bios(m, 0xA0, 0x72, [0; 4])?;
    crate::path(m, &[(0x250, 0x254)]);
    crate::take(m, crate::SYSCALL_STUB);
    psx_bios::bios::syscall(m, [2, 0, 0, 0])?;
    crate::path(m, &[(0x258, 0x268), (0x20, 0x2C)]);
    Ok(l.context)
}

/// `StopCallback`: with the context installed, enter a critical section,
/// keep I_MASK and DPCR, mask and acknowledge every interrupt, disable
/// every DMA channel, and take the dispatcher out (`stopIntr`, +584h).
fn stop_callback(m: &mut M, _a: [u32; 4]) -> Result {
    let l = lib(m, at::STOP)?;
    crate::path(m, &[(0xF4, 0x110), (0x584, 0x5A4)]);
    if !l.active(m) {
        crate::path(m, &[(0x614, 0x620), (0x114, 0x120)]);
        return ret(0);
    }
    crate::path(m, &[(0x5A8, 0x5AC)]);
    crate::take(m, crate::SYSCALL_STUB);
    psx_bios::bios::syscall(m, [1, 0, 0, 0])?;
    crate::path(m, &[(0x5B0, 0x608)]);
    let mask = reg(m, I_MASK, 2)?;
    m.mem.w16(l.context + ctx::SAVED_MASK, mask as u16);
    let dpcr = reg(m, DPCR, 4)?;
    m.mem.w32(l.context + ctx::SAVED_DPCR, dpcr);
    set_reg(m, I_MASK, 2, 0)?;
    let v = reg(m, I_MASK, 2)?;
    set_reg(m, I_STAT, 2, v)?;
    let d = reg(m, DPCR, 4)?;
    set_reg(m, DPCR, 4, d & 0x7777_7777)?;
    // ResetEntryInt.
    crate::take(m, crate::VECTOR_STUB);
    crate::path(m, &[(0x60C, 0x620), (0x114, 0x120)]);
    m.mem.w16(l.context + ctx::ACTIVE, 0);
    ret(l.context)
}

/// `RestartCallback`: the dispatcher back, I_MASK and DPCR as
/// `StopCallback` kept them, and the critical section left
/// (`restartIntr`, +624h).
fn restart_callback(m: &mut M, _a: [u32; 4]) -> Result {
    let l = lib(m, at::RESTART)?;
    crate::path(m, &[(0x124, 0x140), (0x624, 0x644)]);
    if l.active(m) {
        crate::path(m, &[(0x688, 0x698), (0x144, 0x150)]);
        return ret(0);
    }
    // HookEntryInt.
    crate::path(m, &[(0x648, 0x64C)]);
    crate::take(m, crate::VECTOR_STUB);
    crate::path(m, &[(0x650, 0x67C)]);
    m.mem.w16(l.context + ctx::ACTIVE, 1);
    let (mask, dpcr) = (m.mem.r16(l.context + ctx::SAVED_MASK), m.mem.r32(l.context + ctx::SAVED_DPCR));
    set_reg(m, I_MASK, 2, mask as u32)?;
    set_reg(m, DPCR, 4, dpcr)?;
    crate::take(m, crate::SYSCALL_STUB);
    psx_bios::bios::syscall(m, [2, 0, 0, 0])?;
    crate::path(m, &[(0x680, 0x684), (0x68C, 0x698), (0x144, 0x150)]);
    ret(l.context)
}

/// `CheckCallback`: whether the dispatcher is running (a callback is).
fn check_callback(m: &mut M, _a: [u32; 4]) -> Result {
    let l = lib(m, at::CHECK)?;
    crate::path(m, &[(0x154, 0x160)]);
    ret(l.dispatching(m) as u32)
}

/// `InterruptCallback(line, f)`: the previous handler.
fn interrupt_callback(m: &mut M, a: [u32; 4]) -> Result {
    let l = lib(m, at::INTERRUPT)?;
    let irq = a[0] as usize;
    if irq >= LINES {
        return Err(format!("InterruptCallback({irq}, …): no such interrupt line"));
    }
    crate::path(m, &[(0x30, 0x4C)]);
    let prev = set_line(m, l, irq, a[1])?;
    crate::path(m, &[(0x50, 0x5C)]);
    ret(prev)
}

/// `DMACallback(ch, f)` (`intr_dma.o`'s setter, +1CCh): when `f` changes
/// the channel's callback, DICR's master enable and the channel's enable
/// follow it. The previous callback.
fn dma_callback(m: &mut M, a: [u32; 4]) -> Result {
    let l = lib(m, at::DMA)?;
    let (ch, f) = (a[0], a[1]);
    if ch >= 8 {
        return Err(format!("DMACallback({ch}, …): no such DMA channel"));
    }
    crate::path(m, &[(0x60, 0x7C), (0x1CC, 0x1EC)]);
    let slot = l.dma_table + 4 * ch;
    let old = m.mem.r32(slot);
    if f != old {
        // +1F8h or +234h, the same length.
        crate::path(m, &[(0x1F0, 0x1F4), (0x1F8, 0x230)]);
        m.mem.w32(slot, f);
        let d = (reg(m, DICR, 4)? & 0x00FF_FFFF) | 0x0080_0000;
        let bit = 1 << (16 + ch);
        set_reg(m, DICR, 4, if f != 0 { d | bit } else { d & !bit })?;
    }
    crate::path(m, &[(0x270, 0x274), (0x80, 0x8C)]);
    ret(old)
}

/// `VSyncCallbacks(ch, f)` (`intr_vb.o`'s setter): the previous callback.
fn vsync_callbacks(m: &mut M, a: [u32; 4]) -> Result {
    let l = lib(m, at::VSYNC_CALLBACKS)?;
    crate::path(m, &[(0xC4, 0xE0)]);
    let old = set_vsync(m, l, a[0], a[1])?;
    crate::path(m, &[(0xE4, 0xF0)]);
    ret(old)
}

/// `intr_vb.o`'s setter (+C4h).
fn set_vsync(m: &mut M, l: Lib, ch: u32, f: u32) -> std::result::Result<u32, String> {
    if ch >= 8 {
        return Err(format!("VSyncCallbacks({ch}, …): no such channel"));
    }
    crate::path(m, &[(0xC4, 0xE0)]);
    let slot = l.vb_table + 4 * ch;
    let old = m.mem.r32(slot);
    if f != old {
        crate::path(m, &[(0xE4, 0xE4)]);
    }
    crate::path(m, &[(0xE8, 0xEC)]);
    m.mem.w32(slot, f);
    Ok(old)
}

/// `intr_dma.o`'s DMA handler, bound by its place (+4Ch): while DICR
/// flags a channel, acknowledge each flagged one in order and call its
/// `DMACallback` (a0: DICR's address), and look again; with none flagged
/// and a bus error, its messages through the game's `printf` (DICR, then
/// each channel's MADR).
fn dma_handler(m: &mut M, _a: [u32; 4]) -> Result {
    let d = m.sdk.at.wrapping_sub(0x4C);
    let table = pair(m, d + 4).ok_or_else(|| format!("libetc/intr_dma at {d:#010x}: no DMACallback table where PsyQ 4.7's object has it"))?;
    crate::irq_path(m, &[(0x4C, 0x88)]);
    let rest = (reg(m, DICR, 4)? >> 24) & 0x7F;
    if rest == 0 {
        return Ok(dma_end(m, d));
    }
    crate::irq_path(m, &[(0x8C, 0x9C)]);
    Ok(dma_pass(m, d, table, rest))
}

/// The handler's outer loop (+A0h, a header): the flagged channels in
/// `rest`, from channel 0. Its blocks are spent in the exception (D71).
fn dma_pass(m: &mut M, d: u32, table: u32, rest: u32) -> Flow<Psyq> {
    crate::irq_header(m);
    crate::irq_path(m, &[(0xA0, 0xA8)]);
    dma_channel(m, d, table, 0, rest)
}

fn dma_failed(m: &mut M, e: String) -> Flow<Psyq> {
    m.log(&format!("libetc DMA handler: {e}"));
    Flow::ret(0)
}

/// The inner loop (+ACh, a header): channel `ch`, flagged in `rest`'s
/// bit 0. A flagged channel's flag is acknowledged (the enables kept) and
/// its callback called.
fn dma_channel(m: &mut M, d: u32, table: u32, ch: u32, rest: u32) -> Flow<Psyq> {
    crate::irq_header(m);
    crate::irq_path(m, &[(0xAC, 0xB4)]);
    if ch >= 7 {
        return dma_again(m, d, table);
    }
    crate::irq_path(m, &[(0xB8, 0xBC)]);
    if rest & 1 != 0 {
        crate::irq_path(m, &[(0xC0, 0xE8)]);
        if let Err(e) = reg(m, DICR, 4).and_then(|v| set_reg(m, DICR, 4, v & (1 << (24 + ch) | 0x00FF_FFFF))) {
            return dma_failed(m, e);
        }
        let f = m.mem.r32(table + 4 * ch);
        if f != 0 {
            crate::irq_path(m, &[(0xEC, 0xF0)]);
            return Flow::Call { call: guest(f, DICR), then: Box::new(move |m, _| dma_next(m, d, table, ch, rest)) };
        }
    }
    dma_next(m, d, table, ch, rest)
}

/// +F4h: the next channel, while any is flagged.
fn dma_next(m: &mut M, d: u32, table: u32, ch: u32, rest: u32) -> Flow<Psyq> {
    crate::irq_path(m, &[(0xF4, 0x100)]);
    if rest >> 1 != 0 {
        return dma_channel(m, d, table, ch + 1, rest >> 1);
    }
    dma_again(m, d, table)
}

/// +104h: DICR's flags again; the outer loop while any.
fn dma_again(m: &mut M, d: u32, table: u32) -> Flow<Psyq> {
    crate::irq_path(m, &[(0x104, 0x124)]);
    match reg(m, DICR, 4) {
        Ok(v) if (v >> 24) & 0x7F != 0 => dma_pass(m, d, table, (v >> 24) & 0x7F),
        Ok(_) => dma_end(m, d),
        Err(e) => dma_failed(m, e),
    }
}

/// +128h: none flagged; with a bus error, its messages through the game's
/// `printf` (DICR, then each channel's MADR).
fn dma_end(m: &mut M, d: u32) -> Flow<Psyq> {
    crate::irq_path(m, &[(0x128, 0x148)]);
    let dicr = reg(m, DICR, 4).unwrap_or(0);
    let error = dicr & 0xFF00_0000 == 0x8000_0000 || {
        crate::irq_path(m, &[(0x14C, 0x15C)]);
        dicr & 0x8000 != 0
    };
    if !error {
        crate::irq_path(m, &[(0x1A8, 0x1C8)]);
        return Flow::ret(0);
    }
    crate::irq_path(m, &[(0x160, 0x170)]);
    crate::say(m, (d + 0x160, d + 0x164, d + 0x16C), &[dicr], move |m| bus_error(m, d, 0))
}

/// The bus error's dump (+174h, a header): channel `ch`'s MADR, and the
/// rest after it.
fn bus_error(m: &mut M, d: u32, ch: u32) -> Flow<Psyq> {
    if ch >= 7 {
        crate::irq_path(m, &[(0x1A8, 0x1C8)]);
        return Flow::ret(0);
    }
    crate::irq_header(m);
    crate::irq_path(m, &[(0x174, 0x194)]);
    let madr = reg(m, 0x1F80_1080 + 16 * ch, 4).unwrap_or(0);
    crate::say(m, (d + 0x174, d + 0x178, d + 0x194), &[ch, madr], move |m| {
        crate::irq_path(m, &[(0x19C, 0x1A4)]);
        bus_error(m, d, ch + 1)
    })
}

/// `VSyncCallback(f)`: channel 4.
fn vsync_callback(m: &mut M, a: [u32; 4]) -> Result {
    let l = lib(m, at::VSYNC_CALLBACK)?;
    crate::path(m, &[(0x90, 0xB0)]);
    let old = set_vsync(m, l, 4, a[0])?;
    crate::path(m, &[(0xB4, 0xC0)]);
    ret(old)
}

fn get_intr_mask(m: &mut M, _a: [u32; 4]) -> Result {
    crate::path(m, &[(0x164, 0x178)]);
    ret(reg(m, I_MASK, 2)?)
}

/// `SetIntrMask(mask)`: I_MASK, the previous one.
fn set_intr_mask(m: &mut M, a: [u32; 4]) -> Result {
    crate::path(m, &[(0x17C, 0x190)]);
    let old = reg(m, I_MASK, 2)?;
    set_reg(m, I_MASK, 2, a[0] & 0xFFFF)?;
    ret(old)
}

// --- vsync.o ---------------------------------------------------------------

/// Root counter 1 now: hblanks, once `ResetCallback` set its clock.
fn rcnt1(m: &M) -> u32 {
    m.timers[1].value(1, m.clock.video, m.clock.now) as u32
}

fn gpustat(m: &mut M) -> u32 {
    reg(m, GPUSTAT, 4).unwrap_or(0)
}

/// `VSync(mode)`. Its result is the hblanks since the last waiting call
/// returned (root counter 1, 16 bits). Mode < 0: Vcount, no wait. Mode 1:
/// no wait. Mode 0: until the next vblank. Mode n > 1: until n vblanks
/// after the last waiting call, and at least the next. Interlaced, the
/// field must have changed too. Root counter 1 is read until two reads
/// agree, which the device's always do (it moves only at safepoints).
fn vsync(m: &mut M, a: [u32; 4]) -> Result {
    let mode = a[0] as i32;
    let v = vsync_o(m)?;
    crate::path(m, &[(0x0, 0x5C)]);
    if mode < 0 {
        crate::path(m, &[(0x60, 0x6C), (0x164, 0x174)]);
        return ret(m.mem.r32(v.vcount));
    }
    let delta = rcnt1(m).wrapping_sub(m.mem.r32(v.hcount)) & 0xFFFF;
    crate::path(m, &[(0x70, 0x78)]);
    if mode == 1 {
        crate::path(m, &[(0x160, 0x174)]);
        return ret(delta);
    }
    crate::path(m, &[(0x7C, 0x80)]);
    let last = m.mem.r32(v.last);
    let (target, timeout) = if mode > 0 {
        crate::path(m, &[(0x84, 0x98), (0xA4, 0xAC)]);
        (last.wrapping_add(mode as u32 - 1), mode as u32 - 1)
    } else {
        crate::path(m, &[(0x9C, 0xA8)]);
        (last, 0)
    };
    crate::path(m, &[(0xB0, 0xB4)]);
    Ok(wait_vcount(m, v, target, timeout, move |m| {
        crate::path(m, &[(0xB8, 0xD8)]);
        let field = gpustat(m);
        let next = m.mem.r32(v.vcount).wrapping_add(1);
        wait_vcount(m, v, next, 1, move |m| interlace(m, v, field, delta))
    }))
}

/// The safepoint quantum: virtual time the guest's budget takes
/// (`psx_bios::BUDGET`).
pub(crate) fn quantum(m: &M) -> u64 {
    if m.opts.safepoint_quantum == 0 { 20_000 } else { m.opts.safepoint_quantum as u64 }
}

/// `vsync.o`'s wait (+178h): until Vcount reaches `target`, counting down
/// from `timeout << 15` meanwhile; when that runs out it says so through
/// the game's `puts`, has the kernel acknowledge vblank itself, and goes
/// on. Each look is a pass of 14 instructions ([`crate::poll`]) after the
/// first. In NTSC it only runs out when nothing counts vblanks: inside a
/// handler or a critical section, libetc stopped, or line 0 taken by a
/// handler that doesn't call libetc's.
fn wait_vcount(m: &mut M, v: VsyncO, target: u32, timeout: u32, then: impl FnOnce(&mut M) -> Flow<Psyq> + 'static) -> Flow<Psyq> {
    let text = v.text;
    let reached = move |m: &mut M| m.mem.r32(v.vcount) as i32 >= target as i32;
    crate::path(m, &[(0x178, 0x198)]);
    if reached(m) {
        crate::path(m, &[(0x200, 0x20C)]);
        return then(m);
    }
    crate::path(m, &[(0x19C, 0x19C)]);
    crate::poll(
        m,
        VCOUNT_PASS,
        (timeout as u64) << 15,
        std::rc::Rc::new(reached),
        Box::new(move |m, reached| {
            if reached {
                crate::path(m, &[(0x200, 0x20C)]);
                return then(m);
            }
            crate::path(m, &[(0x1A0, 0x1C8)]);
            crate::say(m, (text + 0x1C0, text + 0x1C8, text + 0x1C4), &[], move |m| {
                crate::path(m, &[(0x1CC, 0x1D0)]);
                crate::take(m, crate::VECTOR_STUB);
                let _ = bios(m, 0xB0, 0x5B, [0; 4]);
                crate::path(m, &[(0x1D4, 0x1DC)]);
                crate::take(m, crate::VECTOR_STUB);
                let _ = bios(m, 0xC0, 0x0A, [3, 0, 0, 0]);
                crate::path(m, &[(0x1E0, 0x1E4), (0x200, 0x20C)]);
                then(m)
            })
        }),
    )
}

/// The end of a waiting `VSync` (+DCh): interlaced, while the field is the
/// one it was before the last wait, spin until it changes; then Vcount and
/// root counter 1 are this call's.
fn interlace(m: &mut M, v: VsyncO, before: u32, delta: u32) -> Flow<Psyq> {
    let then = move |m: &mut M, _| {
        crate::path(m, &[(0x124, 0x15C)]);
        let vcount = m.mem.r32(v.vcount);
        m.mem.w32(v.last, vcount);
        let h = rcnt1(m);
        m.mem.w32(v.hcount, h);
        crate::path(m, &[(0x160, 0x174)]);
        Flow::ret(delta)
    };
    crate::path(m, &[(0xDC, 0xE8)]);
    if before & 0x40_0000 == 0 {
        return then(m, true);
    }
    let field_changed = move |m: &mut M| (before ^ gpustat(m)) & 0x8000_0000 != 0;
    crate::path(m, &[(0xEC, 0x108)]);
    if field_changed(m) {
        return then(m, true);
    }
    crate::poll(m, FIELD_PASS, u64::MAX, std::rc::Rc::new(field_changed), Box::new(then))
}

/// btn's libetc as its code addresses it, for the tests: `intr.o`'s and
/// `vsync.o`'s operands, and doubles of `intr_vb.o`'s and `intr_dma.o`'s
/// handlers (guest code in a game).
#[cfg(test)]
pub(crate) mod fixture {
    use super::*;

    pub const INTR: u32 = 0x8003_2D94;
    pub const VSYNC: u32 = 0x8003_2B84;
    const VB: u32 = 0x8003_3454;
    const DMA_O: u32 = 0x8003_3574;
    const DRIVER_PTR: u32 = 0x8005_7594;
    pub const DRIVER: u32 = 0x8005_7574;
    pub const CONTEXT: u32 = 0x8005_650C;
    const SPURIOUS: u32 = 0x8005_75A4;
    pub const VB_TABLE: u32 = 0x8005_75B4;
    pub const VCOUNT: u32 = 0x8005_75D4;
    pub const VB_HANDLER: u32 = 0x8003_34AC;
    const VB_SETTER: u32 = 0x8003_3518;
    pub const DMA_TABLE: u32 = 0x8005_75E8;
    pub const DMA_HANDLER: u32 = 0x8003_35C0;
    const DMA_SETTER: u32 = 0x8003_3740;
    const HCOUNT: u32 = 0x8005_64A4;
    const LAST: u32 = 0x8005_64A8;

    /// A `lui`/`addiu` pair at `pc` that makes `addr`.
    pub(crate) fn pair_at(m: &mut M, pc: u32, addr: u32) {
        m.mem.w32(pc, 0x3C02_0000 | addr.wrapping_add(0x8000) >> 16);
        m.mem.w32(pc + 4, 0x2442_0000 | (addr & 0xFFFF));
    }

    pub(crate) fn jal_to(m: &mut M, pc: u32, target: u32) {
        m.mem.w32(pc, 0x0C00_0000 | (target >> 2) & 0x03FF_FFFF);
    }

    pub fn install(m: &mut M) {
        pair_at(m, INTR, DRIVER_PTR);
        m.mem.w32(DRIVER_PTR, DRIVER);
        m.mem.w32(DRIVER + driver::CONTEXT, CONTEXT);
        jal_to(m, INTR + at::JAL_START_VSYNC, VB);
        jal_to(m, INTR + at::JAL_START_DMA, DMA_O);
        pair_at(m, INTR + at::SPURIOUS_PAIR, SPURIOUS);
        for (off, a) in [(4, VB_TABLE), (0x58, VCOUNT), (0x30, VB_HANDLER), (0x40, VB_SETTER)] {
            pair_at(m, VB + off, a);
        }
        for (off, a) in [(4, DMA_TABLE), (0x24, DMA_HANDLER), (0x34, DMA_SETTER)] {
            pair_at(m, DMA_O + off, a);
        }
        for (off, a) in [(0x60, VCOUNT), (0x48, HCOUNT), (0x84, LAST)] {
            pair_at(m, VSYNC + off, a);
        }
        crate::testing::double(VB_HANDLER, vb_handler);
        crate::testing::double(DMA_HANDLER, dma_handler);
    }

    pub fn reset(m: &mut M) -> u32 {
        run(m, at::RESET, reset_callback, [0; 4])
    }

    pub fn interrupt(m: &mut M, line: u32, f: u32) -> u32 {
        run(m, at::INTERRUPT, interrupt_callback, [line, f, 0, 0])
    }

    pub fn dma_callback(m: &mut M, ch: u32, f: u32) -> u32 {
        run(m, at::DMA, super::dma_callback, [ch, f, 0, 0])
    }

    /// An adapter of `intr.o` at `off`, or `VSync`.
    pub fn run(m: &mut M, off: u32, f: Adapter, a: [u32; 4]) -> u32 {
        m.sdk.at = if f as *const () == vsync as *const () { VSYNC } else { INTR + off };
        crate::testing::call(m, f, a)
    }

    /// `intr_vb.o`'s handler: Vcount, then each channel's callback.
    fn vb_handler(m: &mut M) {
        let v = m.mem.r32(VCOUNT);
        m.mem.w32(VCOUNT, v + 1);
        for ch in 0..8 {
            let f = m.mem.r32(VB_TABLE + 4 * ch);
            if f != 0 {
                crate::testing::guest(m, f);
            }
        }
    }

    /// `intr_dma.o`'s handler: its adapter, as the game's thunk at its
    /// address reaches it.
    fn dma_handler(m: &mut M) {
        let caller = std::mem::replace(&mut m.sdk.at, DMA_HANDLER);
        let r = super::dma_handler(m, [0; 4]);
        crate::testing::finish(m, r);
        m.sdk.at = caller;
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::{CONTEXT, DMA_HANDLER, VB_HANDLER, VCOUNT, install, run};
    use super::*;
    use crate::testing::{irq_calls, machine_with_bus, vblanks};

    fn i_mask(m: &M) -> u32 {
        m.io.as_ref().unwrap().irq.mask
    }

    fn game() -> M {
        let mut m = machine_with_bus();
        install(&mut m);
        m
    }

    fn reset(m: &mut M) -> u32 {
        run(m, at::RESET, reset_callback, [0; 4])
    }

    #[test]
    fn reset_installs_the_dispatcher() {
        let mut m = game();
        m.crit = true;
        assert_eq!(reset(&mut m), CONTEXT);
        assert!(!m.crit, "ResetCallback leaves the critical section");
        assert_eq!(i_mask(&m), 0x9, "vblank and DMA");
        assert_eq!(m.io.as_ref().unwrap().dma.dpcr, 0x3333_3333);
        assert_eq!(m.timers[1].mode, 0x100, "root counter 1 counts hblanks");
        assert_eq!(m.mem.r16(CONTEXT), 1, "active, in the context");
        assert_eq!(m.mem.r32(fixture::DRIVER + driver::VBLANK), 0x8003_3518, "VSyncCallbacks' setter");
        assert_eq!(reset(&mut m), 0, "only once");
        assert_eq!(run(&mut m, 0x164, get_intr_mask, [0; 4]), 9);
        assert_eq!(run(&mut m, at::INTERRUPT, interrupt_callback, [0, 0, 0, 0]), VB_HANDLER);
        assert_eq!(run(&mut m, at::INTERRUPT, interrupt_callback, [0, VB_HANDLER, 0, 0]), 0);
        assert_eq!(run(&mut m, at::INTERRUPT, interrupt_callback, [3, 0x8001_0000, 0, 0]), DMA_HANDLER);
        assert_eq!(m.mem.r32(CONTEXT + ctx::LINES + 12), 0x8001_0000, "line 3's handler, in the context");
    }

    #[test]
    fn vsync_waits_and_counts_hblanks() {
        let mut m = game();
        reset(&mut m);
        assert_eq!(run(&mut m, 0, vsync, [u32::MAX, 0, 0, 0]), 0);
        run(&mut m, 0, vsync, [0; 4]);
        assert_eq!(m.vblanks, 1);
        assert_eq!(m.mem.r32(VCOUNT), 1);
        // Half a frame later, VSync(1) reports the hblanks so far.
        let half = m.clock.now + (m.clock.video.vblank_at(2) - m.clock.video.vblank_at(1)) / 2;
        m.run_until(half);
        let h = run(&mut m, 0, vsync, [1, 0, 0, 0]);
        assert!((130..=132).contains(&h), "{h}");
        // VSync(3): three vblanks after the last waiting call.
        run(&mut m, 0, vsync, [3, 0, 0, 0]);
        assert_eq!(m.mem.r32(VCOUNT), 4);
        // Long after, at least the next one.
        vblanks(&mut m, 5);
        run(&mut m, 0, vsync, [3, 0, 0, 0]);
        assert_eq!(run(&mut m, 0, vsync, [u32::MAX, 0, 0, 0]), 10);
        // With interrupts disabled nothing counts: VSync gives up.
        m.crit = true;
        let before = m.clock.now;
        run(&mut m, 0, vsync, [0; 4]);
        assert_eq!(m.mem.r32(VCOUNT), 10);
        assert!(m.clock.now > before);
    }

    #[test]
    fn callbacks_in_line_and_channel_order() {
        let mut m = game();
        reset(&mut m);
        assert_eq!(run(&mut m, at::VSYNC_CALLBACK, vsync_callback, [0x8001_0004, 0, 0, 0]), 0);
        run(&mut m, at::VSYNC_CALLBACKS, vsync_callbacks, [1, 0x8001_0001, 0, 0]);
        assert_eq!(run(&mut m, at::VSYNC_CALLBACKS, vsync_callbacks, [4, 0, 0, 0]), 0x8001_0004);
        run(&mut m, at::VSYNC_CALLBACKS, vsync_callbacks, [4, 0x8001_0004, 0, 0]);
        assert_eq!(m.mem.r32(fixture::VB_TABLE + 4), 0x8001_0001, "in intr_vb's table");
        vblanks(&mut m, 1);
        assert_eq!(irq_calls(), vec![0x8001_0001, 0x8001_0004]);
        // Root counter 2 at its target, through InterruptCallback(6, …);
        // the kernel's acknowledge flag follows.
        run(&mut m, at::INTERRUPT, interrupt_callback, [6, 0x8002_0000, 0, 0]);
        assert_eq!(m.kernel.clear_rcnt[2], 0);
        assert_eq!(i_mask(&m), 0x49);
        // SetRCnt(RCntCNT2, 0x100, RCntMdINTR | RCntMdSC).
        crate::testing::call(&mut m, crate::libapi::set_rcnt, [0xF200_0002, 0x100, 0x1001, 0]);
        let until = m.clock.now + 0x1000;
        m.run_until(until);
        crate::testing::drain(&mut m);
        assert_eq!(irq_calls(), vec![0x8002_0000]);
        assert_eq!(m.io.as_ref().unwrap().irq.stat & 0x40, 0, "acknowledged");
        // A masked line runs nothing.
        run(&mut m, 0x17C, set_intr_mask, [0x9, 0, 0, 0]);
        m.run_until(m.clock.now + 0x200);
        crate::testing::drain(&mut m);
        assert_eq!(irq_calls(), Vec::<u32>::new());
    }

    #[test]
    fn handlers_run_on_intr_stack() {
        use psx_bios::Sdk;
        let mut m = game();
        reset(&mut m);
        run(&mut m, at::VSYNC_CALLBACK, vsync_callback, [0x8001_0004, 0, 0, 0]);
        // Interrupted with its stack in the scratchpad (btn's menu).
        m.regs.set_sp(0x1F80_0378);
        m.raise_irq(psx_devices::irq::VBLANK);
        let mut sps = Vec::new();
        while let Some(s) = m.irq_calls.pop_front() {
            match s {
                IrqStep::Sdk(s) => Psyq::irq_step(&mut m, s),
                IrqStep::Kernel(k) => {
                    m.kernel_step(k);
                }
                IrqStep::Chain(c, _) => {
                    sps.push(m.regs.sp());
                    assert_eq!(c.args[0] & 1, 1, "a0: I_STAT as the dispatcher read it");
                    crate::testing::guest(&mut m, c.addr);
                }
                IrqStep::Exit => {}
            }
        }
        assert_eq!(irq_calls(), vec![0x8001_0004]);
        assert_eq!(sps, vec![0x8005_7524]);
        assert_eq!(m.regs.sp(), 0x1F80_0378, "the interrupted code's sp back");
        assert_eq!(m.mem.r16(CONTEXT + ctx::DISPATCHING), 0);
    }

    #[test]
    fn dma_callbacks_by_channel() {
        let mut m = game();
        reset(&mut m);
        assert_eq!(run(&mut m, at::DMA, dma_callback, [2, 0x8003_0000, 0, 0]), 0);
        assert_eq!(m.io.as_ref().unwrap().dma.dicr & 0x00FF_0000, 0x0084_0000);
        // A GPU transfer completes: its flag, the master flag, IRQ3.
        assert!(m.io.as_mut().unwrap().dma.complete(2));
        m.raise_irq(psx_devices::irq::DMA);
        crate::testing::drain(&mut m);
        assert_eq!(irq_calls(), vec![0x8003_0000]);
        assert_eq!(m.io.as_ref().unwrap().dma.dicr >> 24 & 0x7F, 0, "flags acknowledged");
        assert_eq!(run(&mut m, at::DMA, dma_callback, [2, 0, 0, 0]), 0x8003_0000);
        assert_eq!(m.io.as_ref().unwrap().dma.dicr & 1 << 18, 0);
    }

    #[test]
    fn handlers_are_functions() {
        let mut m = game();
        reset(&mut m);
        run(&mut m, at::VSYNC_CALLBACKS, vsync_callbacks, [2, 0x8001_0002, 0, 0]);
        // libsnd takes line 0, and calls libetc's handler from its own.
        let prev = run(&mut m, at::INTERRUPT, interrupt_callback, [0, 0x8005_0000, 0, 0]);
        assert_eq!(prev, VB_HANDLER);
        vblanks(&mut m, 1);
        assert_eq!(irq_calls(), vec![0x8005_0000]);
        assert_eq!(m.mem.r32(VCOUNT), 0);
        crate::testing::guest(&mut m, prev);
        assert_eq!(m.mem.r32(VCOUNT), 1);
        assert_eq!(irq_calls(), vec![0x8001_0002]);
        // Line 0 taken by a handler that never calls libetc's: VSync's
        // countdown (1 << 15 passes of 14 instructions, 2 cycles each)
        // runs out, a quantum per budget's worth of them; the rest come off
        // the guest's budget.
        let before = m.clock.now;
        run(&mut m, 0, vsync, [0; 4]);
        assert_eq!(m.mem.r32(VCOUNT), 1);
        let cpi = psxw_abi::guest::CYCLES_PER_INSTRUCTION as u64;
        assert_eq!((m.clock.now - before) / 20_000, (1 << 15) * VCOUNT_PASS as u64 * cpi / 20_000);
    }

    #[test]
    fn stop_and_restart() {
        let mut m = game();
        reset(&mut m);
        assert_eq!(run(&mut m, at::STOP, stop_callback, [0; 4]), CONTEXT);
        assert!(m.crit);
        assert_eq!(i_mask(&m), 0);
        assert_eq!(run(&mut m, at::STOP, stop_callback, [0; 4]), 0);
        // Stopped, InterruptCallback changes nothing.
        assert_eq!(run(&mut m, at::INTERRUPT, interrupt_callback, [2, 0x8004_0000, 0, 0]), 0);
        assert_eq!(run(&mut m, at::RESTART, restart_callback, [0; 4]), CONTEXT);
        assert!(!m.crit);
        assert_eq!(i_mask(&m), 0x9);
        assert_eq!(m.io.as_ref().unwrap().dma.dpcr, 0x3333_3333);
        assert_eq!(run(&mut m, at::RESTART, restart_callback, [0; 4]), 0);
    }
}
