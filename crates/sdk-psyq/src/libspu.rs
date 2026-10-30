//! libspu (chip spu, D59, D63): the functions whose own code reaches the
//! SPU, as adapters over the SPU core's mixer API (`psx_spu::mixer`) and
//! the DMA controller.
//!
//! **The seam is where libspu touches the SPU.** In `spu.o`, the layer
//! everything else goes through: `_spu_init` (the reset), `_spu_FsetRXX`
//! and `_spu_FsetRXXa` (a register by index, raw or as an SPU RAM
//! address), `_spu_Fw` (an SPU RAM write, by DMA4 or through the FIFO)
//! and `_spu_FiDMA` (DMA4's completion callback, reached through
//! `DMACallback`). Above it, the public functions that program the SPU
//! themselves through `_spu_RXX`: the voice, key, mask, common, reverb and
//! noise functions the games and libsnd call, and `SpuClearReverbWorkArea`.
//! Everything else stays original guest code over them: `SpuInit`,
//! `_SpuInit` and `SpuStart` (which open the DMA event and register
//! `_spu_FiDMA`), `SpuWritePartly`, `SpuSetTransferStartAddr`,
//! `SpuSetTransferMode`, `SpuIsTransferCompleted`, the allocator, the
//! `SpuSet/GetNoiseVoice`-style wrappers, `SpuQuit`, and all of libsnd but
//! `_SsInit` ([`crate::libsnd`]).
//!
//! **libspu's state stays where libspu keeps it** (D59's second option,
//! as libgpu's, D61): `spu.o`'s `.data` (`_spu_tsa`, the transfer and
//! memory modes, `_spu_inTransfer`, the callbacks, the DMA bookkeeping),
//! `s_ini.o`'s (`_spu_keystat`, `_spu_env`, the register queue `_spu_RQ`,
//! the centre notes, the reverb attributes). An adapter finds them through
//! the relocated operands of its own code (`Psyq::at`), and calls the
//! functions its code calls where it calls them (`Flow::Call`): `_spu_Fw1ts`
//! (libspu's delay loop), `_spu_note2pitch`, `_SpuIsInAllocateArea_`,
//! `printf`, the BIOS's `DeliverEvent` and `WaitEvent`, the transfer
//! callback, and other adapters (`SpuClearReverbWorkArea`).
//!
//! **The hardware.** Register writes are mixer calls on the SPU core
//! (`Machine::with_spu`), brought up to the current cycle first, as a
//! register access is; register reads read what the registers read. A
//! DMA4 transfer is programmed on the DMA controller as `_spu_t` programs
//! it (DPCR, MADR4, BCR4 with the controller's semantics,
//! [`crate::set_reg`]); its words go through the core's transfer port and
//! its completion is held (`Machine::dma_done_held`, D64), so DICR and
//! IRQ3 reach `_spu_FiDMA`, through whatever runs libetc, at the guest's
//! next safepoint, as the device's would. The SPU's bus delay register
//! (1F801014h) is set as libspu sets it.
//!
//! **Time.** The original's loops spend the guest's safepoint budget
//! (the voice loops, the FIFO's halfwords, the copies), and an adapter
//! spends the same, a safepoint each time it runs out, at the same place
//! among its SPU writes ([`spend`], [`each`]); `_spu_Fw1ts` is the
//! original itself. Waits on the SPU's status bits end at once on the
//! core, as on the device. Held interrupts are let go before the
//! adapter runs guest code or reaches a safepoint, and an adapter's
//! return is no safepoint (`Machine::quiet`): the original makes no
//! runtime call there, so a completion it raised is taken where the
//! device takes it, at the guest's next safepoint, not at the return of
//! another adapter the guest calls first (`SpuWritePartly` calls
//! `_spu_FsetRXXa` after `_spu_Fw`).
//!
//! **What isn't reproduced.** Register-queue mode (`_spu_env` bit 0,
//! `SpuSetEnv`) is followed where the adapted functions follow it, but
//! no game here links `SpuSetEnv` or `SpuFlush`. Waits that would time
//! out (a status bit the core never leaves set) spend their 3,840
//! iterations at once. Values left in `v0` by `void` functions are the
//! original's where the code makes them plain.

use crate::{Adapter, M, Psyq, Result, reg, set_reg};
use psx_bios::{Flow, GuestCall, Wait};
use psx_spu::Spu;
use psx_spu::mixer::{Mask, Volume};
use std::rc::Rc;

pub const ADAPTERS: &[(&str, Adapter)] = &[
    ("_spu_init", spu_init),
    ("_spu_FiDMA", spu_fidma),
    ("_spu_Fw", spu_fw),
    ("_spu_FsetRXX", spu_fsetrxx),
    ("_spu_FsetRXXa", spu_fsetrxxa),
    ("SpuClearReverbWorkArea", clear_reverb_work_area),
    ("SpuSetKey", set_key),
    ("SpuGetKeyStatus", get_key_status),
    ("SpuGetAllKeysStatus", get_all_keys_status),
    ("SpuGetVoiceEnvelope", get_voice_envelope),
    ("SpuGetVoiceVolume", get_voice_volume),
    ("SpuSetVoiceVolume", set_voice_volume),
    ("SpuSetNoiseClock", set_noise_clock),
    ("_SpuSetAnyVoice", set_any_voice),
    ("_SpuGetAnyVoice", get_any_voice),
    ("SpuSetReverb", set_reverb),
    ("SpuSetReverbDepth", set_reverb_depth),
    ("SpuSetReverbModeParam", set_reverb_mode_param),
    ("SpuSetCommonAttr", set_common_attr),
    ("SpuSetVoiceAttr", set_voice_attr),
];

type F = Flow<Psyq>;
pub(crate) type K = Box<dyn FnOnce(&mut M) -> F>;

/// `spu.o` (PsyQ 4.7): offsets in its text.
mod at {
    pub const INIT: u32 = 0x0;
    pub const FIDMA: u32 = 0x440;
    pub const T: u32 = 0x5A4;
    pub const FW: u32 = 0x824;
    pub const FSETRXX: u32 = 0x90C;
    pub const FSETRXXA: u32 = 0x950;
    pub const FW1TS: u32 = 0xAD8;
    /// Relocated operands in `_spu_init`: `.data+0x10` (DPCR's pointer),
    /// `_spu_RQ`, `.rdata` (the timeout format), `printf`.
    pub const DATA_PAIR: (u32, u32) = (0xC, 0x10);
    pub const RQ_PAIR: (u32, u32) = (0xD4, 0xD8);
    pub const RDATA_PAIR: (u32, u32) = (0x94, 0x98);
    pub const JAL_PRINTF: u32 = 0xA0;
    pub const JAL_FW1TS: u32 = 0x54;
    /// In `_spu_FiDMA`: `DeliverEvent`.
    pub const JAL_DELIVER_EVENT: u32 = 0x4E4;
}

/// `spu.o`'s `.data`.
mod d {
    pub const RXX: u32 = 0x0;
    pub const TSA: u32 = 0x18;
    pub const TRANS_MODE: u32 = 0x1C;
    pub const ADDR_MODE: u32 = 0x20;
    pub const MEM_MODE: u32 = 0x24;
    pub const MEM_PLUS: u32 = 0x28;
    pub const MEM_UNIT: u32 = 0x2C;
    pub const MEM_UNITM: u32 = 0x30;
    pub const IN_TRANSFER: u32 = 0x34;
    pub const TRANSFER_CB: u32 = 0x38;
    pub const IRQ_CB: u32 = 0x3C;
    /// The 16-byte block `_spu_init` writes at 1000h (`07` flags: a loop
    /// of silence for the voices it parks there).
    pub const BLOCK: u32 = 0x40;
    /// 1 while the transfer mode set is DMA read (`_spu_t(0)`).
    pub const DMA_READ: u32 = 0x50;
    pub const DMA_ADDR: u32 = 0x54;
    pub const DMA_BLOCKS: u32 = 0x58;
}

/// `.rdata`: the timeout messages (`SPU:T/O [%s]`).
mod msg {
    pub const RESET: u32 = 0x10;
    pub const WRDY: u32 = 0x20;
    pub const DMAF: u32 = 0x34;
}

