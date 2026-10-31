//! libpress (chip mdec, D59, D64): `libpress/libpress`'s functions that
//! reach the MDEC, as adapters over its core.
//!
//! **The seam** is every function of the object that touches the MDEC or
//! its DMA channels: `DecDCTReset`, `DecDCTPutEnv`, `DecDCTin`,
//! `DecDCTout`, `DecDCTinSync`, `DecDCToutSync` (bof4 calls the first,
//! third and fourth; the others are adapted so that no function of the
//! object reads the I/O shadow when the chip is native). `DecDCTGetEnv`,
//! `DecDCTinCallback` and `DecDCToutCallback` stay original:
//! the first reads libpress's tables, which stay where libpress keeps them
//! (its `.data`, found through the object's code as libgpu's is, D61), and
//! the callback setters call libetc's `DMACallback`, whichever way chip irq
//! runs. The run-length coding functions (`DecDCTvlc*`, other objects)
//! touch no hardware and stay guest code.
//!
//! **The hardware.** The MDEC is the bus's core (`psx_devices::mdec`),
//! driven through its decode function ([`Mdec::input`], [`Mdec::output`]),
//! the one the device's DMA0 and DMA1 use. The channels are started
//! through the shared DMA controller's registers as libpress writes them
//! (MADR, BCR, CHCR; psyq-decomp's `func_00000428`/`func_000004B8`), and
//! the controller moves the words: `DecDCTin`'s command and parameters in
//! by DMA0, `DecDCTout`'s words out by DMA1, which waits, as its request
//! line does, while a decode is still taking input and hasn't produced the
//! words asked for; `DecDCTin`, a reset or a timeout lets it go. DMA1's
//! busy bit is CHCR1's, as `DecDCToutSync` polls it. The channels'
//! completions are held ([`psx_bios::Machine::dma_start_held`]): DICR's flags and
//! IRQ3, so the callbacks `DecDCTinCallback`/`DecDCToutCallback` set run
//! through libetc's DMA handler (its adapters' or its own). IRQ3 is held
//! until the adapter returns, as one libpress's register write raises is
//! taken at the guest's next safepoint, not inside the call: bof4's movie
//! player goes on decoding the next frame's run-length codes after
//! `DecDCTout`, and the callbacks that load each slice come when that work
//! reaches a safepoint, natively as on the device. DPCR gets libpress's
//! enables.
//!
//! **Waits spin like libpress's loops** (the MDEC taking input, DMA1
//! running): each iteration the original's 16 instructions from the
//! guest's budget (D71), a safepoint (interrupts delivered) each time it
//! runs out, and after 0x100000 iterations the timeout,
//! which prints libpress's message through the game's `printf` and resets
//! the MDEC and both channels, as the original does.
//!
//! DMA0's and DMA1's address and block registers hold what libpress wrote
//! and, once a transfer ends, what the controller leaves (the address past
//! the last word, no blocks left). `DecDCTPutEnv`'s table copies spend the
//! guest's safepoint budget: two loops of 16 iterations, 5 instructions
//! each.

use crate::{Adapter, M, Psyq, Result, reg, set_reg};
use psx_bios::{Flow, GuestCall};
use psx_devices::mdec::Mdec;

pub const ADAPTERS: &[(&str, Adapter)] = &[
    ("DecDCTReset", dct_reset),
    ("DecDCTPutEnv", dct_put_env),
    ("DecDCTin", dct_in),
    ("DecDCTout", dct_out),
    ("DecDCTinSync", dct_in_sync),
    ("DecDCToutSync", dct_out_sync),
];

/// `libpress/libpress` (PsyQ 4.7): offsets in its text.
mod at {
    pub const RESET: u32 = 0x0;
    pub const PUT_ENV: u32 = 0xc0;
    pub const DCT_IN: u32 = 0x158;
    pub const DCT_OUT: u32 = 0x1d4;
    pub const IN_SYNC: u32 = 0x1f4;
    pub const OUT_SYNC: u32 = 0x230;
    /// Relocated operands: `DecDCTPutEnv`'s `lui`/`addiu` of `.data`, the
    /// reset's of `.rdata` (around its `printf` call), and `jal`s.
    pub const DATA_PAIR: u32 = 0x124;
    pub const RDATA_HI: u32 = 0x394;
    pub const RDATA_LO: u32 = 0x39c;
    pub const JAL_RESET_CALLBACK: u32 = 0x14;
    pub const JAL_PRINTF: u32 = 0x398;
}

