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
//! completion callback, which the queue registers with `DMACallback`: an
//! adapter bound by its place in `sys.o` (`libgpu/sys:0x23a8`, a static
//! function with no name), at its own address, and `_GPU_ResetCallback` an
//! adapter. Waits (`DrawSync(0)`, a full queue, the `*2` functions) spin
//! as libgpu's loops do, counting toward libgpu's timeout (0xF0000
//! iterations or 240 vblanks), which resets the queue and the GPU as
//! libgpu does.
//!
//! **Time** (D71): every adapter spends its original's blocks on the path
//! it takes, its callees' in `sys.o` among them (the pump, the work
//! functions, `memset`, the table's GP1 writer), each loop's passes from
//! its header, where a safepoint can fall (in the pump as DMA2's
//! callback, a quantum: [`crate::irq_header`]); the waits' passes up to
//! the next safepoint at once. The calls into libetc it makes inline
//! spend what their adapters do (`SetIntrMask`'s 6, `VSync(-1)`'s 33);
//! the game's `memcpy`, libapi's A0h vector, its 3.
//!
//! **What isn't reproduced.** Debug output (`SetGraphDebug(n)`, n > 0):
//! libgpu's rectangle checks and `GPU_printf` traces (their time too: the
//! level-0 path is spent). Waits for GPUSTAT's ready bits outside
//! `DrawSync` (the GPU core is always ready between commands: their first
//! look is spent), and for DMA6 (`ClearOTagR`'s is done at once).

use crate::{Adapter, M, Psyq, Result, reg, set_reg};
use crate::layout::{dispenv, drawenv, rect};
use psx_bios::{Flow, GuestCall};

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
    // The queue pump, DMA2's completion callback: static, bound by place.
    ("libgpu/sys:0x23a8", pump_callback),
];

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
    /// The queue pump (static).
    pub const PUMP: u32 = 0x23a8;
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
    /// The timeout's `VSync(-1)`.
    pub const JAL_VSYNC: u32 = 0x2888;
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

/// libgpu's timeout: iterations (its counter passing this), and vblanks.
const TIMEOUT_ITERATIONS: u32 = 0xF0000;
const TIMEOUT_VBLANKS: u32 = 0xF0;

/// Instructions in these ranges of the original's code, as
/// [`crate::path`] spends them: what a batch of a loop's passes spends.
const fn len(r: &[(u32, u32)]) -> u64 {
    let mut n = 0;
    let mut i = 0;
    while i < r.len() {
        n += ((r[i].1 - r[i].0) / 4 + 1) as u64;
        i += 1;
    }
    n
}

/// libetc's functions libgpu calls, whose adapters libgpu's inline (D71):
/// `SetIntrMask` (`intr.o` +17Ch..+190h) and `VSync(-1)` (`vsync.o`
/// +0h..+6Ch, +164h..+174h).
const SET_INTR_MASK: u64 = len(&[(0x17C, 0x190)]);
const VSYNC_VCOUNT: u64 = len(&[(0x0, 0x6C), (0x164, 0x174)]);

/// The timeout check (`sys:0x28b4`) when it doesn't run out, its
/// `VSync(-1)` among them.
const CHECK: u64 = len(&[(0x28B4, 0x28C0), (0x28C4, 0x28D8), (0x28DC, 0x2904), (0x29E4, 0x29F4)]) + VSYNC_VCOUNT;

/// The pump (`sys:0x23a8`) finding DMA2 busy.
const PUMP_BUSY: u64 = len(&[(0x23A8, 0x23D0), (0x25F4, 0x2604)]);

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
    /// libetc's `Vcount`, where `VSync(-1)` reads it.
    pub vcount: u32,
}

impl Sys {
    fn genv(&self) -> u32 {
        self.data + data::GENV
    }
}

/// The chip's adapter state: where the object is.
#[derive(Debug, Default)]
pub struct Gpu {
    pub sys: Option<Sys>,
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
        // VSync(-1): `lui`/`lw` of Vcount at 60h in libetc's vsync.o.
        vcount: find("Vcount", jal(m, text + at::JAL_VSYNC).and_then(|v| hi_lo(m, v + 0x60)))?,
    };
    m.sdk.gpu.sys = Some(s);
    Ok(s)
}

// --- The hardware ----------------------------------------------------------

fn gp1(m: &mut M, v: u32) {
    if let Some(io) = m.io.as_mut() {
        io.gpu.gp1(v);
        // Root counter 0's dot clock runs at the display's width.
        if m.timers[0].set_display_mode(0, io.gpu.display().mode, m.clock.video, m.clock.now) {
            m.reschedule_timer(0);
        }
    }
}

/// The GP1 writer through libgpu's table (`sys:0x2000`).
fn gp1_call(m: &mut M, v: u32) {
    crate::path(m, &[(0x2000, 0x2010)]);
    gp1(m, v);
}

fn gpustat(m: &M) -> u32 {
    m.io.as_ref().map_or(0, |io| io.gpu.status(m.clock.now, m.clock.video))
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
    crate::take(m, SET_INTR_MASK);
    let v = reg(m, I_MASK, 2)?;
    set_reg(m, I_MASK, 2, 0)?;
    m.mem.w32(s.data + slot, v);
    Ok(())
}

fn unmask(m: &mut M, s: Sys, slot: u32) -> std::result::Result<(), String> {
    crate::take(m, SET_INTR_MASK);
    let v = m.mem.r32(s.data + slot);
    set_reg(m, I_MASK, 2, v & 0xFFFF)
}

