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
/// The link's safepoint budget (psxw-build): guest loop iterations per
/// safepoint, each of which moves virtual time by the boot options'
/// quantum. A spin loop's iteration costs quantum / budget cycles.
pub(crate) const LOOP_BUDGET: u64 = 1000;

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

/// `intr.o`'s dispatcher, at the top of its loop.
fn dispatch(m: &mut M) -> std::result::Result<(), String> {
    let Some(l) = m.sdk.etc.lib else { return Ok(()) };
    if !l.active(m) {
        // "unexpected interrupt(%04x)\n" with I_STAT; the original returns
        // from the exception.
        let stat = reg(m, I_STAT, 2)?;
        message(m, l, (0x2B0, 0x2B8, 0x2B4), [stat, 0]);
        return Ok(());
    }
    // The exception's first pass: onto libetc's stack, as the original's
    // exit from the exception (`HookEntryInt`) enters it. The interrupted
    // code's may be the scratchpad, where no DMA reaches (libcd reads
    // sector headers into its callback's frame).
    if !l.dispatching(m) {
        m.sdk.etc.saved_sp = m.regs.sp();
        m.regs.set_sp(l.context + ctx::STACK_TOP);
    }
    m.mem.w16(l.context + ctx::DISPATCHING, 1);
    let p = pending(m, l)?;
    if p != 0 {
        front(m, &[IrqStep::Sdk(step::make(step::LINE, 0, p))]);
        return Ok(());
    }
    // Nothing left for libetc. An unmasked interrupt nobody handles makes
    // the exception come back; after 2048 in a row it gives up on them.
    if reg(m, I_STAT, 2)? & reg(m, I_MASK, 2)? != 0 {
        let n = m.mem.r32(l.spurious);
        m.mem.w32(l.spurious, n + 1);
        if n > 2048 {
            let (stat, mask) = (reg(m, I_STAT, 2)?, reg(m, I_MASK, 2)?);
            // "intr timeout(%04x:%04x)\n".
            message(m, l, (0x3D8, 0x3DC, 0x3E8), [stat, mask]);
            m.mem.w32(l.spurious, 0);
            set_reg(m, I_STAT, 2, 0)?;
        }
    } else {
        m.mem.w32(l.spurious, 0);
    }
    m.regs.set_sp(m.sdk.etc.saved_sp);
    m.mem.w16(l.context + ctx::DISPATCHING, 0);
    Ok(())
}

/// The dispatcher's pass over the pending lines `rest` from line `i`:
/// acknowledge the next one and call its handler, then go on.
fn line(m: &mut M, mut i: u32, mut rest: u32) -> std::result::Result<(), String> {
    let Some(l) = m.sdk.etc.lib else { return Ok(()) };
    while rest != 0 && (i as usize) < LINES {
        if rest & 1 != 0 {
            set_reg(m, I_STAT, 2, !(1u32 << i) & 0xFFFF)?;
            let h = l.line(m, i);
            if h != 0 {
                let next = IrqStep::Sdk(step::make(step::LINE, i + 1, rest >> 1));
                front(m, &[IrqStep::Chain(guest(h, m.sdk.etc.stat as u32), None), next]);
                return Ok(());
            }
        }
        i += 1;
        rest >>= 1;
    }
    front(m, &[IrqStep::Sdk(step::make(step::DISPATCH, 0, 0))]);
    Ok(())
}

// --- intr.o ----------------------------------------------------------------

/// `intr.o`'s line setter (`InterruptCallback`'s body): with the context
/// installed, the handler, libetc's mask and I_MASK (masked meanwhile),
/// and the kernel's acknowledge flags for the lines its own handlers
/// share (vblank: the pad and RCntCNT3; root counters 0-2). The previous
/// handler.
fn set_line(m: &mut M, l: Lib, irq: usize, h: u32) -> std::result::Result<u32, String> {
    let at = l.context + ctx::LINES + 4 * irq as u32;
    let prev = m.mem.r32(at);
    if h != prev && l.active(m) {
        let mut mask = reg(m, I_MASK, 2)?;
        set_reg(m, I_MASK, 2, 0)?;
        let bit = 1u16 << irq;
        let off = h == 0;
        m.mem.w32(at, h);
        let lines = l.mask(m);
        if off {
            mask &= !(bit as u32);
            m.mem.w16(l.context + ctx::MASK, lines & !bit);
        } else {
            mask |= bit as u32;
            m.mem.w16(l.context + ctx::MASK, lines | bit);
        }
        let off = off as u32;
        if irq == 0 {
            bios(m, 0xB0, 0x5B, [off, 0, 0, 0])?;
            bios(m, 0xC0, 0x0A, [3, off, 0, 0])?;
        }
        if (4..=6).contains(&irq) {
            bios(m, 0xC0, 0x0A, [irq as u32 - 4, off, 0, 0])?;
        }
        set_reg(m, I_MASK, 2, mask)?;
    }
    Ok(prev)
}

