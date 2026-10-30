//! libgpu (chip gpu, D59, D61): `libgpu/sys`'s functions that reach the
//! GPU, as adapters over the GPU core and the DMA controller.
//!
//! **libgpu's state stays where libgpu keeps it** (D59's second option):
//! `GEnv` (graph type, queue switch, debug level, VRAM size, the
//! DrawSync callback and the copies of the last DRAWENV and DISPENV), the
//! command queue (`_que`, `_qin`, `_qout`), and the packets libgpu builds
//! in its own data (ClearImage's fill, MoveImage's copy, the ordering
//! table's terminator). The functions that only touch that state stay
//! original guest code over it: `DrawSyncCallback`, `SetGraphDebug`,
//! `ClearOTag`, `GetDrawEnv`, `SetDrawEnv` and `SetDrawArea` with their
//! draw-area packet helpers (`sys:0x1650`, `sys:0x16e8`), the font and
//! kanji functions, which draw through `DrawOTag`, `LoadImage` and
//! `DrawSync`, and libgs.
//!
//! **Where that state is.** An adapter learns the guest address of the
//! function it replaces (`Psyq::at`, D61). The object is the validated
//! PsyQ 4.7 `libgpu/sys`, so each adapted function's place in its text is
//! known, and the relocated operands of its code (a `lui`/`lo16` pair, a
//! `jal`) give the addresses of its `.data`, `.bss`, `_que` and the
//! functions it calls in other libraries ([`Sys`]).
//!
//! **The hardware.** GP0 and GP1 words go to the GPU core
//! (`Machine::gp0`, `Gpu::gp1`), GPUSTAT and GPUREAD come from it. Image
//! and ordering-table transfers are DMA2 (and the ordering-table clear
//! DMA6), programmed on the DMA controller as libgpu programs it, with the
//! controller's own semantics ([`crate::set_reg`]): the controller is
//! shared with libetc, libcd and libspu, so DPCR's enables, DICR's flags
//! and IRQ3 behave as on the device, and a transfer DPCR doesn't enable
//! waits for it, as btn's first `ClearImage` (before `ResetGraph`) does
//! until libgpu's timeout. I_MASK is masked around queue work as
//! `SetIntrMask(0)` does.
//!
//! **The queue.** libgpu runs work at once when it can, and queues it
//! when a DrawSync callback is set or the GPU is busy; its pump runs the
//! queue and calls the callback when the queue drains (with `a0` the
//! callback's own address, as libgpu leaves it). The pump is also DMA2's
//! completion callback, which the queue registers with `DMACallback`; it
//! is a stand-in function here ([`PUMP`]), and `_GPU_ResetCallback` an
//! adapter. Waits (`DrawSync(0)`, a full queue, the `*2` functions) spin
//! as libgpu's loops do, a safepoint quantum per thousand iterations,
//! counting toward libgpu's timeout (0xF0000 iterations or 240 vblanks),
//! which resets the queue and the GPU as libgpu does.
//!
//! **What isn't reproduced.** Debug output (`SetGraphDebug(n)`, n > 0):
//! libgpu's rectangle checks and `GPU_printf` traces. Waits for GPUSTAT's
//! ready bits outside `DrawSync` (the GPU core is always ready between
//! commands). The stand-in pump's address, which `DMACallback(2, …)`
//! returns while it is registered.

use crate::{Adapter, M, Psyq, Result, reg, set_reg};
use crate::layout::{dispenv, drawenv, rect};
use psx_bios::{Flow, GuestCall, StandIn, Wait};

pub const ADAPTERS: &[(&str, Adapter)] = &[
    ("ResetGraph", reset_graph),
    ("SetDispMask", set_disp_mask),
    ("DrawSync", draw_sync),
    ("ClearImage", clear_image),
    ("LoadImage", load_image),
    ("StoreImage", store_image),
    ("MoveImage", move_image),
    ("MoveImage2", move_image2),
    ("ClearOTagR", clear_otag_r),
    ("DrawOTag", draw_otag),
    ("PutDrawEnv", put_draw_env),
    ("DrawOTagEnv", draw_otag_env),
    ("PutDispEnv", put_disp_env),
    ("GetODE", get_ode),
    ("_GPU_ResetCallback", gpu_reset_callback),
];

/// libgpu's queue pump as DMA2's completion callback: a stand-in at an
/// address no game code has (beside libetc's, [`crate::libetc`]).
pub const PUMP: u32 = 0xBFC7_0100;

/// `libgpu/sys` (PsyQ 4.7): offsets in its text.
mod at {
    pub const RESET_GRAPH: u32 = 0x0;
    pub const SET_DISP_MASK: u32 = 0x2e4;
    pub const DRAW_SYNC: u32 = 0x37c;
    pub const CLEAR_IMAGE: u32 = 0x500;
    pub const LOAD_IMAGE: u32 = 0x628;
    pub const STORE_IMAGE: u32 = 0x688;
    pub const MOVE_IMAGE: u32 = 0x6e8;
    pub const CLEAR_OTAG_R: u32 = 0x868;
    pub const DRAW_OTAG: u32 = 0x970;
    pub const PUT_DRAW_ENV: u32 = 0x9e0;
    pub const DRAW_OTAG_ENV: u32 = 0xaa0;
    pub const PUT_DISP_ENV: u32 = 0xbac;
    pub const GET_ODE: u32 = 0x10d8;
    pub const MOVE_IMAGE2: u32 = 0x2c70;
    pub const GPU_RESET_CALLBACK: u32 = 0x2eb0;
    /// The queue's work functions, what `_que` entries name: ClearImage's,
    /// LoadImage's and StoreImage's, and an ordering table's DMA.
    pub const CLEAR: u32 = 0x1914;
    pub const LOAD: u32 = 0x1b44;
    pub const STORE: u32 = 0x1d80;
    pub const EXEC: u32 = 0x205c;
    /// Relocated operands: `lui`/`lo16` pairs (DrawSync's `.data+0x52`,
    /// ClearImage's work's `.bss`, the queue reset's `_que`) and `jal`s.
    pub const DATA_PAIR: u32 = 0x37c;
    pub const BSS_PAIR: u32 = 0x19e8;
    pub const QUE_PAIR: u32 = 0x2690;
    pub const JAL_RESET_CALLBACK: u32 = 0x80;
    pub const JAL_GET_VIDEO_MODE: u32 = 0xca4;
    pub const JAL_DMA_CALLBACK: u32 = 0x2224;
}

/// `.data` offsets.
mod data {
    /// The dispatch table's address.
    pub const TABLE: u32 = 0x48;
    pub const GENV: u32 = 0x50;
    /// VRAM width and height per graph type (u16 in a word each).
    pub const WIDTHS: u32 = 0xD0;
    pub const HEIGHTS: u32 = 0xDC;
    /// MoveImage's packet: a 4-word VRAM copy (tag, 80h, source, dest,
    /// size).
    pub const MOVE: u32 = 0xE8;
    /// The ordering table's end: four NOPs, ending the list, and the
    /// packet `ClearOTag(R)` points the table's last entry at.
    pub const OT_END: u32 = 0xFC;
    pub const OT_TERM: u32 = 0x110;
    /// Video mode tables for PutDispEnv: horizontal start and end per
    /// (mode, width class), and the dot-clock divider per width class.
    pub const H_RANGE: u32 = 0x124;
    pub const DOT_DIV: u32 = 0x14C;
    /// The GP1 register's address (SetDispMask's result).
    pub const GP1_PTR: u32 = 0x158;
    pub const QIN: u32 = 0x178;
    pub const QOUT: u32 = 0x17C;
    /// I_MASK as the queue, the pump, and the reset found it.
    pub const MASK_QUEUE: u32 = 0x180;
    pub const MASK_PUMP: u32 = 0x184;
    pub const MASK_RESET: u32 = 0x188;
    /// The timeout: its Vcount deadline and its iteration counter.
    pub const DEADLINE: u32 = 0x18C;
    pub const COUNT: u32 = 0x190;
}

