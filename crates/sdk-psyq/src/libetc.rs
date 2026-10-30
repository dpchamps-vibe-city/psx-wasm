//! libetc (chip irq, D59): the interrupt layer (`intr.o`, `intr_vb.o`,
//! `intr_dma.o`) and `VSync` (`vsync.o`), as adapters.
//!
//! **State.** The callback tables, `Vcount`, `Hcount` and the dispatcher's
//! flags live here, in the runtime: no code but libetc's own reads them in
//! either game (`psxw sdk scan`; the games' symbol files), and every
//! function that does is adapted. The hardware they drive stays where the
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
//! **libetc's own handlers are functions.** Line 0 is its vblank handler
//! (Vcount, then the eight `VSyncCallbacks` channels), line 3 its DMA
//! handler (DICR's flags, acknowledged one by one, each channel's
//! `DMACallback`). Code may take a line and call the handler it replaced:
//! libsnd's tick handler takes line 0 and calls libetc's first. So the
//! handlers are stand-in functions at addresses no game code has
//! ([`VBLANK_HANDLER`], [`DMA_HANDLER`]), which `InterruptCallback`
//! returns and takes back; a guest call to one reaches the runtime
//! ([`psx_bios::Sdk::stand_in`]), which makes the handler's own calls as
//! nested guest calls.
//!
//! Semantics are PsyQ 4.7's objects (`psxw sdk disasm`; psyq-decomp
//! `libetc/{intr,intr_vb,intr_dma,vsync}`), including what a caller can
//! see of a misuse: `InterruptCallback` does nothing before
//! `ResetCallback`, `StopCallback` leaves interrupts disabled.
//!
//! **What isn't reproduced.** libetc's own addresses: `ResetCallback`,
//! `StopCallback` and `RestartCallback` return a pointer to its context
//! ([`CONTEXT`] here), and its handlers' addresses are the stand-ins', all
//! in the BIOS ROM's range. Callbacks get zero arguments, where the
//! original leaves register values (I_STAT, DICR's address) no callback is
//! meant to read. A callback that leaves with ReturnFromException from
//! inside libetc's own handler isn't supported.

use crate::{Adapter, M, Psyq, Result, reg, ret, set_reg};
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
];

/// `ResetCallback`'s, `StopCallback`'s and `RestartCallback`'s result: the
/// original's is the address of `intr.o`'s context.
pub const CONTEXT: u32 = 0xBFC7_0000;
/// `InterruptCallback(0, …)`'s previous handler: libetc's vblank handler.
pub const VBLANK_HANDLER: u32 = 0xBFC7_0010;
/// `InterruptCallback(3, …)`'s previous handler: libetc's DMA handler.
pub const DMA_HANDLER: u32 = 0xBFC7_0020;

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

/// What handles an interrupt line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Handler {
    #[default]
    None,
    /// An `InterruptCallback` function.
    Guest(u32),
    /// libetc's vblank handler (`intr_vb.o`, line 0).
    Vblank,
    /// libetc's DMA handler (`intr_dma.o`, line 3).
    Dma,
}

impl Handler {
    /// As `InterruptCallback` returns and takes it.
    pub fn word(self) -> u32 {
        match self {
            Handler::None => 0,
            Handler::Guest(f) => f,
            Handler::Vblank => VBLANK_HANDLER,
            Handler::Dma => DMA_HANDLER,
        }
    }

    pub fn from_word(w: u32) -> Self {
        match w {
            0 => Handler::None,
            VBLANK_HANDLER => Handler::Vblank,
            DMA_HANDLER => Handler::Dma,
            f => Handler::Guest(f),
        }
    }
}