/// `ResetCallback`: once (until `StopCallback`), mask and acknowledge every
/// interrupt, set every DMA channel's priority with none enabled, clear
/// the context, install the dispatcher, then `startIntrVSync` (root
/// counter 1 counts hblanks, Vcount and the channels from zero, the
/// vblank handler on line 0, its setter in the driver table) and
/// `startIntrDMA` (no DMA callbacks, DICR's enables cleared, the DMA
/// handler on line 3, its setter in the table), and leave the critical
/// section. The context's address.
fn reset_callback(m: &mut M, _a: [u32; 4]) -> Result {
    let l = lib(m, at::RESET)?;
    if l.active(m) {
        return ret(0);
    }
    set_reg(m, I_MASK, 2, 0)?;
    let v = reg(m, I_MASK, 2)?;
    set_reg(m, I_STAT, 2, v)?;
    set_reg(m, DPCR, 4, 0x3333_3333)?;
    m.mem.write(l.context, &vec![0; 4 * ctx::WORDS as usize]);
    m.mem.w16(l.context + ctx::ACTIVE, 1);
    set_reg(m, RCNT1_MODE, 4, 0x100)?;
    m.mem.w32(l.vcount, 0);
    m.mem.write(l.vb_table, &[0; 32]);
    set_line(m, l, 0, l.vb_handler)?;
    m.mem.w32(l.driver + driver::VBLANK, l.vb_setter);
    m.mem.write(l.dma_table, &[0; 32]);
    set_reg(m, DICR, 4, 0)?;
    set_line(m, l, 3, l.dma_handler)?;
    m.mem.w32(l.driver + driver::DMA, l.dma_setter);
    // _96_remove (the BIOS's CD handlers; this BIOS has none), then
    // ExitCriticalSection.
    psx_bios::bios::syscall(m, [2, 0, 0, 0])?;
    ret(l.context)
}

/// `StopCallback`: with the context installed, enter a critical section,
/// keep I_MASK and DPCR, mask and acknowledge every interrupt, disable
/// every DMA channel, and take the dispatcher out.
fn stop_callback(m: &mut M, _a: [u32; 4]) -> Result {
    let l = lib(m, at::STOP)?;
    if !l.active(m) {
        return ret(0);
    }
    psx_bios::bios::syscall(m, [1, 0, 0, 0])?;
    let mask = reg(m, I_MASK, 2)?;
    m.mem.w16(l.context + ctx::SAVED_MASK, mask as u16);
    let dpcr = reg(m, DPCR, 4)?;
    m.mem.w32(l.context + ctx::SAVED_DPCR, dpcr);
    set_reg(m, I_MASK, 2, 0)?;
    let v = reg(m, I_MASK, 2)?;
    set_reg(m, I_STAT, 2, v)?;
    let d = reg(m, DPCR, 4)?;
    set_reg(m, DPCR, 4, d & 0x7777_7777)?;
    m.mem.w16(l.context + ctx::ACTIVE, 0);
    ret(l.context)
}

/// `RestartCallback`: the dispatcher back, I_MASK and DPCR as
/// `StopCallback` kept them, and the critical section left.
fn restart_callback(m: &mut M, _a: [u32; 4]) -> Result {
    let l = lib(m, at::RESTART)?;
    if l.active(m) {
        return ret(0);
    }
    m.mem.w16(l.context + ctx::ACTIVE, 1);
    let (mask, dpcr) = (m.mem.r16(l.context + ctx::SAVED_MASK), m.mem.r32(l.context + ctx::SAVED_DPCR));
    set_reg(m, I_MASK, 2, mask as u32)?;
    set_reg(m, DPCR, 4, dpcr)?;
    psx_bios::bios::syscall(m, [2, 0, 0, 0])?;
    ret(l.context)
}

/// `CheckCallback`: whether the dispatcher is running (a callback is).
fn check_callback(m: &mut M, _a: [u32; 4]) -> Result {
    let l = lib(m, at::CHECK)?;
    ret(l.dispatching(m) as u32)
}

/// `InterruptCallback(line, f)`: the previous handler.
fn interrupt_callback(m: &mut M, a: [u32; 4]) -> Result {
    let l = lib(m, at::INTERRUPT)?;
    let irq = a[0] as usize;
    if irq >= LINES {
        return Err(format!("InterruptCallback({irq}, …): no such interrupt line"));
    }
    ret(set_line(m, l, irq, a[1])?)
}