const DPCR: u32 = psx_devices::dma::DPCR;
const MADR4: u32 = 0x1F80_10C0;
const BCR4: u32 = 0x1F80_10C4;
const CHCR4: u32 = 0x1F80_10C8;
/// The SPU's bus delay and size register (memory control).
const SPU_DELAY: u32 = 0x1F80_1014;
/// libspu's wait loops give up after this many polls.
const TIMEOUT: u32 = 3840;
/// Loop passes the translated original spends, where clang's code makes
/// them other than one per iteration (measured against the original over
/// the devices, `psxw conform --chip spu`): `_spu_init` parks two voices
/// a pass.
const PARK_PASSES: u32 = 12;
/// `SpuSetReverbModeParam` copies a mode's 68 bytes of parameters a word a
/// pass.
const COPY_PASSES: u32 = 17;

/// Where `spu.o` is in this game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpuO {
    pub text: u32,
    pub data: u32,
    pub rdata: u32,
    pub rq: u32,
    pub printf: u32,
    pub deliver_event: u32,
}

impl SpuO {
    fn fw1ts(&self) -> u32 {
        self.text + at::FW1TS
    }
}

/// The chip's adapter state: where `spu.o` is.
#[derive(Debug, Default)]
pub struct State {
    pub obj: Option<SpuO>,
}

/// The address a `lui` at `hi` and the instruction at `lo` using its
/// register as base (`addiu`, a load or a store) make.
pub(crate) fn pair(m: &M, hi: u32, lo: u32) -> Option<u32> {
    let (h, l) = (m.mem.r32(hi), m.mem.r32(lo));
    let rt = (h >> 16) & 31;
    (h >> 26 == 0x0F && (l >> 21) & 31 == rt).then(|| (h << 16).wrapping_add(l as u16 as i16 as u32))
}

/// The target of the `jal` at `pc`.
pub(crate) fn jal(m: &M, pc: u32) -> Option<u32> {
    let w = m.mem.r32(pc);
    (w >> 26 == 3).then(|| (pc & 0xF000_0000) | (w & 0x03FF_FFFF) << 2)
}

/// An object's relocated operands, read from the running adapter's code.
struct Code {
    text: u32,
    what: &'static str,
}

impl Code {
    /// The adapted function's object, from its place in the object's text.
    fn here(m: &M, off: u32, what: &'static str) -> Self {
        Code { text: m.sdk.at.wrapping_sub(off), what }
    }

    fn pair(&self, m: &M, (hi, lo): (u32, u32), name: &str) -> std::result::Result<u32, String> {
        pair(m, self.text + hi, self.text + lo).ok_or_else(|| self.missing(name))
    }

    fn jal(&self, m: &M, pc: u32, name: &str) -> std::result::Result<u32, String> {
        jal(m, self.text + pc).ok_or_else(|| self.missing(name))
    }

    fn missing(&self, name: &str) -> String {
        format!("{} at {:#010x}: no {name} where PsyQ 4.7's object has it", self.what, self.text)
    }
}

/// `spu.o` from the running adapter's place in it (`off` in its text).
fn spu_o(m: &mut M, off: u32) -> std::result::Result<SpuO, String> {
    spu_o_at(m, m.sdk.at.wrapping_sub(off))
}

/// `spu.o` from its text's address.
fn spu_o_at(m: &mut M, text: u32) -> std::result::Result<SpuO, String> {
    if let Some(o) = m.sdk.spu.obj.filter(|o| o.text == text) {
        return Ok(o);
    }
    let c = Code { text, what: "libspu/spu" };
    let fw1ts = c.jal(m, at::JAL_FW1TS, "_spu_Fw1ts")?;
    if fw1ts != text + at::FW1TS {
        return Err(c.missing("_spu_Fw1ts"));
    }
    let o = SpuO {
        text,
        data: c.pair(m, at::DATA_PAIR, ".data")?.wrapping_sub(0x10),
        rdata: c.pair(m, at::RDATA_PAIR, ".rdata")?,
        rq: c.pair(m, at::RQ_PAIR, "_spu_RQ")?,
        printf: c.jal(m, at::JAL_PRINTF, "printf")?,
        deliver_event: c.jal(m, at::JAL_DELIVER_EVENT, "DeliverEvent")?,
    };
    m.sdk.spu.obj = Some(o);
    Ok(o)
}

// --- Flows -----------------------------------------------------------------

/// An error inside a continuation: the runtime traps with it.
fn fail(e: String) -> F {
    panic!("libspu: {e}")
}

/// Call guest code where the original calls it, then go on with its `v0`.
fn call(m: &mut M, addr: u32, args: [u32; 4], k: impl FnOnce(&mut M, u32) -> F + 'static) -> F {
    // Guest code runs: what the adapter's transfers raised is pending for it.
    m.release_irq();
    Flow::Call { call: GuestCall { addr, args }, then: Box::new(move |m, v| k(m, v as u32)) }
}

/// Return `v` as the original does, with no runtime call: no safepoint
/// (`Machine::quiet`), so what is pending (a transfer's completion
/// among it) waits for the guest's next one, as on the device.
fn back(m: &mut M, v: u32) -> F {
    m.quiet = true;
    Flow::ret(v)
}

/// A safepoint in the code an adapter replaces: the budget starts over, a
/// quantum passes, what fell due is delivered and its callbacks run.
fn safepoint(m: &mut M, k: K) -> F {
    m.release_irq();
    m.regs.reset_budget();
    let until = m.clock.now + crate::libetc::quantum(m);
    m.run_until(until);
    Flow::Block { until: Wait::Cycle(until), then: k }
}

/// `n` passes of the original's loop headers: the guest's safepoint budget
/// spent as they spend it, a safepoint each time it runs out.
pub(crate) fn spend(m: &mut M, n: u32, k: K) -> F {
    let b = m.regs.budget();
    if b == 0 || n < b {
        if b != 0 {
            m.regs.set_budget(b - n);
        }
        return k(m);
    }
    safepoint(m, Box::new(move |m| spend(m, n - b, k)))
}

/// `for i in from..n { body(i) }` as the original's loop runs it: one
/// header pass (budget) before each body.
pub(crate) fn each(m: &mut M, from: u32, n: u32, body: Rc<dyn Fn(&mut M, u32)>, k: K) -> F {
    let mut i = from;
    while i < n {
        let b = m.regs.budget();
        if b == 1 {
            return safepoint(
                m,
                Box::new(move |m| {
                    body(m, i);
                    each(m, i + 1, n, body, k)
                }),
            );
        }
        if b != 0 {
            m.regs.set_budget(b - 1);
        }
        body(m, i);
        i += 1;
    }
    k(m)
}

/// The SPU core, up to now.
fn sp<R>(m: &mut M, f: impl FnOnce(&mut Spu) -> R) -> R {
    match m.with_spu(f) {
        Some(r) => r,
        None => panic!("libspu: no SPU on the bus"),
    }
}

/// The same, as a result.
fn sp_r<R>(m: &mut M, f: impl FnOnce(&mut Spu) -> std::result::Result<R, String>) -> std::result::Result<R, String> {
    m.with_spu(f).unwrap_or_else(|| Err("no SPU on the bus".into()))
}

/// libspu's poll of a status bit: done at once when `cond` holds (the core
/// changes no status bit while time passes); else its 3,840 polls, then
/// `printf("SPU:T/O [%s]\n", what)`.
fn wait(m: &mut M, o: SpuO, cond: impl FnOnce(&Spu) -> bool, what: u32, k: K) -> F {
    if sp(m, |s| cond(&*s)) {
        return k(m);
    }
    spend(m, TIMEOUT, Box::new(move |m| call(m, o.printf, [o.rdata, o.rdata + what, 0, 0], move |m, _| k(m))))
}

/// `_spu_Fw1ts()` `n` times: libspu's delay loop, the original's code.
fn delay(m: &mut M, o: SpuO, n: u32, k: K) -> F {
    call(m, o.fw1ts(), [0; 4], move |m, _| if n == 1 { k(m) } else { delay(m, o, n - 1, k) })
}

// --- Registers by index -------------------------------------------------