/// `.data` offsets.
mod data {
    /// The quantization tables' command (40000001h), then the luma and
    /// chroma tables, 64 bytes each.
    pub const QUANT: u32 = 0x0;
    pub const LUMA: u32 = 0x4;
    pub const CHROMA: u32 = 0x44;
    /// The scale table's command (60000000h), then its 64 halfwords.
    pub const SCALE: u32 = 0x84;
    /// Register pointers: DMA0's CHCR and DMA1's, what `DecDCTin` and
    /// `DecDCTout` leave in v0.
    pub const CHCR0_PTR: u32 = 0x118;
    pub const CHCR1_PTR: u32 = 0x124;
}

/// `.rdata` offsets: libpress's messages.
mod rdata {
    /// "MDEC_rest:bad option(%d)\n"
    pub const BAD_OPTION: u32 = 0x0;
    /// "MDEC_in_sync", "MDEC_out_sync"
    pub const IN_SYNC: u32 = 0x1c;
    pub const OUT_SYNC: u32 = 0x2c;
    /// "%s timeout:\n"
    pub const TIMEOUT: u32 = 0x3c;
}

const DPCR: u32 = psx_devices::dma::DPCR;
/// DPCR's enables for DMA0 and DMA1, which libpress sets before each
/// transfer.
const ENABLES: u32 = 0x88;
/// DMA0's and DMA1's address and block registers.
const MADR0: u32 = 0x1F80_1080;
const BCR0: u32 = 0x1F80_1084;
const MADR1: u32 = 0x1F80_1090;
const BCR1: u32 = 0x1F80_1094;
const CHCR0: u32 = 0x1F80_1088;
const CHCR1: u32 = 0x1F80_1098;
/// The MDEC's control words libpress writes: reset, then both DMA
/// requests on.
const RESET: u32 = 0x8000_0000;
const REQUESTS: u32 = 0x6000_0000;
/// libpress's wait loops count down from this; one more iteration is the
/// timeout.
const TIMEOUT: u32 = 0x10_0000;

/// Where `libpress/libpress` is in this game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Obj {
    pub text: u32,
    pub data: u32,
    pub rdata: u32,
    pub reset_callback: u32,
    pub printf: u32,
}

/// The chip's adapter state.
#[derive(Debug, Default)]
pub struct Press {
    pub obj: Option<Obj>,
}

/// The address a `lui` at `hi` and an immediate at `lo` with the `lui`'s
/// register as its base make.
fn pair(m: &M, hi: u32, lo: u32) -> Option<u32> {
    let (h, l) = (m.mem.r32(hi), m.mem.r32(lo));
    let rt = (h >> 16) & 31;
    (h >> 26 == 0x0F && (l >> 21) & 31 == rt).then(|| (h << 16).wrapping_add(l as u16 as i16 as u32))
}

/// The target of the `jal` at `pc`.
fn jal(m: &M, pc: u32) -> Option<u32> {
    let w = m.mem.r32(pc);
    (w >> 26 == 3).then(|| (pc & 0xF000_0000) | (w & 0x03FF_FFFF) << 2)
}

/// `libpress/libpress` from the running adapter's place in it (D61).
fn obj(m: &mut M, off: u32) -> std::result::Result<Obj, String> {
    let text = m.sdk.at.wrapping_sub(off);
    if let Some(o) = m.sdk.press.obj.filter(|o| o.text == text) {
        return Ok(o);
    }
    let find = |what: &str, v: Option<u32>| v.ok_or_else(|| format!("libpress/libpress at {text:#010x}: no {what} where PsyQ 4.7's object has it"));
    let o = Obj {
        text,
        data: find(".data", pair(m, text + at::DATA_PAIR, text + at::DATA_PAIR + 4))?,
        rdata: find(".rdata", pair(m, text + at::RDATA_HI, text + at::RDATA_LO))?,
        reset_callback: find("ResetCallback", jal(m, text + at::JAL_RESET_CALLBACK))?,
        printf: find("printf", jal(m, text + at::JAL_PRINTF))?,
    };
    m.sdk.press.obj = Some(o);
    Ok(o)
}