/// `DMACallback(ch, f)` (`intr_dma.o`'s setter): when `f` changes the
/// channel's callback, DICR's master enable and the channel's enable
/// follow it. The previous callback.
fn dma_callback(m: &mut M, a: [u32; 4]) -> Result {
    let l = lib(m, at::DMA)?;
    let (ch, f) = (a[0], a[1]);
    if ch >= 8 {
        return Err(format!("DMACallback({ch}, …): no such DMA channel"));
    }
    let slot = l.dma_table + 4 * ch;
    let old = m.mem.r32(slot);
    if f != old {
        m.mem.w32(slot, f);
        let d = (reg(m, DICR, 4)? & 0x00FF_FFFF) | 0x0080_0000;
        let bit = 1 << (16 + ch);
        set_reg(m, DICR, 4, if f != 0 { d | bit } else { d & !bit })?;
    }
    ret(old)
}

/// `VSyncCallbacks(ch, f)` (`intr_vb.o`'s setter): the previous callback.
fn vsync_callbacks(m: &mut M, a: [u32; 4]) -> Result {
    let l = lib(m, at::VSYNC_CALLBACKS)?;
    set_vsync(m, l, a[0], a[1])
}

fn set_vsync(m: &mut M, l: Lib, ch: u32, f: u32) -> Result {
    if ch >= 8 {
        return Err(format!("VSyncCallbacks({ch}, …): no such channel"));
    }
    let slot = l.vb_table + 4 * ch;
    let old = m.mem.r32(slot);
    m.mem.w32(slot, f);
    ret(old)
}

/// `intr_dma.o`'s DMA handler, bound by its place (+4Ch): while DICR
/// flags a channel, acknowledge each flagged one in order and call its
/// `DMACallback` (a0: DICR's address), and look again; with none flagged
/// and a bus error, its messages through the game's `printf` (DICR, then
/// each channel's MADR).
fn dma_handler(m: &mut M, _a: [u32; 4]) -> Result {
    let d = m.sdk.at.wrapping_sub(0x4C);
    let table = pair(m, d + 4).ok_or_else(|| format!("libetc/intr_dma at {d:#010x}: no DMACallback table where PsyQ 4.7's object has it"))?;
    Ok(dma_pass(m, d, table, 0, 0))
}

fn dma_pass(m: &mut M, d: u32, table: u32, mut ch: u32, mut rest: u32) -> Flow<Psyq> {
    let failed = |m: &mut M, e: String| {
        m.log(&format!("libetc DMA handler: {e}"));
        Flow::ret(0)
    };
    loop {
        if rest == 0 {
            let dicr = match reg(m, DICR, 4) {
                Ok(v) => v,
                Err(e) => return failed(m, e),
            };
            (ch, rest) = (0, (dicr >> 24) & 0x7F);
            if rest == 0 {
                if dicr & 0xFF00_0000 == 0x8000_0000 || dicr & 0x8000 != 0 {
                    return crate::say(m, (d + 0x160, d + 0x164, d + 0x16C), &[dicr], move |m| bus_error(m, d, 0));
                }
                return Flow::ret(0);
            }
        }
        while rest != 0 && ch < 7 {
            let (c, flagged) = (ch, rest & 1 != 0);
            (ch, rest) = (ch + 1, rest >> 1);
            if !flagged {
                continue;
            }
            // Acknowledge this channel's flag, keeping the enables.
            let r = reg(m, DICR, 4).and_then(|v| set_reg(m, DICR, 4, v & (1 << (24 + c) | 0x00FF_FFFF)));
            if let Err(e) = r {
                return failed(m, e);
            }
            let f = m.mem.r32(table + 4 * c);
            if f != 0 {
                return Flow::Call { call: guest(f, DICR), then: Box::new(move |m, _| dma_pass(m, d, table, ch, rest)) };
            }
        }
        rest = 0;
    }
}

/// The bus error's dump: channel `ch`'s MADR, and the rest after it.
fn bus_error(m: &mut M, d: u32, ch: u32) -> Flow<Psyq> {
    if ch >= 7 {
        return Flow::ret(0);
    }
    let madr = reg(m, 0x1F80_1080 + 16 * ch, 4).unwrap_or(0);
    crate::say(m, (d + 0x174, d + 0x178, d + 0x194), &[ch, madr], move |m| bus_error(m, d, ch + 1))
}