/// Register `idx` (`_spu_RXX[idx]`, halfwords from 1F801C00h) set through
/// the mixer API.
fn set_index(s: &mut Spu, idx: u32, v: u16) -> std::result::Result<(), String> {
    if idx < 0xC0 {
        let voice = (idx / 8) as usize;
        match idx % 8 {
            0 => s.set_voice_volume(voice, 0, v),
            1 => s.set_voice_volume(voice, 1, v),
            2 => s.set_voice_pitch(voice, v),
            3 => s.set_voice_start(voice, v),
            4 => s.set_voice_adsr1(voice, v),
            5 => s.set_voice_adsr2(voice, v),
            6 => s.set_voice_envelope(voice, v),
            _ => s.set_voice_repeat(voice, v),
        }
        return Ok(());
    }
    let side = (idx & 1) as usize;
    let mask = |s: &mut Spu, m: Mask| s.set_mask_half(m, idx & 1 != 0, v);
    match idx {
        0xC0 | 0xC1 => s.set_common_volume(Volume::Main, side, v),
        0xC2 | 0xC3 => s.set_common_volume(Volume::Reverb, side, v),
        0xC4 | 0xC5 => mask(s, Mask::KeyOn),
        0xC6 | 0xC7 => mask(s, Mask::KeyOff),
        0xC8 | 0xC9 => mask(s, Mask::PitchMod),
        0xCA | 0xCB => mask(s, Mask::Noise),
        0xCC | 0xCD => mask(s, Mask::Reverb),
        0xCE | 0xCF => mask(s, Mask::End),
        0xD1 => s.set_reverb_base(v),
        0xD2 => s.set_irq_address(v),
        0xD3 => s.set_transfer_start(v),
        0xD4 => s.push_transfer(v),
        0xD5 => s.set_control(v),
        0xD6 => s.set_transfer_control(v),
        0xD8 | 0xD9 => s.set_common_volume(Volume::Cd, side, v),
        0xDA | 0xDB => s.set_common_volume(Volume::External, side, v),
        0xE0..=0xFF => s.set_reverb_param((idx - 0xE0) as usize, v),
        _ => return Err(format!("register {:#x} (index {idx:#x}) has no mixer equivalent", 0x1F80_1C00 + 2 * idx)),
    }
    Ok(())
}

/// Register `idx` as it reads.
fn get_index(s: &Spu, idx: u32) -> std::result::Result<u16, String> {
    if idx < 0xC0 {
        let voice = (idx / 8) as usize;
        return Ok(match idx % 8 {
            0 => s.voice_volume(voice, 0),
            1 => s.voice_volume(voice, 1),
            2 => s.voice_pitch(voice),
            3 => s.voice_start(voice),
            4 => s.voice_adsr(voice).0,
            5 => s.voice_adsr(voice).1,
            6 => s.voice_envelope(voice) as u16,
            _ => s.voice_repeat(voice),
        });
    }
    let side = (idx & 1) as usize;
    let high = idx & 1 != 0;
    Ok(match idx {
        0xC0 | 0xC1 => s.common_volume(Volume::Main, side),
        0xC2 | 0xC3 => s.common_volume(Volume::Reverb, side),
        0xC4 | 0xC5 => s.mask_half(Mask::KeyOn, high),
        0xC6 | 0xC7 => s.mask_half(Mask::KeyOff, high),
        0xC8 | 0xC9 => s.mask_half(Mask::PitchMod, high),
        0xCA | 0xCB => s.mask_half(Mask::Noise, high),
        0xCC | 0xCD => s.mask_half(Mask::Reverb, high),
        0xCE | 0xCF => s.mask_half(Mask::End, high),
        0xD1 => s.reverb_base(),
        0xD2 => s.irq_address(),
        0xD3 => s.transfer_start(),
        0xD4 => 0,
        0xD5 => s.control(),
        0xD6 => s.transfer_control(),
        0xD7 => s.status(),
        0xD8 | 0xD9 => s.common_volume(Volume::Cd, side),
        0xDA | 0xDB => s.common_volume(Volume::External, side),
        0xDC | 0xDD => s.main_current_volume(side) as u16,
        0xE0..=0xFF => s.reverb_param((idx - 0xE0) as usize),
        _ => return Err(format!("register {:#x} (index {idx:#x}) has no mixer equivalent", 0x1F80_1C00 + 2 * idx)),
    })
}

// --- spu.o -----------------------------------------------------------------

/// `_spu_init(hot)`: DMA4 enabled at priority 3, the SPU reset (muted,
/// SPUCNT 0, its status cleared), the transfer and memory modes, the
/// register queue cleared, reverb and keys off; cold, also every voice
/// parked on a silent loop at 1000h (written there through the FIFO) and
/// keyed on and off. Then the SPU on and unmuted.
fn spu_init(m: &mut M, a: [u32; 4]) -> Result {
    let o = spu_o(m, at::INIT)?;
    let hot = a[0] != 0;
    let dpcr = reg(m, DPCR, 4)?;
    set_reg(m, DPCR, 4, dpcr | 0xB_0000)?;
    m.mem.w32(o.data + d::TRANS_MODE, 0);
    m.mem.w32(o.data + d::ADDR_MODE, 0);
    m.mem.w16(o.data + d::TSA, 0);
    sp(m, |s| {
        s.set_common_volume(Volume::Main, 0, 0);
        s.set_common_volume(Volume::Main, 1, 0);
        s.set_control(0);
    });
    Ok(delay(
        m,
        o,
        1,
        Box::new(move |m| {
            sp(m, |s| {
                s.set_common_volume(Volume::Main, 0, 0);
                s.set_common_volume(Volume::Main, 1, 0);
            });
            wait(m, o, |s| s.status() & 0x7FF == 0, msg::RESET, Box::new(move |m| init_modes(m, o, hot)))
        }),
    ))
}

fn init_modes(m: &mut M, o: SpuO, hot: bool) -> F {
    let rq = o.rq;
    // The register queue's ten halfwords: a loop the translation unrolls.
    for i in 0..10 {
        m.mem.w16(rq + 2 * i, 0);
    }
    let dd = o.data;
    for (off, v) in [(d::MEM_MODE, 2), (d::MEM_PLUS, 3), (d::MEM_UNIT, 8), (d::MEM_UNITM, 7)] {
        m.mem.w32(dd + off, v);
    }
    sp(m, |s| {
        s.set_transfer_control(4);
        s.set_common_volume(Volume::Reverb, 0, 0);
        s.set_common_volume(Volume::Reverb, 1, 0);
        // Both halves 0xFFFF (only the low 8 bits of the high one latch).
        s.set_mask_half(Mask::KeyOff, false, 0xFFFF);
        s.set_mask_half(Mask::KeyOff, true, 0xFFFF);
        s.set_mask_bits(Mask::Reverb, 0);
    });
    if hot {
        return init_done(m, o);
    }
    m.mem.w16(dd + d::TSA, 0x200);
    sp(m, |s| {
        s.set_mask_bits(Mask::PitchMod, 0);
        s.set_mask_bits(Mask::Noise, 0);
        for side in 0..2 {
            s.set_common_volume(Volume::Cd, side, 0);
            s.set_common_volume(Volume::External, side, 0);
        }
    });
    manual_write(
        m,
        o,
        dd + d::BLOCK,
        16,
        Box::new(move |m| {
            // Two voices a pass: the translation unrolls the loop once.
            let park = Rc::new(|m: &mut M, pass: u32| {
                sp(m, |s| {
                    for v in [2 * pass as usize, 2 * pass as usize + 1] {
                        s.set_voice_volume(v, 0, 0);
                        s.set_voice_volume(v, 1, 0);
                        s.set_voice_pitch(v, 0x3FFF);
                        s.set_voice_start(v, 0x200);
                        s.set_voice_adsr1(v, 0);
                        s.set_voice_adsr2(v, 0);
                    }
                })
            });
            each(
                m,
                0,
                PARK_PASSES,
                park,
                Box::new(move |m| {
                    sp(m, |s| s.set_mask_bits(Mask::KeyOn, 0xFF_FFFF));
                    delay(
                        m,
                        o,
                        4,
                        Box::new(move |m| {
                            sp(m, |s| s.set_mask_bits(Mask::KeyOff, 0xFF_FFFF));
                            delay(m, o, 4, Box::new(move |m| init_done(m, o)))
                        }),
                    )
                }),
            )
        }),
    )
}

fn init_done(m: &mut M, o: SpuO) -> F {
    m.mem.w32(o.data + d::IN_TRANSFER, 1);
    sp(m, |s| s.set_control(0xC000));
    m.mem.w32(o.data + d::TRANSFER_CB, 0);
    m.mem.w32(o.data + d::IRQ_CB, 0);
    back(m, 0)
}

/// `spu.o+0x280`: `size` bytes from `src` to SPU RAM at `_spu_tsa` through
/// the FIFO, 64 at a time, each sent with transfer mode 1; then the mode
/// cleared and the status back to what it was.
fn manual_write(m: &mut M, o: SpuO, src: u32, size: u32, k: K) -> F {
    let tsa = m.mem.r16(o.data + d::TSA);
    let before = sp(m, |s| {
        let st = s.status() & 0x7FF;
        s.set_transfer_start(tsa);
        st
    });
    delay(m, o, 1, Box::new(move |m| manual_chunk(m, o, src, size, before, k)))
}