/// `GEnv` offsets.
mod genv {
    pub const TYPE: u32 = 0x0;
    pub const QUEUE: u32 = 0x1;
    pub const REVERSE: u32 = 0x3;
    pub const WIDTH: u32 = 0x4;
    pub const HEIGHT: u32 = 0x6;
    /// Work queued since the DrawSync callback last ran.
    pub const PENDING: u32 = 0x8;
    pub const CALLBACK: u32 = 0xC;
    pub const DRAW: u32 = 0x10;
    pub const DISP: u32 = 0x6C;
    pub const SIZE: u32 = 0x80;
}

/// A queue entry: the work function, its data, its argument, and room for
/// a copy of the data (21 words).
const ENTRY: u32 = 0x60;
const ENTRY_BUF: u32 = 0xC;
const QUEUE_LEN: u32 = 64;

const I_MASK: u32 = psx_devices::irq::I_MASK;
const DPCR: u32 = psx_devices::dma::DPCR;
const MADR2: u32 = 0x1F80_10A0;
const BCR2: u32 = 0x1F80_10A4;
const CHCR2: u32 = 0x1F80_10A8;
const MADR6: u32 = 0x1F80_10E0;
const BCR6: u32 = 0x1F80_10E4;
const CHCR6: u32 = 0x1F80_10E8;
const BUSY: u32 = psx_devices::dma::CHCR_BUSY;
/// GPUSTAT bit 26: ready for a command word.
const READY: u32 = 1 << 26;

/// Guest loop iterations per safepoint (psxw-build's budget): libgpu's
/// spins pass a quantum of virtual time per thousand.
const LOOP_BUDGET: u32 = 1000;
/// libgpu's timeout: iterations (its counter passing this), and vblanks.
const TIMEOUT_ITERATIONS: u32 = 0xF0000;
const TIMEOUT_VBLANKS: u32 = 0xF0;

/// Where `libgpu/sys` is in this game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sys {
    pub text: u32,
    pub data: u32,
    pub bss: u32,
    pub que: u32,
    pub reset_callback: u32,
    pub get_video_mode: u32,
    pub dma_callback: u32,
}

impl Sys {
    fn genv(&self) -> u32 {
        self.data + data::GENV
    }
}

/// The chip's adapter state: where the object is, and the phase of
/// libgpu's spin loops within a safepoint quantum.
#[derive(Debug, Default)]
pub struct Gpu {
    pub sys: Option<Sys>,
    pub phase: u32,
}

/// The address a `lui`/`lo16` pair at `pc` makes (the `lo16`
/// instruction's base register is the `lui`'s).
fn hi_lo(m: &M, pc: u32) -> Option<u32> {
    let (hi, lo) = (m.mem.r32(pc), m.mem.r32(pc + 4));
    let rt = (hi >> 16) & 31;
    (hi >> 26 == 0x0F && (lo >> 21) & 31 == rt).then(|| (hi << 16).wrapping_add(lo as u16 as i16 as u32))
}

/// The target of the `jal` at `pc`.
fn jal(m: &M, pc: u32) -> Option<u32> {
    let w = m.mem.r32(pc);
    (w >> 26 == 3).then(|| (pc & 0xF000_0000) | (w & 0x03FF_FFFF) << 2)
}

/// `libgpu/sys` from the running adapter's place in it.
fn sys(m: &mut M, off: u32) -> std::result::Result<Sys, String> {
    let text = m.sdk.at.wrapping_sub(off);
    if let Some(s) = m.sdk.gpu.sys.filter(|s| s.text == text) {
        return Ok(s);
    }
    let find = |what: &str, v: Option<u32>| v.ok_or_else(|| format!("libgpu/sys at {text:#010x}: no {what} where PsyQ 4.7's object has it"));
    let s = Sys {
        text,
        data: find(".data", hi_lo(m, text + at::DATA_PAIR))?.wrapping_sub(0x52),
        bss: find(".bss", hi_lo(m, text + at::BSS_PAIR))?,
        que: find("_que", hi_lo(m, text + at::QUE_PAIR))?,
        reset_callback: find("ResetCallback", jal(m, text + at::JAL_RESET_CALLBACK))?,
        get_video_mode: find("GetVideoMode", jal(m, text + at::JAL_GET_VIDEO_MODE))?,
        dma_callback: find("DMACallback", jal(m, text + at::JAL_DMA_CALLBACK))?,
    };
    m.sdk.gpu.sys = Some(s);
    Ok(s)
}

// --- The hardware ----------------------------------------------------------

fn gp1(m: &mut M, v: u32) {
    if let Some(io) = m.io.as_mut() {
        io.gpu.gp1(v);
    }
}

fn gpustat(m: &M) -> u32 {
    m.io.as_ref().map_or(0, |io| io.gpu.status())
}

fn gpuread(m: &mut M) -> u32 {
    m.io.as_mut().map_or(0, |io| io.gpu.read())
}

/// GP1(10h) `n`: the GPU's answer.
fn gpu_info(m: &mut M, n: u32) -> u32 {
    gp1(m, 0x1000_0000 | n);
    gpuread(m) & 0xFF_FFFF
}

/// DMA2 is running (or waiting for DPCR to enable it).
fn busy(m: &mut M) -> bool {
    reg(m, CHCR2, 4).is_ok_and(|c| c & BUSY != 0)
}

fn ready(m: &M) -> bool {
    gpustat(m) & READY != 0
}

/// `SetIntrMask(0)`: I_MASK cleared, kept at `.data + slot`.
fn mask(m: &mut M, s: Sys, slot: u32) -> std::result::Result<(), String> {
    let v = reg(m, I_MASK, 2)?;
    set_reg(m, I_MASK, 2, 0)?;
    m.mem.w32(s.data + slot, v);
    Ok(())
}

fn unmask(m: &mut M, s: Sys, slot: u32) -> std::result::Result<(), String> {
    let v = m.mem.r32(s.data + slot);
    set_reg(m, I_MASK, 2, v & 0xFFFF)
}

/// An ordering table's DMA (the queue's `EXEC` work): GPU DMA mode
/// CPU→GPU, then a linked-list transfer from `p`.
fn exec_ot(m: &mut M, p: u32) -> std::result::Result<(), String> {
    gp1(m, 0x0400_0002);
    set_reg(m, MADR2, 4, p)?;
    set_reg(m, BCR2, 4, 0)?;
    set_reg(m, CHCR2, 4, 0x0100_0401)
}

fn qin(m: &M, s: Sys) -> u32 {
    m.mem.r32(s.data + data::QIN)
}

fn qout(m: &M, s: Sys) -> u32 {
    m.mem.r32(s.data + data::QOUT)
}

fn queued(m: &M, s: Sys) -> u32 {
    qin(m, s).wrapping_sub(qout(m, s)) & (QUEUE_LEN - 1)
}

fn callback(m: &M, s: Sys) -> u32 {
    m.mem.r32(s.genv() + genv::CALLBACK)
}

fn dma_callback(s: Sys, f: u32) -> GuestCall {
    GuestCall { addr: s.dma_callback, args: [2, f, 0, 0] }
}

fn r16s(m: &M, a: u32) -> i32 {
    m.mem.r16(a) as i16 as i32
}

// --- Waits and the timeout ---------------------------------------------

type K = Box<dyn FnOnce(&mut M, u32) -> Flow<Psyq>>;
type KOk = Box<dyn FnOnce(&mut M, bool) -> Flow<Psyq>>;

/// libgpu's Vcount (`VSync(-1)`): libetc's adapters' count when they run,
/// else the vblanks so far.
fn vcount(m: &M) -> u32 {
    if m.sdk.etc.active { m.sdk.etc.vcount } else { m.vblanks as u32 }
}

/// `sys:0x2880`: the timeout starts.
fn timeout_start(m: &mut M, s: Sys) {
    let v = vcount(m).wrapping_add(TIMEOUT_VBLANKS);
    m.mem.w32(s.data + data::DEADLINE, v);
    m.mem.w32(s.data + data::COUNT, 0);
}