/// `VSyncCallback(f)`: channel 4.
fn vsync_callback(m: &mut M, a: [u32; 4]) -> Result {
    let l = lib(m, at::VSYNC_CALLBACK)?;
    set_vsync(m, l, 4, a[0])
}

fn get_intr_mask(m: &mut M, _a: [u32; 4]) -> Result {
    ret(reg(m, I_MASK, 2)?)
}

/// `SetIntrMask(mask)`: I_MASK, the previous one.
fn set_intr_mask(m: &mut M, a: [u32; 4]) -> Result {
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
/// field must have changed too.
fn vsync(m: &mut M, a: [u32; 4]) -> Result {
    let mode = a[0] as i32;
    let v = vsync_o(m)?;
    if mode < 0 {
        return ret(m.mem.r32(v.vcount));
    }
    let delta = rcnt1(m).wrapping_sub(m.mem.r32(v.hcount)) & 0xFFFF;
    if mode == 1 {
        return ret(delta);
    }
    let last = m.mem.r32(v.last);
    let (target, timeout) = if mode > 0 { (last.wrapping_add(mode as u32 - 1), mode as u32 - 1) } else { (last, 0) };
    Ok(wait_vcount(m, v, target, timeout, move |m| {
        let field = gpustat(m);
        let next = m.mem.r32(v.vcount).wrapping_add(1);
        wait_vcount(m, v, next, 1, move |m| interlace(m, v, field, delta))
    }))
}

/// The safepoint quantum: virtual time a guest loop's thousand iterations
/// take ([`LOOP_BUDGET`]).
pub(crate) fn quantum(m: &M) -> u64 {
    if m.opts.safepoint_quantum == 0 { 20_000 } else { m.opts.safepoint_quantum as u64 }
}

/// `vsync.o`'s wait: until Vcount reaches `target`, counting down from
/// `timeout << 15` meanwhile; when that runs out it says so through the
/// game's `puts`, has the kernel acknowledge vblank itself, and goes on.
/// Each look is a pass of the guest's budget ([`crate::poll`]). In NTSC it
/// only runs out when nothing counts vblanks: inside a handler or a
/// critical section, libetc stopped, or line 0 taken by a handler that
/// doesn't call libetc's.
fn wait_vcount(m: &mut M, v: VsyncO, target: u32, timeout: u32, then: impl FnOnce(&mut M) -> Flow<Psyq> + 'static) -> Flow<Psyq> {
    let text = v.text;
    let iterations = ((timeout as u64) << 15) + 1;
    let reached = std::rc::Rc::new(move |m: &mut M| m.mem.r32(v.vcount) as i32 >= target as i32);
    crate::poll(
        m,
        iterations,
        reached,
        Box::new(move |m, reached| {
            if reached {
                return then(m);
            }
            crate::say(m, (text + 0x1C0, text + 0x1C8, text + 0x1C4), &[], move |m| {
                let _ = bios(m, 0xB0, 0x5B, [0; 4]);
                let _ = bios(m, 0xC0, 0x0A, [3, 0, 0, 0]);
                then(m)
            })
        }),
    )
}

/// The end of a waiting `VSync`: interlaced, while the field is the one it
/// was before the last wait, spin until it changes; then Vcount and root
/// counter 1 are this call's.
fn interlace(m: &mut M, v: VsyncO, before: u32, delta: u32) -> Flow<Psyq> {
    let field_changed = move |m: &mut M| before & 0x40_0000 == 0 || (before ^ gpustat(m)) & 0x8000_0000 != 0;
    let then = move |m: &mut M, _| {
        let vcount = m.mem.r32(v.vcount);
        m.mem.w32(v.last, vcount);
        let h = rcnt1(m);
        m.mem.w32(v.hcount, h);
        Flow::ret(delta)
    };
    crate::poll(m, u64::MAX, std::rc::Rc::new(field_changed), Box::new(then))
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

    fn pair_at(m: &mut M, pc: u32, addr: u32) {
        m.mem.w32(pc, 0x3C02_0000 | addr.wrapping_add(0x8000) >> 16);
        m.mem.w32(pc + 4, 0x2442_0000 | (addr & 0xFFFF));
    }

    fn jal_to(m: &mut M, pc: u32, target: u32) {
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
        // countdown (1 << 15 passes) runs out, a quantum per budget's worth
        // of them; the rest come off the guest's budget.
        let before = m.clock.now;
        run(&mut m, 0, vsync, [0; 4]);
        assert_eq!(m.mem.r32(VCOUNT), 1);
        assert_eq!((m.clock.now - before) / 20_000, (1 << 15) / LOOP_BUDGET);
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
