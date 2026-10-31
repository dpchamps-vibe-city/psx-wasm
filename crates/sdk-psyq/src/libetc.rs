//! libetc (chip irq, D59): the interrupt layer (`intr.o`, `intr_vb.o`,
//! `intr_dma.o`) and `VSync` (`vsync.o`), as adapters.
//!
//! **State stays where libetc keeps it** (D67): `intr.o`'s context (its
//! flags, the line handlers, its mask), `intr_vb.o`'s `VSyncCallbacks`
//! table and `Vcount`, `intr_dma.o`'s `DMACallback` table, `vsync.o`'s last
//! `Vcount` and `Hcount`, found through the objects' code ([`Lib`],
//! [`VsyncO`]). Code that stays original reads them there, so a game sees
//! one copy. The hardware they drive stays where the
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
//! (Vcount, then the eight `VSyncCallbacks` channels): a stand-in at its
//! address, which increments the `Vcount` in RAM and calls the channels. A
//! guest call to it reaches the runtime ([`psx_bios::Sdk::stand_in`]),
//! which makes the handler's own calls as nested guest calls. Line 3 is
//! `intr_dma.o`'s DMA handler (DICR's flags, acknowledged one by one, each
//! channel's `DMACallback`): DICR is the chip's hardware, which a native
//! chip doesn't run as guest code, so it is an adapter bound by its place
//! (`libetc/intr_dma:0x4c`).
//!
//! Semantics are PsyQ 4.7's objects (`psxw sdk disasm`; psyq-decomp
//! `libetc/{intr,intr_vb,intr_dma,vsync}`), including what a caller can
//! see of a misuse: `InterruptCallback` does nothing before
//! `ResetCallback`, `StopCallback` leaves interrupts disabled, handlers
//! and callbacks get the a0 the original leaves (I_STAT for the line
//! handlers, DICR's address for the DMA callbacks).
//!
//! **What isn't reproduced.** The custom exception exit: the dispatcher is
//! entered as adapter steps, not by the BIOS longjmp-ing into the context's
//! `jmp_buf`, which `ResetCallback` doesn't fill (registers are wasm
//! locals). A callback that leaves with ReturnFromException from inside
//! libetc's own handler isn't supported.

use crate::{Adapter, M, Psyq, Result, bios, reg, ret, set_reg};
use psx_bios::{Flow, GuestCall, IrqStep, StandIn, Wait};

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
    /// in a0 for the handlers and the vblank handler's callbacks.
    pub stat: u16,
    /// The interrupted code's sp while the dispatcher runs on `intr.o`'s
    /// stack.
    pub saved_sp: u32,
}

/// Where `intr.o`, `intr_vb.o` and `intr_dma.o` keep libetc's state (D67),
/// from `intr.o`'s code (psyq-decomp `libetc-intr.h`): the driver-table
/// pointer `intr.o`'s functions load first, the table's context, the `jal`s
/// of `startIntrVSync` and `startIntrDMA`, and their operands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lib {
    /// `intr.o`'s text (`ResetCallback`).
    pub intr: u32,
    pub driver: u32,
    pub context: u32,
    /// The dispatcher's count of unhandled exceptions in a row.
    pub spurious: u32,
    /// `intr_vb.o`: the `VSyncCallbacks` table, `Vcount` and its handler.
    pub vb_table: u32,
    pub vcount: u32,
    pub vb_handler: u32,
    /// `intr_dma.o`: the `DMACallback` table and its handler.
    pub dma_table: u32,
    pub dma_handler: u32,
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