fn mdec(m: &mut M) -> std::result::Result<&mut Mdec, String> {
    m.io.as_mut().map(|io| &mut io.mdec).ok_or_else(|| "libpress: no MDEC (the runtime has no device bus)".to_string())
}

/// An error inside a continuation: the runtime traps with it.
fn fail(e: String) -> Flow<Psyq> {
    panic!("libpress: {e}")
}

// --- DMA1 and the resets -----------------------------------------------------

/// The shared controller runs what's ready, each completion held (D64):
/// DMA1, which `DecDCTout` started, goes once the MDEC isn't holding it
/// back (a decode still taking input that hasn't produced the words asked
/// for). What the device does after every MDEC write.
fn pump(m: &mut M) -> std::result::Result<(), String> {
    m.run_dma_held()
}

/// DPCR with DMA0 and DMA1 on, as libpress sets it before a transfer.
fn enable(m: &mut M) -> std::result::Result<(), String> {
    let d = reg(m, DPCR, 4)?;
    set_reg(m, DPCR, 4, d | ENABLES)
}

/// The MDEC reset and both channels stopped, then its DMA requests on
/// (the reset's modes 0 and 1, and the timeout's): DMA1, if it was
/// waiting, takes what the reset left (nothing: zeros) first, as on the
/// device.
fn stop(m: &mut M) -> std::result::Result<(), String> {
    mdec(m)?.control(RESET);
    pump(m)?;
    set_reg(m, CHCR0, 4, 0)?;
    set_reg(m, CHCR1, 4, 0)?;
    mdec(m)?.control(REQUESTS);
    pump(m)
}

// --- Waits -------------------------------------------------------------------

type K = Box<dyn FnOnce(&mut M, u32) -> Flow<Psyq>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Busy {
    /// The MDEC's command is busy (status bit 29): taking input, or a
    /// decode whose output DMA1 hasn't read.
    In,
    /// DMA1 is running (CHCR1 bit 24).
    Out,
}

fn busy(m: &mut M, b: Busy) -> bool {
    match b {
        Busy::In => m.io.as_ref().is_some_and(|io| io.mdec.command_busy()),
        Busy::Out => reg(m, CHCR1, 4).is_ok_and(|c| c & psx_devices::dma::CHCR_BUSY != 0),
    }
}

/// The waits (PsyQ 4.7 `libpress:0x4cc`, the input; `0x560`, DMA1), the
/// same blocks from each: the first look (+0h..+28h); while busy (+2Ch),
/// the countdown from 0x100000, a pass (+30h..+4Ch, +64h..+80h, its header
/// at +30h) of 16 instructions (D71) a look; past zero, the timeout
/// (+50h..+58h, then +5Ch..+60h); the return (+84h..+90h).
fn base(b: Busy) -> u32 {
    if b == Busy::In { 0x4CC } else { 0x560 }
}

const WAIT_PASS: u32 = 16;

/// `k` gets 0, or -1 after the timeout.
fn wait(m: &mut M, o: Obj, b: Busy, k: K) -> Flow<Psyq> {
    let w = base(b);
    crate::path(m, &[(w, w + 0x28)]);
    if !busy(m, b) {
        crate::path(m, &[(w + 0x84, w + 0x90)]);
        return k(m, 0);
    }
    crate::path(m, &[(w + 0x2C, w + 0x2C)]);
    spin(m, o, b, TIMEOUT, k)
}