/// `sys:0x28b4` after it runs out: the message, the queue emptied, DMA2
/// stopped and enabled, the GPU's interrupt acknowledged and its command
/// buffer reset.
fn timeout(m: &mut M, s: Sys) -> std::result::Result<(), String> {
    let que = queued(m, s);
    let (stat, chcr, madr) = (gpustat(m), reg(m, CHCR2, 4)?, reg(m, MADR2, 4)?);
    m.host.log(2, &format!("GPU timeout:que={que},stat={stat:08x},chcr={chcr:08x},madr={madr:08x}"));
    mask(m, s, data::MASK_RESET)?;
    m.mem.w32(s.data + data::QOUT, 0);
    m.mem.w32(s.data + data::QIN, 0);
    set_reg(m, CHCR2, 4, 0x401)?;
    let d = reg(m, DPCR, 4)?;
    set_reg(m, DPCR, 4, d | 0x800)?;
    gp1(m, 0x0200_0000);
    gp1(m, 0x0100_0000);
    unmask(m, s, data::MASK_RESET)
}

fn quantum(m: &M) -> u64 {
    if m.opts.safepoint_quantum == 0 { 20_000 } else { m.opts.safepoint_quantum as u64 }
}

/// `n` iterations of one of libgpu's wait loops, each a timeout check;
/// at a thousand a safepoint quantum passes. `k(false)` once the timeout
/// ran out (libgpu's reset done), else `k(true)`.
fn iterate(m: &mut M, s: Sys, n: u32, k: KOk) -> Flow<Psyq> {
    let deadline = m.mem.r32(s.data + data::DEADLINE);
    let count = m.mem.r32(s.data + data::COUNT);
    // An iteration fails when Vcount is past the deadline, or when the
    // counter, before its increment, is past the limit.
    let room = (TIMEOUT_ITERATIONS + 1).saturating_sub(count);
    if (deadline as i32) < vcount(m) as i32 || room < n {
        let done = if (deadline as i32) < vcount(m) as i32 { 0 } else { room + 1 };
        m.mem.w32(s.data + data::COUNT, count + done);
        return match timeout(m, s) {
            Ok(()) => k(m, false),
            Err(e) => fail(m, e),
        };
    }
    m.mem.w32(s.data + data::COUNT, count + n);
    m.sdk.gpu.phase += n;
    if m.sdk.gpu.phase < LOOP_BUDGET {
        return k(m, true);
    }
    m.sdk.gpu.phase -= LOOP_BUDGET;
    let until = m.clock.now + quantum(m);
    m.run_until(until);
    Flow::Block { until: Wait::Cycle(until), then: Box::new(move |m| k(m, true)) }
}

/// Iterations until the next safepoint: what a loop that can't make
/// progress spins through before anything can change.
fn to_safepoint(m: &M) -> u32 {
    LOOP_BUDGET - m.sdk.gpu.phase.min(LOOP_BUDGET - 1)
}

/// `while (DMA2 busy || GPU not ready) if (timeout) fail;` (the `*2`
/// functions and `DrawSync(0)`'s second loop).
fn wait_idle(m: &mut M, s: Sys, k: KOk) -> Flow<Psyq> {
    if !busy(m) && ready(m) {
        return k(m, true);
    }
    let n = to_safepoint(m);
    iterate(m, s, n, Box::new(move |m, ok| if ok { wait_idle(m, s, k) } else { k(m, false) }))
}

/// An error inside a continuation: the runtime traps with it.
fn fail(_m: &mut M, e: String) -> Flow<Psyq> {
    panic!("libgpu: {e}")
}

// --- The queue ---------------------------------------------------------------

/// Run queue work `func` (an address in libgpu/sys's text) on `p`, `arg`.
fn work(m: &mut M, s: Sys, func: u32, p: u32, arg: u32) -> std::result::Result<u32, String> {
    match func.wrapping_sub(s.text) {
        at::EXEC => exec_ot(m, p).map(|_| 0),
        at::CLEAR => clear_work(m, s, p, arg),
        at::LOAD => load_work(m, s, p, arg),
        at::STORE => store_work(m, s, p, arg),
        _ => Err(format!("queue entry with an unknown function {func:#010x}")),
    }
}

/// `sys:0x20f8`: queue work `func` on `p` (copying `len` bytes of it into
/// the entry when queued) with `arg`, or run it at once when nothing is
/// queued, DMA2 is idle and no DrawSync callback is set (or the queue is
/// off). `k` gets what's left in the queue, or -1 on a timeout.
fn enqueue(m: &mut M, s: Sys, func: u32, p: u32, len: u32, arg: u32, k: K) -> Flow<Psyq> {
    timeout_start(m, s);
    enqueue_room(m, s, func, p, len, arg, k)
}

fn enqueue_room(m: &mut M, s: Sys, func: u32, p: u32, len: u32, arg: u32, k: K) -> Flow<Psyq> {
    if (qin(m, s) + 1) & (QUEUE_LEN - 1) == qout(m, s) {
        // Full: `if (timeout) fail; pump();` until there's room.
        let n = if busy(m) { to_safepoint(m) } else { 1 };
        let again = move |m: &mut M, ok: bool| {
            if !ok {
                return k(m, u32::MAX);
            }
            pump(m, s, Box::new(move |m, _| enqueue_room(m, s, func, p, len, arg, k)))
        };
        return iterate(m, s, n, Box::new(again));
    }
    let r = (|| -> std::result::Result<Option<u32>, String> {
        mask(m, s, data::MASK_QUEUE)?;
        let g = s.genv();
        m.mem.w32(g + genv::PENDING, 1);
        let now = m.mem.r8(g + genv::QUEUE) == 0 || (queued(m, s) == 0 && !busy(m) && callback(m, s) == 0);
        if now {
            work(m, s, s.text + func, p, arg)?;
            unmask(m, s, data::MASK_QUEUE)?;
            return Ok(Some(0));
        }
        Ok(None)
    })();
    match r {
        Ok(Some(v)) => k(m, v),
        Ok(None) => Flow::Call {
            call: dma_callback(s, PUMP),
            then: Box::new(move |m, _| {
                let i = qin(m, s);
                let e = s.que + i * ENTRY;
                let at = if len != 0 {
                    for w in 0..len / 4 {
                        let v = m.mem.r32(p + 4 * w);
                        m.mem.w32(e + ENTRY_BUF + 4 * w, v);
                    }
                    e + ENTRY_BUF
                } else {
                    p
                };
                m.mem.w32(e + 4, at);
                m.mem.w32(e + 8, arg);
                m.mem.w32(e, s.text + func);
                m.mem.w32(s.data + data::QIN, (i + 1) & (QUEUE_LEN - 1));
                if let Err(e) = unmask(m, s, data::MASK_QUEUE) {
                    return fail(m, e);
                }
                pump(m, s, Box::new(move |m, _| {
                    let n = queued(m, s);
                    k(m, n)
                }))
            }),
        },
        Err(e) => fail(m, e),
    }
}

/// One step of the pump (`sys:0x23a8`) from `state`: a guest call it
/// makes and the state after, or what it returns.
enum Step {
    Call(GuestCall, u32),
    Done(u32),
}

mod pump_state {
    pub const START: u32 = 0;
    pub const LOOP: u32 = 1;
    pub const WORK: u32 = 2;
    pub const DRAINED: u32 = 3;
    pub const RETURN: u32 = 4;
}

/// The pump: while DMA2 is idle, run queued work in order (taking
/// `DMACallback(2, 0)` off before the last when no DrawSync callback is
/// set); when the queue has drained and work was queued since, call the
/// DrawSync callback with its own address. Returns what's left queued, or
/// 1 when DMA2 was busy.
fn pump_step(m: &mut M, s: Sys, mut state: u32) -> std::result::Result<Step, String> {
    use pump_state::*;
    loop {
        match state {
            START => {
                if busy(m) {
                    return Ok(Step::Done(1));
                }
                mask(m, s, data::MASK_PUMP)?;
                state = LOOP;
            }
            LOOP => {
                if queued(m, s) != 0 && !busy(m) {
                    if (qout(m, s) + 1) & (QUEUE_LEN - 1) == qin(m, s) && callback(m, s) == 0 {
                        return Ok(Step::Call(dma_callback(s, 0), WORK));
                    }
                    state = WORK;
                } else {
                    unmask(m, s, data::MASK_PUMP)?;
                    state = DRAINED;
                }
            }
            WORK => {
                let o = qout(m, s);
                let e = s.que + o * ENTRY;
                let (func, p, arg) = (m.mem.r32(e), m.mem.r32(e + 4), m.mem.r32(e + 8));
                work(m, s, func, p, arg)?;
                m.mem.w32(s.data + data::QOUT, (o + 1) & (QUEUE_LEN - 1));
                state = LOOP;
            }
            DRAINED => {
                let g = s.genv();
                let cb = callback(m, s);
                if queued(m, s) == 0 && !busy(m) && m.mem.r32(g + genv::PENDING) != 0 && cb != 0 {
                    m.mem.w32(g + genv::PENDING, 0);
                    return Ok(Step::Call(GuestCall { addr: cb, args: [cb, 0, 0, 0] }, RETURN));
                }
                return Ok(Step::Done(queued(m, s)));
            }
            _ => return Ok(Step::Done(queued(m, s))),
        }
    }
}