fn manual_chunk(m: &mut M, o: SpuO, src: u32, left: u32, before: u16, k: K) -> F {
    if left == 0 {
        sp(m, |s| s.set_control(s.control() & !0x30));
        return wait(m, o, move |s| s.status() & 0x7FF == before, msg::DMAF, k);
    }
    let n = left.min(64);
    // The outer loop's header, then a pass per halfword.
    spend(
        m,
        1,
        Box::new(move |m| {
            let halves = n.div_ceil(2);
            each(
                m,
                0,
                halves,
                Rc::new(move |m, i| {
                    let h = m.mem.r16(src + 2 * i);
                    sp(m, |s| s.push_transfer(h));
                }),
                Box::new(move |m| {
                    sp(m, |s| s.set_control((s.control() & !0x30) | 0x10));
                    delay(
                        m,
                        o,
                        1,
                        Box::new(move |m| {
                            wait(
                                m,
                                o,
                                |s| s.status() & 0x400 == 0,
                                msg::WRDY,
                                Box::new(move |m| delay(m, o, 2, Box::new(move |m| manual_chunk(m, o, src + 2 * halves, left - n, before, k)))),
                            )
                        }),
                    )
                }),
            )
        }),
    )
}

/// `_spu_t(mode, …)` (PsyQ 4.7): 2 sets the transfer address (`_spu_tsa`,
/// in 8-byte units), 1 and 0 the transfer mode (DMA write, DMA read), 3
/// starts DMA4 on `addr`, `size` bytes, in 16-word blocks.
fn spu_t(m: &mut M, o: SpuO, mode: u32, addr: u32, size: u32) -> std::result::Result<(), String> {
    let dd = o.data;
    match mode {
        2 => {
            let tsa = addr >> (m.mem.r32(dd + d::MEM_PLUS) & 31);
            m.mem.w16(dd + d::TSA, tsa as u16);
            sp(m, |s| s.set_transfer_start(tsa as u16));
        }
        0 | 1 => {
            m.mem.w32(dd + d::DMA_READ, if mode == 0 { 1 } else { 0 });
            // The wait for the transfer address to read back holds at once.
            sp(m, |s| {
                let c = s.control();
                s.set_control(if mode == 0 { c | 0x30 } else { (c & !0x30) | 0x20 });
            });
        }
        3 => {
            let read = m.mem.r32(dd + d::DMA_READ) == 1;
            let delay = reg(m, SPU_DELAY, 4)?;
            set_reg(m, SPU_DELAY, 4, (delay & 0xF0FF_FFFF) | if read { 0x2200_0000 } else { 0x2000_0000 })?;
            m.mem.w32(dd + d::DMA_ADDR, addr);
            set_reg(m, MADR4, 4, addr)?;
            let blocks = (size >> 6) + u32::from(size & 0x3F != 0);
            m.mem.w32(dd + d::DMA_BLOCKS, blocks);
            set_reg(m, BCR4, 4, blocks << 16 | 0x10)?;
            dma4(m, addr, blocks * 16, read)?;
        }
        _ => {}
    }
    Ok(())
}

/// DMA4 started (CHCR4 with its start bit), as the controller runs it:
/// when DPCR enables the channel, the words between RAM and SPU RAM at
/// the transfer address through the core's transfer port, MADR4 past
/// them and BCR4's block count spent, then the completion held
/// (`Machine::dma_done_held`, D64): DICR and IRQ3 reach `_spu_FiDMA`
/// at the guest's next safepoint, as the device's guest register write
/// would. A channel DPCR leaves off waits on the controller, as on the
/// device.
fn dma4(m: &mut M, addr: u32, words: u32, read: bool) -> std::result::Result<(), String> {
    let chcr = if read { 0x0100_0200 } else { 0x0100_0201 };
    if reg(m, DPCR, 4)? & 8 << 16 == 0 {
        return set_reg(m, CHCR4, 4, chcr);
    }
    let from = addr & 0x1F_FFFC;
    let at = |i: u32| from.wrapping_add(4 * i) & 0x1F_FFFC;
    if read {
        let mut buf = vec![0u32; words as usize];
        sp(m, |s| s.dma_read(&mut buf));
        for (i, w) in buf.into_iter().enumerate() {
            m.mem.w32(at(i as u32), w);
        }
    } else {
        let buf: Vec<u32> = (0..words).map(|i| m.mem.r32(at(i))).collect();
        sp(m, |s| s.dma_write(&buf));
    }
    let io = m.io.as_mut().ok_or("no DMA controller on the bus")?;
    io.dma.chcr[4] = chcr;
    io.dma.madr[4] = at(words);
    io.dma.bcr[4] &= 0xFFFF;
    m.dma_done_held(4);
    Ok(())
}

/// `_spu_Fw(addr, size)`: SPU RAM at `_spu_tsa` from `addr`, by DMA4
/// (`_spu_transMode` 0) or through the FIFO. Returns `size`.
fn spu_fw(m: &mut M, a: [u32; 4]) -> Result {
    let o = spu_o(m, at::FW)?;
    let (addr, size) = (a[0], a[1]);
    if m.mem.r32(o.data + d::TRANS_MODE) != 0 {
        return Ok(manual_write(m, o, addr, size, Box::new(move |m| back(m, size))));
    }
    let plus = m.mem.r32(o.data + d::MEM_PLUS) & 31;
    let tsa = (m.mem.r16(o.data + d::TSA) as u32) << plus;
    spu_t(m, o, 2, tsa, 0)?;
    spu_t(m, o, 1, 0, 0)?;
    spu_t(m, o, 3, addr, size)?;
    Ok(back(m, size))
}

/// `_spu_FiDMA()`, DMA4's completion callback (registered by `SpuStart`
/// through `_SpuDataCallback`): after a write, libspu's delay; the
/// transfer mode cleared; then the transfer callback, or the SPU's
/// transfer event (`DeliverEvent(HwSPU, EvSpCOMP)`).
fn spu_fidma(m: &mut M, _a: [u32; 4]) -> Result {
    let o = spu_o(m, at::FIDMA)?;
    let then = move |m: &mut M| -> F {
        sp(m, |s| s.set_control(s.control() & !0x30));
        let cb = m.mem.r32(o.data + d::TRANSFER_CB);
        if cb != 0 { call(m, cb, [0xF000_0000, 0, 0, 0], back) } else { call(m, o.deliver_event, [0xF000_0009, 0x20, 0, 0], back) }
    };
    Ok(if m.mem.r32(o.data + d::DMA_READ) == 0 { delay(m, o, 1, Box::new(then)) } else { then(m) })
}

/// `_spu_FsetRXX(idx, value, as_address)`: register `idx` set to `value`,
/// or to `value` as an SPU RAM address (shifted to 8-byte units). Returns
/// the register's address.
fn spu_fsetrxx(m: &mut M, a: [u32; 4]) -> Result {
    let o = spu_o(m, at::FSETRXX)?;
    let (idx, value) = (a[0], a[1]);
    let v = if a[2] == 0 { value } else { value >> (m.mem.r32(o.data + d::MEM_PLUS) & 31) };
    sp_r(m, |s| set_index(s, idx, v as u16))?;
    let rxx = m.mem.r32(o.data + d::RXX);
    Ok(back(m, rxx.wrapping_add(idx.wrapping_mul(2))))
}

/// `_spu_FsetRXXa(idx, addr)`: `addr` rounded up to the memory unit; -2
/// returns it, -1 returns it in 8-byte units, else register `idx` is set
/// to that and it is returned.
fn spu_fsetrxxa(m: &mut M, a: [u32; 4]) -> Result {
    let o = spu_o(m, at::FSETRXXA)?;
    let v = fsetrxxa(m, o, a[0], a[1])?;
    Ok(back(m, v))
}

fn fsetrxxa(m: &mut M, o: SpuO, idx: u32, mut addr: u32) -> std::result::Result<u32, String> {
    let dd = o.data;
    if m.mem.r32(dd + d::MEM_MODE) != 0 {
        let unit = m.mem.r32(dd + d::MEM_UNIT);
        if unit == 0 {
            return Err("_spu_FsetRXXa: division by a zero memory unit".into());
        }
        if addr % unit != 0 {
            addr = addr.wrapping_add(unit) & !m.mem.r32(dd + d::MEM_UNITM);
        }
    }
    let units = addr >> (m.mem.r32(dd + d::MEM_PLUS) & 31);
    match idx as i32 {
        -2 => Ok(addr),
        -1 => Ok(units & 0xFFFF),
        _ => {
            sp_r(m, |s| set_index(s, idx, units as u16))?;
            Ok(addr)
        }
    }
}