/// The driver table: where it keeps `intr.o`'s context.
mod driver {
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
        dma_table: find(m, "DMACallback's table", dma + 4)?,
        dma_handler: find(m, "the DMA handler", dma + 0x24)?,
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
        self.context != 0 && m.mem.r16(self.context + ctx::ACTIVE) != 0
    }

    fn dispatching(&self, m: &M) -> bool {
        self.context != 0 && m.mem.r16(self.context + ctx::DISPATCHING) != 0
    }

    fn line(&self, m: &M, i: u32) -> u32 {
        m.mem.r32(self.context + ctx::LINES + 4 * i)
    }

    fn mask(&self, m: &M) -> u16 {
        m.mem.r16(self.context + ctx::MASK)
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
        // "unexpected interrupt": the original returns from the exception.
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
            m.log(&format!("intr timeout({stat:04x}:{mask:04x})"));
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
/// acknowledge the next one and call its handler (libetc's own are at
/// their addresses; see [`stand_in`] and [`dma_handler`]), then go on.
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

/// `intr_vb.o`'s handler, which `InterruptCallback` hands out for line 0
/// and code may call (libsnd's tick handler takes line 0 and calls the
/// previous handler first): [`Vcount`](Lib::vcount), then each channel.
/// A step from `state`.
pub fn stand_in(m: &mut M, addr: u32, state: u32) -> Option<StandIn> {
    let l = m.sdk.etc.lib?;
    (addr == l.vb_handler).then(|| vblank_handler(m, l, state))
}

/// `intr_vb.o`'s handler: Vcount, then each `VSyncCallbacks` channel in
/// order, with the a0 the dispatcher's pass leaves (a stand-in can't see a
/// direct call's own). State: 0 on entry, else 1 + the channel to look at
/// next.
fn vblank_handler(m: &mut M, l: Lib, state: u32) -> StandIn {
    if state == 0 {
        let v = m.mem.r32(l.vcount).wrapping_add(1);
        m.mem.w32(l.vcount, v);
    }
    let from = state.saturating_sub(1) as usize;
    let channel = |c: usize| m.mem.r32(l.vb_table + 4 * c as u32);
    match (from..8).find(|&c| channel(c) != 0) {
        Some(c) => StandIn::Call(guest(channel(c), m.sdk.etc.stat as u32), c as u32 + 2),
        None => StandIn::Return(0),
    }
}

/// `intr_dma.o`'s DMA handler, bound by its place (+4Ch): while DICR
/// flags a channel, acknowledge each flagged one in order and call its
/// `DMACallback` (a0: DICR's address), and look again; with none flagged
/// and a bus error, report it.
fn dma_handler(m: &mut M, _a: [u32; 4]) -> Result {
    let d = m.sdk.at.wrapping_sub(0x4C);
    let table = pair(m, d + 4).ok_or_else(|| format!("libetc/intr_dma at {d:#010x}: no DMACallback table where PsyQ 4.7's object has it"))?;
    Ok(dma_loop(m, table))
}

/// The handler's outer loop: DICR's flagged channels, from channel 0.
fn dma_loop(m: &mut M, table: u32) -> Flow<Psyq> {
    let flags = match reg(m, DICR, 4) {
        Ok(v) => (v >> 24) & 0x7F,
        Err(e) => return dma_failed(m, e),
    };
    if flags == 0 {
        let dicr = reg(m, DICR, 4).unwrap_or(0);
        if dicr & 0xFF00_0000 == 0x8000_0000 || dicr & 0x8000 != 0 {
            m.log(&format!("DMA bus error: code={dicr:08x}"));
        }
        return Flow::ret(0);
    }
    dma_channel(m, table, 0, flags)
}

fn dma_failed(m: &mut M, e: String) -> Flow<Psyq> {
    m.log(&format!("libetc DMA handler: {e}"));
    Flow::ret(0)
}

/// The inner loop: channel `ch`, flagged in `rest`'s bit 0. A flagged
/// channel's flag is acknowledged (the enables kept) and its callback
/// called.
fn dma_channel(m: &mut M, table: u32, ch: u32, rest: u32) -> Flow<Psyq> {
    if rest == 0 || ch >= 7 {
        return dma_loop(m, table);
    }
    if rest & 1 == 0 {
        return dma_channel(m, table, ch + 1, rest >> 1);
    }
    let r = reg(m, DICR, 4).and_then(|v| set_reg(m, DICR, 4, v & (1 << (24 + ch) | 0x00FF_FFFF)));
    if let Err(e) = r {
        return dma_failed(m, e);
    }
    let f = m.mem.r32(table + 4 * ch);
    if f == 0 {
        return dma_channel(m, table, ch + 1, rest >> 1);
    }
    Flow::Call { call: guest(f, DICR), then: Box::new(move |m, _| dma_channel(m, table, ch + 1, rest >> 1)) }
}

// --- intr.o ----------------------------------------------------------------

/// `intr.o`'s line setter (`InterruptCallback`'s body): with the context
/// installed, the handler, libetc's mask and I_MASK (masked meanwhile),
/// and the kernel's acknowledge flags for the lines its own handlers
/// share (vblank: the pad and RCntCNT3; root counters 0-2). The previous
/// handler.
fn set_line(m: &mut M, l: Lib, irq: usize, h: u32) -> std::result::Result<u32, String> {
    if l.context == 0 {
        return Ok(0);
    }
    let at = l.context + ctx::LINES + 4 * irq as u32;
    let prev = m.mem.r32(at);
    if h == prev {
        return Ok(prev);
    }
    if !l.active(m) {
        return Ok(prev);
    }
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
    Ok(prev)
}

/// `ResetCallback`: once (until `StopCallback`), mask and acknowledge every
/// interrupt, set every DMA channel's priority with none enabled, clear
/// the context, install the dispatcher, then `startIntrVSync` (root
/// counter 1 counts hblanks, Vcount and the channels from zero, the
/// vblank handler on line 0) and `startIntrDMA` (no DMA callbacks, DICR's
/// enables cleared, the DMA handler on line 3), and leave the critical
/// section. The context's address.
fn reset_callback(m: &mut M, _a: [u32; 4]) -> Result {
    let l = lib(m, at::RESET)?;
    if l.context == 0 {
        return Err(format!("libetc/intr at {:#010x}: no context", l.intr));
    }
    if l.active(m) {
        return ret(0);
    }
    set_reg(m, I_MASK, 2, 0)?;
    let v = reg(m, I_MASK, 2)?;
    set_reg(m, I_STAT, 2, v)?;
    set_reg(m, DPCR, 4, 0x3333_3333)?;
    // Clearing the context, its fields and the dispatcher's stack.
    m.mem.write(l.context, &vec![0; 4 * ctx::WORDS as usize]);
    m.mem.w16(l.context + ctx::ACTIVE, 1);
    set_reg(m, RCNT1_MODE, 4, 0x100)?;
    m.mem.w32(l.vcount, 0);
    m.mem.write(l.vb_table, &[0; 32]);
    set_line(m, l, 0, l.vb_handler)?;
    m.mem.write(l.dma_table, &[0; 32]);
    set_reg(m, DICR, 4, 0)?;
    set_line(m, l, 3, l.dma_handler)?;
    // _96_remove (the BIOS's CD handlers; this BIOS has none), then
    // ExitCriticalSection.
    m.crit = false;
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
    m.crit = true;
    let (mask, dpcr) = (reg(m, I_MASK, 2)? as u16, reg(m, DPCR, 4)?);
    m.mem.w16(l.context + ctx::SAVED_MASK, mask);
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
    if l.context == 0 || l.active(m) {
        return ret(0);
    }
    m.mem.w16(l.context + ctx::ACTIVE, 1);
    let (mask, dpcr) = (m.mem.r16(l.context + ctx::SAVED_MASK), m.mem.r32(l.context + ctx::SAVED_DPCR));
    set_reg(m, I_MASK, 2, mask as u32)?;
    set_reg(m, DPCR, 4, dpcr)?;
    m.crit = false;
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
    ret(set_vsync(m, l, a[0], a[1])?)
}

/// `intr_vb.o`'s setter: the channel's slot, from `ch`.
fn set_vsync(m: &mut M, l: Lib, ch: u32, f: u32) -> std::result::Result<u32, String> {
    if ch >= 8 {
        return Err(format!("VSyncCallbacks({ch}, …): no such channel"));
    }
    let slot = l.vb_table + 4 * ch;
    let old = m.mem.r32(slot);
    m.mem.w32(slot, f);
    Ok(old)
}

/// `VSyncCallback(f)`: channel 4.
fn vsync_callback(m: &mut M, a: [u32; 4]) -> Result {
    let l = lib(m, at::VSYNC_CALLBACK)?;
    ret(set_vsync(m, l, 4, a[0])?)
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

/// `vsync.o`'s spin, one safepoint at a time, while `done` doesn't hold:
/// what fell due in the quantum is delivered, then the interrupt handlers
/// run (the driver drains before resuming), as at the spinning loop's
/// safepoints (§6). Waiting event by event instead would take interrupts
/// at once where the original takes them a quantum late, and devices see
/// that: bof4's CD streaming takes another path.
fn spin(m: &mut M, done: Box<dyn Fn(&mut M) -> bool>, then: Box<dyn FnOnce(&mut M) -> Flow<Psyq>>) -> Flow<Psyq> {
    if done(m) {
        return then(m);
    }
    let until = m.clock.now + quantum(m);
    m.run_until(until);
    Flow::Block { until: Wait::Cycle(until), then: Box::new(move |m| spin(m, done, then)) }
}

/// `vsync.o`'s wait: until Vcount reaches `target`, counting down from
/// `timeout << 15` meanwhile; when that runs out it says so, has the
/// kernel acknowledge vblank itself, and goes on. A spin iteration costs a
/// quantum per [`LOOP_BUDGET`] in virtual time, so the countdown is a
/// deadline. In NTSC it only runs out when nothing counts vblanks: inside
/// a handler or a critical section, libetc stopped, or line 0 taken by a
/// handler that doesn't call libetc's.
fn wait_vcount(m: &mut M, v: VsyncO, target: u32, timeout: u32, then: impl FnOnce(&mut M) -> Flow<Psyq> + 'static) -> Flow<Psyq> {
    let iterations = ((timeout as u64) << 15) + 1;
    let deadline = m.clock.now + iterations * quantum(m) / LOOP_BUDGET;
    let reached = move |m: &M| m.mem.r32(v.vcount) as i32 >= target as i32;
    let then = move |m: &mut M| {
        if !reached(m) {
            m.log("VSync: timeout");
            let _ = bios(m, 0xB0, 0x5B, [0; 4]);
            let _ = bios(m, 0xC0, 0x0A, [3, 0, 0, 0]);
        }
        then(m)
    };
    spin(m, Box::new(move |m| reached(m) || m.clock.now >= deadline), Box::new(then))
}

/// The end of a waiting `VSync`: interlaced, while the field is the one it
/// was before the last wait, spin until it changes; then Vcount and root
/// counter 1 are this call's.
fn interlace(m: &mut M, v: VsyncO, before: u32, delta: u32) -> Flow<Psyq> {
    let field_changed = move |m: &mut M| before & 0x40_0000 == 0 || (before ^ gpustat(m)) & 0x8000_0000 != 0;
    let then = move |m: &mut M| {
        let vcount = m.mem.r32(v.vcount);
        m.mem.w32(v.last, vcount);
        let h = rcnt1(m);
        m.mem.w32(v.hcount, h);
        Flow::ret(delta)
    };
    spin(m, Box::new(field_changed), Box::new(then))
}

/// btn's libetc as its code addresses it, for the tests: `intr.o`'s,
/// `intr_vb.o`'s, `intr_dma.o`'s and `vsync.o`'s operands, and the DMA
/// handler's adapter at its address.
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
    pub const SPURIOUS: u32 = 0x8005_75A4;
    pub const VB_TABLE: u32 = 0x8005_75B4;
    pub const VCOUNT: u32 = 0x8005_75D4;
    pub const VB_HANDLER: u32 = 0x8003_34AC;
    pub const DMA_TABLE: u32 = 0x8005_75E8;
    pub const DMA_HANDLER: u32 = 0x8003_35C0;
    pub const HCOUNT: u32 = 0x8005_64A4;
    pub const LAST: u32 = 0x8005_64A8;

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
        for (off, a) in [(4, VB_TABLE), (0x58, VCOUNT), (0x30, VB_HANDLER)] {
            pair_at(m, VB + off, a);
        }
        for (off, a) in [(4, DMA_TABLE), (0x24, DMA_HANDLER)] {
            pair_at(m, DMA_O + off, a);
        }
        for (off, a) in [(0x60, VCOUNT), (0x48, HCOUNT), (0x84, LAST)] {
            pair_at(m, VSYNC + off, a);
        }
        // The DMA handler's adapter, as the game's thunk at its address
        // reaches it.
        crate::testing::double(DMA_HANDLER, |m| {
            let caller = std::mem::replace(&mut m.sdk.at, DMA_HANDLER);
            let r = super::dma_handler(m, [0; 4]);
            crate::testing::finish(m, r);
            m.sdk.at = caller;
        });
    }

    /// `ResetCallback`, from `intr.o`'s body.
    pub fn reset(m: &mut M) -> u32 {
        m.sdk.at = INTR;
        crate::testing::call(m, reset_callback, [0; 4])
    }

    /// `InterruptCallback`, from `intr.o`'s body.
    pub fn interrupt(m: &mut M, line: u32, h: u32) -> u32 {
        m.sdk.at = INTR + at::INTERRUPT;
        crate::testing::call(m, interrupt_callback, [line, h, 0, 0])
    }

    /// `DMACallback`, from `intr_dma.o`'s setter.
    pub fn dma_callback(m: &mut M, ch: u32, f: u32) -> u32 {
        m.sdk.at = INTR + at::DMA;
        crate::testing::call(m, super::dma_callback, [ch, f, 0, 0])
    }

    /// `VSyncCallback`, from `intr.o`'s body.
    pub fn vsync_callback(m: &mut M, f: u32) -> u32 {
        m.sdk.at = INTR + at::VSYNC_CALLBACK;
        crate::testing::call(m, super::vsync_callback, [f, 0, 0, 0])
    }

    /// `VSync`, from `vsync.o`'s body.
    pub fn vsync(m: &mut M, mode: u32) -> u32 {
        m.sdk.at = VSYNC;
        crate::testing::call(m, super::vsync, [mode, 0, 0, 0])
    }

    /// `VSyncCallbacks`, from `intr_vb.o`'s setter.
    pub fn vsync_callbacks(m: &mut M, ch: u32, f: u32) -> u32 {
        m.sdk.at = INTR + at::VSYNC_CALLBACKS;
        crate::testing::call(m, super::vsync_callbacks, [ch, f, 0, 0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{call, irq_args, irq_calls, machine_with_bus, vblanks};

    fn i_mask(m: &M) -> u32 {
        m.io.as_ref().unwrap().irq.mask
    }

    fn machine() -> M {
        let mut m = machine_with_bus();
        fixture::install(&mut m);
        m
    }

    fn line(m: &M, i: u32) -> u32 {
        m.mem.r32(fixture::CONTEXT + ctx::LINES + 4 * i)
    }

    #[test]
    fn finds_its_state_through_its_code() {
        let mut m = machine();
        m.sdk.at = fixture::INTR;
        let l = lib(&mut m, at::RESET).unwrap();
        assert_eq!(
            (l.intr, l.driver, l.context, l.spurious),
            (fixture::INTR, fixture::DRIVER, fixture::CONTEXT, fixture::SPURIOUS)
        );
        assert_eq!((l.vb_table, l.vcount, l.vb_handler), (fixture::VB_TABLE, fixture::VCOUNT, fixture::VB_HANDLER));
        assert_eq!((l.dma_table, l.dma_handler), (fixture::DMA_TABLE, fixture::DMA_HANDLER));
        m.sdk.at = fixture::VSYNC;
        let v = vsync_o(&mut m).unwrap();
        assert_eq!((v.vcount, v.hcount, v.last), (fixture::VCOUNT, fixture::HCOUNT, fixture::LAST));
        // Anything else at that place is refused.
        m.sdk.etc.lib = None;
        m.sdk.etc.vsync = None;
        m.sdk.at = fixture::INTR + 4;
        assert!(lib(&mut m, at::RESET).is_err(), "not the validated layout");
        m.sdk.at = fixture::VSYNC + 4;
        assert!(vsync_o(&mut m).is_err(), "not the validated layout");
    }

    #[test]
    fn reset_installs_the_dispatcher() {
        let mut m = machine();
        // Before ResetCallback there is no context: nothing changes.
        assert_eq!(fixture::interrupt(&mut m, 2, 0x8004_0000), 0);
        m.sdk.at = fixture::INTR + at::CHECK;
        assert_eq!(call(&mut m, check_callback, [0; 4]), 0);
        m.crit = true;
        assert_eq!(fixture::reset(&mut m), fixture::CONTEXT);
        assert!(!m.crit, "ResetCallback leaves the critical section");
        assert_eq!(i_mask(&m), 0x9, "vblank and DMA");
        assert_eq!(m.io.as_ref().unwrap().dma.dpcr, 0x3333_3333);
        assert_eq!(m.timers[1].mode, 0x100, "root counter 1 counts hblanks");
        // libetc's context, in its own RAM.
        assert_ne!(m.mem.r16(fixture::CONTEXT + ctx::ACTIVE), 0);
        assert_eq!(m.mem.r16(fixture::CONTEXT + ctx::MASK), 0x9);
        assert_eq!(line(&m, 0), fixture::VB_HANDLER);
        assert_eq!(line(&m, 3), fixture::DMA_HANDLER);
        assert_eq!(m.mem.r32(fixture::VCOUNT), 0);
        assert_eq!(m.mem.r32(fixture::VB_TABLE), 0, "no VSync callbacks");
        assert_eq!(m.mem.r32(fixture::DMA_TABLE), 0, "no DMA callbacks");
        assert_eq!(fixture::reset(&mut m), 0, "only once");
        assert_eq!(call(&mut m, get_intr_mask, [0; 4]), 9);
        // Line 0's handler is libetc's own, at its address.
        assert_eq!(fixture::interrupt(&mut m, 0, 0), fixture::VB_HANDLER);
        assert_eq!(line(&m, 0), 0);
        assert_eq!(i_mask(&m), 0x8, "I_MASK's vblank bit cleared");
        assert_eq!(fixture::interrupt(&mut m, 0, fixture::VB_HANDLER), 0);
        assert_eq!(line(&m, 0), fixture::VB_HANDLER);
        assert_eq!(fixture::interrupt(&mut m, 3, 0x8001_0000), fixture::DMA_HANDLER);
        assert_eq!(line(&m, 3), 0x8001_0000);
    }

    #[test]
    fn vsync_waits_and_counts_hblanks() {
        let mut m = machine();
        fixture::reset(&mut m);
        assert_eq!(fixture::vsync(&mut m, u32::MAX), 0);
        fixture::vsync(&mut m, 0);
        assert_eq!(m.vblanks, 1);
        assert_eq!(m.mem.r32(fixture::VCOUNT), 1, "Vcount where libetc keeps it");
        // Half a frame later, VSync(1) reports the hblanks so far.
        let half = m.clock.now + (m.clock.video.vblank_at(2) - m.clock.video.vblank_at(1)) / 2;
        m.run_until(half);
        let h = fixture::vsync(&mut m, 1);
        assert!((130..=132).contains(&h), "{h}");
        // VSync(3): three vblanks after the last waiting call.
        fixture::vsync(&mut m, 3);
        assert_eq!(m.mem.r32(fixture::VCOUNT), 4);
        // Long after, at least the next one.
        vblanks(&mut m, 5);
        fixture::vsync(&mut m, 3);
        assert_eq!(fixture::vsync(&mut m, u32::MAX), 10);
        // With interrupts disabled nothing counts: VSync gives up.
        m.crit = true;
        let before = m.clock.now;
        fixture::vsync(&mut m, 0);
        assert_eq!(m.mem.r32(fixture::VCOUNT), 10);
        assert!(m.clock.now > before);
    }

    #[test]
    fn callbacks_in_line_and_channel_order() {
        let mut m = machine();
        fixture::reset(&mut m);
        assert_eq!(fixture::vsync_callback(&mut m, 0x8001_0004), 0);
        fixture::vsync_callbacks(&mut m, 1, 0x8001_0001);
        assert_eq!(fixture::vsync_callbacks(&mut m, 4, 0), 0x8001_0004);
        fixture::vsync_callbacks(&mut m, 4, 0x8001_0004);
        vblanks(&mut m, 1);
        assert_eq!(irq_calls(), vec![0x8001_0001, 0x8001_0004]);
        assert_eq!(m.mem.r32(fixture::VCOUNT), 1);
        // The channels get the dispatcher's I_STAT in a0: vblank is bit 0.
        for args in irq_args() {
            assert_eq!(args[0] & 1, 1, "a0: I_STAT as the dispatcher read it");
        }
        // Root counter 2 at its target, through InterruptCallback(6, …);
        // the kernel's acknowledge flag follows.
        fixture::interrupt(&mut m, 6, 0x8002_0000);
        assert_eq!(m.kernel.clear_rcnt[2], 0);
        assert_eq!(i_mask(&m), 0x49);
        // SetRCnt(RCntCNT2, 0x100, RCntMdINTR | RCntMdSC).
        call(&mut m, crate::libapi::set_rcnt, [0xF200_0002, 0x100, 0x1001, 0]);
        let until = m.clock.now + 0x1000;
        m.run_until(until);
        crate::testing::drain(&mut m);
        assert_eq!(irq_calls(), vec![0x8002_0000]);
        assert_eq!(m.io.as_ref().unwrap().irq.stat & 0x40, 0, "acknowledged");
        // A masked line runs nothing.
        call(&mut m, set_intr_mask, [0x9, 0, 0, 0]);
        m.run_until(m.clock.now + 0x200);
        crate::testing::drain(&mut m);
        assert_eq!(irq_calls(), Vec::<u32>::new());
    }

    #[test]
    fn handlers_run_on_intr_stack() {
        use psx_bios::Sdk;
        let mut m = machine();
        m.sdk.at = fixture::INTR;
        call(&mut m, reset_callback, [0; 4]);
        fixture::vsync_callbacks(&mut m, 4, 0x8001_0004);
        // Interrupted with its stack in the scratchpad (btn's menu).
        m.regs.set_sp(0x1F80_0378);
        m.raise_irq(psx_devices::irq::VBLANK);
        let mut sps = Vec::new();
        while let Some(s) = m.irq_calls.pop_front() {
            match s {
                IrqStep::Sdk(s) => Psyq::irq_step(&mut m, s),
                IrqStep::Chain(c, handler) => {
                    if !psxw_abi::KERNEL_STAND_INS.contains(&c.addr) {
                        sps.push(m.regs.sp());
                    }
                    m.in_irq = true;
                    let v = crate::testing::guest_with_args(&mut m, c);
                    if let Some(addr) = handler.filter(|_| v != 0 && !m.rfe) {
                        crate::testing::guest_with_args(&mut m, GuestCall { addr, args: [v, 0, 0, 0] });
                    }
                    m.in_irq = false;
                }
                IrqStep::Exit => {}
            }
        }
        assert_eq!(irq_calls(), vec![0x8001_0004]);
        let stack_top = fixture::CONTEXT + ctx::STACK_TOP;
        assert_eq!(sps, vec![stack_top]);
        assert_eq!(m.regs.sp(), 0x1F80_0378, "the interrupted code's sp back");
        assert_eq!(m.mem.r32(fixture::VCOUNT), 1, "libetc's own handler counted it");
    }

    #[test]
    fn dma_callbacks_by_channel() {
        let mut m = machine();
        fixture::reset(&mut m);
        assert_eq!(fixture::dma_callback(&mut m, 2, 0x8003_0000), 0);
        assert_eq!(m.io.as_ref().unwrap().dma.dicr & 0x00FF_0000, 0x0084_0000);
        assert_eq!(m.mem.r32(fixture::DMA_TABLE + 8), 0x8003_0000);
        // A GPU transfer completes: its flag, the master flag, IRQ3.
        assert!(m.io.as_mut().unwrap().dma.complete(2));
        m.raise_irq(psx_devices::irq::DMA);
        crate::testing::drain(&mut m);
        assert_eq!(irq_calls(), vec![0x8003_0000]);
        // The callback gets DICR's address in a0.
        assert_eq!(irq_args().last().unwrap()[0], psx_devices::dma::DICR);
        assert_eq!(m.io.as_ref().unwrap().dma.dicr >> 24 & 0x7F, 0, "flags acknowledged");
        assert_eq!(fixture::dma_callback(&mut m, 2, 0), 0x8003_0000);
        assert_eq!(m.mem.r32(fixture::DMA_TABLE + 8), 0);
        assert_eq!(m.io.as_ref().unwrap().dma.dicr & 1 << 18, 0);
    }

    #[test]
    fn handlers_are_functions() {
        let mut m = machine();
        fixture::reset(&mut m);
        fixture::vsync_callbacks(&mut m, 2, 0x8001_0002);
        // libsnd takes line 0, and calls libetc's handler from its own.
        let prev = fixture::interrupt(&mut m, 0, 0x8005_0000);
        assert_eq!(prev, fixture::VB_HANDLER);
        vblanks(&mut m, 1);
        assert_eq!(irq_calls(), vec![0x8005_0000]);
        assert_eq!(m.mem.r32(fixture::VCOUNT), 0);
        crate::testing::guest(&mut m, prev);
        assert_eq!(m.mem.r32(fixture::VCOUNT), 1);
        assert_eq!(irq_calls(), vec![0x8001_0002]);
        // Line 0 taken by a handler that never calls libetc's: VSync's
        // countdown runs out.
        let before = m.clock.now;
        fixture::vsync(&mut m, 0);
        assert_eq!(m.mem.r32(fixture::VCOUNT), 1);
        assert!(m.clock.now - before >= (1 << 15) * 20);
    }

    #[test]
    fn stop_and_restart() {
        let mut m = machine();
        fixture::reset(&mut m);
        m.sdk.at = fixture::INTR + at::STOP;
        assert_eq!(call(&mut m, stop_callback, [0; 4]), fixture::CONTEXT);
        assert!(m.crit);
        assert_eq!(i_mask(&m), 0);
        assert_eq!(m.mem.r16(fixture::CONTEXT + ctx::ACTIVE), 0);
        assert_eq!(m.mem.r16(fixture::CONTEXT + ctx::SAVED_MASK), 0x9);
        assert_eq!(call(&mut m, stop_callback, [0; 4]), 0);
        // Stopped, InterruptCallback changes nothing.
        assert_eq!(fixture::interrupt(&mut m, 2, 0x8004_0000), 0);
        assert_eq!(line(&m, 2), 0);
        m.sdk.at = fixture::INTR + at::RESTART;
        assert_eq!(call(&mut m, restart_callback, [0; 4]), fixture::CONTEXT);
        assert!(!m.crit);
        assert_eq!(i_mask(&m), 0x9);
        assert_eq!(m.io.as_ref().unwrap().dma.dpcr, 0x3333_3333);
        assert_eq!(call(&mut m, restart_callback, [0; 4]), 0);
    }
}