/// The pump as a flow, from an adapter.
fn pump(m: &mut M, s: Sys, k: K) -> Flow<Psyq> {
    pump_from(m, s, pump_state::START, k)
}

fn pump_from(m: &mut M, s: Sys, state: u32, k: K) -> Flow<Psyq> {
    match pump_step(m, s, state) {
        Ok(Step::Done(v)) => k(m, v),
        Ok(Step::Call(call, next)) => Flow::Call { call, then: Box::new(move |m, _| pump_from(m, s, next, k)) },
        Err(e) => fail(m, e),
    }
}

/// The pump as DMA2's completion callback, called through [`PUMP`].
pub fn stand_in(m: &mut M, addr: u32, state: u32) -> Option<StandIn> {
    if addr != PUMP {
        return None;
    }
    let Some(s) = m.sdk.gpu.sys else {
        m.log("libgpu: the queue pump ran before any libgpu adapter");
        return Some(StandIn::Return(0));
    };
    Some(match pump_step(m, s, state) {
        Ok(Step::Done(v)) => StandIn::Return(v as u64),
        Ok(Step::Call(c, next)) => StandIn::Call(c, next),
        Err(e) => {
            m.log(&format!("libgpu pump: {e}"));
            StandIn::Return(0)
        }
    })
}

/// `sys:0x2744`: `DrawSync(mode)`.
fn sync(m: &mut M, s: Sys, mode: u32, k: K) -> Flow<Psyq> {
    if mode != 0 {
        let n = queued(m, s);
        let then = move |m: &mut M, _| {
            let v = if n != 0 {
                n
            } else if !busy(m) && ready(m) {
                0
            } else {
                1
            };
            k(m, v)
        };
        return if n != 0 { pump(m, s, Box::new(then)) } else { then(m, 0) };
    }
    timeout_start(m, s);
    sync_queue(m, s, k)
}

/// `DrawSync(0)`'s first loop: `while (queued) { pump(); if (timeout)
/// fail; }`, then its second, until DMA2 is idle and the GPU ready.
fn sync_queue(m: &mut M, s: Sys, k: K) -> Flow<Psyq> {
    if queued(m, s) == 0 {
        return wait_idle(m, s, Box::new(move |m, ok| k(m, if ok { 0 } else { u32::MAX })));
    }
    let n = if busy(m) { to_safepoint(m) } else { 1 };
    pump(m, s, Box::new(move |m, _| iterate(m, s, n, Box::new(move |m, ok| if ok { sync_queue(m, s, k) } else { k(m, u32::MAX) }))))
}

// --- The work functions --------------------------------------------------

/// A RECT's width and height clamped, as the image functions do: below 0
/// to 0, past `limit(VRAM)` to it; written back.
fn clamp_rect(m: &mut M, s: Sys, r: u32, less: i32) {
    let g = s.genv();
    for (field, size) in [(rect::W, genv::WIDTH), (rect::H, genv::HEIGHT)] {
        let v = r16s(m, r + field);
        let lim = r16s(m, g + size) - less;
        let c = if v < 0 {
            0
        } else if lim < v {
            (m.mem.r16(g + size) as i32 - less) as u16
        } else {
            v as u16
        };
        m.mem.w16(r + field, c);
    }
}

/// ClearImage's work (`sys:0x1914`): a fill of `r` with `color` (bit 31:
/// draw to the display area too), as a packet in libgpu's `.bss`: a VRAM
/// fill when the rectangle is 64-pixel aligned, else a rectangle with the
/// draw area, offset and mask cleared, then restored from the GPU.
fn clear_work(m: &mut M, s: Sys, r: u32, color: u32) -> std::result::Result<u32, String> {
    clamp_rect(m, s, r, 1);
    let p = s.bss;
    let x = m.mem.r16(r + rect::X) as u32;
    let w = m.mem.r16(r + rect::W) as u32;
    let mode = |m: &M| 0xE100_0000 | (color >> 31) << 10 | (gpustat(m) & 0x7FF);
    if x & 0x3F != 0 || w & 0x3F != 0 {
        let q = p + 0x28;
        m.mem.w32(p, (q & 0xFF_FFFF) | 0x0800_0000);
        m.mem.w32(p + 16, 0xE600_0000);
        m.mem.w32(p + 4, 0xE300_0000);
        m.mem.w32(p + 8, 0xE4FF_FFFF);
        m.mem.w32(p + 12, 0xE500_0000);
        let e1 = mode(m);
        m.mem.w32(p + 24, (color & 0xFF_FFFF) | 0x6000_0000);
        m.mem.w32(p + 20, e1);
        let (xy, wh) = (m.mem.r32(r), m.mem.r32(r + 4));
        m.mem.w32(p + 28, xy);
        m.mem.w32(q, 0x03FF_FFFF);
        m.mem.w32(p + 32, wh);
        for (i, cmd) in [(3u32, 0xE300_0000u32), (4, 0xE400_0000), (5, 0xE500_0000)] {
            let v = gpu_info(m, i) | cmd;
            m.mem.w32(q + 4 * (i - 2), v);
        }
    } else {
        m.mem.w32(p, 0x05FF_FFFF);
        m.mem.w32(p + 4, 0xE600_0000);
        m.mem.w32(p + 12, (color & 0xFF_FFFF) | 0x0200_0000);
        let e1 = mode(m);
        m.mem.w32(p + 8, e1);
        let (xy, wh) = (m.mem.r32(r), m.mem.r32(r + 4));
        m.mem.w32(p + 16, xy);
        m.mem.w32(p + 20, wh);
    }
    exec_ot(m, p)?;
    Ok(0)
}

/// An image transfer's size: (words, of which by DMA in 16-word blocks);
/// `None` for an empty rectangle.
fn image_words(m: &M, r: u32) -> Option<(u32, u32)> {
    let size = r16s(m, r + rect::W).wrapping_mul(r16s(m, r + rect::H)).wrapping_add(1);
    let size = size + ((size as u32) >> 31) as i32;
    let words = size >> 1;
    let blocks = size >> 5;
    (words > 0).then_some((words as u32, blocks as u32))
}

/// LoadImage's work (`sys:0x1b44`): CPU→VRAM. The header and the words
/// that don't fill a block through the GP0 port, the blocks by DMA2.
fn load_work(m: &mut M, s: Sys, r: u32, mut p: u32) -> std::result::Result<u32, String> {
    timeout_start(m, s);
    clamp_rect(m, s, r, 0);
    let Some((words, blocks)) = image_words(m, r) else { return Ok(u32::MAX) };
    gp1(m, 0x0400_0000);
    m.gp0(0x0100_0000);
    m.gp0(0xA000_0000);
    let (xy, wh) = (m.mem.r32(r), m.mem.r32(r + 4));
    m.gp0(xy);
    m.gp0(wh);
    for _ in 0..words - blocks * 16 {
        let w = m.mem.r32(p);
        m.gp0(w);
        p = p.wrapping_add(4);
    }
    if blocks != 0 {
        gp1(m, 0x0400_0002);
        set_reg(m, MADR2, 4, p)?;
        set_reg(m, BCR2, 4, blocks << 16 | 0x10)?;
        set_reg(m, CHCR2, 4, 0x0100_0201)?;
    }
    Ok(0)
}