// --- SpuClearReverbWorkArea -----------------------------------------------

/// `SpuClearReverbWorkArea(mode)`: the reverb work area of `mode` (0: the
/// last 128 bytes) zeroed from `_spu_zerobuf` by DMA4, 1 KB at a time,
/// each waited for with `WaitEvent`, the FIFO mode and the transfer
/// callback set aside meanwhile. -1 for a bad mode or an area the
/// allocator holds.
fn clear_reverb_work_area(m: &mut M, a: [u32; 4]) -> Result {
    let c = Code::here(m, 0, "libspu/s_crwa");
    let start = c.pair(m, (0x34, 0x38), "_spu_rev_startaddr")?;
    let in_area = c.jal(m, 0x48, "_SpuIsInAllocateArea_")?;
    let plus_at = c.pair(m, (0x68, 0x6C), "_spu_mem_mode_plus")?;
    let trans_mode = c.pair(m, (0x9C, 0xA0), "_spu_transMode")?;
    let cb_at = c.pair(m, (0xBC, 0xC0), "_spu_transferCallback")?;
    let zerobuf = c.pair(m, (0x114, 0x118), "_spu_zerobuf")?;
    let ev = c.pair(m, (0x124, 0x128), "_spu_EVdma")?;
    let wait_event = c.jal(m, 0x130, "WaitEvent")?;
    let t = c.jal(m, 0x100, "_spu_t")?;
    let o = spu_o_at(m, t.wrapping_sub(at::T))?;
    let mode = a[0];
    if mode >= 10 {
        return Ok(back(m, u32::MAX));
    }
    let base = m.mem.r32(start + 4 * mode);
    Ok(call(m, in_area, [base, 0, 0, 0], move |m, held| {
        if held != 0 {
            return back(m, u32::MAX);
        }
        let plus = m.mem.r32(plus_at) & 31;
        let (size, addr) = if mode == 0 { (16 << plus, 0xFFF0 << plus) } else { ((0x1_0000u32.wrapping_sub(base)) << plus, base << plus) };
        let fifo = m.mem.r32(trans_mode);
        let restore = fifo == 1;
        if restore {
            m.mem.w32(trans_mode, 0);
        }
        let saved = m.mem.r32(cb_at);
        if saved != 0 {
            m.mem.w32(cb_at, 0);
        }
        let ev = m.mem.r32(ev);
        let end = Rc::new(move |m: &mut M| {
            if restore {
                m.mem.w32(trans_mode, fifo);
            }
            if saved != 0 {
                m.mem.w32(cb_at, saved);
            }
            back(m, 0)
        });
        // The chunk loop's header passes once per chunk after the first
        // (measured against the original over the devices).
        clear_chunk(m, o, zerobuf, wait_event, ev, size, addr, end, 0)
    }))
}

#[allow(clippy::too_many_arguments)]
fn clear_chunk(m: &mut M, o: SpuO, zerobuf: u32, wait_event: u32, ev: u32, size: u32, addr: u32, end: Rc<dyn Fn(&mut M) -> F>, pass: u32) -> F {
    spend(
        m,
        pass,
        Box::new(move |m| {
            let last = size < 1025;
            let n = if last { size } else { 1024 };
            let r = spu_t(m, o, 2, addr, 0).and_then(|_| spu_t(m, o, 1, 0, 0)).and_then(|_| spu_t(m, o, 3, zerobuf, n));
            if let Err(e) = r {
                return fail(e);
            }
            call(m, wait_event, [ev, 0, 0, 0], move |m, _| {
                if last { end(m) } else { clear_chunk(m, o, zerobuf, wait_event, ev, size.wrapping_sub(1024), addr.wrapping_add(1024), end, 1) }
            })
        }),
    )
}

// --- Keys --------------------------------------------------------------------

/// `SpuSetKey(on, voices)`: KON (1) or KOFF (0) for the voices, with
/// `_spu_keystat`; in queue mode into `_spu_RQ` instead. Returns what the
/// original leaves in `v0`.
fn set_key(m: &mut M, a: [u32; 4]) -> Result {
    let c = Code::here(m, 0, "libspu/s_sk");
    let env = c.pair(m, (0x20, 0x24), "_spu_env")?;
    let rq = c.pair(m, (0x38, 0x3C), "_spu_RQ")?;
    let rqmask = c.pair(m, (0x48, 0x4C), "_spu_RQmask")?;
    let rqvoice = c.pair(m, (0x60, 0x64), "_spu_RQvoice")?;
    let keystat = c.pair(m, (0xC4, 0xC8), "_spu_keystat")?;
    let bits = a[1] & 0xFF_FFFF;
    let (lo, hi) = (bits & 0xFFFF, bits >> 16);
    let queue = m.mem.r32(env) & 1 != 0;
    let v = match a[0] {
        0 | 1 => {
            let on = a[0] == 1;
            if queue {
                // KON at RQ[0..2], KOFF at RQ[2..4]: set ours, clear the other's.
                let (set, other) = if on { (rq, rq + 4) } else { (rq + 4, rq) };
                m.mem.w16(set, lo as u16);
                m.mem.w16(set + 2, hi as u16);
                let rm = m.mem.r32(rqmask);
                m.mem.w32(rqmask, rm | 1);
                let rv = m.mem.r32(rqvoice);
                m.mem.w32(rqvoice, if on { rv | bits } else { rv & !bits });
                let o0 = m.mem.r16(other) as u32;
                if o0 & bits != 0 {
                    m.mem.w16(other, (o0 & !bits) as u16);
                }
                let o1 = m.mem.r16(other + 2) as u32;
                if o1 & hi != 0 {
                    let v = o1 & !hi;
                    m.mem.w16(other + 2, v as u16);
                    v
                } else {
                    0
                }
            } else {
                sp(m, |s| s.set_mask_bits(if on { Mask::KeyOn } else { Mask::KeyOff }, bits));
                let k = m.mem.r32(keystat);
                let v = if on { k | bits } else { k & !bits };
                m.mem.w32(keystat, v);
                v
            }
        }
        _ => 1,
    };
    Ok(back(m, v))
}

/// A voice's key status: 1 keyed on and sounding, 3 keyed on and silent,
/// 2 keyed off and still sounding, 0 off.
fn key_status(on: bool, envx: i16) -> u32 {
    match (on, envx != 0) {
        (true, true) => 1,
        (true, false) => 3,
        (false, true) => 2,
        (false, false) => 0,
    }
}

/// `SpuGetKeyStatus(voice_bit)`: the status of the lowest voice in the
/// mask, -1 for none.
fn get_key_status(m: &mut M, a: [u32; 4]) -> Result {
    let c = Code::here(m, 0, "libspu/s_gks");
    let keystat = c.pair(m, (0x50, 0x54), "_spu_keystat")?;
    let first = (0..24).find(|v| a[0] & 1 << v != 0);
    // The search for the lowest bit spends nothing: the translation
    // makes it a count of trailing zeros.
    Ok(spend(
        m,
        0,
        Box::new(move |m| {
            let Some(v) = first else { return back(m, u32::MAX) };
            let on = m.mem.r32(keystat) & 1 << v != 0;
            let envx = sp(m, |s| s.voice_envelope(v as usize));
            back(m, key_status(on, envx))
        }),
    ))
}

/// `SpuGetAllKeysStatus(status)`: every voice's key status, a byte each.
fn get_all_keys_status(m: &mut M, a: [u32; 4]) -> Result {
    let c = Code::here(m, 0xD4, "libspu/sr_gaks");
    let keystat = c.pair(m, (0xF8, 0xFC), "_spu_keystat")?;
    let buf = a[0];
    Ok(each(
        m,
        0,
        24,
        Rc::new(move |m, v| {
            let on = m.mem.r32(keystat) & 1 << v != 0;
            let envx = sp(m, |s| s.voice_envelope(v as usize));
            m.mem.w8(buf + v, key_status(on, envx) as u8);
        }),
        Box::new(|m| back(m, 0)),
    ))
}

// --- Voices ------------------------------------------------------------------