/// The countdown's passes, `left` before the one that times out. What
/// `busy` reads changes only at a safepoint, so after a pass that sees it
/// busy, those up to the next safepoint are spent at once.
fn spin(m: &mut M, o: Obj, b: Busy, left: u32, k: K) -> Flow<Psyq> {
    crate::libspu::header(
        m,
        Box::new(move |m| {
            let w = base(b);
            if left == 0 {
                crate::path(m, &[(w + 0x30, w + 0x4C), (w + 0x50, w + 0x58)]);
                return timeout(m, o, b, k);
            }
            crate::path(m, &[(w + 0x30, w + 0x4C), (w + 0x64, w + 0x80)]);
            if !busy(m, b) {
                crate::path(m, &[(w + 0x84, w + 0x90)]);
                return k(m, 0);
            }
            let room = crate::room(m, WAIT_PASS) as u32;
            let n = room.min(left - 1);
            crate::take(m, n as u64 * WAIT_PASS as u64);
            if n == room {
                return crate::safepoint(m, move |m| spin(m, o, b, left - 1 - n, k));
            }
            spin(m, o, b, left - 1 - n, k)
        }),
    )
}

/// `libpress:0x60c`: "<what> timeout:" through `printf`, then the MDEC and
/// both channels reset.
fn timeout(m: &mut M, o: Obj, b: Busy, k: K) -> Flow<Psyq> {
    crate::path(m, &[(0x60C, 0x624)]);
    let what = o.rdata + if b == Busy::In { rdata::IN_SYNC } else { rdata::OUT_SYNC };
    Flow::Call {
        call: GuestCall { addr: o.printf, args: [o.rdata + rdata::TIMEOUT, what, 0, 0] },
        then: Box::new(move |m, _| {
            crate::path(m, &[(0x628, 0x684)]);
            if let Err(e) = stop(m) {
                return fail(e);
            }
            let w = base(b);
            crate::path(m, &[(w + 0x5C, w + 0x60), (w + 0x84, w + 0x90)]);
            k(m, u32::MAX)
        }),
    }
}

// --- Transfers ---------------------------------------------------------------

/// `libpress:0x3b0`: after the input wait, DPCR's enables, then the
/// command at `p` and its parameters (DMA0: `n` words in blocks, from
/// `p + 4`) into the MDEC; DMA0's completion, then DMA1's if it was waiting
/// for them. `k` gets what the original leaves in v0 (DMA0's CHCR pointer).
fn send(m: &mut M, o: Obj, p: u32, n: u32, k: K) -> Flow<Psyq> {
    crate::path(m, &[(0x3B0, 0x3C8)]);
    wait(
        m,
        o,
        Busy::In,
        Box::new(move |m, _| {
            crate::path(m, &[(0x3CC, 0x43C)]);
            let r = (|| -> std::result::Result<(), String> {
                enable(m)?;
                pump(m)?;
                let cmd = m.mem.r32(p);
                mdec(m)?.input(&[cmd]);
                pump(m)?;
                set_reg(m, MADR0, 4, p.wrapping_add(4) & 0x00FF_FFFF)?;
                set_reg(m, BCR0, 4, (n >> 5) << 16 | 0x20)?;
                m.dma_start_held(0, 0x0100_0201)
            })();
            match r {
                Ok(()) => {
                    let v = m.mem.r32(o.data + data::CHCR0_PTR);
                    k(m, v)
                }
                Err(e) => fail(e),
            }
        }),
    )
}

/// The tables in libpress's `.data` into the MDEC: quantization, then
/// scale, the caller's blocks `between` and `after` the two.
fn tables(m: &mut M, o: Obj, between: (u32, u32), after: (u32, u32), k: K) -> Flow<Psyq> {
    send(
        m,
        o,
        o.data + data::QUANT,
        32,
        Box::new(move |m, _| {
            crate::path(m, &[between]);
            send(
                m,
                o,
                o.data + data::SCALE,
                32,
                Box::new(move |m, v| {
                    crate::path(m, &[after]);
                    k(m, v)
                }),
            )
        }),
    )
}

/// Return `v` after the blocks `tail` (each adapter's spends its
/// original's path, D71).
fn back(tail: &'static [(u32, u32)]) -> K {
    Box::new(move |m, v| {
        crate::path(m, tail);
        Flow::ret(v)
    })
}

// --- The adapters --------------------------------------------------------