/// StoreImage's work (`sys:0x1d80`): VRAM→CPU, the same way round.
fn store_work(m: &mut M, s: Sys, r: u32, mut p: u32) -> std::result::Result<u32, String> {
    timeout_start(m, s);
    clamp_rect(m, s, r, 0);
    let Some((words, blocks)) = image_words(m, r) else { return Ok(u32::MAX) };
    gp1(m, 0x0400_0000);
    m.gp0(0x0100_0000);
    m.gp0(0xC000_0000);
    let (xy, wh) = (m.mem.r32(r), m.mem.r32(r + 4));
    m.gp0(xy);
    m.gp0(wh);
    for _ in 0..words - blocks * 16 {
        let w = gpuread(m);
        m.mem.w32(p, w);
        p = p.wrapping_add(4);
    }
    if blocks != 0 {
        gp1(m, 0x0400_0003);
        set_reg(m, MADR2, 4, p)?;
        set_reg(m, BCR2, 4, blocks << 16 | 0x10)?;
        set_reg(m, CHCR2, 4, 0x0100_0200)?;
    }
    Ok(0)
}

/// `sys:0x2608`: the queue emptied and, by `mode`, the GPU reset (0, 5:
/// GP1(0) and the queue cleared; 1, 3: its interrupt acknowledged and
/// command buffer reset), DMA2 stopped and enabled; for 0, the GPU's
/// type (`sys:0x29f8`).
fn reset(m: &mut M, s: Sys, mode: u32) -> std::result::Result<u32, String> {
    mask(m, s, data::MASK_RESET)?;
    m.mem.w32(s.data + data::QOUT, 0);
    m.mem.w32(s.data + data::QIN, 0);
    let hard = matches!(mode & 7, 0 | 5);
    if hard || matches!(mode & 7, 1 | 3) {
        set_reg(m, CHCR2, 4, 0x401)?;
        let d = reg(m, DPCR, 4)?;
        set_reg(m, DPCR, 4, d | 0x800)?;
        if hard {
            gp1(m, 0);
            m.mem.write(s.que, &[0; (QUEUE_LEN * ENTRY) as usize]);
        } else {
            gp1(m, 0x0200_0000);
            gp1(m, 0x0100_0000);
        }
    }
    unmask(m, s, data::MASK_RESET)?;
    if mode & 7 != 0 {
        return Ok(0);
    }
    // The GPU's version (GP1(10h) 7): an old GPU gets a draw mode and a
    // read; a new one is type 1, or 2 with mode bit 3 (GP1(09h)).
    if gpu_info(m, 7) != 2 {
        let v = 0xE100_1000 | (gpustat(m) & 0x3FFF);
        m.gp0(v);
        gpuread(m);
        return Ok(0);
    }
    if mode & 8 == 0 {
        return Ok(1);
    }
    gp1(m, 0x0900_0001);
    Ok(2)
}

// --- Draw environments ----------------------------------------------------

/// The draw area corner packets (`sys:0x1650`, `sys:0x16e8`): `x`, `y`
/// clamped to VRAM.
fn area(m: &M, s: Sys, cmd: u32, x: u32, y: u32) -> u32 {
    let g = s.genv();
    let clamp = |v: u32, size: u32| {
        let v16 = v as u16 as i16 as i32;
        if v16 < 0 {
            0
        } else if r16s(m, g + size) - 1 < v16 {
            (m.mem.r16(g + size) as u32).wrapping_sub(1)
        } else {
            v
        }
    };
    cmd | (clamp(x, genv::WIDTH) & 0x3FF) | (clamp(y, genv::HEIGHT) & 0x3FF) << 10
}

/// `sys:0x13c0`: a DRAWENV's DR_ENV packet at `p` (its length set, its
/// link left): draw area, offset, mode, texture window, mask, and with
/// `isbg` a fill of the clip rectangle (a VRAM fill when 64-pixel
/// aligned, else a rectangle relative to the offset).
fn dr_env(m: &mut M, s: Sys, p: u32, e: u32) {
    let h = |m: &M, o: u32| m.mem.r16(e + o) as u32;
    let (cx, cy, cw, ch) = (h(m, drawenv::CLIP), h(m, drawenv::CLIP + 2), h(m, drawenv::CLIP + 4), h(m, drawenv::CLIP + 6));
    let (ox, oy) = (h(m, drawenv::OFS), h(m, drawenv::OFS + 2));
    let mut code = vec![
        area(m, s, 0xE300_0000, cx, cy),
        area(m, s, 0xE400_0000, (cw + cx).wrapping_sub(1), (cy + ch).wrapping_sub(1)),
        0xE500_0000 | (oy & 0x7FF) << 11 | (ox & 0x7FF),
    ];
    let (dtd, dfe) = (m.mem.r8(e + drawenv::DTD), m.mem.r8(e + drawenv::DFE));
    let tpage = h(m, drawenv::TPAGE);
    code.push(0xE100_0000 | if dtd != 0 { 0x200 } else { 0 } | if dfe != 0 { 0x400 } else { 0 } | (tpage & 0x9FF));
    let tw = |o: u32| h(m, drawenv::TW + o) as i16 as i32;
    let (tx, ty, tww, twh) = (tw(0), tw(2), tw(4), tw(6));
    let field = |v: i32| ((v & 0xFF) >> 3) as u32;
    code.push(0xE200_0000 | field(ty) << 15 | field(tx) << 10 | field(-twh) << 5 | field(-tww));
    code.push(0xE600_0000);
    for (i, c) in code.iter().enumerate() {
        m.mem.w32(p + 4 + 4 * i as u32, *c);
    }
    let mut len = 7;
    if m.mem.r8(e + drawenv::ISBG) != 0 {
        let g = s.genv();
        let lim = |m: &M, size: u32| r16s(m, g + size) - 1;
        let clampv = |v: u32, lim: i32| {
            let v = v as u16 as i16 as i32;
            if v < 0 { 0 } else if v > lim { lim as u32 & 0xFFFF } else { v as u32 }
        };
        let (mut rx, mut ry) = (cx, cy);
        let rw = clampv(cw, lim(m, genv::WIDTH));
        let rh = clampv(ch, lim(m, genv::HEIGHT));
        let rgb = (m.mem.r8(e + drawenv::B0) as u32) << 16 | (m.mem.r8(e + drawenv::G0) as u32) << 8 | m.mem.r8(e + drawenv::R0) as u32;
        let cmd = if rx & 0x3F != 0 || rw & 0x3F != 0 {
            rx = rx.wrapping_sub(ox);
            ry = ry.wrapping_sub(oy);
            0x6000_0000
        } else {
            0x0200_0000
        };
        m.mem.w32(p + 4 * len, cmd | rgb);
        m.mem.w32(p + 4 * (len + 1), (rx & 0xFFFF) | (ry & 0xFFFF) << 16);
        m.mem.w32(p + 4 * (len + 2), (rw & 0xFFFF) | (rh & 0xFFFF) << 16);
        len += 3;
    }
    m.mem.w8(p + 3, (len - 1) as u8);
}

// --- The adapters ------------------------------------------------------------

fn ret(v: u32) -> Result {
    Ok(Flow::ret(v))
}

fn k_ret() -> K {
    Box::new(|_, v| Flow::ret(v))
}

/// `ResetGraph(mode)`. Modes 0, 3 and 5: GEnv cleared, `ResetCallback`,
/// a GP0 NOP through the BIOS (`GPU_cw`), the reset, then the GPU's type,
/// the queue on, VRAM's size for the type, and the DRAWENV and DISPENV
/// copies invalid; the type. Others: the reset of mode 1.
fn reset_graph(m: &mut M, a: [u32; 4]) -> Result {
    let s = sys(m, at::RESET_GRAPH)?;
    let mode = a[0];
    let g = s.genv();
    if !matches!(mode & 7, 0 | 3 | 5) {
        return ret(reset(m, s, 1)?);
    }
    if mode & 7 != 5 {
        let table = m.mem.r32(s.data + data::TABLE);
        m.host.log(2, &format!("ResetGraph:jtb={table:08x},env={g:08x}"));
    }
    m.mem.write(g, &[0; genv::SIZE as usize]);
    Ok(Flow::Call {
        call: GuestCall { addr: s.reset_callback, args: [0; 4] },
        then: Box::new(move |m, _| {
            let r = (|| -> std::result::Result<u32, String> {
                let table = m.mem.r32(s.data + data::TABLE);
                psx_bios::bios::call(m, 0xA0, 0x49, [table & 0xFF_FFFF, 0, 0, 0])?;
                let t = reset(m, s, mode)? & 0xFF;
                m.mem.w8(g + genv::TYPE, t as u8);
                m.mem.w8(g + genv::QUEUE, 1);
                let w = m.mem.r16(s.data + data::WIDTHS + 4 * t);
                m.mem.w16(g + genv::WIDTH, w);
                let h = m.mem.r16(s.data + data::HEIGHTS + 4 * t);
                m.mem.w16(g + genv::HEIGHT, h);
                m.mem.write(g + genv::DRAW, &[0xFF; drawenv::SIZEOF as usize]);
                m.mem.write(g + genv::DISP, &[0xFF; dispenv::SIZEOF as usize]);
                Ok(t)
            })();
            match r {
                Ok(t) => Flow::ret(t),
                Err(e) => fail(m, e),
            }
        }),
    })
}