/// `SpuGetVoiceEnvelope(voice, *envx)`.
fn get_voice_envelope(m: &mut M, a: [u32; 4]) -> Result {
    let e = sp_r(m, |s| get_index(s, a[0].wrapping_mul(8).wrapping_add(6)))?;
    m.mem.w16(a[1], e);
    Ok(back(m, e as u32))
}

/// `SpuGetVoiceVolume(voice, *left, *right)`: each a level (0-3FFFh) or,
/// for a sweep, the register less 8000h.
fn get_voice_volume(m: &mut M, a: [u32; 4]) -> Result {
    let base = a[0].wrapping_mul(8);
    let (l, r) = sp_r(m, |s| Ok((get_index(s, base)?, get_index(s, base + 1)?)))?;
    let shown = |v: u16| if v < 0x4000 { v as u32 } else { (v as u32).wrapping_sub(0x8000) };
    m.mem.w16(a[1], shown(l) as u16);
    m.mem.w16(a[2], shown(r) as u16);
    Ok(back(m, if r < 0x4000 { 0x8000 } else { shown(r) }))
}

/// `SpuSetVoiceVolume(voice, left, right)`: levels (bit 15 cleared), then
/// libspu's short delay.
fn set_voice_volume(m: &mut M, a: [u32; 4]) -> Result {
    let base = a[0].wrapping_mul(8);
    sp_r(m, |s| {
        set_index(s, base, (a[1] & 0x7FFF) as u16)?;
        set_index(s, base + 1, (a[2] & 0x7FFF) as u16)
    })?;
    Ok(spend(m, 2, Box::new(|m| back(m, 0))))
}

/// `SpuSetNoiseClock(n)`: the noise clock (0-63) in SPUCNT bits 8-13.
fn set_noise_clock(m: &mut M, a: [u32; 4]) -> Result {
    let n = (a[0] as i32).clamp(0, 63) as u32;
    sp(m, |s| s.set_control((s.control() & 0xC0FF) | (n as u16) << 8));
    Ok(back(m, n))
}

/// `_SpuSetAnyVoice(on, voices, lo, hi)`: a voice mask held in registers
/// `lo` and `hi` (PMON, NON, EON) — voices on (1), off (0), or the mask
/// set (8) — in queue mode in `_spu_RQ`. Returns the mask as it read
/// before, with the change.
fn set_any_voice(m: &mut M, a: [u32; 4]) -> Result {
    let c = Code::here(m, 0, "libspu/s_sav");
    let env = c.pair(m, (0x4, 0x8), "_spu_env")?;
    let rqb = c.pair(m, (0x14, 0x18), "_spu_RQ")?;
    let rqmask = c.pair(m, (0xD8, 0xDC), "_spu_RQmask")?;
    let (op, bits, lo, hi) = (a[0], a[1], a[2], a[3]);
    let queue = m.mem.r32(env) & 1 != 0;
    let q = |i: u32| rqb.wrapping_add(i.wrapping_mul(2));
    let (cur_lo, cur_hi) =
        if queue { (m.mem.r16(q(lo)) as u32, m.mem.r16(q(hi)) as u32) } else { sp_r(m, |s| Ok((get_index(s, lo)? as u32, get_index(s, hi)? as u32)))? };
    let cur = cur_lo | (cur_hi & 0xFF) << 16;
    let high = (bits >> 16) & 0xFF;
    let (new_lo, new_hi, result) = match op {
        1 => (cur_lo | bits, cur_hi | high, cur | (bits & 0xFF_FFFF)),
        0 => (cur_lo & !bits, cur_hi & !high, cur & !(bits & 0xFF_FFFF)),
        8 => (bits, high, bits & 0xFF_FFFF),
        _ => return Ok(back(m, cur & 0xFF_FFFF)),
    };
    if queue {
        m.mem.w16(q(lo), new_lo as u16);
        m.mem.w16(q(hi), new_hi as u16);
        let bit = (lo.wrapping_sub(198) as i32) >> 1;
        let rm = m.mem.r32(rqmask);
        m.mem.w32(rqmask, rm | 1u32.wrapping_shl(bit as u32));
    } else {
        sp_r(m, |s| {
            set_index(s, lo, new_lo as u16)?;
            set_index(s, hi, new_hi as u16)
        })?;
    }
    Ok(back(m, result & 0xFF_FFFF))
}

/// `_SpuGetAnyVoice(lo, hi)`: the voice mask in registers `lo` and `hi`.
fn get_any_voice(m: &mut M, a: [u32; 4]) -> Result {
    let (lo, hi) = sp_r(m, |s| Ok((get_index(s, a[0])? as u32, get_index(s, a[1])? as u32)))?;
    Ok(back(m, lo | (hi & 0xFF) << 16))
}

// --- Reverb ------------------------------------------------------------------

/// `SpuSetReverb(on)`: the reverb unit off (and its depth zeroed), or on
/// unless the allocator holds its work area (`_spu_rev_reserve_wa` off).
/// Returns `_spu_rev_flag`.
fn set_reverb(m: &mut M, a: [u32; 4]) -> Result {
    let c = Code::here(m, 0, "libspu/s_sr");
    let flag = c.pair(m, (0x38, 0x3C), "_spu_rev_flag")?;
    let attr = c.pair(m, (0x50, 0x54), "_spu_rev_attr")?.wrapping_sub(8);
    let reserve = c.pair(m, (0x64, 0x68), "_spu_rev_reserve_wa")?;
    let offset = c.pair(m, (0x78, 0x7C), "_spu_rev_offsetaddr")?;
    let in_area = c.jal(m, 0x80, "_SpuIsInAllocateArea_")?;
    let enable = move |m: &mut M, on: bool| {
        m.mem.w32(flag, on as u32);
        sp(m, |s| s.set_control(if on { s.control() | 0x80 } else { s.control() & !0x80 }));
        let f = m.mem.r32(flag);
        back(m, f)
    };
    Ok(match a[0] {
        0 => {
            m.mem.w32(flag, 0);
            sp(m, |s| {
                s.set_control(s.control() & !0x80);
                s.set_common_volume(Volume::Reverb, 0, 0);
                s.set_common_volume(Volume::Reverb, 1, 0);
            });
            m.mem.w16(attr + 8, 0);
            m.mem.w16(attr + 10, 0);
            let f = m.mem.r32(flag);
            back(m, f)
        }
        1 if m.mem.r32(reserve) == 1 => enable(m, true),
        1 => {
            let off = m.mem.r32(offset);
            call(m, in_area, [off, 0, 0, 0], move |m, held| enable(m, held == 0))
        }
        _ => {
            let f = m.mem.r32(flag);
            back(m, f)
        }
    })
}

/// `SpuSetReverbDepth(attr)`: the reverb output volume (mask bits 1, 2;
/// all when 0), kept in `_spu_rev_attr`.
fn set_reverb_depth(m: &mut M, a: [u32; 4]) -> Result {
    let c = Code::here(m, 0, "libspu/s_srd");
    let rev = c.pair(m, (0x34, 0x38), "_spu_rev_attr")?.wrapping_sub(8);
    let p = a[0];
    let mask = m.mem.r32(p);
    for (bit, side) in [(2, 0usize), (4, 1)] {
        if mask == 0 || mask & bit != 0 {
            let v = m.mem.r16(p + 8 + 2 * side as u32);
            sp(m, |s| s.set_common_volume(Volume::Reverb, side, v));
            m.mem.w16(rev + 8 + 2 * side as u32, v);
        }
    }
    Ok(back(m, 0))
}

/// `_spu_setReverbAttr(raw)`: reverb configuration register `i` from
/// `raw + 4 + 2*i`, for each mask bit `i` (all when 0).
fn reverb_attr(m: &mut M, raw: [u8; 68]) {
    let mask = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
    sp(m, |s| {
        for i in 0..32 {
            if mask == 0 || mask & 1 << i != 0 {
                s.set_reverb_param(i, u16::from_le_bytes([raw[4 + 2 * i], raw[5 + 2 * i]]));
            }
        }
    });
}

/// `x / 127`, as GCC's multiply by 81020409h computes it.
fn div127(x: i32) -> i32 {
    let hi = ((x as i64 * 0x8102_0409u32 as i32 as i64) >> 32) as i32;
    (hi.wrapping_add(x) >> 6) - (x >> 31)
}