/// An ordering table's DMA (the queue's `EXEC` work, `sys:0x205c`): GPU
/// DMA mode CPU→GPU, then a linked-list transfer from `p`.
fn exec_ot(m: &mut M, p: u32) -> std::result::Result<(), String> {
    crate::path(m, &[(0x205C, 0x20A0)]);
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

// --- Time, waits and the timeout ------------------------------------------

type K = Box<dyn FnOnce(&mut M, u32) -> Flow<Psyq>>;
type KOk = Box<dyn FnOnce(&mut M, bool) -> Flow<Psyq>>;
type Then = Box<dyn FnOnce(&mut M) -> Flow<Psyq>>;

/// One of the original's loop headers (D71): where the budget has run out,
/// a safepoint before `k`; in an interrupt handler (`irq`, the pump as
/// DMA2's callback) a quantum, delivering nothing ([`crate::irq_header`]).
fn header(m: &mut M, irq: bool, k: impl FnOnce(&mut M) -> Flow<Psyq> + 'static) -> Flow<Psyq> {
    if irq {
        crate::irq_header(m);
    } else if crate::budget(m) <= 0 {
        return crate::safepoint(m, k);
    }
    k(m)
}

/// `n` passes of `per` instructions of one of the original's loops whose
/// header starts each pass; then `k`.
fn passes(m: &mut M, irq: bool, per: u32, n: u32, k: Then) -> Flow<Psyq> {
    if n == 0 {
        return k(m);
    }
    if irq {
        for _ in 0..n {
            crate::irq_header(m);
            crate::take(m, per as u64);
        }
        return k(m);
    }
    header(m, false, move |m| crate::spin(m, per, n as u64, k))
}

/// libgpu's `memset` (`sys:0x2ed8`): `n` bytes at `p` set to `v`, a byte a
/// pass of its loop (+2EE4h); then `k`.
fn memset(m: &mut M, p: u32, v: u8, n: u32, k: Then) -> Flow<Psyq> {
    m.mem.write(p, &vec![v; n as usize]);
    crate::path(m, &[(0x2ED8, 0x2EDC)]);
    if n == 0 {
        crate::path(m, &[(0x2EF4, 0x2EF8)]);
        return k(m);
    }
    crate::path(m, &[(0x2EE0, 0x2EE0)]);
    passes(m, false, 4, n, Box::new(move |m| {
        crate::path(m, &[(0x2EF4, 0x2EF8)]);
        k(m)
    }))
}

/// The game's `memcpy(dst, src, n)` that the `jal` at `pc` calls, then `k`
/// with its result (`dst`): libapi's A0h:2Ah vector (its three
/// instructions; the BIOS copies), else a guest function, which spends its
/// own.
fn memcpy(m: &mut M, pc: u32, (dst, src, n): (u32, u32, u32), k: impl FnOnce(&mut M, u32) -> Flow<Psyq> + 'static) -> Flow<Psyq> {
    match jal(m, pc) {
        Some(f) if [f, f + 4, f + 8].map(|a| m.mem.r32(a)) != [0x240A_00A0, 0x0140_0008, 0x2409_002A] => {
            Flow::Call { call: GuestCall { addr: f, args: [dst, src, n, 0] }, then: Box::new(move |m, v| k(m, v as u32)) }
        }
        _ => {
            crate::take(m, crate::VECTOR_STUB);
            let b = m.mem.bytes(src, n as usize);
            m.mem.write(dst, &b);
            k(m, dst)
        }
    }
}

/// `sys:0x3e4`, the debug level's rectangle check, at level 0 (debug
/// output isn't reproduced).
fn check_rect(m: &mut M) {
    crate::path(m, &[(0x3E4, 0x408), (0x40C, 0x414), (0x418, 0x41C), (0x4F0, 0x4FC)]);
}

/// libgpu's Vcount (`VSync(-1)`): libetc's, where it keeps it (D67).
fn vcount(m: &M, s: Sys) -> u32 {
    m.mem.r32(s.vcount)
}

/// The timeout's deadline and counter set.
fn deadline(m: &mut M, s: Sys) {
    let v = vcount(m, s).wrapping_add(TIMEOUT_VBLANKS);
    m.mem.w32(s.data + data::DEADLINE, v);
    m.mem.w32(s.data + data::COUNT, 0);
}

/// `sys:0x2880`: the timeout starts.
fn timeout_start(m: &mut M, s: Sys) {
    crate::path(m, &[(0x2880, 0x288C)]);
    crate::take(m, VSYNC_VCOUNT);
    crate::path(m, &[(0x2890, 0x28B0)]);
    deadline(m, s);
}

/// `sys:0x28b4` running out (`counted`: its counter, else Vcount past the
/// deadline): the message through the game's `printf`
/// ("GPU timeout:que=%d,stat=%08x,chcr=%08x,madr=%08x\n"), then
/// [`reset_after_timeout`]; then `k(false)`.
fn timeout(m: &mut M, s: Sys, counted: bool, k: KOk) -> Flow<Psyq> {
    crate::path(m, &[(0x28B4, 0x28C0)]);
    crate::take(m, VSYNC_VCOUNT);
    crate::path(m, &[(0x28C4, 0x28D8)]);
    if counted {
        crate::path(m, &[(0x28DC, 0x2904)]);
    }
    crate::path(m, &[(0x2908, 0x2954)]);
    let que = queued(m, s);
    let (stat, chcr, madr) = match (|| Ok::<_, String>((gpustat(m), reg(m, CHCR2, 4)?, reg(m, MADR2, 4)?)))() {
        Ok(v) => v,
        Err(e) => return fail(m, e),
    };
    let t = s.text;
    crate::say(m, (t + 0x2910, t + 0x2914, t + 0x2950), &[que, stat, chcr, madr], move |m| {
        crate::path(m, &[(0x2958, 0x295C)]);
        match reset_after_timeout(m, s) {
            Ok(()) => {
                crate::path(m, &[(0x29DC, 0x29E0), (0x29E8, 0x29F4)]);
                k(m, false)
            }
            Err(e) => fail(m, e),
        }
    })
}

/// The queue emptied, DMA2 stopped and enabled, the GPU's interrupt
/// acknowledged and its command buffer reset.
fn reset_after_timeout(m: &mut M, s: Sys) -> std::result::Result<(), String> {
    mask(m, s, data::MASK_RESET)?;
    crate::path(m, &[(0x2960, 0x29D8)]);
    m.mem.w32(s.data + data::QOUT, 0);
    m.mem.w32(s.data + data::QIN, 0);
    set_reg(m, CHCR2, 4, 0x401)?;
    let d = reg(m, DPCR, 4)?;
    set_reg(m, DPCR, 4, d | 0x800)?;
    gp1(m, 0x0200_0000);
    gp1(m, 0x0100_0000);
    unmask(m, s, data::MASK_RESET)
}

/// `n` passes of one of libgpu's waits, `before` and `after` instructions
/// around the timeout check (`sys:0x28b4`) in each: `k(true)` after them;
/// or, on the pass whose check runs out (Vcount past the deadline, or the
/// counter, before its increment, past its limit), that pass's `before`,
/// the timeout, and `k(false)`.
fn iterate(m: &mut M, s: Sys, before: u64, after: u64, n: u64, k: KOk) -> Flow<Psyq> {
    let count = m.mem.r32(s.data + data::COUNT);
    let late = (m.mem.r32(s.data + data::DEADLINE) as i32) < (vcount(m, s) as i32);
    let ok = if late { 0 } else { (TIMEOUT_ITERATIONS as u64 + 1).saturating_sub(count as u64) };
    let per = before + CHECK + after;
    if ok >= n {
        crate::take(m, n * per);
        m.mem.w32(s.data + data::COUNT, count.wrapping_add(n as u32));
        return k(m, true);
    }
    crate::take(m, ok * per + before);
    m.mem.w32(s.data + data::COUNT, count.wrapping_add(ok as u32 + !late as u32));
    timeout(m, s, !late, k)
}

/// The passes of a wait from its header while nothing it looks at can
/// change before a safepoint (DMA2 busy, the GPU not ready), `before` and
/// `after` instructions around the timeout check in each: those up to the
/// next safepoint are spent at once, then the safepoint, and `k(true)`
/// (at the header, its check taken); `k(false)` once the timeout ran out.
fn spin_checks(m: &mut M, s: Sys, before: u64, after: u64, k: KOk) -> Flow<Psyq> {
    let n = crate::room(m, (before + CHECK + after) as u32).max(1);
    iterate(m, s, before, after, n, Box::new(move |m, ok| if ok { crate::safepoint(m, move |m| k(m, true)) } else { k(m, false) }))
}

/// An error inside a continuation: the runtime traps with it.
fn fail(_m: &mut M, e: String) -> Flow<Psyq> {
    panic!("libgpu: {e}")
}

// --- The queue ---------------------------------------------------------------

/// Queue work: the work function (an offset in libgpu/sys's text), its
/// data (`len` bytes of it copied into the entry when queued) and its
/// argument.
#[derive(Clone, Copy)]
struct Job {
    func: u32,
    p: u32,
    len: u32,
    arg: u32,
}

/// Run queue work `func` (an address in libgpu/sys's text) on `p`, `arg`,
/// then `k`.
fn work(m: &mut M, s: Sys, irq: bool, func: u32, p: u32, arg: u32, k: Then) -> Flow<Psyq> {
    let r = match func.wrapping_sub(s.text) {
        at::EXEC => exec_ot(m, p),
        at::CLEAR => clear_work(m, s, p, arg),
        at::LOAD => return load_work(m, s, irq, p, arg, k),
        at::STORE => return store_work(m, s, irq, p, arg, k),
        _ => Err(format!("queue entry with an unknown function {func:#010x}")),
    };
    match r {
        Ok(()) => k(m),
        Err(e) => fail(m, e),
    }
}

/// `sys:0x20f8`: queue `job`, or run it at once when nothing is queued,
/// DMA2 is idle and no DrawSync callback is set (or the queue is off).
/// `k` gets what's left in the queue, or -1 on a timeout.
fn enqueue(m: &mut M, s: Sys, job: Job, k: K) -> Flow<Psyq> {
    crate::path(m, &[(0x20F8, 0x2120)]);
    timeout_start(m, s);
    crate::path(m, &[(0x2124, 0x2128)]);
    enqueue_room(m, s, job, k)
}

/// The queue's room, at its loop's header (+2144h): while full, `if
/// (timeout) fail; pump();`.
fn enqueue_room(m: &mut M, s: Sys, job: Job, k: K) -> Flow<Psyq> {
    header(m, false, move |m| enqueue_look(m, s, job, k))
}

fn enqueue_look(m: &mut M, s: Sys, job: Job, k: K) -> Flow<Psyq> {
    let full = (qin(m, s) + 1) & (QUEUE_LEN - 1) == qout(m, s);
    let failed = |m: &mut M, k: K| {
        crate::path(m, &[(0x2134, 0x2138), (0x238C, 0x23A4)]);
        k(m, u32::MAX)
    };
    if full && busy(m) {
        let (before, after) = (len(&[(0x2144, 0x2160), (0x212C, 0x2130)]), len(&[(0x2134, 0x2140)]) + PUMP_BUSY);
        return spin_checks(m, s, before, after, Box::new(move |m, ok| if ok { enqueue_look(m, s, job, k) } else { failed(m, k) }));
    }
    crate::path(m, &[(0x2144, 0x2160)]);
    if full {
        crate::path(m, &[(0x212C, 0x2130)]);
        return iterate(m, s, 0, 0, 1, Box::new(move |m, ok| {
            if !ok {
                return failed(m, k);
            }
            crate::path(m, &[(0x2134, 0x2140)]);
            pump(m, s, false, Box::new(move |m, _| enqueue_room(m, s, job, k)))
        }));
    }
    crate::path(m, &[(0x2164, 0x2168)]);
    if let Err(e) = mask(m, s, data::MASK_QUEUE) {
        return fail(m, e);
    }
    crate::path(m, &[(0x216C, 0x2188)]);
    let g = s.genv();
    m.mem.w32(g + genv::PENDING, 1);
    let now = m.mem.r8(g + genv::QUEUE) == 0 || {
        crate::path(m, &[(0x218C, 0x21A4)]);
        queued(m, s) == 0 && {
            crate::path(m, &[(0x21A8, 0x21C4)]);
            !busy(m) && {
                crate::path(m, &[(0x21C8, 0x21D4)]);
                callback(m, s) == 0
            }
        }
    };
    if now {
        crate::path(m, &[(0x21D8, 0x21E0)]);
        // GPUSTAT's ready bit, at its loop's header (+21E4h): the GPU core
        // is between commands, ready at the first look.
        return header(m, false, move |m| {
            crate::path(m, &[(0x21E4, 0x21F4), (0x21F8, 0x2200)]);
            work(m, s, false, s.text + job.func, job.p, job.arg, Box::new(move |m| {
                crate::path(m, &[(0x2204, 0x2210)]);
                if let Err(e) = unmask(m, s, data::MASK_QUEUE) {
                    return fail(m, e);
                }
                crate::path(m, &[(0x2214, 0x2218), (0x238C, 0x23A4)]);
                k(m, 0)
            }))
        });
    }
    crate::path(m, &[(0x221C, 0x2228)]);
    Flow::Call {
        call: dma_callback(s, s.text + at::PUMP),
        then: Box::new(move |m, _| {
            crate::path(m, &[(0x222C, 0x2230)]);
            let i = qin(m, s);
            let e = s.que + i * ENTRY;
            let at = if job.len != 0 {
                for w in 0..job.len / 4 {
                    let v = m.mem.r32(job.p + 4 * w);
                    m.mem.w32(e + ENTRY_BUF + 4 * w, v);
                }
                e + ENTRY_BUF
            } else {
                job.p
            };
            let queue = move |m: &mut M| {
                crate::path(m, &[(0x22FC, 0x2364)]);
                m.mem.w32(e + 4, at);
                m.mem.w32(e + 8, job.arg);
                m.mem.w32(e, s.text + job.func);
                m.mem.w32(s.data + data::QIN, (i + 1) & (QUEUE_LEN - 1));
                if let Err(e) = unmask(m, s, data::MASK_QUEUE) {
                    return fail(m, e);
                }
                crate::path(m, &[(0x2368, 0x236C)]);
                pump(m, s, false, Box::new(move |m, _| {
                    crate::path(m, &[(0x2370, 0x2388), (0x238C, 0x23A4)]);
                    let n = queued(m, s);
                    k(m, n)
                }))
            };
            if job.len == 0 {
                crate::path(m, &[(0x22D8, 0x22F8)]);
                return queue(m);
            }
            // The copy, a word a pass from its header (+2244h), which
            // looks once more to leave.
            crate::path(m, &[(0x2234, 0x2240)]);
            let per = len(&[(0x2244, 0x2248), (0x2250, 0x2290)]) as u32;
            passes(m, false, per, job.len / 4, Box::new(move |m| {
                header(m, false, move |m| {
                    crate::path(m, &[(0x2244, 0x2248), (0x2250, 0x225C), (0x2294, 0x22D4)]);
                    queue(m)
                })
            }))
        }),
    }
}

/// The pump (`sys:0x23a8`): while DMA2 is idle, run queued work in order
/// (taking `DMACallback(2, 0)` off before the last when no DrawSync
/// callback is set); when the queue has drained and work was queued since,
/// call the DrawSync callback with its own address. `k` gets what's left
/// queued, or 1 when DMA2 was busy. `irq`: run as DMA2's callback, in the
/// exception.
fn pump(m: &mut M, s: Sys, irq: bool, k: K) -> Flow<Psyq> {
    crate::path(m, &[(0x23A8, 0x23D0)]);
    if busy(m) {
        crate::path(m, &[(0x25F4, 0x2604)]);
        return k(m, 1);
    }
    crate::path(m, &[(0x23D4, 0x23D8)]);
    if let Err(e) = mask(m, s, data::MASK_PUMP) {
        return fail(m, e);
    }
    crate::path(m, &[(0x23DC, 0x23F4)]);
    if queued(m, s) == 0 {
        return pump_drained(m, s, k);
    }
    crate::path(m, &[(0x23F8, 0x2414)]);
    if busy(m) {
        return pump_drained(m, s, k);
    }
    crate::path(m, &[(0x2418, 0x241C)]);
    pump_next(m, s, irq, k)
}

/// The pump's loop, at its header (+2420h): the next entry's work.
fn pump_next(m: &mut M, s: Sys, irq: bool, k: K) -> Flow<Psyq> {
    header(m, irq, move |m| {
        crate::path(m, &[(0x2420, 0x243C)]);
        let last = (qout(m, s) + 1) & (QUEUE_LEN - 1) == qin(m, s) && {
            crate::path(m, &[(0x2440, 0x2450)]);
            callback(m, s) == 0
        };
        if !last {
            return pump_work(m, s, irq, k);
        }
        crate::path(m, &[(0x2454, 0x2458)]);
        Flow::Call { call: dma_callback(s, 0), then: Box::new(move |m, _| pump_work(m, s, irq, k)) }
    })
}

fn pump_work(m: &mut M, s: Sys, irq: bool, k: K) -> Flow<Psyq> {
    // GPUSTAT's ready bit: the GPU core is between commands.
    crate::path(m, &[(0x245C, 0x2478), (0x2490, 0x24FC)]);
    let o = qout(m, s);
    let e = s.que + o * ENTRY;
    let (func, p, arg) = (m.mem.r32(e), m.mem.r32(e + 4), m.mem.r32(e + 8));
    work(m, s, irq, func, p, arg, Box::new(move |m| {
        crate::path(m, &[(0x2500, 0x2534)]);
        m.mem.w32(s.data + data::QOUT, (o + 1) & (QUEUE_LEN - 1));
        if queued(m, s) == 0 {
            return pump_drained(m, s, k);
        }
        crate::path(m, &[(0x2538, 0x2554)]);
        if busy(m) {
            return pump_drained(m, s, k);
        }
        pump_next(m, s, irq, k)
    }))
}

/// The pump past its loop (+2558h): I_MASK back, and the DrawSync
/// callback when the queue drained.
fn pump_drained(m: &mut M, s: Sys, k: K) -> Flow<Psyq> {
    crate::path(m, &[(0x2558, 0x2564)]);
    if let Err(e) = unmask(m, s, data::MASK_PUMP) {
        return fail(m, e);
    }
    crate::path(m, &[(0x2568, 0x2580)]);
    let g = s.genv();
    let cb = callback(m, s);
    let call = queued(m, s) == 0 && {
        crate::path(m, &[(0x2584, 0x25A0)]);
        !busy(m) && {
            crate::path(m, &[(0x25A4, 0x25B8)]);
            m.mem.r32(g + genv::PENDING) != 0 && {
                crate::path(m, &[(0x25BC, 0x25C8)]);
                cb != 0
            }
        }
    };
    let done = move |m: &mut M| {
        crate::path(m, &[(0x25D8, 0x25F0), (0x25F4, 0x2604)]);
        let n = queued(m, s);
        k(m, n)
    };
    if !call {
        return done(m);
    }
    crate::path(m, &[(0x25CC, 0x25D4)]);
    m.mem.w32(g + genv::PENDING, 0);
    Flow::Call { call: GuestCall { addr: cb, args: [cb, 0, 0, 0] }, then: Box::new(move |m, _| done(m)) }
}

/// The pump as DMA2's completion callback (`DMACallback(2, …)`).
fn pump_callback(m: &mut M, _a: [u32; 4]) -> Result {
    let s = sys(m, at::PUMP)?;
    Ok(pump(m, s, true, k_ret()))
}

/// `sys:0x2744`: `DrawSync(mode)`.
fn sync(m: &mut M, s: Sys, mode: u32, k: K) -> Flow<Psyq> {
    crate::path(m, &[(0x2744, 0x2750)]);
    if mode != 0 {
        crate::path(m, &[(0x27F8, 0x2818)]);
        let n = queued(m, s);
        let then = move |m: &mut M, _| {
            crate::path(m, &[(0x2824, 0x2840)]);
            let idle = !busy(m) && {
                crate::path(m, &[(0x2844, 0x2860)]);
                ready(m)
            };
            if !idle {
                crate::path(m, &[(0x2864, 0x2868)]);
                if n == 0 {
                    crate::path(m, &[(0x286C, 0x286C)]);
                }
            }
            crate::path(m, &[(0x2870, 0x287C)]);
            k(m, if n != 0 || idle { n } else { 1 })
        };
        if n == 0 {
            return then(m, 0);
        }
        crate::path(m, &[(0x281C, 0x2820)]);
        return pump(m, s, false, Box::new(then));
    }
    crate::path(m, &[(0x2754, 0x2758)]);
    timeout_start(m, s);
    crate::path(m, &[(0x275C, 0x2760)]);
    sync_queue(m, s, k)
}

/// `DrawSync(0)`'s first loop, at its header (+277Ch): `while (queued) {
/// pump(); if (timeout) fail; }`.
fn sync_queue(m: &mut M, s: Sys, k: K) -> Flow<Psyq> {
    header(m, false, move |m| sync_queue_look(m, s, k))
}

fn sync_queue_look(m: &mut M, s: Sys, k: K) -> Flow<Psyq> {
    let failed = |m: &mut M, k: K| {
        crate::path(m, &[(0x2774, 0x2778), (0x2870, 0x287C)]);
        k(m, u32::MAX)
    };
    if queued(m, s) == 0 {
        crate::path(m, &[(0x277C, 0x2794)]);
        return sync_idle(m, s, k);
    }
    let lead = len(&[(0x277C, 0x2794), (0x2798, 0x279C), (0x2764, 0x2768)]);
    if busy(m) {
        let before = lead + PUMP_BUSY + len(&[(0x276C, 0x2770)]);
        return spin_checks(m, s, before, 2, Box::new(move |m, ok| if ok { sync_queue_look(m, s, k) } else { failed(m, k) }));
    }
    crate::take(m, lead);
    pump(m, s, false, Box::new(move |m, _| {
        crate::path(m, &[(0x276C, 0x2770)]);
        iterate(m, s, 0, 0, 1, Box::new(move |m, ok| {
            if !ok {
                return failed(m, k);
            }
            crate::path(m, &[(0x2774, 0x2778)]);
            sync_queue(m, s, k)
        }))
    }))
}

/// `DrawSync(0)`'s second loop, at its header (+27B0h): until DMA2 is idle
/// and the GPU ready, `if (timeout) fail;`.
fn sync_idle(m: &mut M, s: Sys, k: K) -> Flow<Psyq> {
    header(m, false, move |m| sync_idle_look(m, s, k))
}

fn sync_idle_look(m: &mut M, s: Sys, k: K) -> Flow<Psyq> {
    let before = if busy(m) {
        len(&[(0x27B0, 0x27CC), (0x27A0, 0x27A4)])
    } else if !ready(m) {
        len(&[(0x27B0, 0x27CC), (0x27D0, 0x27EC), (0x27A0, 0x27A4)])
    } else {
        crate::path(m, &[(0x27B0, 0x27CC), (0x27D0, 0x27EC), (0x27F0, 0x27F4), (0x2870, 0x287C)]);
        return k(m, 0);
    };
    spin_checks(m, s, before, 2, Box::new(move |m, ok| {
        if ok {
            return sync_idle_look(m, s, k);
        }
        crate::path(m, &[(0x27A8, 0x27AC), (0x2870, 0x287C)]);
        k(m, u32::MAX)
    }))
}

// --- The work functions --------------------------------------------------

/// Where a RECT's width or height fell against its limit.
#[derive(Clone, Copy, PartialEq)]
enum Fit {
    Neg,
    In,
    Over,
}

/// A RECT's width and height clamped, as the image functions do: below 0
/// to 0, past `limit(VRAM)` to it; written back. Where each fell.
fn clamp_rect(m: &mut M, s: Sys, r: u32, less: i32) -> [Fit; 2] {
    let g = s.genv();
    [(rect::W, genv::WIDTH), (rect::H, genv::HEIGHT)].map(|(field, size)| {
        let v = r16s(m, r + field);
        let lim = r16s(m, g + size) - less;
        let (fit, c) = if v < 0 {
            (Fit::Neg, 0)
        } else if lim < v {
            (Fit::Over, (m.mem.r16(g + size) as i32 - less) as u16)
        } else {
            (Fit::In, v as u16)
        };
        m.mem.w16(r + field, c);
        fit
    })
}

/// ClearImage's work (`sys:0x1914`): a fill of `r` with `color` (bit 31:
/// draw to the display area too), as a packet in libgpu's `.bss`: a VRAM
/// fill when the rectangle is 64-pixel aligned, else a rectangle with the
/// draw area, offset and mask cleared, then restored from the GPU.
fn clear_work(m: &mut M, s: Sys, r: u32, color: u32) -> std::result::Result<(), String> {
    let [w, h] = clamp_rect(m, s, r, 1);
    crate::path(m, &[(0x1914, 0x1938)]);
    match w {
        Fit::Neg => crate::path(m, &[(0x1968, 0x1968)]),
        Fit::Over => crate::path(m, &[(0x193C, 0x195C)]),
        Fit::In => crate::path(m, &[(0x193C, 0x195C), (0x1960, 0x1964)]),
    }
    crate::path(m, &[(0x196C, 0x1978)]);
    match h {
        Fit::Neg => crate::path(m, &[(0x19A8, 0x19A8)]),
        Fit::Over => crate::path(m, &[(0x197C, 0x199C)]),
        Fit::In => crate::path(m, &[(0x197C, 0x199C), (0x19A0, 0x19A4)]),
    }
    let p = s.bss;
    let x = m.mem.r16(r + rect::X) as u32;
    let w = m.mem.r16(r + rect::W) as u32;
    let mode = |m: &M| 0xE100_0000 | (color >> 31) << 10 | (gpustat(m) & 0x7FF);
    crate::path(m, &[(0x19AC, 0x19BC)]);
    let aligned = x & 0x3F == 0 && {
        crate::path(m, &[(0x19C0, 0x19D0)]);
        w & 0x3F == 0
    };
    if !aligned {
        crate::path(m, &[(0x19D4, 0x1A78)]);
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
        for (i, cmd, after) in [(3u32, 0xE300_0000u32, (0x1A7C, 0x1A88)), (4, 0xE400_0000, (0x1A8C, 0x1A9C)), (5, 0xE500_0000, (0x1AA0, 0x1AA8))] {
            // GP1(10h) through `sys:0x20a4`.
            crate::path(m, &[(0x20A4, 0x20D0)]);
            let v = gpu_info(m, i) | cmd;
            m.mem.w32(q + 4 * (i - 2), v);
            crate::path(m, &[after]);
        }
    } else {
        crate::path(m, &[(0x1AAC, 0x1B18)]);
        m.mem.w32(p, 0x05FF_FFFF);
        m.mem.w32(p + 4, 0xE600_0000);
        m.mem.w32(p + 12, (color & 0xFF_FFFF) | 0x0200_0000);
        let e1 = mode(m);
        m.mem.w32(p + 8, e1);
        let (xy, wh) = (m.mem.r32(r), m.mem.r32(r + 4));
        m.mem.w32(p + 16, xy);
        m.mem.w32(p + 20, wh);
    }
    crate::path(m, &[(0x1B1C, 0x1B24)]);
    exec_ot(m, p)?;
    crate::path(m, &[(0x1B28, 0x1B40)]);
    Ok(())
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

/// An image transfer's start: the timeout, the RECT clamped (each work
/// function's blocks at `at`'s offsets for neither, either or both
/// limits), and its size; `None` (its blocks spent) for an empty one.
/// `at`: the blocks after the width's test, its limit's, its clamp, the
/// height's test, its limit's, its clamp's two, the size's, and the empty
/// return's.
fn image_start(m: &mut M, s: Sys, r: u32, at: [(u32, u32); 9]) -> Option<(u32, u32)> {
    timeout_start(m, s);
    let [w, h] = clamp_rect(m, s, r, 0);
    crate::path(m, &[at[0]]);
    match w {
        Fit::Neg => crate::path(m, &[at[1]]),
        Fit::In => crate::path(m, &[at[2]]),
        Fit::Over => crate::path(m, &[at[2], at[3]]),
    }
    crate::path(m, &[at[4]]);
    match h {
        Fit::Neg => crate::path(m, &[at[5]]),
        Fit::In => crate::path(m, &[at[6]]),
        Fit::Over => crate::path(m, &[at[6], at[7], (at[5].1, at[5].1)]),
    }
    crate::path(m, &[at[8]]);
    image_words(m, r)
}

/// LoadImage's work (`sys:0x1b44`): CPU→VRAM. The header and the words
/// that don't fill a block through the GP0 port, a word a pass of its loop
/// (+1CF0h), the blocks by DMA2.
fn load_work(m: &mut M, s: Sys, irq: bool, r: u32, mut p: u32, k: Then) -> Flow<Psyq> {
    crate::path(m, &[(0x1B44, 0x1B6C)]);
    #[rustfmt::skip]
    let blocks = [(0x1B70, 0x1B7C), (0x1BA8, 0x1BA8), (0x1B80, 0x1B9C), (0x1BA0, 0x1BA4), (0x1BAC, 0x1BB8),
        (0x1BE4, 0x1BE8), (0x1BBC, 0x1BD8), (0x1BDC, 0x1BE0), (0x1BEC, 0x1C14)];
    let Some((words, blocks)) = image_start(m, s, r, blocks) else {
        crate::path(m, &[(0x1C18, 0x1C1C), (0x1D5C, 0x1D7C)]);
        return k(m);
    };
    // GPUSTAT's ready bit: the GPU core is between commands.
    crate::path(m, &[(0x1C20, 0x1C48), (0x1C80, 0x1CAC), (0x1CB4, 0x1CE8)]);
    gp1(m, 0x0400_0000);
    m.gp0(0x0100_0000);
    m.gp0(0xA000_0000);
    let (xy, wh) = (m.mem.r32(r), m.mem.r32(r + 4));
    m.gp0(xy);
    m.gp0(wh);
    let n = words - blocks * 16;
    for _ in 0..n {
        let w = m.mem.r32(p);
        m.gp0(w);
        p = p.wrapping_add(4);
    }
    if n != 0 {
        crate::path(m, &[(0x1CEC, 0x1CEC)]);
    }
    let per = len(&[(0x1CF0, 0x1D08)]) as u32;
    passes(m, irq, per, n, Box::new(move |m| {
        crate::path(m, &[(0x1D0C, 0x1D10)]);
        if blocks != 0 {
            crate::path(m, &[(0x1D14, 0x1D54)]);
            gp1(m, 0x0400_0002);
            let r = set_reg(m, MADR2, 4, p).and_then(|_| set_reg(m, BCR2, 4, blocks << 16 | 0x10)).and_then(|_| set_reg(m, CHCR2, 4, 0x0100_0201));
            if let Err(e) = r {
                return fail(m, e);
            }
        }
        crate::path(m, &[(0x1D58, 0x1D58), (0x1D5C, 0x1D7C)]);
        k(m)
    }))
}

/// StoreImage's work (`sys:0x1d80`): VRAM→CPU, the same way round (its
/// loop +1F70h).
fn store_work(m: &mut M, s: Sys, irq: bool, r: u32, mut p: u32, k: Then) -> Flow<Psyq> {
    crate::path(m, &[(0x1D80, 0x1DA4)]);
    #[rustfmt::skip]
    let blocks = [(0x1DA8, 0x1DB4), (0x1DDC, 0x1DDC), (0x1DB8, 0x1DD0), (0x1DD4, 0x1DD8), (0x1DE0, 0x1DEC),
        (0x1E18, 0x1E1C), (0x1DF0, 0x1E0C), (0x1E10, 0x1E14), (0x1E20, 0x1E48)];
    let Some((words, blocks)) = image_start(m, s, r, blocks) else {
        crate::path(m, &[(0x1E4C, 0x1E50), (0x1FE0, 0x1FFC)]);
        return k(m);
    };
    // GPUSTAT's ready bits: the GPU core is between commands, and has the
    // pixels at once.
    crate::path(m, &[(0x1E54, 0x1E7C), (0x1EB4, 0x1F28), (0x1F60, 0x1F6C)]);
    gp1(m, 0x0400_0000);
    m.gp0(0x0100_0000);
    m.gp0(0xC000_0000);
    let (xy, wh) = (m.mem.r32(r), m.mem.r32(r + 4));
    m.gp0(xy);
    m.gp0(wh);
    let n = words - blocks * 16;
    for _ in 0..n {
        let w = gpuread(m);
        m.mem.w32(p, w);
        p = p.wrapping_add(4);
    }
    let per = len(&[(0x1F70, 0x1F8C)]) as u32;
    passes(m, irq, per, n, Box::new(move |m| {
        crate::path(m, &[(0x1F90, 0x1F94)]);
        if blocks != 0 {
            crate::path(m, &[(0x1F98, 0x1FD8)]);
            gp1(m, 0x0400_0003);
            let r = set_reg(m, MADR2, 4, p).and_then(|_| set_reg(m, BCR2, 4, blocks << 16 | 0x10)).and_then(|_| set_reg(m, CHCR2, 4, 0x0100_0200));
            if let Err(e) = r {
                return fail(m, e);
            }
        }
        crate::path(m, &[(0x1FDC, 0x1FDC), (0x1FE0, 0x1FFC)]);
        k(m)
    }))
}

/// `sys:0x2608`: the queue emptied and, by `mode`, the GPU reset (0, 5:
/// GP1(0) and the queue cleared; 1, 3: its interrupt acknowledged and
/// command buffer reset), DMA2 stopped and enabled; for 0, the GPU's
/// type (`sys:0x29f8`). Then `k` with it.
fn reset(m: &mut M, s: Sys, mode: u32, k: K) -> Flow<Psyq> {
    crate::path(m, &[(0x2608, 0x261C)]);
    if let Err(e) = mask(m, s, data::MASK_RESET) {
        return fail(m, e);
    }
    crate::path(m, &[(0x2620, 0x264C)]);
    m.mem.w32(s.data + data::QOUT, 0);
    m.mem.w32(s.data + data::QIN, 0);
    let m7 = mode & 7;
    match m7 {
        1 => {}
        0 => crate::path(m, &[(0x2650, 0x2654), (0x2658, 0x265C)]),
        3 => crate::path(m, &[(0x2650, 0x2654), (0x2668, 0x266C)]),
        _ => crate::path(m, &[(0x2650, 0x2654), (0x2668, 0x266C), (0x2670, 0x2674)]),
    }
    let r = (|| -> std::result::Result<(), String> {
        if matches!(m7, 0 | 1 | 3 | 5) {
            set_reg(m, CHCR2, 4, 0x401)?;
            let d = reg(m, DPCR, 4)?;
            set_reg(m, DPCR, 4, d | 0x800)?;
        }
        Ok(())
    })();
    if let Err(e) = r {
        return fail(m, e);
    }
    let rest: Then = Box::new(move |m| {
        crate::path(m, &[(0x2710, 0x271C)]);
        if let Err(e) = unmask(m, s, data::MASK_RESET) {
            return fail(m, e);
        }
        crate::path(m, &[(0x2720, 0x2728)]);
        if m7 != 0 {
            crate::path(m, &[(0x2734, 0x2740)]);
            return k(m, 0);
        }
        crate::path(m, &[(0x272C, 0x2730)]);
        let t = gpu_type(m, mode);
        crate::path(m, &[(0x2734, 0x2740)]);
        k(m, t)
    });
    match m7 {
        0 | 5 => {
            crate::path(m, &[(0x2678, 0x26B8)]);
            gp1(m, 0);
            memset(m, s.que, 0, QUEUE_LEN * ENTRY, Box::new(move |m| {
                crate::path(m, &[(0x26BC, 0x26C0)]);
                rest(m)
            }))
        }
        1 | 3 => {
            crate::path(m, &[(0x26C4, 0x270C)]);
            gp1(m, 0x0200_0000);
            gp1(m, 0x0100_0000);
            rest(m)
        }
        _ => rest(m),
    }
}

/// `sys:0x29f8`: the GPU's version (GP1(10h) 7): an old GPU gets a draw
/// mode and a read, type 0; a new one is type 1, or 2 with mode bit 3
/// (GP1(09h)).
fn gpu_type(m: &mut M, mode: u32) -> u32 {
    crate::path(m, &[(0x29F8, 0x2A2C)]);
    if gpu_info(m, 7) != 2 {
        crate::path(m, &[(0x2A30, 0x2A64), (0x2A90, 0x2A94)]);
        let v = 0xE100_1000 | (gpustat(m) & 0x3FFF);
        m.gp0(v);
        gpuread(m);
        return 0;
    }
    crate::path(m, &[(0x2A68, 0x2A70)]);
    if mode & 8 == 0 {
        crate::path(m, &[(0x2A8C, 0x2A94)]);
        return 1;
    }
    crate::path(m, &[(0x2A74, 0x2A88), (0x2A90, 0x2A94)]);
    gp1(m, 0x0900_0001);
    2
}

// --- Draw environments ----------------------------------------------------

/// Where a signed 16-bit `v` falls against `lim`.
fn fit(v: i32, lim: i32) -> Fit {
    if v < 0 {
        Fit::Neg
    } else if lim < v {
        Fit::Over
    } else {
        Fit::In
    }
}

/// The draw area corner packets (`sys:0x1650` with `cmd` E3h, its twin
/// `sys:0x16e8` with E4h, `at`): `x`, `y` clamped to VRAM.
fn area(m: &mut M, s: Sys, at: u32, cmd: u32, x: u32, y: u32) -> u32 {
    let g = s.genv();
    let clamp = |m: &M, v: u32, size: u32| {
        let lim = r16s(m, g + size) - 1;
        match fit(v as u16 as i16 as i32, lim) {
            Fit::Neg => (Fit::Neg, 0),
            Fit::Over => (Fit::Over, (m.mem.r16(g + size) as u32).wrapping_sub(1)),
            Fit::In => (Fit::In, v),
        }
    };
    let (fx, x) = clamp(m, x, genv::WIDTH);
    let (fy, y) = clamp(m, y, genv::HEIGHT);
    crate::path(m, &[(at, at + 0xC)]);
    if fx != Fit::Neg {
        crate::path(m, &[(at + 0x10, at + 0x30)]);
    }
    if fx == Fit::In {
        crate::path(m, &[(at + 0x34, at + 0x34)]);
    }
    crate::path(m, &[(at + 0x38, at + 0x48)]);
    match fy {
        Fit::Neg => crate::path(m, &[(at + 0x78, at + 0x94)]),
        Fit::Over => crate::path(m, &[(at + 0x4C, at + 0x74), (at + 0x7C, at + 0x94)]),
        Fit::In => crate::path(m, &[(at + 0x4C, at + 0x6C), (at + 0x80, at + 0x94)]),
    }
    cmd | (x & 0x3FF) | (y & 0x3FF) << 10
}

/// `sys:0x13c0`: a DRAWENV's DR_ENV packet at `p` (its length set, its
/// link left): draw area, offset, mode, texture window, mask, and with
/// `isbg` a fill of the clip rectangle (a VRAM fill when 64-pixel
/// aligned, else a rectangle relative to the offset).
fn dr_env(m: &mut M, s: Sys, p: u32, e: u32) {
    let h = |m: &M, o: u32| m.mem.r16(e + o) as u32;
    let (cx, cy, cw, ch) = (h(m, drawenv::CLIP), h(m, drawenv::CLIP + 2), h(m, drawenv::CLIP + 4), h(m, drawenv::CLIP + 6));
    let (ox, oy) = (h(m, drawenv::OFS), h(m, drawenv::OFS + 2));
    crate::path(m, &[(0x13C0, 0x13E4)]);
    let mut code = vec![area(m, s, 0x1650, 0xE300_0000, cx, cy)];
    crate::path(m, &[(0x13E8, 0x141C)]);
    code.push(area(m, s, 0x16E8, 0xE400_0000, (cw + cx).wrapping_sub(1), (cy + ch).wrapping_sub(1)));
    crate::path(m, &[(0x1420, 0x1430), (0x1780, 0x1798)]);
    code.push(0xE500_0000 | (oy & 0x7FF) << 11 | (ox & 0x7FF));
    let (dtd, dfe) = (m.mem.r8(e + drawenv::DTD), m.mem.r8(e + drawenv::DFE));
    let tpage = h(m, drawenv::TPAGE);
    crate::path(m, &[(0x1434, 0x1448), (0x1630, 0x1634), (0x163C, 0x1640), (0x1648, 0x164C)]);
    crate::take(m, (dtd != 0) as u64 + (dfe != 0) as u64);
    code.push(0xE100_0000 | if dtd != 0 { 0x200 } else { 0 } | if dfe != 0 { 0x400 } else { 0 } | (tpage & 0x9FF));
    let tw = |o: u32| h(m, drawenv::TW + o) as i16 as i32;
    let (tx, ty, tww, twh) = (tw(0), tw(2), tw(4), tw(6));
    let field = |v: i32| ((v & 0xFF) >> 3) as u32;
    crate::path(m, &[(0x144C, 0x1454), (0x179C, 0x17A0), (0x17AC, 0x1818)]);
    code.push(0xE200_0000 | field(ty) << 15 | field(tx) << 10 | field(-twh) << 5 | field(-tww));
    code.push(0xE600_0000);
    for (i, c) in code.iter().enumerate() {
        m.mem.w32(p + 4 + 4 * i as u32, *c);
    }
    crate::path(m, &[(0x1458, 0x1470)]);
    let mut len = 7;
    if m.mem.r8(e + drawenv::ISBG) != 0 {
        let g = s.genv();
        let lim = |m: &M, size: u32| r16s(m, g + size) - 1;
        let clampv = |v: u32, lim: i32| match fit(v as u16 as i16 as i32, lim) {
            Fit::Neg => (Fit::Neg, 0),
            Fit::Over => (Fit::Over, lim as u32 & 0xFFFF),
            Fit::In => (Fit::In, v as u16 as u32),
        };
        let (mut rx, mut ry) = (cx, cy);
        let (fw, rw) = clampv(cw, lim(m, genv::WIDTH));
        let (fh, rh) = clampv(ch, lim(m, genv::HEIGHT));
        crate::path(m, &[(0x1474, 0x14B0)]);
        if fw != Fit::Neg {
            crate::path(m, &[(0x14B4, 0x14D4)]);
        }
        if fw == Fit::In {
            crate::path(m, &[(0x14D8, 0x14D8)]);
        }
        crate::path(m, &[(0x14DC, 0x14E8)]);
        match fh {
            Fit::Neg => crate::path(m, &[(0x1518, 0x1518)]),
            Fit::Over => crate::path(m, &[(0x14EC, 0x150C)]),
            Fit::In => crate::path(m, &[(0x14EC, 0x150C), (0x1510, 0x1514)]),
        }
        let rgb = (m.mem.r8(e + drawenv::B0) as u32) << 16 | (m.mem.r8(e + drawenv::G0) as u32) << 8 | m.mem.r8(e + drawenv::R0) as u32;
        crate::path(m, &[(0x151C, 0x152C)]);
        let aligned = rx & 0x3F == 0 && {
            crate::path(m, &[(0x1530, 0x1540)]);
            rw & 0x3F == 0
        };
        let cmd = if !aligned {
            crate::path(m, &[(0x1544, 0x15B8)]);
            rx = rx.wrapping_sub(ox);
            ry = ry.wrapping_sub(oy);
            0x6000_0000
        } else {
            crate::path(m, &[(0x15BC, 0x1610)]);
            0x0200_0000
        };
        m.mem.w32(p + 4 * len, cmd | rgb);
        m.mem.w32(p + 4 * (len + 1), (rx & 0xFFFF) | (ry & 0xFFFF) << 16);
        m.mem.w32(p + 4 * (len + 2), (rw & 0xFFFF) | (rh & 0xFFFF) << 16);
        len += 3;
    }
    crate::path(m, &[(0x1614, 0x162C)]);
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
    let m7 = mode & 7;
    crate::path(m, &[(0x0, 0x1C)]);
    let full = m7 == 3 || {
        crate::path(m, &[(0x20, 0x28)]);
        if m7 < 4 {
            crate::path(m, &[(0x2C, 0x30)]);
            m7 == 0
        } else {
            crate::path(m, &[(0x3C, 0x40)]);
            m7 == 5
        }
    };
    if !full {
        // The table's reset, mode 1 (+34h or +44h, the same length).
        crate::path(m, &[(0x34, 0x38), (0x110, 0x124), (0x144, 0x15C)]);
        return Ok(reset(m, s, 1, Box::new(|m, v| {
            crate::path(m, &[(0x160, 0x170)]);
            Flow::ret(v)
        })));
    }
    let rest = move |m: &mut M| {
        crate::path(m, &[(0x68, 0x7C)]);
        memset(m, g, 0, genv::SIZE, Box::new(move |m| reset_graph_rest(m, s, mode)))
    };
    if m7 == 5 {
        return Ok(rest(m));
    }
    // "ResetGraph:jtb=%08x,env=%08x\n" with the addresses of the table and
    // GEnv.
    crate::path(m, &[(0x4C, 0x64)]);
    let t = s.text + at::RESET_GRAPH;
    Ok(crate::say(m, (t + 0x4C, t + 0x50, t + 0x60), &[s.data + 8, g], rest))
}

/// `ResetGraph` from its `ResetCallback` on.
fn reset_graph_rest(m: &mut M, s: Sys, mode: u32) -> Flow<Psyq> {
    let g = s.genv();
    crate::path(m, &[(0x80, 0x84)]);
    Flow::Call {
        call: GuestCall { addr: s.reset_callback, args: [0; 4] },
        then: Box::new(move |m, _| {
            crate::path(m, &[(0x88, 0x9C)]);
            crate::take(m, crate::VECTOR_STUB);
            let table = m.mem.r32(s.data + data::TABLE);
            if let Err(e) = psx_bios::bios::call(m, 0xA0, 0x49, [table & 0xFF_FFFF, 0, 0, 0]) {
                return fail(m, e);
            }
            crate::path(m, &[(0xA0, 0xA4)]);
            reset(m, s, mode, Box::new(move |m, t| {
                crate::path(m, &[(0xA8, 0xF0)]);
                let t = t & 0xFF;
                m.mem.w8(g + genv::TYPE, t as u8);
                m.mem.w8(g + genv::QUEUE, 1);
                let w = m.mem.r16(s.data + data::WIDTHS + 4 * t);
                m.mem.w16(g + genv::WIDTH, w);
                let h = m.mem.r16(s.data + data::HEIGHTS + 4 * t);
                m.mem.w16(g + genv::HEIGHT, h);
                memset(m, g + genv::DRAW, 0xFF, drawenv::SIZEOF, Box::new(move |m| {
                    crate::path(m, &[(0xF4, 0x100)]);
                    memset(m, g + genv::DISP, 0xFF, dispenv::SIZEOF, Box::new(move |m| {
                        crate::path(m, &[(0x104, 0x10C), (0x160, 0x170)]);
                        Flow::ret(t)
                    }))
                }))
            }))
        }),
    }
}

/// `SetDispMask(mask)`: GP1(03h); with 0 the DISPENV copy invalid. Its
/// result is the GP1 register's address, which libgpu's GP1 writer leaves.
fn set_disp_mask(m: &mut M, a: [u32; 4]) -> Result {
    let s = sys(m, at::SET_DISP_MASK)?;
    let on = a[0] != 0;
    crate::path(m, &[(0x2E4, 0x30C), (0x32C, 0x330)]);
    let then = move |m: &mut M| {
        crate::path(m, &[(0x340, 0x350)]);
        if on {
            crate::path(m, &[(0x354, 0x354)]);
        }
        crate::path(m, &[(0x358, 0x364)]);
        gp1_call(m, if on { 0x0300_0000 } else { 0x0300_0001 });
        crate::path(m, &[(0x368, 0x378)]);
        Flow::ret(m.mem.r32(s.data + data::GP1_PTR))
    };
    if on {
        return Ok(then(m));
    }
    crate::path(m, &[(0x334, 0x33C)]);
    Ok(memset(m, s.genv() + genv::DISP, 0xFF, dispenv::SIZEOF, Box::new(then)))
}

/// `DrawSync(mode)`: 0 waits for the queue and the GPU (-1 on libgpu's
/// timeout); otherwise what's queued, or whether the GPU is busy. Through
/// libgpu's table.
fn draw_sync(m: &mut M, a: [u32; 4]) -> Result {
    let s = sys(m, at::DRAW_SYNC)?;
    crate::path(m, &[(0x37C, 0x398), (0x3B8, 0x3D0)]);
    Ok(sync(m, s, a[0], Box::new(|m, v| {
        crate::path(m, &[(0x3D4, 0x3E0)]);
        Flow::ret(v)
    })))
}

/// The image functions' tail: queue `job` and return what `enqueue` does,
/// through their last block, `last`.
fn image(m: &mut M, s: Sys, job: Job, last: (u32, u32)) -> Result {
    Ok(enqueue(m, s, job, Box::new(move |m, v| {
        crate::path(m, &[last]);
        Flow::ret(v)
    })))
}

/// `ClearImage(rect, r, g, b)`.
fn clear_image(m: &mut M, a: [u32; 4]) -> Result {
    let s = sys(m, at::CLEAR_IMAGE)?;
    let color = (a[3] & 0xFF) << 16 | (a[2] & 0xFF) << 8 | (a[1] & 0xFF);
    crate::path(m, &[(0x500, 0x534)]);
    check_rect(m);
    crate::path(m, &[(0x538, 0x570)]);
    image(m, s, Job { func: at::CLEAR, p: a[0], len: 8, arg: color }, (0x574, 0x58C))
}

/// `LoadImage(rect, p)`.
fn load_image(m: &mut M, a: [u32; 4]) -> Result {
    let s = sys(m, at::LOAD_IMAGE)?;
    crate::path(m, &[(0x628, 0x64C)]);
    check_rect(m);
    crate::path(m, &[(0x650, 0x670)]);
    image(m, s, Job { func: at::LOAD, p: a[0], len: 8, arg: a[1] }, (0x674, 0x684))
}

/// `StoreImage(rect, p)`.
fn store_image(m: &mut M, a: [u32; 4]) -> Result {
    let s = sys(m, at::STORE_IMAGE)?;
    crate::path(m, &[(0x688, 0x6AC)]);
    check_rect(m);
    crate::path(m, &[(0x6B0, 0x6D0)]);
    image(m, s, Job { func: at::STORE, p: a[0], len: 8, arg: a[1] }, (0x6D4, 0x6E4))
}

/// MoveImage's packet in libgpu's data: source, destination, size; false
/// for an empty rectangle, after the blocks that test it (`at`: the
/// width's test, the height's, the height's empty return; offsets in
/// `MoveImage` and `MoveImage2`).
fn move_packet(m: &mut M, s: Sys, r: u32, x: u32, y: u32, at: [(u32, u32); 3]) -> bool {
    crate::path(m, &[at[0]]);
    if m.mem.r16(r + rect::W) == 0 {
        return false;
    }
    crate::path(m, &[at[1]]);
    if m.mem.r16(r + rect::H) == 0 {
        crate::path(m, &[at[2]]);
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
    crate::path(m, &[(0x6E8, 0x714)]);
    check_rect(m);
    if !move_packet(m, s, a[0], a[1], a[2], [(0x718, 0x724), (0x728, 0x734), (0x738, 0x73C)]) {
        crate::path(m, &[(0x788, 0x79C)]);
        return ret(u32::MAX);
    }
    crate::path(m, &[(0x740, 0x784)]);
    image(m, s, Job { func: at::EXEC, p: s.data + data::MOVE, len: 0x14, arg: 0 }, (0x788, 0x79C))
}

/// `MoveImage2(rect, x, y)`: past the queue. Waits for the GPU (-1 on the
/// timeout), makes `_GPU_ResetCallback` DMA2's callback, and copies.
fn move_image2(m: &mut M, a: [u32; 4]) -> Result {
    let s = sys(m, at::MOVE_IMAGE2)?;
    let (r, x, y) = (a[0], a[1], a[2]);
    crate::path(m, &[(0x2C70, 0x2C9C)]);
    check_rect(m);
    // The timeout starts, inline.
    crate::path(m, &[(0x2CA0, 0x2CA4)]);
    crate::take(m, VSYNC_VCOUNT);
    crate::path(m, &[(0x2CA8, 0x2CCC)]);
    deadline(m, s);
    let go = move |m: &mut M| {
        crate::path(m, &[(0x2D20, 0x2D2C)]);
        Flow::Call {
            call: dma_callback(s, s.text + at::GPU_RESET_CALLBACK),
            then: Box::new(move |m, _| {
                if !move_packet(m, s, r, x, y, [(0x2D30, 0x2D3C), (0x2D40, 0x2D4C), (0x2D50, 0x2D54)]) {
                    crate::path(m, &[(0x2D9C, 0x2DB0)]);
                    return Flow::ret(u32::MAX);
                }
                crate::path(m, &[(0x2D58, 0x2D94)]);
                if let Err(e) = exec_ot(m, s.data + data::MOVE) {
                    return fail(m, e);
                }
                crate::path(m, &[(0x2D98, 0x2D98), (0x2D9C, 0x2DB0)]);
                Flow::ret(0)
            }),
        }
    };
    Ok(header(m, false, move |m| move_image2_look(m, s, Box::new(go))))
}

/// MoveImage2's wait, at its header (+2CF4h): until DMA2 is idle and the
/// GPU ready, `if (timeout) fail;`; then `go`.
fn move_image2_look(m: &mut M, s: Sys, go: Then) -> Flow<Psyq> {
    let busy_test = (0x2CF4, 0x2CFC);
    let round = len(&[(0x2CD8, 0x2CDC), (0x2CE0, 0x2CF0)]);
    let before = if busy(m) {
        len(&[busy_test, (0x2CD0, 0x2CD4)])
    } else if !ready(m) {
        len(&[busy_test, (0x2D00, 0x2D1C), (0x2CD0, 0x2CD4)])
    } else {
        crate::path(m, &[busy_test, (0x2D00, 0x2D1C)]);
        return go(m);
    };
    spin_checks(m, s, before, round, Box::new(move |m, ok| {
        if ok {
            return move_image2_look(m, s, go);
        }
        crate::path(m, &[(0x2CD8, 0x2CDC), (0x2D9C, 0x2DB0)]);
        Flow::ret(u32::MAX)
    }))
}

/// `_GPU_ResetCallback()`: the pump DMA2's callback again.
fn gpu_reset_callback(m: &mut M, _a: [u32; 4]) -> Result {
    let s = sys(m, at::GPU_RESET_CALLBACK)?;
    crate::path(m, &[(0x2EB0, 0x2EC4)]);
    Ok(Flow::Call {
        call: dma_callback(s, s.text + at::PUMP),
        then: Box::new(|m, v| {
            crate::path(m, &[(0x2EC8, 0x2ED4)]);
            Flow::Return(v)
        }),
    })
}

/// `ClearOTagR(ot, n)`: the table cleared backwards by DMA6 (through the
/// table's `sys:0x1834`; DMA6 is done at once), then its first entry
/// pointed at libgpu's terminator. Returns `ot`.
fn clear_otag_r(m: &mut M, a: [u32; 4]) -> Result {
    let s = sys(m, at::CLEAR_OTAG_R)?;
    let (ot, n) = (a[0], a[1]);
    crate::path(m, &[(0x868, 0x88C), (0x8B0, 0x8C8), (0x1834, 0x18A8)]);
    let d = reg(m, DPCR, 4)?;
    set_reg(m, DPCR, 4, d | 0x0800_0000)?;
    set_reg(m, CHCR6, 4, 0)?;
    set_reg(m, MADR6, 4, ot.wrapping_add(n << 2).wrapping_sub(4))?;
    set_reg(m, BCR6, 4, n)?;
    set_reg(m, CHCR6, 4, 0x1100_0002)?;
    timeout_start(m, s);
    crate::path(m, &[(0x18AC, 0x18C8), (0x1900, 0x1910), (0x8CC, 0x910)]);
    let term = s.data + data::OT_TERM;
    m.mem.w32(term, ((s.data + data::OT_END) & 0xFF_FFFF) | 0x0400_0000);
    m.mem.w32(ot, term & 0xFF_FFFF);
    ret(ot)
}

/// `DrawOTag(p)`.
fn draw_otag(m: &mut M, a: [u32; 4]) -> Result {
    let s = sys(m, at::DRAW_OTAG)?;
    crate::path(m, &[(0x970, 0x98C), (0x9AC, 0x9CC)]);
    image(m, s, Job { func: at::EXEC, p: a[0], len: 0, arg: 0 }, (0x9D0, 0x9DC))
}

/// `PutDrawEnv(env)`: its DR_ENV packet built in `env` and drawn, and the
/// DRAWENV copied (the game's `memcpy`). Returns `env`.
fn put_draw_env(m: &mut M, a: [u32; 4]) -> Result {
    let s = sys(m, at::PUT_DRAW_ENV)?;
    let env = a[0];
    let p = env + drawenv::DR_ENV;
    crate::path(m, &[(0x9E0, 0xA0C), (0xA2C, 0xA38)]);
    dr_env(m, s, p, env);
    crate::path(m, &[(0xA3C, 0xA70)]);
    let tag = m.mem.r32(p) | 0xFF_FFFF;
    m.mem.w32(p, tag);
    Ok(enqueue(m, s, Job { func: at::EXEC, p, len: 0x40, arg: 0 }, Box::new(move |m, _| {
        crate::path(m, &[(0xA74, 0xA80)]);
        memcpy(m, s.text + 0xA7C, (s.genv() + genv::DRAW, env, drawenv::SIZEOF), move |m, _| {
            crate::path(m, &[(0xA84, 0xA9C)]);
            Flow::ret(env)
        })
    })))
}

/// `DrawOTagEnv(p, env)`: `env`'s packet, linked to the table at `p`,
/// drawn. Its result is memcpy's, the DRAWENV copy's address.
fn draw_otag_env(m: &mut M, a: [u32; 4]) -> Result {
    let s = sys(m, at::DRAW_OTAG_ENV)?;
    let (ot, env) = (a[0], a[1]);
    let p = env + drawenv::DR_ENV;
    crate::path(m, &[(0xAA0, 0xAD4), (0xAF8, 0xB04)]);
    dr_env(m, s, p, env);
    crate::path(m, &[(0xB08, 0xB48)]);
    let tag = (m.mem.r32(p) & 0xFF00_0000) | (ot & 0xFF_FFFF);
    m.mem.w32(p, tag);
    Ok(enqueue(m, s, Job { func: at::EXEC, p, len: 0x40, arg: 0 }, Box::new(move |m, _| {
        crate::path(m, &[(0xB4C, 0xB58)]);
        memcpy(m, s.text + 0xB54, (s.genv() + genv::DRAW, env, drawenv::SIZEOF), move |m, v| {
            crate::path(m, &[(0xB5C, 0xB74)]);
            Flow::ret(v)
        })
    })))
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
    crate::path(m, &[(0xBAC, 0xBDC), (0xBFC, 0xC2C)]);
    gp1_call(m, 0x0500_0000 | (y & 0x3FF) << 10 | (x & 0x3FF));
    let same = |m: &M, o: u32| m.mem.r16(copy + o) == m.mem.r16(env + o);
    crate::path(m, &[(0xC30, 0xC40)]);
    let same_mode = m.mem.r32(copy + dispenv::ISINTER) == m.mem.r32(env + dispenv::ISINTER) && {
        crate::path(m, &[(0xC44, 0xC58)]);
        same(m, dispenv::DISP) && {
            crate::path(m, &[(0xC5C, 0xC70)]);
            same(m, dispenv::DISP + 2) && {
                crate::path(m, &[(0xC74, 0xC88)]);
                same(m, dispenv::DISP + 4) && {
                    crate::path(m, &[(0xC8C, 0xCA0)]);
                    same(m, dispenv::DISP + 6)
                }
            }
        }
    };
    if same_mode {
        return Ok(disp_screen(m, s, env));
    }
    crate::path(m, &[(0xCA4, 0xCA8)]);
    Ok(Flow::Call {
        call: GuestCall { addr: s.get_video_mode, args: [0; 4] },
        then: Box::new(move |m, v| {
            crate::path(m, &[(0xCAC, 0xCBC)]);
            m.mem.w8(env + dispenv::PAD0, v as u8);
            let mut mode = 0x0800_0000;
            if v as u8 == 1 {
                crate::path(m, &[(0xCC0, 0xCC0)]);
                mode |= 0x8;
            }
            crate::path(m, &[(0xCC4, 0xCD0)]);
            if m.mem.r8(env + dispenv::ISRGB24) != 0 {
                crate::path(m, &[(0xCD4, 0xCD4)]);
                mode |= 0x10;
            }
            crate::path(m, &[(0xCD8, 0xCE4)]);
            if m.mem.r8(env + dispenv::ISINTER) != 0 {
                crate::path(m, &[(0xCE8, 0xCE8)]);
                mode |= 0x20;
            }
            crate::path(m, &[(0xCEC, 0xCFC)]);
            if m.mem.r8(s.genv() + genv::REVERSE) != 0 {
                crate::path(m, &[(0xD00, 0xD00)]);
                mode |= 0x80;
            }
            crate::path(m, &[(0xD04, 0xD14)]);
            let w = r16s(m, env + dispenv::DISP + 4);
            if w >= 281 {
                crate::path(m, &[(0xD18, 0xD1C)]);
                mode |= if w < 353 {
                    crate::path(m, &[(0xD20, 0xD24)]);
                    0x1
                } else {
                    crate::path(m, &[(0xD28, 0xD2C)]);
                    if w < 401 {
                        crate::path(m, &[(0xD30, 0xD34)]);
                        0x40
                    } else {
                        crate::path(m, &[(0xD38, 0xD3C)]);
                        if w < 561 {
                            crate::path(m, &[(0xD40, 0xD44)]);
                            0x2
                        } else {
                            crate::path(m, &[(0xD48, 0xD48)]);
                            0x3
                        }
                    }
                };
            }
            crate::path(m, &[(0xD4C, 0xD58)]);
            let limit = if m.mem.r8(env + dispenv::PAD0) == 0 {
                crate::path(m, &[(0xD5C, 0xD5C)]);
                257
            } else {
                289
            };
            crate::path(m, &[(0xD60, 0xD64)]);
            if r16s(m, env + dispenv::DISP + 6) >= limit {
                crate::path(m, &[(0xD68, 0xD68)]);
                mode |= 0x24;
            }
            crate::path(m, &[(0xD6C, 0xD84)]);
            gp1_call(m, mode);
            crate::path(m, &[(0xD88, 0xD8C)]);
            m.mem.w8(env + dispenv::PAD0, 8);
            disp_screen(m, s, env)
        }),
    })
}

/// PutDispEnv's screen ranges (+D90h), then the DISPENV copy.
fn disp_screen(m: &mut M, s: Sys, env: u32) -> Flow<Psyq> {
    let copy = s.genv() + genv::DISP;
    let same = |m: &M, i: u32| m.mem.r16(copy + dispenv::SCREEN + 2 * i) == m.mem.r16(env + dispenv::SCREEN + 2 * i);
    crate::path(m, &[(0xD90, 0xDAC)]);
    let same = same(m, 0) && {
        crate::path(m, &[(0xDB0, 0xDC4)]);
        same(m, 1) && {
            crate::path(m, &[(0xDC8, 0xDDC)]);
            same(m, 2) && {
                crate::path(m, &[(0xDE0, 0xDF4)]);
                same(m, 3)
            }
        }
    };
    if same {
        crate::path(m, &[(0xDF8, 0xE04)]);
        if m.mem.r8(env + dispenv::PAD0) != 8 {
            return disp_done(m, s, env);
        }
    }
    crate::path(m, &[(0xE08, 0xE0C)]);
    Flow::Call {
        call: GuestCall { addr: s.get_video_mode, args: [0; 4] },
        then: Box::new(move |m, v| {
            crate::path(m, &[(0xE10, 0xE20)]);
            m.mem.w8(env + dispenv::PAD0, v as u8);
            let pal = v as u8 != 0;
            let sc = |m: &M, i: u32| r16s(m, env + dispenv::SCREEN + 2 * i);
            let (sx, sy, sw, sh) = (sc(m, 0), sc(m, 1), sc(m, 2), sc(m, 3));
            if !pal {
                crate::path(m, &[(0xE24, 0xE24)]);
            }
            let y0 = sy + if pal { 19 } else { 16 };
            crate::path(m, &[(0xE28, 0xE34)]);
            if sh == 0 {
                crate::path(m, &[(0xE38, 0xE38)]);
            }
            let y1 = y0 + if sh != 0 { sh } else { 240 };
            crate::path(m, &[(0xE3C, 0xE4C)]);
            let w = r16s(m, env + dispenv::DISP + 4);
            let class = if w < 281 {
                0
            } else {
                crate::path(m, &[(0xE50, 0xE58)]);
                if w < 353 {
                    1
                } else {
                    crate::path(m, &[(0xE5C, 0xE64)]);
                    if w < 401 {
                        2
                    } else {
                        crate::path(m, &[(0xE68, 0xE70)]);
                        if w < 561 {
                            crate::path(m, &[(0xE74, 0xE74)]);
                            3
                        } else {
                            4
                        }
                    }
                }
            };
            crate::path(m, &[(0xE78, 0xED0)]);
            if sw != 0 {
                crate::path(m, &[(0xED4, 0xEDC)]);
            }
            crate::path(m, &[(0xEE0, 0xEF0)]);
            let div = m.mem.r8(s.data + data::DOT_DIV + class) as i32;
            let t = s.data + data::H_RANGE + (m.mem.r8(env + dispenv::PAD0) as u32 * 5 + class) * 4;
            let (start, end) = (m.mem.r16(t) as i32, m.mem.r16(t + 2) as i32);
            let x0 = start + sx * div;
            let x1 = x0 + if sw != 0 { ((end - start) * sw) >> 8 } else { end - start };
            // The video mode's ranges, and the blocks (PAL's, NTSC's) that
            // clamp to them: x0 below, above, within; x1 at least x0's
            // four dots, above, within, both; y0 below, above, within,
            // after; y1 at least two lines, above, within.
            #[rustfmt::skip]
            let (x_lo, x_hi, x_end, y_lo, y_hi, y_end, b) = if pal {
                (540, 3220, 3260, 19, 303, 305, [(0xEF4, 0xEF8), (0xEFC, 0xF04), (0xF08, 0xF08), (0xF0C, 0xF2C), (0xF30, 0xF38),
                    (0xF3C, 0xF3C), (0xF40, 0xF40), (0xF44, 0xF48), (0xF60, 0xF60), (0xF4C, 0xF54), (0xF58, 0xF5C), (0xF64, 0xF74),
                    (0xF78, 0xF7C), (0x1014, 0x1014)])
            } else {
                (500, 3250, 3290, 16, 257, 258, [(0xF80, 0xF88), (0xF8C, 0xF94), (0xF98, 0xF98), (0xF9C, 0xFBC), (0xFC0, 0xFC8),
                    (0xFCC, 0xFCC), (0xFD0, 0xFD0), (0xFD4, 0xFD8), (0xFF0, 0xFF0), (0xFDC, 0xFE4), (0xFE8, 0xFEC), (0xFF4, 0x1004),
                    (0x1008, 0x1010), (0x1014, 0x1014)])
            };
            crate::path(m, &[b[0]]);
            if x0 >= x_lo {
                crate::path(m, &[b[1]]);
                if x0 <= x_hi {
                    crate::path(m, &[b[2]]);
                }
            }
            let x0 = x0.clamp(x_lo, x_hi);
            crate::path(m, &[b[3]]);
            let x1 = if x1 < x0 + div * 4 {
                x0 + div * 4
            } else {
                crate::path(m, &[b[4]]);
                if x1 <= x_end {
                    crate::path(m, &[b[5]]);
                }
                crate::path(m, &[b[6]]);
                x1.min(x_end)
            };
            crate::path(m, &[b[7]]);
            if y0 < y_lo {
                crate::path(m, &[b[8]]);
            } else {
                crate::path(m, &[b[9]]);
                if y0 <= y_hi {
                    crate::path(m, &[b[10]]);
                }
            }
            let y0 = y0.clamp(y_lo, y_hi);
            crate::path(m, &[b[11]]);
            let y1 = if y1 < y0 + 2 {
                y0 + 2
            } else {
                crate::path(m, &[b[12]]);
                if pal {
                    // Its `j` to the test NTSC's falls into.
                    crate::path(m, &[(0x100C, 0x1010)]);
                }
                if y1 <= y_end {
                    crate::path(m, &[b[13]]);
                }
                y1.min(y_end)
            };
            crate::path(m, &[(0x1018, 0x1044)]);
            gp1_call(m, 0x0600_0000 | (x1 as u32 & 0xFFF) << 12 | (x0 as u32 & 0xFFF));
            crate::path(m, &[(0x1048, 0x1070)]);
            gp1_call(m, 0x0700_0000 | (y1 as u32 & 0x3FF) << 10 | (y0 as u32 & 0x3FF));
            disp_done(m, s, env)
        }),
    }
}

/// The DISPENV copied (the game's `memcpy`); `env`.
fn disp_done(m: &mut M, s: Sys, env: u32) -> Flow<Psyq> {
    crate::path(m, &[(0x1074, 0x1084)]);
    memcpy(m, s.text + 0x1080, (s.genv() + genv::DISP, env, dispenv::SIZEOF), move |m, _| {
        crate::path(m, &[(0x1088, 0x10A0)]);
        Flow::ret(env)
    })
}

/// `GetODE()`: GPUSTAT's field bit (which field the display is reading),
/// through the table's `sys:0x181c`.
fn get_ode(m: &mut M, _a: [u32; 4]) -> Result {
    sys(m, at::GET_ODE)?;
    crate::path(m, &[(0x10D8, 0x10F4), (0x181C, 0x1830), (0x10F8, 0x1104)]);
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
    const VSYNC: u32 = 0x8001_0040;
    const VCOUNT: u32 = 0x8005_75D4;
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
        jal_to(&mut m, TEXT + at::JAL_VSYNC, VSYNC);
        pair(&mut m, VSYNC + 0x60, VCOUNT);
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
        // 0xF0001 passes of 71 instructions (the timeout check's VSync(-1)
        // among them), a quantum each 141: the test machine keeps no
        // budget, so each batch is a whole one's.
        let quanta = (m.clock.now - before) / 20_000;
        assert!((6960..=6980).contains(&quanta), "{quanta} quanta");
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