/// `SetDispMask(mask)`: GP1(03h); with 0 the DISPENV copy invalid. Its
/// result is the GP1 register's address, which libgpu's GP1 writer leaves.
fn set_disp_mask(m: &mut M, a: [u32; 4]) -> Result {
    let s = sys(m, at::SET_DISP_MASK)?;
    if a[0] == 0 {
        m.mem.write(s.genv() + genv::DISP, &[0xFF; dispenv::SIZEOF as usize]);
    }
    gp1(m, if a[0] != 0 { 0x0300_0000 } else { 0x0300_0001 });
    ret(m.mem.r32(s.data + data::GP1_PTR))
}

/// `DrawSync(mode)`: 0 waits for the queue and the GPU (-1 on libgpu's
/// timeout); otherwise what's queued, or whether the GPU is busy.
fn draw_sync(m: &mut M, a: [u32; 4]) -> Result {
    let s = sys(m, at::DRAW_SYNC)?;
    Ok(sync(m, s, a[0], k_ret()))
}

/// `ClearImage(rect, r, g, b)`.
fn clear_image(m: &mut M, a: [u32; 4]) -> Result {
    let s = sys(m, at::CLEAR_IMAGE)?;
    let color = (a[3] & 0xFF) << 16 | (a[2] & 0xFF) << 8 | (a[1] & 0xFF);
    Ok(enqueue(m, s, at::CLEAR, a[0], 8, color, k_ret()))
}

/// `LoadImage(rect, p)`.
fn load_image(m: &mut M, a: [u32; 4]) -> Result {
    let s = sys(m, at::LOAD_IMAGE)?;
    Ok(enqueue(m, s, at::LOAD, a[0], 8, a[1], k_ret()))
}

/// `StoreImage(rect, p)`.
fn store_image(m: &mut M, a: [u32; 4]) -> Result {
    let s = sys(m, at::STORE_IMAGE)?;
    Ok(enqueue(m, s, at::STORE, a[0], 8, a[1], k_ret()))
}

/// MoveImage's packet in libgpu's data: source, destination, size.
fn move_packet(m: &mut M, s: Sys, r: u32, x: u32, y: u32) -> bool {
    if m.mem.r16(r + rect::W) == 0 || m.mem.r16(r + rect::H) == 0 {
        return false;
    }
    let p = s.data + data::MOVE + 8;
    let (src, size) = (m.mem.r32(r), m.mem.r32(r + 4));
    m.mem.w32(p, src);
    m.mem.w32(p + 4, y << 16 | (x & 0xFFFF));
    m.mem.w32(p + 8, size);
    true
}

/// `MoveImage(rect, x, y)`: -1 for an empty rectangle.
fn move_image(m: &mut M, a: [u32; 4]) -> Result {
    let s = sys(m, at::MOVE_IMAGE)?;
    if !move_packet(m, s, a[0], a[1], a[2]) {
        return ret(u32::MAX);
    }
    Ok(enqueue(m, s, at::EXEC, s.data + data::MOVE, 0x14, 0, k_ret()))
}

/// `MoveImage2(rect, x, y)`: past the queue. Waits for the GPU (-1 on the
/// timeout), makes `_GPU_ResetCallback` DMA2's callback, and copies.
fn move_image2(m: &mut M, a: [u32; 4]) -> Result {
    let s = sys(m, at::MOVE_IMAGE2)?;
    let (r, x, y) = (a[0], a[1], a[2]);
    timeout_start(m, s);
    Ok(wait_idle(
        m,
        s,
        Box::new(move |_, ok| {
            if !ok {
                return Flow::ret(u32::MAX);
            }
            Flow::Call {
                call: dma_callback(s, s.text + at::GPU_RESET_CALLBACK),
                then: Box::new(move |m, _| {
                    if !move_packet(m, s, r, x, y) {
                        return Flow::ret(u32::MAX);
                    }
                    match exec_ot(m, s.data + data::MOVE) {
                        Ok(()) => Flow::ret(0),
                        Err(e) => fail(m, e),
                    }
                }),
            }
        }),
    ))
}

/// `_GPU_ResetCallback()`: the pump DMA2's callback again.
fn gpu_reset_callback(m: &mut M, _a: [u32; 4]) -> Result {
    let s = sys(m, at::GPU_RESET_CALLBACK)?;
    Ok(Flow::Call { call: dma_callback(s, PUMP), then: Box::new(|_, v| Flow::Return(v)) })
}

/// `ClearOTagR(ot, n)`: the table cleared backwards by DMA6, then its
/// first entry pointed at libgpu's terminator. Returns `ot`.
fn clear_otag_r(m: &mut M, a: [u32; 4]) -> Result {
    let s = sys(m, at::CLEAR_OTAG_R)?;
    let (ot, n) = (a[0], a[1]);
    let d = reg(m, DPCR, 4)?;
    set_reg(m, DPCR, 4, d | 0x0800_0000)?;
    set_reg(m, CHCR6, 4, 0)?;
    set_reg(m, MADR6, 4, ot.wrapping_add(n << 2).wrapping_sub(4))?;
    set_reg(m, BCR6, 4, n)?;
    set_reg(m, CHCR6, 4, 0x1100_0002)?;
    timeout_start(m, s);
    let term = s.data + data::OT_TERM;
    m.mem.w32(term, ((s.data + data::OT_END) & 0xFF_FFFF) | 0x0400_0000);
    m.mem.w32(ot, term & 0xFF_FFFF);
    ret(ot)
}

/// `DrawOTag(p)`.
fn draw_otag(m: &mut M, a: [u32; 4]) -> Result {
    let s = sys(m, at::DRAW_OTAG)?;
    Ok(enqueue(m, s, at::EXEC, a[0], 0, 0, k_ret()))
}

/// The DRAWENV copy after `PutDrawEnv`/`DrawOTagEnv`.
fn keep_draw_env(m: &mut M, s: Sys, env: u32) {
    let b = m.mem.bytes(env, drawenv::SIZEOF as usize);
    m.mem.write(s.genv() + genv::DRAW, &b);
}

/// `PutDrawEnv(env)`: its DR_ENV packet built in `env` and drawn. Returns
/// `env`.
fn put_draw_env(m: &mut M, a: [u32; 4]) -> Result {
    let s = sys(m, at::PUT_DRAW_ENV)?;
    let env = a[0];
    let p = env + drawenv::DR_ENV;
    dr_env(m, s, p, env);
    let tag = m.mem.r32(p) | 0xFF_FFFF;
    m.mem.w32(p, tag);
    Ok(enqueue(
        m,
        s,
        at::EXEC,
        p,
        0x40,
        0,
        Box::new(move |m, _| {
            keep_draw_env(m, s, env);
            Flow::ret(env)
        }),
    ))
}

/// `DrawOTagEnv(p, env)`: `env`'s packet, linked to the table at `p`,
/// drawn. Its result is memcpy's, the DRAWENV copy's address.
fn draw_otag_env(m: &mut M, a: [u32; 4]) -> Result {
    let s = sys(m, at::DRAW_OTAG_ENV)?;
    let (ot, env) = (a[0], a[1]);
    let p = env + drawenv::DR_ENV;
    dr_env(m, s, p, env);
    let tag = (m.mem.r32(p) & 0xFF00_0000) | (ot & 0xFF_FFFF);
    m.mem.w32(p, tag);
    Ok(enqueue(
        m,
        s,
        at::EXEC,
        p,
        0x40,
        0,
        Box::new(move |m, _| {
            keep_draw_env(m, s, env);
            Flow::ret(s.genv() + genv::DRAW)
        }),
    ))
}