/// `SpuSetReverbModeParam(attr)`: the reverb mode (its parameter set from
/// `_spu_rev_param`, the work area at `_spu_rev_startaddr`, reverb off
/// meanwhile and its depth zeroed, the area cleared with bit 100h), the
/// echo and delay modes' delay time and feedback, the depth. Returns 0, or
/// -1 for a bad mode or an area the allocator holds.
fn set_reverb_mode_param(m: &mut M, a: [u32; 4]) -> Result {
    let c = Code::here(m, 0, "libspu/s_srmp");
    let start = c.pair(m, (0x98, 0xA0), "_spu_rev_startaddr")?;
    let in_area = c.jal(m, 0x9C, "_SpuIsInAllocateArea_")?;
    let rev = c.pair(m, (0xBC, 0xC0), "_spu_rev_attr")?.wrapping_sub(4);
    let params = c.pair(m, (0xE4, 0xE8), "_spu_rev_param")?;
    let offset = c.pair(m, (0xF4, 0xF8), "_spu_rev_offsetaddr")?;
    let clear_wa = c.jal(m, 0x45C, "SpuClearReverbWorkArea")?;
    let fsetrxx = c.jal(m, 0x474, "_spu_FsetRXX")?;
    // `_spu_FsetRXX` (ESA) is the adapter's own, in `spu.o`.
    spu_o_at(m, fsetrxx.wrapping_sub(at::FSETRXX))?;
    let p = a[0];
    let mask = m.mem.r32(p);
    let all = mask == 0;
    let mut clear = false;
    if !(all || mask & 1 != 0) {
        return Ok(srmp_rest(m, Srmp { p, mask, rev, params, offset, clear_wa, set: false, clear }, [0; 68]));
    }
    let mut mode = m.mem.r32(p + 4);
    if mode & 0x100 != 0 {
        mode &= !0x100;
        clear = true;
    }
    if mode >= 10 {
        return Ok(back(m, u32::MAX));
    }
    let base = m.mem.r32(start + 4 * mode);
    Ok(call(m, in_area, [base, 0, 0, 0], move |m, held| {
        if held != 0 {
            return back(m, u32::MAX);
        }
        m.mem.w32(rev + 4, mode);
        let off = m.mem.r32(start + 4 * mode);
        m.mem.w32(offset, off);
        let buf = copy_params(m, params, mode);
        spend(
            m,
            COPY_PASSES,
            Box::new(move |m| {
                let (delay, feedback) = match mode {
                    7 => (127, 127),
                    8 => (127, 0),
                    _ => (0, 0),
                };
                m.mem.w32(rev + 16, feedback);
                m.mem.w32(rev + 12, delay);
                srmp_rest(m, Srmp { p, mask, rev, params, offset, clear_wa, set: true, clear }, buf)
            }),
        )
    }))
}

/// Mode `mode`'s 68 bytes of `_spu_rev_param` (a mask word, then the 32
/// configuration registers).
fn copy_params(m: &M, params: u32, mode: u32) -> [u8; 68] {
    let mut buf = [0u8; 68];
    m.mem.read(params + 68 * mode, &mut buf);
    buf
}

#[derive(Clone, Copy)]
struct Srmp {
    p: u32,
    mask: u32,
    rev: u32,
    params: u32,
    offset: u32,
    clear_wa: u32,
    /// The mode was set (the parameters in the buffer are its).
    set: bool,
    clear: bool,
}

fn put16(buf: &mut [u8; 68], at: usize, v: i32) {
    buf[at..at + 2].copy_from_slice(&(v as u16).to_le_bytes());
}

fn get16(buf: &[u8; 68], at: usize) -> i32 {
    u16::from_le_bytes([buf[at], buf[at + 1]]) as i32
}

/// SpuSetReverbModeParam after the mode: the delay time, the feedback,
/// the depth, then the registers.
fn srmp_rest(m: &mut M, s: Srmp, mut buf: [u8; 68]) -> F {
    let all = s.mask == 0;
    let mode = m.mem.r32(s.rev + 4);
    let echo = (7..=8).contains(&mode);
    let mut passes = 0;
    let mut delay_set = false;
    if (all || s.mask & 8 != 0) && echo {
        delay_set = true;
        if !s.set {
            buf = copy_params(m, s.params, mode);
            passes += COPY_PASSES;
            buf[0..4].copy_from_slice(&0x0C01_1C00u32.to_le_bytes());
        }
        let delay = m.mem.r32(s.p + 12);
        m.mem.w32(s.rev + 12, delay);
        let q13 = div127((delay << 13) as i32);
        let q12 = div127((delay << 12) as i32);
        for (to, from, plus, minus) in
            [(24, 4, q13, true), (26, 6, q12, true), (36, 38, q12, false), (28, 30, q12, false), (58, 62, q12, false), (56, 60, q12, false)]
        {
            let v = get16(&buf, from);
            put16(&mut buf, to, if minus { plus - v } else { v + plus });
        }
    }
    let mut feedback_set = false;
    if (all || s.mask & 0x10 != 0) && echo {
        feedback_set = true;
        if !s.set {
            if !delay_set {
                buf = copy_params(m, s.params, mode);
                passes += COPY_PASSES;
                buf[0..4].copy_from_slice(&0x80u32.to_le_bytes());
            } else {
                let w = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) | 0x80;
                buf[0..4].copy_from_slice(&w.to_le_bytes());
            }
        }
        let fb = m.mem.r32(s.p + 16);
        m.mem.w32(s.rev + 16, fb);
        put16(&mut buf, 18, div127((fb.wrapping_mul(0x8100)) as i32));
    }
    spend(
        m,
        passes,
        Box::new(move |m| {
            let mut was_on = false;
            if s.set {
                was_on = sp(m, |sp| {
                    let on = sp.control() & 0x80 != 0;
                    if on {
                        sp.set_control(sp.control() & !0x80);
                    }
                    on
                });
                sp(m, |sp| {
                    sp.set_common_volume(Volume::Reverb, 0, 0);
                    sp.set_common_volume(Volume::Reverb, 1, 0);
                });
                m.mem.w16(s.rev + 8, 0);
                m.mem.w16(s.rev + 10, 0);
            } else {
                for (bit, side) in [(2, 0usize), (4, 1)] {
                    if all || s.mask & bit != 0 {
                        let v = m.mem.r16(s.p + 8 + 2 * side as u32);
                        sp(m, |sp| sp.set_common_volume(Volume::Reverb, side, v));
                        m.mem.w16(s.rev + 8 + 2 * side as u32, v);
                    }
                }
            }
            if s.set || delay_set || feedback_set {
                reverb_attr(m, buf);
            }
            let finish = move |m: &mut M| -> F {
                if s.set {
                    let off = m.mem.r32(s.offset);
                    if let Err(e) = sp_r(m, |sp| set_index(sp, 0xD1, off as u16)) {
                        return fail(e);
                    }
                    if was_on {
                        sp(m, |sp| sp.set_control(sp.control() | 0x80));
                    }
                }
                back(m, 0)
            };
            if s.clear {
                let mode = m.mem.r32(s.rev + 4);
                call(m, s.clear_wa, [mode, 0, 0, 0], move |m, _| finish(m))
            } else {
                finish(m)
            }
        }),
    )
}

// --- Common and voice attributes ---------------------------------------

/// A volume's register value from an attribute's level and mode: mode 0
/// (direct) the level's 15 bits; modes 1-7 a sweep (8000h + 1000h per
/// mode) whose rate is the level, clamped to 0-127.
fn volume_value(level: u16, mode: Option<i32>) -> u16 {
    let sweep = match mode {
        Some(m @ 1..=7) => 0x8000 + 0x1000 * (m as u32 - 1),
        _ => 0,
    };
    if sweep == 0 {
        return level & 0x7FFF;
    }
    let l = level as i16;
    let rate = if l >= 128 {
        127
    } else if l < 0 {
        0
    } else {
        level
    };
    (rate & 0x7FFF) | sweep as u16
}