/// `DecDCTReset(mode)`: mode 0 calls `ResetCallback` first, then resets the
/// MDEC and loads libpress's tables; mode 1 only resets; another mode
/// prints libpress's complaint.
fn dct_reset(m: &mut M, a: [u32; 4]) -> Result {
    let o = obj(m, at::RESET)?;
    let mode = a[0];
    crate::path(m, &[(0x0, 0x10)]);
    if mode == 0 {
        crate::path(m, &[(0x14, 0x18)]);
        return Ok(Flow::Call {
            call: GuestCall { addr: o.reset_callback, args: [0; 4] },
            then: Box::new(move |m, _| {
                crate::path(m, &[(0x1C, 0x20)]);
                reset(m, o, mode)
            }),
        });
    }
    crate::path(m, &[(0x1C, 0x20)]);
    Ok(reset(m, o, mode))
}

/// `libpress:0x2c0`, the reset by mode.
fn reset(m: &mut M, o: Obj, mode: u32) -> Flow<Psyq> {
    const TAIL: &[(u32, u32)] = &[(0x3A0, 0x3AC), (0x24, 0x30)];
    crate::path(m, &[(0x2C0, 0x2CC)]);
    match mode {
        0 | 1 => {
            crate::path(m, if mode == 0 { &[(0x2E4, 0x328)] } else { &[(0x2D0, 0x2D8), (0x344, 0x390)] });
            if let Err(e) = stop(m) {
                return fail(e);
            }
            if mode == 1 {
                return back(TAIL)(m, REQUESTS);
            }
            tables(m, o, (0x32C, 0x338), (0x33C, 0x340), back(TAIL))
        }
        _ => {
            crate::path(m, &[(0x2D0, 0x2D8), (0x2DC, 0x2E0), (0x394, 0x39C)]);
            Flow::Call { call: GuestCall { addr: o.printf, args: [o.rdata + rdata::BAD_OPTION, mode, 0, 0] }, then: Box::new(|m, v| back(TAIL)(m, v as u32)) }
        }
    }
}

/// `DecDCTPutEnv(env)`: the environment's quantization tables into
/// libpress's (two copy loops of 16 passes, +E0h and +10Ch), then both of
/// libpress's tables into the MDEC (the scale table is libpress's own, not
/// the environment's). Returns `env`.
fn dct_put_env(m: &mut M, a: [u32; 4]) -> Result {
    let o = obj(m, at::PUT_ENV)?;
    let env = a[0];
    for i in 0..16 {
        let (luma, chroma) = (m.mem.r32(env + 4 * i), m.mem.r32(env + 64 + 4 * i));
        m.mem.w32(o.data + data::LUMA + 4 * i, luma);
        m.mem.w32(o.data + data::CHROMA + 4 * i, chroma);
    }
    crate::path(m, &[(0xC0, 0xDC)]);
    let none = || std::rc::Rc::new(|_: &mut M, _: u32| {});
    let load: crate::libspu::K = Box::new(move |m| {
        crate::path(m, &[(0x124, 0x130)]);
        tables(m, o, (0x134, 0x140), (0x144, 0x154), Box::new(move |_, _| Flow::ret(env)))
    });
    let chroma: crate::libspu::K = Box::new(move |m| {
        crate::path(m, &[(0xF8, 0x108)]);
        crate::libspu::each(m, 0, 16, 6, none(), load)
    });
    Ok(crate::libspu::each(m, 0, 16, 6, none(), chroma))
}

/// `DecDCTin(buf, mode)`: the command word's depth (mode bit 0: 24-bit,
/// else 15-bit, or 4-bit and 8-bit for monochrome) and bit 15 (mode bit
/// 1) set in `buf[0]`, then the command and its `buf[0] & 0xFFFF`
/// parameter words in.
fn dct_in(m: &mut M, a: [u32; 4]) -> Result {
    let o = obj(m, at::DCT_IN)?;
    let (buf, mode) = (a[0], a[1]);
    let mut w = m.mem.r32(buf);
    w = if mode & 1 != 0 { w & !0x0800_0000 } else { w | 0x0800_0000 };
    w = if mode & 2 != 0 { w | 0x0200_0000 } else { w & !0x0200_0000 };
    m.mem.w32(buf, w);
    let depth = if mode & 1 != 0 { (0x168, 0x178) } else { (0x17C, 0x184) };
    let half = if mode & 2 != 0 { (0x198, 0x1A0) } else { (0x1A4, 0x1B0) };
    crate::path(m, &[(0x158, 0x164), depth, (0x188, 0x194), half, (0x1B4, 0x1C0)]);
    Ok(send(m, o, buf, w & 0xFFFF, back(&[(0x1C4, 0x1D0)])))
}