/// `PutDispEnv(env)`: the display area's start (GP1(05h)); when the
/// display rectangle or the interlace/24-bit flags changed, the display
/// mode (GP1(08h)) for the video mode, width and height; when the screen
/// rectangle changed (or the mode did), the horizontal and vertical
/// display ranges (GP1(06h), GP1(07h)), clamped to the video mode's. The
/// DISPENV's `pad0` keeps the video mode. Returns `env`.
fn put_disp_env(m: &mut M, a: [u32; 4]) -> Result {
    let s = sys(m, at::PUT_DISP_ENV)?;
    let env = a[0];
    let copy = s.genv() + genv::DISP;
    let (x, y) = (m.mem.r16(env + dispenv::DISP) as u32, m.mem.r16(env + dispenv::DISP + 2) as u32);
    gp1(m, 0x0500_0000 | (y & 0x3FF) << 10 | (x & 0x3FF));
    let same = |m: &M, o: u32, n: u32| (0..n).all(|i| m.mem.r16(copy + o + 2 * i) == m.mem.r16(env + o + 2 * i));
    let same_mode = m.mem.r32(copy + dispenv::ISINTER) == m.mem.r32(env + dispenv::ISINTER) && same(m, dispenv::DISP, 4);
    if same_mode {
        return Ok(disp_screen(m, s, env));
    }
    Ok(Flow::Call {
        call: GuestCall { addr: s.get_video_mode, args: [0; 4] },
        then: Box::new(move |m, v| {
            m.mem.w8(env + dispenv::PAD0, v as u8);
            let mut mode = 0x0800_0000;
            if v as u8 == 1 {
                mode |= 0x8;
            }
            if m.mem.r8(env + dispenv::ISRGB24) != 0 {
                mode |= 0x10;
            }
            if m.mem.r8(env + dispenv::ISINTER) != 0 {
                mode |= 0x20;
            }
            if m.mem.r8(s.genv() + genv::REVERSE) != 0 {
                mode |= 0x80;
            }
            let w = r16s(m, env + dispenv::DISP + 4);
            if w >= 281 {
                mode |= if w < 353 {
                    0x1
                } else if w < 401 {
                    0x40
                } else if w < 561 {
                    0x2
                } else {
                    0x3
                };
            }
            let limit = if m.mem.r8(env + dispenv::PAD0) == 0 { 257 } else { 289 };
            if r16s(m, env + dispenv::DISP + 6) >= limit {
                mode |= 0x24;
            }
            gp1(m, mode);
            m.mem.w8(env + dispenv::PAD0, 8);
            disp_screen(m, s, env)
        }),
    })
}

/// PutDispEnv's screen ranges, then the DISPENV copy.
fn disp_screen(m: &mut M, s: Sys, env: u32) -> Flow<Psyq> {
    let copy = s.genv() + genv::DISP;
    let same = (0..4).all(|i| m.mem.r16(copy + dispenv::SCREEN + 2 * i) == m.mem.r16(env + dispenv::SCREEN + 2 * i));
    if same && m.mem.r8(env + dispenv::PAD0) != 8 {
        return disp_done(m, s, env);
    }
    Flow::Call {
        call: GuestCall { addr: s.get_video_mode, args: [0; 4] },
        then: Box::new(move |m, v| {
            m.mem.w8(env + dispenv::PAD0, v as u8);
            let pal = v as u8 != 0;
            let sc = |m: &M, i: u32| r16s(m, env + dispenv::SCREEN + 2 * i);
            let (sx, sy, sw, sh) = (sc(m, 0), sc(m, 1), sc(m, 2), sc(m, 3));
            let y0 = sy + if pal { 19 } else { 16 };
            let y1 = y0 + if sh != 0 { sh } else { 240 };
            let w = r16s(m, env + dispenv::DISP + 4);
            let class = if w < 281 {
                0
            } else if w < 353 {
                1
            } else if w < 401 {
                2
            } else if w < 561 {
                3
            } else {
                4
            };
            let div = m.mem.r8(s.data + data::DOT_DIV + class) as i32;
            let t = s.data + data::H_RANGE + (m.mem.r8(env + dispenv::PAD0) as u32 * 5 + class) * 4;
            let (start, end) = (m.mem.r16(t) as i32, m.mem.r16(t + 2) as i32);
            let x0 = start + sx * div;
            let x1 = x0 + if sw != 0 { ((end - start) * sw) >> 8 } else { end - start };
            let (x_lo, x_hi, x_end, y_lo, y_hi, y_end) = if pal { (540, 3220, 3260, 19, 303, 305) } else { (500, 3250, 3290, 16, 257, 258) };
            let x0 = x0.clamp(x_lo, x_hi);
            let x1 = if x1 < x0 + div * 4 { x0 + div * 4 } else { x1.min(x_end) };
            let y0 = y0.clamp(y_lo, y_hi);
            let y1 = if y1 < y0 + 2 { y0 + 2 } else { y1.min(y_end) };
            gp1(m, 0x0600_0000 | (x1 as u32 & 0xFFF) << 12 | (x0 as u32 & 0xFFF));
            gp1(m, 0x0700_0000 | (y1 as u32 & 0x3FF) << 10 | (y0 as u32 & 0x3FF));
            disp_done(m, s, env)
        }),
    }
}

fn disp_done(m: &mut M, s: Sys, env: u32) -> Flow<Psyq> {
    let b = m.mem.bytes(env, dispenv::SIZEOF as usize);
    m.mem.write(s.genv() + genv::DISP, &b);
    Flow::ret(env)
}