/// libetc's state.
#[derive(Debug, Default)]
pub struct Etc {
    /// `intr.o`'s context is installed: `ResetCallback` (or
    /// `RestartCallback`) ran, and no `StopCallback` since.
    pub active: bool,
    /// The dispatcher is running (`CheckCallback`).
    pub dispatching: bool,
    /// Each line's handler, and the lines with one (libetc's mask, which
    /// the dispatcher ANDs with I_STAT and I_MASK).
    pub lines: [Handler; LINES],
    pub mask: u16,
    /// I_MASK and DPCR as `StopCallback` found them.
    pub saved_mask: u16,
    pub saved_dpcr: u32,
    /// Exceptions in a row that left an unmasked interrupt no handler
    /// takes; past 2048 the dispatcher clears I_STAT.
    pub spurious: u32,
    /// `intr_vb.o`: vblanks its handler has counted, and the
    /// `VSyncCallbacks` channels it calls after counting.
    pub vcount: u32,
    pub vsync: [u32; 8],
    /// `intr_dma.o`: the `DMACallback` per channel.
    pub dma: [u32; 8],
    /// `vsync.o`: Vcount and root counter 1 when the last waiting `VSync`
    /// returned.
    pub last: u32,
    pub hcount: u32,
    /// The top of `intr.o`'s own stack, where its dispatcher runs the
    /// handlers (0: not found, they run on the interrupted code's), and
    /// the interrupted code's sp while it does.
    pub stack: u32,
    pub saved_sp: u32,
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
    m.sdk.etc.active.then(|| step::make(step::DISPATCH, 0, 0))
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

fn guest(f: u32) -> GuestCall {
    GuestCall { addr: f, args: [0; 4] }
}

fn pending(m: &mut M) -> std::result::Result<u32, String> {
    Ok(m.sdk.etc.mask as u32 & reg(m, I_STAT, 2)? & reg(m, I_MASK, 2)?)
}

/// `intr.o`'s dispatcher, at the top of its loop.
fn dispatch(m: &mut M) -> std::result::Result<(), String> {
    if !m.sdk.etc.active {
        // "unexpected interrupt": the original returns from the exception.
        return Ok(());
    }
    // The exception's first pass: onto libetc's stack, as the original's
    // exit from the exception (`HookEntryInt`) enters it. The interrupted
    // code's may be the scratchpad, where no DMA reaches (libcd reads
    // sector headers into its callback's frame).
    if !m.sdk.etc.dispatching && m.sdk.etc.stack != 0 {
        m.sdk.etc.saved_sp = m.regs.sp();
        m.regs.set_sp(m.sdk.etc.stack);
    }
    m.sdk.etc.dispatching = true;
    let p = pending(m)?;
    if p != 0 {
        front(m, &[IrqStep::Sdk(step::make(step::LINE, 0, p))]);
        return Ok(());
    }
    // Nothing left for libetc. An unmasked interrupt nobody handles makes
    // the exception come back; after 2048 in a row it gives up on them.
    if reg(m, I_STAT, 2)? & reg(m, I_MASK, 2)? != 0 {
        let n = m.sdk.etc.spurious;
        m.sdk.etc.spurious = n + 1;
        if n > 2048 {
            let (stat, mask) = (reg(m, I_STAT, 2)?, reg(m, I_MASK, 2)?);
            m.log(&format!("intr timeout({stat:04x}:{mask:04x})"));
            m.sdk.etc.spurious = 0;
            set_reg(m, I_STAT, 2, 0)?;
        }
    } else {
        m.sdk.etc.spurious = 0;
    }
    if m.sdk.etc.stack != 0 {
        m.regs.set_sp(m.sdk.etc.saved_sp);
    }
    m.sdk.etc.dispatching = false;
    Ok(())
}

/// The dispatcher's pass over the pending lines `rest` from line `i`:
/// acknowledge the next one and call its handler (libetc's own are
/// stand-ins, [`stand_in`]), then go on.
fn line(m: &mut M, mut i: u32, mut rest: u32) -> std::result::Result<(), String> {
    while rest != 0 && (i as usize) < LINES {
        if rest & 1 != 0 {
            set_reg(m, I_STAT, 2, !(1u32 << i) & 0xFFFF)?;
            let h = m.sdk.etc.lines[i as usize];
            if h != Handler::None {
                let next = IrqStep::Sdk(step::make(step::LINE, i + 1, rest >> 1));
                front(m, &[IrqStep::Chain(guest(h.word()), None), next]);
                return Ok(());
            }
        }
        i += 1;
        rest >>= 1;
    }
    front(m, &[IrqStep::Sdk(step::make(step::DISPATCH, 0, 0))]);
    Ok(())
}

/// libetc's vblank and DMA handlers, which `InterruptCallback` hands out
/// for lines 0 and 3 and which code may call (libsnd's tick handler takes
/// line 0 and calls the previous handler first): a step from `state`.
pub fn stand_in(m: &mut M, addr: u32, state: u32) -> Option<StandIn> {
    match addr {
        VBLANK_HANDLER => Some(vblank_handler(m, state)),
        DMA_HANDLER => Some(dma_handler(m, state).unwrap_or_else(|e| {
            m.log(&format!("libetc DMA handler: {e}"));
            StandIn::Return(0)
        })),
        _ => None,
    }
}

/// `intr_vb.o`'s handler: Vcount, then each `VSyncCallbacks` channel in
/// order. State: 0 on entry, else 1 + the channel to look at next.
fn vblank_handler(m: &mut M, state: u32) -> StandIn {
    if state == 0 {
        m.sdk.etc.vcount = m.sdk.etc.vcount.wrapping_add(1);
    }
    let from = state.saturating_sub(1) as usize;
    match (from..8).find(|&c| m.sdk.etc.vsync[c] != 0) {
        Some(c) => StandIn::Call(guest(m.sdk.etc.vsync[c]), c as u32 + 2),
        None => StandIn::Return(0),
    }
}

/// `intr_dma.o`'s handler: while DICR flags a channel, acknowledge each
/// flagged one in order and call its `DMACallback`. State: 0 at the top
/// of the loop, else 1 + (the channel to look at next) << 8 + the flags
/// from it.
fn dma_handler(m: &mut M, mut state: u32) -> std::result::Result<StandIn, String> {
    loop {
        let (mut ch, mut rest) = if state == 0 {
            let flags = (reg(m, DICR, 4)? >> 24) & 0x7F;
            if flags == 0 {
                let d = reg(m, DICR, 4)?;
                if d & 0xFF00_0000 == 0x8000_0000 || d & 0x8000 != 0 {
                    m.log(&format!("DMA bus error: code={d:08x}"));
                }
                return Ok(StandIn::Return(0));
            }
            (0, flags)
        } else {
            ((state - 1) >> 8, (state - 1) & 0xFF)
        };
        while rest != 0 && ch < 7 {
            if rest & 1 != 0 {
                // Acknowledge this channel's flag, keeping the enables.
                let d = reg(m, DICR, 4)?;
                set_reg(m, DICR, 4, d & (1 << (24 + ch) | 0x00FF_FFFF))?;
                let f = m.sdk.etc.dma[ch as usize];
                if f != 0 {
                    return Ok(StandIn::Call(guest(f), 1 + ((ch + 1) << 8 | rest >> 1)));
                }
            }
            ch += 1;
            rest >>= 1;
        }
        state = 0;
    }
}

// --- intr.o ----------------------------------------------------------------

/// `intr.o`'s line setter (`InterruptCallback`'s body): with the context
/// installed, the handler, libetc's mask and I_MASK (masked meanwhile),
/// and the kernel's acknowledge flags for the lines its own handlers
/// share (vblank: the pad and RCntCNT3; root counters 0-2).
fn set_line(m: &mut M, irq: usize, h: Handler) -> std::result::Result<u32, String> {
    let prev = m.sdk.etc.lines[irq];
    if h != prev && m.sdk.etc.active {
        let mut mask = reg(m, I_MASK, 2)?;
        set_reg(m, I_MASK, 2, 0)?;
        let bit = 1u16 << irq;
        let off = h == Handler::None;
        m.sdk.etc.lines[irq] = h;
        if off {
            mask &= !(bit as u32);
            m.sdk.etc.mask &= !bit;
        } else {
            mask |= bit as u32;
            m.sdk.etc.mask |= bit;
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
    Ok(prev.word())
}

/// A BIOS call that returns at once (ChangeClearPAD, ChangeClearRCnt).
fn bios(m: &mut M, table: u32, func: u32, a: [u32; 4]) -> std::result::Result<(), String> {
    psx_bios::bios::call(m, table, func, a).map(|_| ())
}

/// `ResetCallback`: once (until `StopCallback`), mask and acknowledge every
/// interrupt, set every DMA channel's priority with none enabled, clear
/// the context, install the dispatcher, then `startIntrVSync` (root
/// counter 1 counts hblanks, Vcount and the channels from zero, the
/// vblank handler on line 0) and `startIntrDMA` (no DMA callbacks, DICR's
/// enables cleared, the DMA handler on line 3), and leave the critical
/// section.
fn reset_callback(m: &mut M, _a: [u32; 4]) -> Result {
    if m.sdk.etc.active {
        return ret(0);
    }
    set_reg(m, I_MASK, 2, 0)?;
    let v = reg(m, I_MASK, 2)?;
    set_reg(m, I_STAT, 2, v)?;
    set_reg(m, DPCR, 4, 0x3333_3333)?;
    let e = &mut m.sdk.etc;
    (e.dispatching, e.lines, e.mask, e.saved_mask, e.saved_dpcr) = (false, Default::default(), 0, 0, 0);
    e.active = true;
    let reset = m.sdk.at;
    m.sdk.etc.stack = intr_stack(&mut |a| m.mem.r32(a), reset).map_or(0, |s| s.end - 4 * 20);
    set_reg(m, RCNT1_MODE, 4, 0x100)?;
    m.sdk.etc.vcount = 0;
    m.sdk.etc.vsync = [0; 8];
    set_line(m, 0, Handler::Vblank)?;
    m.sdk.etc.dma = [0; 8];
    set_reg(m, DICR, 4, 0)?;
    set_line(m, 3, Handler::Dma)?;
    // _96_remove (the BIOS's CD handlers; this BIOS has none), then
    // ExitCriticalSection.
    m.crit = false;
    ret(CONTEXT)
}

/// `StopCallback`: with the context installed, enter a critical section,
/// keep I_MASK and DPCR, mask and acknowledge every interrupt, disable
/// every DMA channel, and take the dispatcher out.
fn stop_callback(m: &mut M, _a: [u32; 4]) -> Result {
    if !m.sdk.etc.active {
        return ret(0);
    }
    m.crit = true;
    m.sdk.etc.saved_mask = reg(m, I_MASK, 2)? as u16;
    m.sdk.etc.saved_dpcr = reg(m, DPCR, 4)?;
    set_reg(m, I_MASK, 2, 0)?;
    let v = reg(m, I_MASK, 2)?;
    set_reg(m, I_STAT, 2, v)?;
    let d = reg(m, DPCR, 4)?;
    set_reg(m, DPCR, 4, d & 0x7777_7777)?;
    m.sdk.etc.active = false;
    ret(CONTEXT)
}

/// `RestartCallback`: the dispatcher back, I_MASK and DPCR as
/// `StopCallback` kept them, and the critical section left.
fn restart_callback(m: &mut M, _a: [u32; 4]) -> Result {
    if m.sdk.etc.active {
        return ret(0);
    }
    m.sdk.etc.active = true;
    let (mask, dpcr) = (m.sdk.etc.saved_mask, m.sdk.etc.saved_dpcr);
    set_reg(m, I_MASK, 2, mask as u32)?;
    set_reg(m, DPCR, 4, dpcr)?;
    m.crit = false;
    ret(CONTEXT)
}

/// `CheckCallback`: whether the dispatcher is running (a callback is).
fn check_callback(m: &mut M, _a: [u32; 4]) -> Result {
    ret(m.sdk.etc.dispatching as u32)
}

/// `InterruptCallback(line, f)`: the previous handler.
fn interrupt_callback(m: &mut M, a: [u32; 4]) -> Result {
    let irq = a[0] as usize;
    if irq >= LINES {
        return Err(format!("InterruptCallback({irq}, …): no such interrupt line"));
    }
    ret(set_line(m, irq, Handler::from_word(a[1]))?)
}

/// `DMACallback(ch, f)` (`intr_dma.o`): when `f` changes the channel's
/// callback, DICR's master enable and the channel's enable follow it. The
/// previous callback.
fn dma_callback(m: &mut M, a: [u32; 4]) -> Result {
    let (ch, f) = (a[0] as usize, a[1]);
    if ch >= 8 {
        return Err(format!("DMACallback({ch}, …): no such DMA channel"));
    }
    let old = m.sdk.etc.dma[ch];
    if f != old {
        m.sdk.etc.dma[ch] = f;
        let d = (reg(m, DICR, 4)? & 0x00FF_FFFF) | 0x0080_0000;
        let bit = 1 << (16 + ch);
        set_reg(m, DICR, 4, if f != 0 { d | bit } else { d & !bit })?;
    }
    ret(old)
}

/// `VSyncCallbacks(ch, f)` (`intr_vb.o`): the previous callback.
fn vsync_callbacks(m: &mut M, a: [u32; 4]) -> Result {
    let (ch, f) = (a[0] as usize, a[1]);
    if ch >= 8 {
        return Err(format!("VSyncCallbacks({ch}, …): no such channel"));
    }
    ret(std::mem::replace(&mut m.sdk.etc.vsync[ch], f))
}

/// `VSyncCallback(f)`: channel 4.
fn vsync_callback(m: &mut M, a: [u32; 4]) -> Result {
    vsync_callbacks(m, [4, a[0], 0, 0])
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
    let delta = rcnt1(m).wrapping_sub(m.sdk.etc.hcount) & 0xFFFF;
    if mode < 0 {
        return ret(m.sdk.etc.vcount);
    }
    if mode == 1 {
        return ret(delta);
    }
    let last = m.sdk.etc.last;
    let (target, timeout) = if mode > 0 { (last.wrapping_add(mode as u32 - 1), mode as u32 - 1) } else { (last, 0) };
    Ok(wait_vcount(m, target, timeout, move |m| {
        let field = gpustat(m);
        let next = m.sdk.etc.vcount.wrapping_add(1);
        wait_vcount(m, next, 1, move |m| interlace(m, field, delta))
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
fn wait_vcount(m: &mut M, target: u32, timeout: u32, then: impl FnOnce(&mut M) -> Flow<Psyq> + 'static) -> Flow<Psyq> {
    let iterations = ((timeout as u64) << 15) + 1;
    let deadline = m.clock.now + iterations * quantum(m) / LOOP_BUDGET;
    let reached = move |m: &M| m.sdk.etc.vcount as i32 >= target as i32;
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
fn interlace(m: &mut M, before: u32, delta: u32) -> Flow<Psyq> {
    let field_changed = move |m: &mut M| before & 0x40_0000 == 0 || (before ^ gpustat(m)) & 0x8000_0000 != 0;
    let then = move |m: &mut M| {
        m.sdk.etc.last = m.sdk.etc.vcount;
        m.sdk.etc.hcount = rcnt1(m);
        Flow::ret(delta)
    };
    spin(m, Box::new(field_changed), Box::new(then))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{call, irq_calls, machine_with_bus, vblanks};

    fn i_mask(m: &M) -> u32 {
        m.io.as_ref().unwrap().irq.mask
    }

    #[test]
    fn reset_installs_the_dispatcher() {
        let mut m = machine_with_bus();
        m.crit = true;
        assert_eq!(call(&mut m, reset_callback, [0; 4]), CONTEXT);
        assert!(!m.crit, "ResetCallback leaves the critical section");
        assert_eq!(i_mask(&m), 0x9, "vblank and DMA");
        assert_eq!(m.io.as_ref().unwrap().dma.dpcr, 0x3333_3333);
        assert_eq!(m.timers[1].mode, 0x100, "root counter 1 counts hblanks");
        assert_eq!(call(&mut m, reset_callback, [0; 4]), 0, "only once");
        assert_eq!(call(&mut m, get_intr_mask, [0; 4]), 9);
        assert_eq!(call(&mut m, interrupt_callback, [0, 0, 0, 0]), VBLANK_HANDLER);
        assert_eq!(call(&mut m, interrupt_callback, [0, VBLANK_HANDLER, 0, 0]), 0);
        assert_eq!(call(&mut m, interrupt_callback, [3, 0x8001_0000, 0, 0]), DMA_HANDLER);
    }

    #[test]
    fn vsync_waits_and_counts_hblanks() {
        let mut m = machine_with_bus();
        call(&mut m, reset_callback, [0; 4]);
        assert_eq!(call(&mut m, vsync, [u32::MAX, 0, 0, 0]), 0);
        call(&mut m, vsync, [0; 4]);
        assert_eq!(m.vblanks, 1);
        assert_eq!(m.sdk.etc.vcount, 1);
        // Half a frame later, VSync(1) reports the hblanks so far.
        let half = m.clock.now + (m.clock.video.vblank_at(2) - m.clock.video.vblank_at(1)) / 2;
        m.run_until(half);
        let h = call(&mut m, vsync, [1, 0, 0, 0]);
        assert!((130..=132).contains(&h), "{h}");
        // VSync(3): three vblanks after the last waiting call.
        call(&mut m, vsync, [3, 0, 0, 0]);
        assert_eq!(m.sdk.etc.vcount, 4);
        // Long after, at least the next one.
        vblanks(&mut m, 5);
        call(&mut m, vsync, [3, 0, 0, 0]);
        assert_eq!(call(&mut m, vsync, [u32::MAX, 0, 0, 0]), 10);
        // With interrupts disabled nothing counts: VSync gives up.
        m.crit = true;
        let before = m.clock.now;
        call(&mut m, vsync, [0; 4]);
        assert_eq!(m.sdk.etc.vcount, 10);
        assert!(m.clock.now > before);
    }

    #[test]
    fn callbacks_in_line_and_channel_order() {
        let mut m = machine_with_bus();
        call(&mut m, reset_callback, [0; 4]);
        assert_eq!(call(&mut m, vsync_callback, [0x8001_0004, 0, 0, 0]), 0);
        call(&mut m, vsync_callbacks, [1, 0x8001_0001, 0, 0]);
        assert_eq!(call(&mut m, vsync_callbacks, [4, 0, 0, 0]), 0x8001_0004);
        call(&mut m, vsync_callbacks, [4, 0x8001_0004, 0, 0]);
        vblanks(&mut m, 1);
        assert_eq!(irq_calls(), vec![0x8001_0001, 0x8001_0004]);
        // Root counter 2 at its target, through InterruptCallback(6, …);
        // the kernel's acknowledge flag follows.
        call(&mut m, interrupt_callback, [6, 0x8002_0000, 0, 0]);
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
        let mut m = machine_with_bus();
        // btn's ResetCallback: `lui v0, 0x8005; lw v0, 0x7594(v0)` loads
        // .data + 0x10E0; the driver table's eighth word is the context.
        m.mem.w32(0x8003_2D94, 0x3C02_8005);
        m.mem.w32(0x8003_2D98, 0x8C42_7594);
        m.mem.w32(0x8005_7594 - 0x10E0 + 0x10DC, 0x8005_650C);
        m.sdk.at = 0x8003_2D94;
        call(&mut m, reset_callback, [0; 4]);
        assert_eq!(m.sdk.etc.stack, 0x8005_7524);
        call(&mut m, vsync_callback, [0x8001_0004, 0, 0, 0]);
        // Interrupted with its stack in the scratchpad (btn's menu).
        m.regs.set_sp(0x1F80_0378);
        m.raise_irq(psx_devices::irq::VBLANK);
        let mut sps = Vec::new();
        while let Some(s) = m.irq_calls.pop_front() {
            match s {
                IrqStep::Sdk(s) => Psyq::irq_step(&mut m, s),
                IrqStep::Chain(c, _) => {
                    sps.push(m.regs.sp());
                    crate::testing::guest(&mut m, c.addr);
                }
                IrqStep::Exit => {}
            }
        }
        assert_eq!(irq_calls(), vec![0x8001_0004]);
        assert_eq!(sps, vec![0x8005_7524]);
        assert_eq!(m.regs.sp(), 0x1F80_0378, "the interrupted code's sp back");
    }

    #[test]
    fn dma_callbacks_by_channel() {
        let mut m = machine_with_bus();
        call(&mut m, reset_callback, [0; 4]);
        assert_eq!(call(&mut m, dma_callback, [2, 0x8003_0000, 0, 0]), 0);
        assert_eq!(m.io.as_ref().unwrap().dma.dicr & 0x00FF_0000, 0x0084_0000);
        // A GPU transfer completes: its flag, the master flag, IRQ3.
        assert!(m.io.as_mut().unwrap().dma.complete(2));
        m.raise_irq(psx_devices::irq::DMA);
        crate::testing::drain(&mut m);
        assert_eq!(irq_calls(), vec![0x8003_0000]);
        assert_eq!(m.io.as_ref().unwrap().dma.dicr >> 24 & 0x7F, 0, "flags acknowledged");
        assert_eq!(call(&mut m, dma_callback, [2, 0, 0, 0]), 0x8003_0000);
        assert_eq!(m.io.as_ref().unwrap().dma.dicr & 1 << 18, 0);
    }

    #[test]
    fn handlers_are_functions() {
        let mut m = machine_with_bus();
        call(&mut m, reset_callback, [0; 4]);
        call(&mut m, vsync_callbacks, [2, 0x8001_0002, 0, 0]);
        // libsnd takes line 0, and calls libetc's handler from its own.
        let prev = call(&mut m, interrupt_callback, [0, 0x8005_0000, 0, 0]);
        assert_eq!(prev, VBLANK_HANDLER);
        vblanks(&mut m, 1);
        assert_eq!(irq_calls(), vec![0x8005_0000]);
        assert_eq!(m.sdk.etc.vcount, 0);
        crate::testing::guest(&mut m, prev);
        assert_eq!(m.sdk.etc.vcount, 1);
        assert_eq!(irq_calls(), vec![0x8001_0002]);
        // Line 0 taken by a handler that never calls libetc's: VSync's
        // countdown runs out.
        let before = m.clock.now;
        call(&mut m, vsync, [0; 4]);
        assert_eq!(m.sdk.etc.vcount, 1);
        assert!(m.clock.now - before >= (1 << 15) * 20);
    }

    #[test]
    fn stop_and_restart() {
        let mut m = machine_with_bus();
        call(&mut m, reset_callback, [0; 4]);
        assert_eq!(call(&mut m, stop_callback, [0; 4]), CONTEXT);
        assert!(m.crit);
        assert_eq!(i_mask(&m), 0);
        assert_eq!(call(&mut m, stop_callback, [0; 4]), 0);
        // Stopped, InterruptCallback changes nothing.
        assert_eq!(call(&mut m, interrupt_callback, [2, 0x8004_0000, 0, 0]), 0);
        assert_eq!(call(&mut m, restart_callback, [0; 4]), CONTEXT);
        assert!(!m.crit);
        assert_eq!(i_mask(&m), 0x9);
        assert_eq!(m.io.as_ref().unwrap().dma.dpcr, 0x3333_3333);
        assert_eq!(call(&mut m, restart_callback, [0; 4]), 0);
    }
}