/// `DecDCTout(buf, size)` (`libpress:0x440`): after the DMA1 wait, DMA1
/// for `size` words (in blocks) into `buf`, now or when the MDEC has them.
fn dct_out(m: &mut M, a: [u32; 4]) -> Result {
    let o = obj(m, at::DCT_OUT)?;
    let (buf, size) = (a[0], a[1]);
    crate::path(m, &[(0x1D4, 0x1E0), (0x440, 0x458)]);
    Ok(wait(
        m,
        o,
        Busy::Out,
        Box::new(move |m, _| {
            crate::path(m, &[(0x45C, 0x4C8)]);
            let r = (|| -> std::result::Result<(), String> {
                enable(m)?;
                pump(m)?;
                set_reg(m, CHCR1, 4, 0)?;
                set_reg(m, MADR1, 4, buf & 0x00FF_FFFF)?;
                set_reg(m, BCR1, 4, (size >> 5) << 16 | 0x20)?;
                m.dma_start_held(1, 0x0100_0200)
            })();
            match r {
                Ok(()) => {
                    let v = m.mem.r32(o.data + data::CHCR1_PTR);
                    back(&[(0x1E4, 0x1F0)])(m, v)
                }
                Err(e) => fail(e),
            }
        }),
    ))
}

/// `DecDCTinSync(mode)`: mode 0 waits while the MDEC takes input (0, or
/// -1 after the timeout); otherwise whether it is (`libpress:0x5f4`).
fn dct_in_sync(m: &mut M, a: [u32; 4]) -> Result {
    let o = obj(m, at::IN_SYNC)?;
    crate::path(m, &[(0x1F4, 0x1FC)]);
    if a[0] == 0 {
        crate::path(m, &[(0x200, 0x204)]);
        return Ok(wait(m, o, Busy::In, back(&[(0x208, 0x20C), (0x220, 0x22C)])));
    }
    crate::path(m, &[(0x210, 0x214), (0x5F4, 0x608), (0x218, 0x21C), (0x220, 0x22C)]);
    Ok(Flow::ret(busy(m, Busy::In) as u32))
}