/// `SpuSetCommonAttr(attr)`: main volume and its mode, CD and external
/// input volumes, reverb and enable switches, by mask (all when 0).
/// Returns SPUCNT as last written when the external enable is in the
/// mask (or the mask is 0), else 0.
fn set_common_attr(m: &mut M, a: [u32; 4]) -> Result {
    let p = a[0];
    let mask = m.mem.r32(p);
    let all = mask == 0;
    let has = |b: u32| all || mask & b != 0;
    let r16 = |m: &M, o: u32| m.mem.r16(p + o);
    let r32 = |m: &M, o: u32| m.mem.r32(p + o);
    let mut v0 = 0u32;
    for (side, (vol, mode_bit)) in [(1u32, 4u32), (2, 8)].into_iter().enumerate() {
        if has(vol) {
            // Mode 0 and modes past 7 are direct.
            let mode = has(mode_bit).then(|| r16(m, 8 + 2 * side as u32) as i16 as i32);
            let value = volume_value(r16(m, 4 + 2 * side as u32), mode);
            sp(m, |s| s.set_common_volume(Volume::Main, side, value));
        }
    }
    for (bit, which, side, off) in [(0x40, Volume::Cd, 0, 16), (0x80, Volume::Cd, 1, 18), (0x400, Volume::External, 0, 28), (0x800, Volume::External, 1, 30)] {
        if has(bit) {
            let v = r16(m, off);
            sp(m, |s| s.set_common_volume(which, side, v));
        }
    }
    for (bit, cnt, off) in [(0x100u32, 0x4u16, 20u32), (0x200, 0x1, 24), (0x1000, 0x8, 32), (0x2000, 0x2, 36)] {
        if has(bit) {
            let on = r32(m, off) != 0;
            let c = sp(m, |s| {
                let c = if on { s.control() | cnt } else { s.control() & !cnt };
                s.set_control(c);
                c
            });
            if bit == 0x2000 {
                v0 = c as u32;
            }
        }
    }
    Ok(back(m, v0))
}

/// `SpuSetVoiceAttr(attr)`: for each voice in `attr->voice`, by mask (all
/// when 0): pitch, centre note and note (through `_spu_note2pitch`),
/// volumes and their modes, start and repeat addresses (as SPU RAM
/// addresses), ADSR whole, then attack, decay, sustain and release rates
/// and modes and the sustain level into it. Then libspu's short delay.
fn set_voice_attr(m: &mut M, a: [u32; 4]) -> Result {
    let c = Code::here(m, 0, "libspu/s_sva");
    let centre = c.pair(m, (0x2C, 0x30), "_spu_voice_centerNote")?;
    let n2p = c.jal(m, 0xBC, "_spu_note2pitch")?;
    let rxxa = c.jal(m, 0x2AC, "_spu_FsetRXXa")?;
    let o = spu_o_at(m, rxxa.wrapping_sub(at::FSETRXXA))?;
    let sva = Sva { p: a[0], centre, n2p, o };
    Ok(sva_voice(m, sva, 0))
}

#[derive(Clone, Copy)]
struct Sva {
    p: u32,
    centre: u32,
    n2p: u32,
    o: SpuO,
}

fn sva_voice(m: &mut M, s: Sva, v: u32) -> F {
    if v == 24 {
        return spend(m, 2, Box::new(|m| back(m, 0)));
    }
    spend(
        m,
        1,
        Box::new(move |m| {
            if m.mem.r32(s.p) & 1 << v == 0 {
                return sva_voice(m, s, v + 1);
            }
            let mask = m.mem.r32(s.p + 4);
            let all = mask == 0;
            let has = move |b: u32| all || mask & b != 0;
            let vu = v as usize;
            if has(0x10) {
                let pitch = m.mem.r16(s.p + 20);
                sp(m, |sp| sp.set_voice_pitch(vu, pitch));
            }
            if has(0x40) {
                let n = m.mem.r16(s.p + 24);
                m.mem.w16(s.centre + 2 * v, n);
            }
            if has(0x20) {
                let cn = m.mem.r16(s.centre + 2 * v) as u32;
                let note = m.mem.r16(s.p + 22) as u32;
                return call(m, s.n2p, [cn >> 8, cn & 0xFF, note >> 8, note & 0xFF], move |m, pitch| {
                    sp(m, |sp| sp.set_voice_pitch(vu, pitch as u16));
                    sva_rest(m, s, v)
                });
            }
            sva_rest(m, s, v)
        }),
    )
}

fn sva_rest(m: &mut M, s: Sva, v: u32) -> F {
    let p = s.p;
    let mask = m.mem.r32(p + 4);
    let all = mask == 0;
    let has = |b: u32| all || mask & b != 0;
    let vu = v as usize;
    for (side, (vol, mode_bit)) in [(1u32, 4u32), (2, 8)].into_iter().enumerate() {
        if has(vol) {
            let level = m.mem.r16(p + 8 + 2 * side as u32);
            // Modes outside 1-7 (the original's `mode - 1 < 7`) are direct.
            let mode = has(mode_bit).then(|| m.mem.r16(p + 12 + 2 * side as u32) as i32);
            let value = volume_value(level, mode);
            sp(m, |sp| sp.set_voice_volume(vu, side, value));
        }
    }
    let r = (|| -> std::result::Result<(), String> {
        if has(0x80) {
            let addr = m.mem.r32(p + 28);
            fsetrxxa(m, s.o, v * 8 + 3, addr)?;
        }
        if has(0x1_0000) {
            let addr = m.mem.r32(p + 32);
            fsetrxxa(m, s.o, v * 8 + 7, addr)?;
        }
        Ok(())
    })();
    if let Err(e) = r {
        return fail(e);
    }
    if has(0x2_0000) {
        let x = m.mem.r16(p + 58);
        sp(m, |sp| sp.set_voice_adsr1(vu, x));
    }
    if has(0x4_0000) {
        let x = m.mem.r16(p + 60);
        sp(m, |sp| sp.set_voice_adsr2(vu, x));
    }
    let adsr = |m: &mut M, second: bool, f: &dyn Fn(u16) -> u16| {
        sp(m, |sp| {
            let (a1, a2) = sp.voice_adsr(vu);
            if second {
                sp.set_voice_adsr2(vu, f(a2));
            } else {
                sp.set_voice_adsr1(vu, f(a1));
            }
        })
    };
    if has(0x800) {
        let ar = m.mem.r16(p + 48).min(127);
        let exp = if has(0x100) && m.mem.r32(p + 36) == 5 { 0x80 } else { 0 };
        adsr(m, false, &|x| (x & 0xFF) | (ar | exp) << 8);
    }
    if has(0x1000) {
        let dr = m.mem.r16(p + 50).min(15);
        adsr(m, false, &|x| (x & 0xFF0F) | dr << 4);
    }
    if has(0x2000) {
        let sr = m.mem.r16(p + 52).min(127);
        let dir = if has(0x200) {
            match m.mem.r32(p + 40) {
                5 => 0x200,
                1 => 0,
                7 => 0x300,
                _ => 0x100,
            }
        } else {
            0x100
        };
        adsr(m, true, &|x| (x & 0x3F) | (sr | dir) << 6);
    }
    if has(0x4000) {
        let rr = m.mem.r16(p + 54).min(31);
        let exp = if has(0x400) && m.mem.r32(p + 44) == 7 { 0x20 } else { 0 };
        adsr(m, true, &|x| (x & 0xFFC0) | rr | exp);
    }
    if has(0x8000) {
        let sl = m.mem.r16(p + 56).min(15);
        adsr(m, false, &|x| (x & 0xFFF0) | sl);
    }
    sva_voice(m, s, v + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divide_by_127_as_gcc_does() {
        for x in [0i32, 1, 126, 127, 128, 127 << 13, 100 << 13, 33 << 12, -5, 0x7FFF * 0x8100, i32::MAX] {
            assert_eq!(div127(x), x / 127, "{x}");
        }
    }

    #[test]
    fn volume_values() {
        assert_eq!(volume_value(0x3FFF, None), 0x3FFF);
        assert_eq!(volume_value(0xFFFF, Some(0)), 0x7FFF);
        assert_eq!(volume_value(200, Some(1)), 0x8000 | 127);
        assert_eq!(volume_value(0xFFFF, Some(7)), 0xE000);
        assert_eq!(volume_value(40, Some(3)), 0xA000 | 40);
    }

    #[test]
    fn registers_by_index_round_trip() {
        let mut s = Spu::new();
        for idx in (0..0xC0).filter(|i| i % 8 != 6).chain([0xC0, 0xC2, 0xC8, 0xCA, 0xCC, 0xD1, 0xD2, 0xD3, 0xD6, 0xD8, 0xDA, 0xE0, 0xFF]) {
            set_index(&mut s, idx, 0x1234).unwrap();
            let want = if idx >= 0xC8 && idx < 0xCE && idx & 1 == 1 { 0x34 } else { 0x1234 };
            assert_eq!(get_index(&s, idx).unwrap(), if idx == 0xC8 || idx == 0xCA || idx == 0xCC { 0x1234 } else { want }, "{idx:#x}");
        }
        assert!(set_index(&mut s, 0xD0, 0).is_err());
    }
}