/// `GetODE()`: GPUSTAT's field bit (which field the display is reading).
fn get_ode(m: &mut M, _a: [u32; 4]) -> Result {
    sys(m, at::GET_ODE)?;
    ret(gpustat(m) >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{call, irq_calls, machine_with_bus};

    /// A stand-in for libgpu/sys in a game: its text, data, bss and queue,
    /// with the relocated operands the adapters read.
    const TEXT: u32 = 0x8003_0000;
    const DATA: u32 = 0x8005_0000;
    const BSS: u32 = 0x8005_1000;
    const QUE: u32 = 0x8006_0000;
    const RESET_CALLBACK: u32 = 0x8001_0000;
    const GET_VIDEO_MODE: u32 = 0x8001_0010;
    const DMA_CALLBACK: u32 = 0x8001_0020;
    const CALLBACK: u32 = 0x8001_0030;
    const RECT: u32 = 0x8010_0000;
    const BUF: u32 = 0x8010_1000;

    fn pair(m: &mut M, pc: u32, addr: u32) {
        let hi = addr.wrapping_add(0x8000) >> 16;
        m.mem.w32(pc, 0x3C02_0000 | hi);
        m.mem.w32(pc + 4, 0x8C42_0000 | (addr & 0xFFFF));
    }

    fn jal_to(m: &mut M, pc: u32, target: u32) {
        m.mem.w32(pc, 0x0C00_0000 | (target >> 2) & 0x03FF_FFFF);
    }

    fn game() -> M {
        let mut m = machine_with_bus();
        pair(&mut m, TEXT + at::DATA_PAIR, DATA + 0x52);
        pair(&mut m, TEXT + at::BSS_PAIR, BSS);
        pair(&mut m, TEXT + at::QUE_PAIR, QUE);
        jal_to(&mut m, TEXT + at::JAL_RESET_CALLBACK, RESET_CALLBACK);
        jal_to(&mut m, TEXT + at::JAL_GET_VIDEO_MODE, GET_VIDEO_MODE);
        jal_to(&mut m, TEXT + at::JAL_DMA_CALLBACK, DMA_CALLBACK);
        m.mem.w32(DATA + data::TABLE, DATA + 8);
        for (t, h) in [512, 512, 1024].iter().enumerate() {
            m.mem.w32(DATA + data::WIDTHS + 4 * t as u32, 1024);
            m.mem.w32(DATA + data::HEIGHTS + 4 * t as u32, *h);
        }
        m.mem.w32(DATA + data::OT_END, 0x04FF_FFFF);
        m.mem.w32(DATA + data::MOVE, 0x04FF_FFFF);
        m.mem.w32(DATA + data::MOVE + 4, 0x8000_0000);
        m.mem.w32(DATA + data::GP1_PTR, 0x1F80_1814);
        m
    }

    fn run(m: &mut M, off: u32, f: Adapter, a: [u32; 4]) -> u32 {
        m.sdk.at = TEXT + off;
        call(m, f, a)
    }

    fn rect(m: &mut M, x: u16, y: u16, w: u16, h: u16) {
        for (i, v) in [x, y, w, h].into_iter().enumerate() {
            m.mem.w16(RECT + 2 * i as u32, v);
        }
    }

    #[test]
    fn finds_the_object_from_its_code() {
        let mut m = game();
        m.sdk.at = TEXT + at::DRAW_SYNC;
        let s = sys(&mut m, at::DRAW_SYNC).unwrap();
        assert_eq!((s.text, s.data, s.bss, s.que), (TEXT, DATA, BSS, QUE));
        assert_eq!((s.reset_callback, s.get_video_mode, s.dma_callback), (RESET_CALLBACK, GET_VIDEO_MODE, DMA_CALLBACK));
        // Anything else at that place is refused.
        m.sdk.gpu.sys = None;
        m.sdk.at = TEXT + at::DRAW_SYNC + 4;
        assert!(sys(&mut m, at::DRAW_SYNC).is_err());
    }

    #[test]
    fn reset_graph_sets_up_genv_and_dma() {
        let mut m = game();
        assert_eq!(run(&mut m, at::RESET_GRAPH, reset_graph, [0; 4]), 1, "a new GPU: type 1");
        assert_eq!(irq_calls(), vec![RESET_CALLBACK]);
        let g = DATA + data::GENV;
        assert_eq!((m.mem.r8(g), m.mem.r8(g + 1)), (1, 1));
        assert_eq!((m.mem.r16(g + 4), m.mem.r16(g + 6)), (1024, 512));
        assert_eq!(m.mem.r32(g + genv::DRAW), u32::MAX);
        assert_ne!(m.io.as_ref().unwrap().dma.dpcr & 0x800, 0, "DMA2 enabled");
        // Mode 1 resets the queue and the GPU's command buffer only.
        m.mem.w32(DATA + data::QIN, 5);
        assert_eq!(run(&mut m, at::RESET_GRAPH, reset_graph, [1, 0, 0, 0]), 0);
        assert_eq!(m.mem.r32(DATA + data::QIN), 0);
        assert_eq!(m.mem.r8(g), 1, "GEnv kept");
    }

    #[test]
    fn images_go_through_the_queue_with_a_callback() {
        let mut m = game();
        run(&mut m, at::RESET_GRAPH, reset_graph, [0; 4]);
        irq_calls();
        // At once: StoreImage reads back 16x8 pixels (zeros from the test
        // host) and clamps nothing.
        rect(&mut m, 0, 0, 16, 8);
        m.mem.write(BUF, &[0xAA; 0x110]);
        assert_eq!(run(&mut m, at::STORE_IMAGE, store_image, [RECT, BUF, 0, 0]), 0);
        assert_eq!(m.mem.bytes(BUF, 0x100), vec![0; 0x100]);
        assert_eq!(m.mem.r32(BUF + 0x100), 0xAAAA_AAAA);
        // Queued: a DrawSync callback makes the queue take every call; the
        // pump runs it, then the callback with its own address.
        m.mem.w32(DATA + data::GENV + genv::CALLBACK, CALLBACK);
        m.mem.write(BUF, &[0xAA; 0x110]);
        rect(&mut m, 8, 8, 16, 8);
        assert_eq!(run(&mut m, at::STORE_IMAGE, store_image, [RECT, BUF, 0, 0]), 0);
        assert_eq!(irq_calls(), vec![DMA_CALLBACK, CALLBACK]);
        assert_eq!(m.mem.bytes(BUF, 0x100), vec![0; 0x100]);
        assert_eq!(m.mem.r32(DATA + data::QIN), 1);
        assert_eq!(m.mem.r32(DATA + data::QOUT), 1);
        assert_eq!(m.mem.r32(QUE), TEXT + at::STORE, "the entry names the work");
    }

    #[test]
    fn clear_image_clamps_and_fills() {
        let mut m = game();
        run(&mut m, at::RESET_GRAPH, reset_graph, [0; 4]);
        rect(&mut m, 64, 0, 2000, 0xFFFF);
        assert_eq!(run(&mut m, at::CLEAR_IMAGE, clear_image, [RECT, 1, 2, 3]), 0);
        assert_eq!((m.mem.r16(RECT + 4), m.mem.r16(RECT + 6)), (1023, 0), "w to VRAM - 1, h from -1 to 0");
        // Not 64-aligned (1023): a rectangle between draw-area resets.
        assert_eq!(m.mem.r32(BSS), ((BSS + 0x28) & 0xFF_FFFF) | 0x0800_0000);
        assert_eq!(m.mem.r32(BSS + 24), 0x6003_0201);
    }

    #[test]
    fn before_reset_graph_a_transfer_waits_and_draw_sync_times_out() {
        let mut m = game();
        // DPCR as ResetCallback leaves it: every channel off.
        m.io.as_mut().unwrap().dma.dpcr = 0x3333_3333;
        rect(&mut m, 0, 0, 64, 64);
        assert_eq!(run(&mut m, at::CLEAR_IMAGE, clear_image, [RECT, 0, 0, 0]), 0);
        assert_eq!(run(&mut m, at::DRAW_SYNC, draw_sync, [1, 0, 0, 0]), 1, "busy");
        let before = m.clock.now;
        assert_eq!(run(&mut m, at::DRAW_SYNC, draw_sync, [0; 4]), u32::MAX);
        let quanta = (m.clock.now - before) / 20_000;
        assert!((982..=984).contains(&quanta), "{quanta} quanta");
        assert_eq!(m.mem.r32(DATA + data::COUNT), TIMEOUT_ITERATIONS + 2);
        assert_ne!(m.io.as_ref().unwrap().dma.dpcr & 0x800, 0, "the timeout enables DMA2");
        assert_eq!(run(&mut m, at::DRAW_SYNC, draw_sync, [1, 0, 0, 0]), 0);
    }

    #[test]
    fn load_then_store_queued() {
        let mut m = game();
        run(&mut m, at::RESET_GRAPH, reset_graph, [0; 4]);
        m.mem.w32(DATA + data::GENV + genv::CALLBACK, CALLBACK);
        let src = 0x8010_3000;
        m.mem.write(src, &[0x11; 0x200]);
        rect(&mut m, 320, 0, 16, 8);
        assert_eq!(run(&mut m, at::LOAD_IMAGE, load_image, [RECT, src, 0, 0]), 0);
        rect(&mut m, 320, 0, 16, 8);
        m.mem.write(BUF, &[0xAA; 0x110]);
        assert_eq!(run(&mut m, at::STORE_IMAGE, store_image, [RECT, BUF, 0, 0]), 0);
        assert_eq!(m.mem.bytes(BUF, 0x100), vec![0; 0x100]);
    }

    #[test]
    fn draw_env_packet() {
        let mut m = game();
        run(&mut m, at::RESET_GRAPH, reset_graph, [0; 4]);
        let env = 0x8010_2000;
        m.mem.write(env, &[0; 0x5C]);
        // clip (0,0,320,240), offset (0,0), dtd, isbg, rgb (1,2,3).
        for (i, v) in [0u16, 0, 320, 240].into_iter().enumerate() {
            m.mem.w16(env + 2 * i as u32, v);
        }
        m.mem.write(env + drawenv::DTD, &[1, 0, 1, 1, 2, 3]);
        assert_eq!(run(&mut m, at::PUT_DRAW_ENV, put_draw_env, [env, 0, 0, 0]), env);
        let p = env + drawenv::DR_ENV;
        let words: Vec<u32> = (0..10).map(|i| m.mem.r32(p + 4 * i)).collect();
        assert_eq!(
            words,
            vec![0x09FF_FFFF, 0xE300_0000, 0xE403_BD3F, 0xE500_0000, 0xE100_0200, 0xE200_0000, 0xE600_0000, 0x0203_0201, 0, 0x00F0_0140]
        );
        assert_eq!(m.mem.bytes(DATA + data::GENV + genv::DRAW, 0x5C), m.mem.bytes(env, 0x5C));
    }
}