/// `DecDCToutSync(mode)`: mode 0 waits while DMA1 runs (0, or -1 after the
/// timeout); otherwise whether it does.
fn dct_out_sync(m: &mut M, a: [u32; 4]) -> Result {
    let o = obj(m, at::OUT_SYNC)?;
    crate::path(m, &[(0x230, 0x238)]);
    if a[0] == 0 {
        crate::path(m, &[(0x23C, 0x240)]);
        return Ok(wait(m, o, Busy::Out, back(&[(0x244, 0x248), (0x268, 0x274)])));
    }
    crate::path(m, &[(0x24C, 0x264), (0x268, 0x274)]);
    Ok(Flow::ret(busy(m, Busy::Out) as u32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{call, irq_calls, machine_with_bus, vblanks};

    /// A stand-in for libpress/libpress in a game: its text, data and
    /// rdata, with the relocated operands the adapters read, and libpress's
    /// tables (flat quantization, the DCT basis).
    const TEXT: u32 = 0x8003_0000;
    const DATA: u32 = 0x8005_0000;
    const RDATA: u32 = 0x8005_1000;
    const RESET_CALLBACK: u32 = 0x8001_0000;
    const PRINTF: u32 = 0x8001_0010;
    const CALLBACK: u32 = 0x8001_0020;
    const IN: u32 = 0x8010_0000;
    const OUT: u32 = 0x8011_0000;

    fn lui(m: &mut M, pc: u32, lo: u32, addr: u32) {
        let hi = addr.wrapping_add(0x8000) >> 16;
        m.mem.w32(pc, 0x3C04_0000 | hi);
        m.mem.w32(lo, 0x2484_0000 | (addr & 0xFFFF));
    }

    fn jal_to(m: &mut M, pc: u32, target: u32) {
        m.mem.w32(pc, 0x0C00_0000 | (target >> 2) & 0x03FF_FFFF);
    }

    fn game() -> M {
        let mut m = machine_with_bus();
        lui(&mut m, TEXT + at::DATA_PAIR, TEXT + at::DATA_PAIR + 4, DATA);
        lui(&mut m, TEXT + at::RDATA_HI, TEXT + at::RDATA_LO, RDATA);
        jal_to(&mut m, TEXT + at::JAL_RESET_CALLBACK, RESET_CALLBACK);
        jal_to(&mut m, TEXT + at::JAL_PRINTF, PRINTF);
        m.mem.w32(DATA + data::QUANT, 0x4000_0001);
        for i in 0..128 {
            m.mem.w8(DATA + data::LUMA + i, 2);
        }
        m.mem.w32(DATA + data::SCALE, 0x6000_0000);
        for i in 0..8u32 {
            for j in 0..8u32 {
                let c = if i == 0 { 1.0 / 8f64.sqrt() } else { 0.5 };
                let v = c * (((2 * j + 1) as f64 * i as f64 * std::f64::consts::PI) / 16.0).cos();
                m.mem.w16(DATA + data::SCALE + 4 + 2 * (i * 8 + j), (v * 65536.0).round().clamp(-32768.0, 32767.0) as i16 as u16);
            }
        }
        m.mem.w32(DATA + data::CHCR0_PTR, 0x1F80_1088);
        m.mem.w32(DATA + data::CHCR1_PTR, 0x1F80_1098);
        m
    }

    fn run(m: &mut M, off: u32, f: Adapter, a: [u32; 4]) -> u32 {
        m.sdk.at = TEXT + off;
        call(m, f, a)
    }

    /// A 24-bit decode of one flat macroblock (DC terms only) and its
    /// header, padded to 32 words.
    fn flat_macroblock(m: &mut M, at: u32, words: u32) {
        let mut h: Vec<u16> = Vec::new();
        for b in 0..6 {
            h.extend([1 << 10 | if b < 2 { 0 } else { 0x40 }, 0xFE00]);
        }
        h.resize(64, 0xFE00);
        m.mem.w32(at, 0x3800_0000 | words);
        for (i, p) in h.chunks(2).enumerate() {
            m.mem.w32(at + 4 + 4 * i as u32, p[0] as u32 | (p[1] as u32) << 16);
        }
    }

    #[test]
    fn finds_the_object_from_its_code() {
        let mut m = game();
        m.sdk.at = TEXT + at::DCT_IN;
        assert_eq!(obj(&mut m, at::DCT_IN).unwrap(), Obj { text: TEXT, data: DATA, rdata: RDATA, reset_callback: RESET_CALLBACK, printf: PRINTF });
        m.sdk.at = TEXT + 0x1000 + at::DCT_IN;
        assert!(obj(&mut m, at::DCT_IN).is_err(), "no object there");
    }

    #[test]
    fn decodes_into_ram_and_calls_back() {
        let mut m = game();
        assert_eq!(run(&mut m, at::RESET, dct_reset, [0; 4]), 0x1F80_1088);
        assert_eq!(irq_calls(), vec![RESET_CALLBACK]);
        assert_eq!(m.io.as_ref().unwrap().dma.dpcr & 0x88, 0x88);
        // libetc running, with DMACallback(1, f).
        crate::libetc::fixture::install(&mut m);
        crate::libetc::fixture::reset(&mut m);
        crate::libetc::fixture::dma_callback(&mut m, 1, CALLBACK);
        flat_macroblock(&mut m, IN, 32);
        assert_eq!(run(&mut m, at::DCT_IN, dct_in, [IN, 2, 0, 0]), 0x1F80_1088);
        assert_eq!(m.mem.r32(IN) >> 24, 0x3A, "15-bit, bit 15 set");
        assert_eq!(run(&mut m, at::OUT_SYNC, dct_out_sync, [1, 0, 0, 0]), 0);
        // 16×16 halfwords: 128 words.
        assert_eq!(run(&mut m, at::DCT_OUT, dct_out, [OUT, 128, 0, 0]), 0x1F80_1098);
        assert_eq!(irq_calls(), vec![CALLBACK], "DMA1's callback");
        // The controller's registers as the transfers leave them.
        let d = &m.io.as_ref().unwrap().dma;
        assert_eq!((d.madr[0], d.bcr[0]), ((IN + 4 + 4 * 32) & 0x1F_FFFC, 0x20));
        assert_eq!((d.madr[1], d.bcr[1]), ((OUT + 4 * 128) & 0x1F_FFFC, 0x20));
        let px = m.mem.r32(OUT);
        assert_eq!(px & 0x8000, 0x8000);
        assert!((0..128).all(|i| m.mem.r32(OUT + 4 * i) == px), "flat");
        assert_eq!(run(&mut m, at::IN_SYNC, dct_in_sync, [1, 0, 0, 0]), 0);
    }

    #[test]
    fn dma1_waits_for_its_input() {
        let mut m = game();
        run(&mut m, at::RESET, dct_reset, [1, 0, 0, 0]);
        assert_eq!(irq_calls(), Vec::<u32>::new(), "mode 1 doesn't call ResetCallback");
        run(&mut m, at::RESET, dct_reset, [0, 0, 0, 0]);
        assert_eq!(irq_calls(), vec![RESET_CALLBACK]);
        // Out first: nothing decoded, nothing waiting for input, so DMA1
        // runs at once (zeros).
        m.mem.w32(OUT, 0xDEAD_BEEF);
        run(&mut m, at::DCT_OUT, dct_out, [OUT, 32, 0, 0]);
        assert_eq!(m.mem.r32(OUT), 0);
        // A decode that wants 40 words and gets 32: busy, with one
        // macroblock (128 words) out, so DMA1 for 128 runs and DMA1 for
        // 256 waits.
        flat_macroblock(&mut m, IN, 40);
        run(&mut m, at::DCT_IN, dct_in, [IN, 0, 0, 0]);
        assert_eq!(run(&mut m, at::IN_SYNC, dct_in_sync, [1, 0, 0, 0]), 1);
        run(&mut m, at::DCT_OUT, dct_out, [OUT, 128, 0, 0]);
        assert_eq!(run(&mut m, at::OUT_SYNC, dct_out_sync, [1, 0, 0, 0]), 0);
        assert_ne!(m.mem.r32(OUT + 4 * 127), 0);
        flat_macroblock(&mut m, IN, 40);
        run(&mut m, at::DCT_OUT, dct_out, [OUT, 256, 0, 0]);
        assert_eq!(run(&mut m, at::OUT_SYNC, dct_out_sync, [1, 0, 0, 0]), 1);
        // The wait for DMA1 times out after 0x100000 iterations of 16
        // instructions and the start of one more (this machine has no guest
        // budget: a quantum each 626 passes, 1,675 and some), prints, and
        // resets: DMA1 takes zeros.
        let t = m.clock.now;
        assert_eq!(run(&mut m, at::OUT_SYNC, dct_out_sync, [0, 0, 0, 0]), u32::MAX);
        assert_eq!((m.clock.now - t) / 20_000, 1675);
        assert_eq!(irq_calls(), vec![PRINTF]);
        assert_eq!(run(&mut m, at::OUT_SYNC, dct_out_sync, [1, 0, 0, 0]), 0);
        assert_eq!(run(&mut m, at::IN_SYNC, dct_in_sync, [1, 0, 0, 0]), 0);
        assert_eq!(m.mem.r32(OUT + 4 * 255), 0);
        vblanks(&mut m, 1);
        // Bad option.
        run(&mut m, at::RESET, dct_reset, [2, 0, 0, 0]);
        assert_eq!(irq_calls(), vec![PRINTF]);
    }

    #[test]
    fn put_env_loads_its_quantization() {
        let mut m = game();
        let env = 0x8012_0000;
        for i in 0..64 {
            m.mem.w8(env + i, 1);
            m.mem.w8(env + 64 + i, 3);
        }
        assert_eq!(run(&mut m, at::PUT_ENV, dct_put_env, [env, 0, 0, 0]), env);
        assert_eq!(m.mem.r8(DATA + data::LUMA + 5), 1);
        assert_eq!(m.mem.r8(DATA + data::CHROMA + 5), 3);
    }
}
